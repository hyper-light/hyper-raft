//! hyper-durable against mantle's own shell, on mantle's replica workload (`docs/durable.md`
//! §13): a range group of 1, 3 or 5 members, each on its own log, committing one entry at a time
//! (closed loop) through the leader, the same entries applied by mantle's engine and layer on
//! both sides.
//!
//! ```text
//! hyper-durable-compare [--members 1,3,5] [--shapes register,put] [--devices file,sim]
//!                       [--rounds R] [--entries N] [--warm W]
//! ```
//!
//! For each point, rounds alternate the two shells, the order rotated each round (mantle first
//! in even rounds, hyper-durable first in odd), so a drift of the machine's load falls on both.
//! Each run opens a fresh group on fresh logs, commits `--warm` entries, then measures `--entries`:
//! each entry's commit latency (proposal to the leader's answer), and over the run the entries a
//! second, the frames every member's log flushed, the process's allocations and reallocations
//! (every thread, the logs' included), its minor and major page faults and context switches,
//! and the threads alive at the end. Latencies are pooled over the rounds for p50, p99 and p99.9;
//! the rest are each round's per-entry rates, medians over the rounds. The load average is read
//! before and after each point.
//!
//! Devices: `file` is a real file on this machine's disk, written with direct I/O where the file
//! system takes it and flushed with the platform's full flush (`F_FULLFSYNC` on macOS); `sim` is
//! hyper-block's simulated device, whose flush costs nothing, so the shells' own costs show.
mod durable;
mod mantle;
mod workload;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use hyper_measure::alloc::{self, Counting};
use hyper_measure::faults;

use workload::Shape;

#[global_allocator]
static ALLOCATOR: Counting = Counting;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shell {
    Mantle,
    Durable,
}

impl Shell {
    fn name(self) -> &'static str {
        match self {
            Self::Mantle => "mantle 1c179e8",
            Self::Durable => "hyper-durable",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Device {
    File,
    Sim,
}

/// One run's numbers.
#[derive(Clone, Debug, Default)]
struct Run {
    latencies: Vec<Duration>,
    entries_per_second: f64,
    flushes: f64,
    allocations: f64,
    reallocations: f64,
    faults: f64,
    switches: f64,
    threads: usize,
    /// The allocations and reallocations of the driving thread alone: the shell, the core, the
    /// state machine and the store's calls, without the logs' threads.
    own_allocations: f64,
    own_reallocations: f64,
}

fn dir() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("compare-files");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn load() -> String {
    let out = std::process::Command::new("uptime").output();
    out.ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| {
            s.split("load average")
                .nth(1)
                .map(|l| l.trim_start_matches(['s', ':', ' ']).trim().to_owned())
        })
        .unwrap_or_else(|| "unknown".to_owned())
}

/// Commits `warm` then `entries` entries through `commit`; `flushes` reads the frames flushed.
fn measure(
    warm: u64,
    entries: u64,
    shape: Shape,
    mut commit: impl FnMut(&mantle_meta::wire::Entry),
    flushes: impl Fn() -> u64,
) -> Run {
    let batch: Vec<mantle_meta::wire::Entry> =
        (0..warm + entries).map(|n| shape.entry(n + 1)).collect();
    for entry in &batch[..warm as usize] {
        commit(entry);
    }
    let mut latencies = Vec::with_capacity(entries as usize);
    let flushed = flushes();
    let faults_before = faults::read().unwrap();
    let switches_before = faults::switches().unwrap_or(0);
    alloc::begin_process();
    alloc::begin();
    let started = Instant::now();
    for entry in &batch[warm as usize..] {
        let at = Instant::now();
        commit(entry);
        latencies.push(at.elapsed());
    }
    let elapsed = started.elapsed();
    let own = alloc::end();
    let counts = alloc::end_process();
    let faults = faults::read().unwrap().since(&faults_before);
    let switches = faults::switches()
        .unwrap_or(0)
        .saturating_sub(switches_before);
    let n = entries as f64;
    Run {
        latencies,
        entries_per_second: n / elapsed.as_secs_f64(),
        flushes: (flushes() - flushed) as f64 / n,
        allocations: counts.allocations as f64 / n,
        reallocations: counts.reallocations as f64 / n,
        faults: faults.total() as f64 / n,
        switches: switches as f64 / n,
        threads: hyper_block::threads::count().unwrap_or(0),
        own_allocations: own.allocations as f64 / n,
        own_reallocations: own.reallocations as f64 / n,
    }
}

fn run(
    shell: Shell,
    device: Device,
    members: u64,
    shape: Shape,
    warm: u64,
    entries: u64,
    round: u64,
) -> Run {
    let path = |id: u64| {
        dir().join(format!(
            "{}-{round}-{id}.log",
            shell.name().replace(' ', "-")
        ))
    };
    let remove = |id: u64| {
        let _ = std::fs::remove_file(path(id));
    };
    (1..=members).for_each(remove);
    let result = match (shell, device) {
        (Shell::Mantle, Device::Sim) => {
            let mut g = mantle::Group::open(members, |id| {
                mantle_hyper_block::sim::SimFile::new(
                    mantle_hyper_block::buf::Alignment::new(4096).unwrap(),
                    mantle_hyper_block::buf::Alignment::new(512).unwrap(),
                    id,
                )
                .unwrap()
            });
            measure_group(&mut g, warm, entries, shape)
        }
        (Shell::Mantle, Device::File) => {
            let mut g = mantle::Group::open(members, |id| {
                mantle_hyper_block::file::DeviceFile::open(
                    &path(id),
                    true,
                    mantle_hyper_block::file::CachingRequest::PreferDirect,
                    mantle_hyper_block::buf::Alignment::new(4096).unwrap(),
                )
                .unwrap()
            });
            measure_group(&mut g, warm, entries, shape)
        }
        (Shell::Durable, Device::Sim) => {
            let mut g = durable::Group::open(members, |id| {
                hyper_block::sim::SimFile::new(
                    hyper_block::buf::Alignment::new(4096).unwrap(),
                    hyper_block::buf::Alignment::new(512).unwrap(),
                    id,
                )
                .unwrap()
            });
            measure_durable(&mut g, warm, entries, shape)
        }
        (Shell::Durable, Device::File) => {
            let mut g = durable::Group::open(members, |id| {
                hyper_block::file::DeviceFile::open(
                    &path(id),
                    true,
                    hyper_block::file::CachingRequest::PreferDirect,
                    hyper_block::buf::Alignment::new(4096).unwrap(),
                )
                .unwrap()
            });
            measure_durable(&mut g, warm, entries, shape)
        }
    };
    (1..=members).for_each(remove);
    result
}

fn measure_group<F: mantle_hyper_block::block::BlockFile + 'static>(
    g: &mut mantle::Group<F>,
    warm: u64,
    entries: u64,
    shape: Shape,
) -> Run {
    let g = std::cell::RefCell::new(g);
    measure(
        warm,
        entries,
        shape,
        |e| g.borrow_mut().commit(e),
        || g.borrow().flushes(),
    )
}

fn measure_durable<F: hyper_block::block::BlockFile + 'static>(
    g: &mut durable::Group<F>,
    warm: u64,
    entries: u64,
    shape: Shape,
) -> Run {
    let g = std::cell::RefCell::new(g);
    let (w0, k0) = g.borrow().diagnosis();
    let r = measure(
        warm,
        entries,
        shape,
        |e| g.borrow_mut().commit(e),
        || g.borrow().flushes(),
    );
    if std::env::var_os("HYPER_DURABLE_DIAGNOSIS").is_some() {
        let (w, k) = g.borrow().diagnosis();
        let n = (warm + entries) as f64;
        println!(
            "  writes an entry: readies {:.2} empty {:.2} fenced {:.2} quiet {:.2} starts {:.2}; wakes {:.2}",
            (w.readies - w0.readies) as f64 / n,
            (w.empty - w0.empty) as f64 / n,
            (w.fenced - w0.fenced) as f64 / n,
            (w.quiet - w0.quiet) as f64 / n,
            (w.starts - w0.starts) as f64 / n,
            (k - k0) as f64 / n
        );
    }
    r
}

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    let at = ((sorted.len() as f64) * p).ceil() as usize;
    sorted[at.clamp(1, sorted.len()) - 1]
}

fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(|a, b| a.total_cmp(b));
    values.get(values.len() / 2).copied().unwrap_or(0.0)
}

fn report(shell: Shell, runs: &[Run]) -> String {
    let mut all: Vec<Duration> = runs
        .iter()
        .flat_map(|r| r.latencies.iter().copied())
        .collect();
    all.sort();
    let us = |d: Duration| d.as_secs_f64() * 1e6;
    let med = |f: fn(&Run) -> f64| median(runs.iter().map(f).collect());
    format!(
        "| {} | {:.0} | {:.0} | {:.0} | {:.0} | {:.2} | {:.1} | {:.2} | {:.2} | {:.1} | {} | {:.1} | {:.2} |",
        shell.name(),
        us(percentile(&all, 0.50)),
        us(percentile(&all, 0.99)),
        us(percentile(&all, 0.999)),
        med(|r| r.entries_per_second),
        med(|r| r.flushes),
        med(|r| r.allocations),
        med(|r| r.reallocations),
        med(|r| r.faults),
        med(|r| r.switches),
        runs.iter().map(|r| r.threads).max().unwrap_or(0),
        med(|r| r.own_allocations),
        med(|r| r.own_reallocations),
    )
}

fn list<T>(args: &[String], name: &str, default: &str, parse: impl Fn(&str) -> T) -> Vec<T> {
    let value = args
        .iter()
        .position(|a| a == name)
        .and_then(|at| args.get(at + 1))
        .map_or(default, String::as_str);
    value.split(',').map(parse).collect()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    assert!(
        alloc::installed(),
        "the counting allocator is not installed"
    );
    let members = list(&args, "--members", "1,3,5", |m| m.parse::<u64>().unwrap());
    let shapes = list(&args, "--shapes", "register,put", |s| match s {
        "register" => Shape::Register,
        "put" => Shape::Put1k,
        other => panic!("a shape {other}"),
    });
    let devices = list(&args, "--devices", "file,sim", |d| match d {
        "file" => Device::File,
        "sim" => Device::Sim,
        other => panic!("a device {other}"),
    });
    let one = |name: &str, default: u64| {
        list(&args, name, &default.to_string(), |v| {
            v.parse::<u64>().unwrap()
        })[0]
    };
    let rounds = one("--rounds", 5);
    let entries = one("--entries", 300);
    let warm = one("--warm", 50);
    println!("rounds {rounds}, {entries} entries measured after {warm}, rotated");
    for device in devices {
        for &shape in &shapes {
            for &m in &members {
                let before = load();
                let mut runs: [Vec<Run>; 2] = [Vec::new(), Vec::new()];
                for round in 0..rounds {
                    let order = if round % 2 == 0 {
                        [Shell::Mantle, Shell::Durable]
                    } else {
                        [Shell::Durable, Shell::Mantle]
                    };
                    let only = std::env::var("HYPER_DURABLE_ONLY").ok();
                    for shell in order.into_iter().filter(|s| only.as_deref().is_none_or(|o| s.name().starts_with(o))) {
                        let r = run(shell, device, m, shape, warm, entries, round);
                        runs[usize::from(shell == Shell::Durable)].push(r);
                    }
                }
                let after = load();
                println!(
                    "\n{device:?}, {} entries, {m} member(s); load {before} → {after}",
                    shape.name()
                );
                println!(
                    "| shell | p50 µs | p99 µs | p99.9 µs | entries/s | flushes/entry | allocs/entry | reallocs/entry | faults/entry | switches/entry | threads | driver's allocs/entry | driver's reallocs/entry |"
                );
                println!("|---|---|---|---|---|---|---|---|---|---|---|---|---|");
                if !runs[0].is_empty() { println!("{}", report(Shell::Mantle, &runs[0])); }
                if !runs[1].is_empty() { println!("{}", report(Shell::Durable, &runs[1])); }
            }
        }
    }
}
