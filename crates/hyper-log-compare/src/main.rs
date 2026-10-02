//! hyper-log measured against the logs it replaces, on one workload, the same hardware and
//! recorded commands (hyper-raft `CLAUDE.md` §1a, `docs/benchmarks.md`):
//!
//! ```text
//! hyper-log-compare one <hyper|focal> <dir> <entry bytes> <replicas> <seconds> [count]
//! hyper-log-compare table <dir> <rounds> <seconds> <mantle binary> [sizes] [replicas]
//! hyper-log-compare pairs <dir> <rounds> <seconds> <sizes> <replicas> <run>...
//! hyper-log-compare replica <hyper|mantle>
//! hyper-log-compare replicas <rounds>
//! ```
//!
//! The workload is mantle's `mantle bench log` (mantle `crates/mantle/src/bench_log.rs` at
//! `147f035`): closed-loop replicas, each appending one entry to its own group and waiting for
//! it to be durable before the next, held as records by at most a granted core's worth of
//! driver threads that hear of each answer through the replica's waker; every replica keeps 64
//! entries behind its last. Each point ends with the log reopened, timed, and every replica's
//! kept entries read back.
//!
//! - **hyper**: hyper-log, this repository's.
//! - **mantle**: mantle-log at mantle `147f035`, through mantle's own binary, `mantle bench log
//!   <dir> --seconds <s> --sizes <bytes> --replicas <n> --skip-device`.
//! - **mantle-log**: mantle-log at mantle `a2021df` in this process, driven exactly as hyper-log
//!   is (`mantle.rs`), so that the two differ only in the log.
//! - **focal**: focal-log's `SharedWal` at focal `4bf7b64`, each replica a `WalLease` of its own
//!   group appending with `append_async_notified`, its notification the waker. focal keeps a
//!   group's window by a checkpoint (`rewrite_checkpoint_async_notified`) of the entries it
//!   keeps, where mantle and hyper-log write a start: every 64th append is that checkpoint of the
//!   64 entries the replica keeps, the cadence note 32 §3.9 asks the two to be compared at.
//!
//! `one` runs a single point in this process and prints it as one line; with `count` it counts
//! the process's allocations over the appends instead of timing them. `table` runs every point
//! of every log in a fresh process of its own, the logs in a rotated order each round so that
//! drift in the device and the machine falls on each alike, and prints a Markdown table of each
//! row's medians, with the least and the most. `pairs` runs each point of each `run` (a log of
//! this binary, `hyper` or `mantle-log`, or `label=binary:log` for a log of another build of this
//! tool) in a fresh process, the runs in a rotated order each round, prints each run with the load
//! average read just before it, and then each run's medians and ranges and, paired by round, the
//! rounds in which the first run made more appends a second than each other.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::disallowed_macros,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    missing_docs
)]

mod focal;
mod hyper;
mod mantle;
mod replica;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use hyper_measure::alloc::{self, Counting};

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Entries a replica keeps behind its last, as mantle's bench does.
pub const KEEP: u64 = 64;

/// What one point measured.
#[derive(Debug, Clone, Copy, Default)]
pub struct Point {
    pub appends_per_s: f64,
    pub p50_ns: u64,
    pub p99_ns: u64,
    pub p999_ns: u64,
    pub per_flush: f64,
    pub reopen_ns: u64,
    pub read: u64,
    pub threads: usize,
    /// Allocations per append, every thread's, in a counting run.
    pub allocs: f64,
    /// The file's flushes a second while the appends ran, frames' and confirmations', and the
    /// share of that time spent in them (`Flushes`).
    pub syncs_per_s: f64,
    pub busy: f64,
}

impl Point {
    fn line(&self) -> String {
        format!(
            "{} {} {} {} {} {} {} {} {} {} {}",
            self.appends_per_s,
            self.p50_ns,
            self.p99_ns,
            self.p999_ns,
            self.per_flush,
            self.reopen_ns,
            self.read,
            self.threads,
            self.allocs,
            self.syncs_per_s,
            self.busy
        )
    }

    fn parse(line: &str) -> Option<Self> {
        let f: Vec<&str> = line.split_whitespace().collect();
        Some(Self {
            appends_per_s: f.first()?.parse().ok()?,
            p50_ns: f.get(1)?.parse().ok()?,
            p99_ns: f.get(2)?.parse().ok()?,
            p999_ns: f.get(3)?.parse().ok()?,
            per_flush: f.get(4)?.parse().ok()?,
            reopen_ns: f.get(5)?.parse().ok()?,
            read: f.get(6)?.parse().ok()?,
            threads: f.get(7)?.parse().ok()?,
            allocs: f.get(8)?.parse().ok()?,
            syncs_per_s: f.get(9)?.parse().ok()?,
            busy: f.get(10)?.parse().ok()?,
        })
    }
}

/// The flushes of a log's file in this process and the nanoseconds spent in them: each log's
/// driver wraps its file to count them (`hyper.rs`, `mantle.rs`).
pub static FLUSHES: AtomicU64 = AtomicU64::new(0);
pub static FLUSH_NS: AtomicU64 = AtomicU64::new(0);

/// Runs a flush, counting it and its time.
pub fn counted_flush<E>(flush: impl FnOnce() -> Result<(), E>) -> Result<(), E> {
    let started = Instant::now();
    let result = flush();
    FLUSH_NS.fetch_add(started.elapsed().as_nanos() as u64, Ordering::Relaxed);
    FLUSHES.fetch_add(1, Ordering::Relaxed);
    result
}

/// The flushes a second and the share of `elapsed` spent flushing since the counts were zeroed.
pub fn flushes(elapsed: Duration) -> (f64, f64) {
    let secs = elapsed.as_secs_f64();
    (
        FLUSHES.load(Ordering::Relaxed) as f64 / secs,
        FLUSH_NS.load(Ordering::Relaxed) as f64 / 1e9 / secs,
    )
}

/// Each append's latency, sorted: its quantile `p`.
pub fn quantile(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let at = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[at.min(sorted.len() - 1)]
}

/// Driver threads for `replicas`: the granted cores, and no more than the replicas.
pub fn drivers(replicas: usize) -> usize {
    std::thread::available_parallelism()
        .map_or(1, std::num::NonZeroUsize::get)
        .min(replicas)
        .max(1)
}

/// Runs `appends` under a process count when `count`, giving its allocations per append.
pub fn counted(count: bool, appends: impl FnOnce() -> u64) -> f64 {
    if !count {
        appends();
        return 0.0;
    }
    alloc::begin_process();
    let n = appends();
    let counts = alloc::end_process();
    counts.allocations as f64 / n.max(1) as f64
}

fn one(args: &[String]) {
    let log = args[0].as_str();
    let dir = PathBuf::from(&args[1]);
    let size: usize = args[2].parse().unwrap();
    let replicas: usize = args[3].parse().unwrap();
    let step = Duration::from_secs_f64(args[4].parse().unwrap());
    let count = args.get(5).is_some_and(|a| a == "count");
    let point = match log {
        "hyper" => hyper::point(&dir, size, replicas, step, count),
        "focal" => focal::point(&dir, size, replicas, step, count),
        "mantle-log" => mantle::point(&dir, size, replicas, step, count),
        other => panic!("no log named {other}"),
    };
    println!("{}", point.line());
}

fn run_one(
    log: &str,
    dir: &Path,
    size: usize,
    replicas: usize,
    seconds: f64,
    count: bool,
) -> Point {
    run_with(
        &std::env::current_exe().unwrap(),
        log,
        dir,
        size,
        replicas,
        seconds,
        count,
    )
}

fn run_with(
    me: &Path,
    log: &str,
    dir: &Path,
    size: usize,
    replicas: usize,
    seconds: f64,
    count: bool,
) -> Point {
    let mut command = Command::new(me);
    command.args([
        "one",
        log,
        &dir.display().to_string(),
        &size.to_string(),
        &replicas.to_string(),
        &seconds.to_string(),
    ]);
    if count {
        command.arg("count");
    }
    let out = command.output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    Point::parse(text.lines().last().unwrap_or("")).unwrap_or_else(|| {
        panic!(
            "{log} printed {text} {}",
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

/// A duration as mantle's display prints it: `950 ns`, `13.5 µs`, `21.5 ms`, `1.20 s`.
fn mantle_nanos(value: &str, unit: &str) -> Option<u64> {
    let v: f64 = value.parse().ok()?;
    let scale = match unit {
        "ns" => 1.0,
        "µs" => 1e3,
        "ms" => 1e6,
        "s" => 1e9,
        _ => return None,
    };
    Some((v * scale) as u64)
}

/// A count as mantle's display prints it: `950`, `13.5K`, `1.02M`.
fn mantle_count(value: &str) -> Option<f64> {
    let (number, scale) = match value.chars().last()? {
        'K' => (&value[..value.len() - 1], 1e3),
        'M' => (&value[..value.len() - 1], 1e6),
        'G' => (&value[..value.len() - 1], 1e9),
        _ => (value, 1.0),
    };
    Some(number.parse::<f64>().ok()? * scale)
}

/// One point of mantle's own `mantle bench log`, parsed from its row: `append 128 B, replicas,
/// threads, cpu (value unit), appends/s, throughput (value unit), p50, p99, p99.9 (value unit
/// each), per flush, reopen (value unit), read back (n in value unit)`.
fn run_mantle(mantle: &Path, dir: &Path, size: usize, replicas: usize, seconds: f64) -> Point {
    let out = Command::new(mantle)
        .args([
            "bench",
            "log",
            &dir.display().to_string(),
            "--seconds",
            &seconds.to_string(),
            "--sizes",
            &size.to_string(),
            "--replicas",
            &replicas.to_string(),
            "--skip-device",
        ])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    let row = text
        .lines()
        .find(|l| l.trim_start().starts_with("append "))
        .unwrap_or_else(|| panic!("mantle printed {text}"));
    let f: Vec<&str> = row.split_whitespace().collect();
    // append, size, unit, replicas, threads, cpu, unit, appends/s, rate, unit... : the fields
    // from the size's unit on.
    let after = 3;
    Point {
        threads: f[after + 1].parse().unwrap(),
        appends_per_s: mantle_count(f[after + 4]).unwrap(),
        p50_ns: mantle_nanos(f[after + 7], f[after + 8]).unwrap(),
        p99_ns: mantle_nanos(f[after + 9], f[after + 10]).unwrap(),
        p999_ns: mantle_nanos(f[after + 11], f[after + 12]).unwrap(),
        per_flush: f[after + 13].parse().unwrap(),
        reopen_ns: mantle_nanos(f[after + 14], f[after + 15]).unwrap(),
        read: f[after + 16].parse().unwrap(),
        allocs: f64::NAN,
        syncs_per_s: f64::NAN,
        busy: f64::NAN,
    }
}

fn median(values: &mut [f64]) -> f64 {
    hyper_measure::stats::median(values).unwrap_or(f64::NAN)
}

fn ms(ns: f64) -> String {
    format!("{:.2}", ns / 1e6)
}

fn table(args: &[String]) {
    let dir = PathBuf::from(&args[0]);
    let rounds: usize = args[1].parse().unwrap();
    let seconds: f64 = args[2].parse().unwrap();
    let mantle = PathBuf::from(&args[3]);
    let list = |at: usize, default: &[usize]| {
        args.get(at).map_or_else(
            || default.to_vec(),
            |s| s.split(',').map(|n| n.parse().unwrap()).collect(),
        )
    };
    let sizes = list(4, &[128, 1 << 10, 16 << 10]);
    let replicas = list(5, &[1, 4, 16, 64, 256]);
    let logs = ["mantle", "hyper", "focal"];
    println!(
        "| entry | replicas | log | appends/s, median (least–most) | p50 ms | p99 ms | p99.9 ms | appends a flush | reopen ms | threads | allocations an append |"
    );
    println!("|---|---|---|---|---|---|---|---|---|---|---|");
    for &size in &sizes {
        for &n in &replicas {
            let mut runs: Vec<Vec<Point>> = vec![Vec::new(); logs.len()];
            for round in 0..rounds {
                for k in 0..logs.len() {
                    let at = (k + round) % logs.len();
                    let p = match logs[at] {
                        "mantle" => run_mantle(&mantle, &dir, size, n, seconds),
                        log => run_one(log, &dir, size, n, seconds, false),
                    };
                    runs[at].push(p);
                }
            }
            for (at, log) in logs.iter().enumerate() {
                let allocs = match *log {
                    "mantle" => "–".to_owned(),
                    log => format!("{:.2}", run_one(log, &dir, size, n, seconds, true).allocs),
                };
                let r = &runs[at];
                let mut rate: Vec<f64> = r.iter().map(|p| p.appends_per_s).collect();
                let (least, most) = rate
                    .iter()
                    .fold((f64::MAX, f64::MIN), |(a, b), &v| (a.min(v), b.max(v)));
                let mut p50: Vec<f64> = r.iter().map(|p| p.p50_ns as f64).collect();
                let mut p99: Vec<f64> = r.iter().map(|p| p.p99_ns as f64).collect();
                let mut p999: Vec<f64> = r.iter().map(|p| p.p999_ns as f64).collect();
                let mut flush: Vec<f64> = r.iter().map(|p| p.per_flush).collect();
                let mut reopen: Vec<f64> = r.iter().map(|p| p.reopen_ns as f64).collect();
                let threads = r.iter().map(|p| p.threads).max().unwrap_or(0);
                println!(
                    "| {size} B | {n} | {log} | {:.0} ({least:.0}–{most:.0}) | {} | {} | {} | {:.1} | {} | {threads} | {allocs} |",
                    median(&mut rate),
                    ms(median(&mut p50)),
                    ms(median(&mut p99)),
                    ms(median(&mut p999)),
                    median(&mut flush),
                    ms(median(&mut reopen)),
                );
            }
        }
    }
}

/// One run of the replica's path in this process, printed as one line.
fn replica_one(log: &str) {
    let run = match log {
        "hyper" => replica::run::<replica::hyper::Store>(),
        "mantle" => replica::run::<replica::mantle::Store>(),
        other => panic!("no log named {other}"),
    };
    println!("{}", run.line());
}

/// `rounds` runs of the replica's path for each log, each in a fresh process, the logs
/// alternated: the median and range of each column.
fn replicas(args: &[String]) {
    let rounds: usize = args[0].parse().unwrap();
    let logs = ["mantle", "hyper"];
    let mut runs: Vec<Vec<replica::Run>> = vec![Vec::new(); logs.len()];
    for round in 0..rounds {
        for k in 0..logs.len() {
            let at = (k + round) % logs.len();
            let me = std::env::current_exe().unwrap();
            let out = Command::new(me)
                .args(["replica", logs[at]])
                .output()
                .unwrap();
            let text = String::from_utf8_lossy(&out.stdout);
            let run = replica::Run::parse(text.lines().last().unwrap_or("")).unwrap_or_else(|| {
                panic!(
                    "{} printed {text} {}",
                    logs[at],
                    String::from_utf8_lossy(&out.stderr)
                )
            });
            eprintln!("{} {}", logs[at], run.line());
            runs[at].push(run);
        }
    }
    println!(
        "| log | wall µs a committed entry, median (least–most) | writes | views | terms | fetches | reads another thread answered | allocations | reallocations | context switches |"
    );
    println!("|---|---|---|---|---|---|---|---|---|---|");
    for (at, log) in logs.iter().enumerate() {
        let r = &runs[at];
        let mut wall: Vec<f64> = r.iter().map(|x| x.wall_ns / 1e3).collect();
        let (least, most) = wall
            .iter()
            .fold((f64::MAX, f64::MIN), |(a, b), &v| (a.min(v), b.max(v)));
        let col = |f: &dyn Fn(&replica::Run) -> f64| {
            let mut v: Vec<f64> = r.iter().map(f).collect();
            median(&mut v)
        };
        let calls: Vec<f64> = (0..5).map(|i| col(&|x| x.calls[i])).collect();
        let allocations = col(&|x| x.allocations);
        let reallocations = col(&|x| x.reallocations);
        let switches = col(&|x| x.switches);
        println!(
            "| {log} | {:.0} ({least:.0}–{most:.0}) | {:.2} | {:.2} | {:.2} | {:.2} | {:.2} | {allocations:.1} | {reallocations:.1} | {switches:.1} |",
            median(&mut wall),
            calls[0],
            calls[1],
            calls[2],
            calls[3],
            calls[4],
        );
    }
}

/// The one-minute load average, read just before a run: `vm.loadavg` on macOS, `/proc/loadavg`
/// on Linux.
fn load() -> f64 {
    let text = std::fs::read_to_string("/proc/loadavg").ok().or_else(|| {
        let out = Command::new("sysctl")
            .args(["-n", "vm.loadavg"])
            .output()
            .ok()?;
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    });
    text.and_then(|t| t.split_whitespace().find_map(|f| f.parse::<f64>().ok()))
        .unwrap_or(f64::NAN)
}

/// One run of `pairs`: its label, the binary and the log.
struct Run {
    label: String,
    binary: PathBuf,
    log: String,
}

fn parse_run(spec: &str) -> Run {
    match spec.split_once('=') {
        Some((label, rest)) => {
            let (binary, log) = rest.rsplit_once(':').unwrap();
            Run {
                label: label.to_owned(),
                binary: PathBuf::from(binary),
                log: log.to_owned(),
            }
        }
        None => Run {
            label: spec.to_owned(),
            binary: std::env::current_exe().unwrap(),
            log: spec.to_owned(),
        },
    }
}

fn pairs(args: &[String]) {
    let dir = PathBuf::from(&args[0]);
    let rounds: usize = args[1].parse().unwrap();
    let seconds: f64 = args[2].parse().unwrap();
    let list = |s: &str| -> Vec<usize> { s.split(',').map(|n| n.parse().unwrap()).collect() };
    let sizes = list(&args[3]);
    let replicas = list(&args[4]);
    let runs: Vec<Run> = args[5..].iter().map(|s| parse_run(s)).collect();
    for &size in &sizes {
        for &n in &replicas {
            // Each run's points and loads, by round.
            let mut got: Vec<Vec<(Point, f64)>> = vec![Vec::new(); runs.len()];
            for round in 0..rounds {
                for k in 0..runs.len() {
                    let at = (k + round) % runs.len();
                    let run = &runs[at];
                    let before = load();
                    let p = run_with(&run.binary, &run.log, &dir, size, n, seconds, false);
                    println!(
                        "round {round} {size} B x {n} {}: load {before:.1}, {:.0} appends/s, p50 {} p99 {} ms, {:.1} a frame, {:.1} frames/s, {:.1} file flushes/s, {:.0}% flushing",
                        run.label,
                        p.appends_per_s,
                        ms(p.p50_ns as f64),
                        ms(p.p99_ns as f64),
                        p.per_flush,
                        p.appends_per_s / p.per_flush.max(1.0),
                        p.syncs_per_s,
                        p.busy * 100.0,
                    );
                    got[at].push((p, before));
                }
            }
            println!(
                "| entry | replicas | run | load | appends/s, median (least–most) | p50 ms | p99 ms | appends a frame (least–most) | file flushes/s | flushing | more appends/s than the first, by round |"
            );
            println!("|---|---|---|---|---|---|---|---|---|---|---|");
            for (at, run) in runs.iter().enumerate() {
                let r = &got[at];
                let spread = |f: &dyn Fn(&Point) -> f64| {
                    let mut v: Vec<f64> = r.iter().map(|(p, _)| f(p)).collect();
                    let least = v.iter().copied().fold(f64::MAX, f64::min);
                    let most = v.iter().copied().fold(f64::MIN, f64::max);
                    (median(&mut v), least, most)
                };
                let loads = r.iter().map(|(_, l)| *l);
                let (lo, hi) = loads.fold((f64::MAX, f64::MIN), |(a, b), l| (a.min(l), b.max(l)));
                let (rate, rl, rh) = spread(&|p| p.appends_per_s);
                let (p50, _, _) = spread(&|p| p.p50_ns as f64);
                let (p99, _, _) = spread(&|p| p.p99_ns as f64);
                let (flush, fl, fh) = spread(&|p| p.per_flush);
                let (syncs, _, _) = spread(&|p| p.syncs_per_s);
                let (busy, _, _) = spread(&|p| p.busy * 100.0);
                let wins = if at == 0 {
                    "–".to_owned()
                } else {
                    let first = &got[0];
                    let more = first
                        .iter()
                        .zip(r)
                        .filter(|((a, _), (b, _))| b.appends_per_s > a.appends_per_s)
                        .count();
                    format!("{more}/{}", r.len())
                };
                println!(
                    "| {size} B | {n} | {} | {lo:.1}–{hi:.1} | {rate:.0} ({rl:.0}–{rh:.0}) | {} | {} | {flush:.1} ({fl:.1}–{fh:.1}) | {syncs:.0} | {busy:.0}% | {wins} |",
                    run.label,
                    ms(p50),
                    ms(p99),
                );
            }
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("one") => one(&args[1..]),
        Some("table") => table(&args[1..]),
        Some("pairs") => pairs(&args[1..]),
        Some("replica") => replica_one(&args[1]),
        Some("replicas") => replicas(&args[1..]),
        _ => panic!("hyper-log-compare one|table ..."),
    }
}
