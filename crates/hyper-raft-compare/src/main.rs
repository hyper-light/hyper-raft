//! hyper-raft measured against each core it replaces, on the same workloads, the same
//! hardware and recorded commands (hyper-raft `CLAUDE.md` §1a, `docs/benchmarks.md`).
//!
//! ```text
//! hyper-raft-compare one <core> <workload> <voters> <batch> <bytes> <rounds> <seed>
//! hyper-raft-compare table <runs> [<workload>...]
//! hyper-raft-compare sweep <runs> <bytes>
//! ```
//!
//! `one` runs a single measurement in this process and prints it as one line. `table` and
//! `sweep` run every measurement in a fresh process of their own, the cores interleaved run by
//! run, so that no core inherits another's heap or page tables, and print a Markdown table.
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

mod core;
mod family;
mod slates;
mod workload;

use std::process::Command;

use hyper_measure::{alloc::Counting, stats};
use workload::{Measured, Spec, Workload};

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// The cores, by the name the command line gives them.
const CORES: [&str; 6] = [
    "hyper",
    "control",
    "mantle",
    "slates",
    "slates-core",
    "raftrs",
];

fn measure(core: &str, spec: &Spec, seed: u64) -> Option<Measured> {
    match core {
        "hyper" => workload::run::<family::hyper::Node>(spec, seed),
        "hyper-copy" => workload::run::<family::hyper_copy::Node>(spec, seed),
        "control" => workload::run::<family::control::Node>(spec, seed),
        "mantle" => workload::run::<family::mantle::Node>(spec, seed),
        "slates" => workload::run::<slates::Node<true>>(spec, seed),
        "slates-core" => workload::run::<slates::Node<false>>(spec, seed),
        "raftrs" => workload::run::<family::raftrs::Node>(spec, seed),
        _ => panic!("no core named {core}"),
    }
}

fn label(core: &str) -> &'static str {
    use crate::core::Core;
    match core {
        "hyper" => family::hyper::Node::NAME,
        "hyper-copy" => family::hyper_copy::Node::NAME,
        "control" => family::control::Node::NAME,
        "mantle" => family::mantle::Node::NAME,
        "slates" => slates::Node::<true>::NAME,
        "slates-core" => slates::Node::<false>::NAME,
        "raftrs" => family::raftrs::Node::NAME,
        _ => panic!("no core named {core}"),
    }
}

/// One run's numbers, as `one` prints them and `table` reads them back.
#[derive(Clone, Copy, Debug, Default)]
struct Row {
    ops: f64,
    ns: f64,
    allocs: f64,
    reallocs: f64,
    bytes: f64,
    total_allocs: f64,
    total_reallocs: f64,
    total_bytes: f64,
    peak: f64,
    minor: f64,
    major: f64,
    task: f64,
}

impl Row {
    fn of(measured: &Measured) -> Self {
        let core = measured.total.less(&measured.aside);
        Self {
            ops: measured.ops as f64,
            ns: measured.elapsed.as_nanos() as f64,
            allocs: core.allocations as f64,
            reallocs: core.reallocations as f64,
            bytes: core.bytes as f64,
            total_allocs: measured.total.allocations as f64,
            total_reallocs: measured.total.reallocations as f64,
            total_bytes: measured.total.bytes as f64,
            peak: measured.total.peak as f64,
            minor: measured.faults.minor as f64,
            major: measured.faults.major.unwrap_or(0) as f64,
            task: measured
                .faults
                .task
                .map_or(f64::NAN, |task| task.faults as f64),
        }
    }
    fn print(&self) -> String {
        format!(
            "ops={} ns={} allocs={} reallocs={} bytes={} total_allocs={} total_reallocs={} total_bytes={} peak={} minor={} major={} task={}",
            self.ops,
            self.ns,
            self.allocs,
            self.reallocs,
            self.bytes,
            self.total_allocs,
            self.total_reallocs,
            self.total_bytes,
            self.peak,
            self.minor,
            self.major,
            self.task
        )
    }
    fn parse(line: &str) -> Option<Self> {
        let mut row = Self::default();
        for field in line.split_whitespace() {
            let (key, value) = field.split_once('=')?;
            let value: f64 = value.parse().ok()?;
            match key {
                "ops" => row.ops = value,
                "ns" => row.ns = value,
                "allocs" => row.allocs = value,
                "reallocs" => row.reallocs = value,
                "bytes" => row.bytes = value,
                "total_allocs" => row.total_allocs = value,
                "total_reallocs" => row.total_reallocs = value,
                "total_bytes" => row.total_bytes = value,
                "peak" => row.peak = value,
                "minor" => row.minor = value,
                "major" => row.major = value,
                "task" => row.task = value,
                _ => return None,
            }
        }
        Some(row)
    }
}

fn spec_args(spec: &Spec) -> Vec<String> {
    vec![
        spec.workload.name().to_string(),
        spec.voters.to_string(),
        spec.batch.to_string(),
        spec.bytes.to_string(),
        spec.rounds.to_string(),
    ]
}

/// Runs one measurement in fresh processes: a timed run and a counting run of the same seed,
/// whose rows are joined (time and faults from the first, counts from the second).
fn spawn(core: &str, spec: &Spec, seed: u64) -> Option<Row> {
    let timed = spawn_one(core, spec, seed, "time")?;
    let counted = spawn_one(core, spec, seed, "count")?;
    Some(Row {
        ops: timed.ops,
        ns: timed.ns,
        minor: timed.minor,
        major: timed.major,
        task: timed.task,
        ..counted
    })
}

fn spawn_one(core: &str, spec: &Spec, seed: u64, mode: &str) -> Option<Row> {
    let output = Command::new(std::env::current_exe().expect("this program"))
        .arg("one")
        .arg(core)
        .args(spec_args(spec))
        .arg(seed.to_string())
        .arg(mode)
        .output()
        .expect("a run");
    if !output.status.success() {
        panic!(
            "{core} {:?} failed: {}",
            spec,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let line = text.lines().last()?;
    if line == "unsupported" {
        return None;
    }
    Row::parse(line)
}

/// The workloads of the table: every one at three and five voters.
fn table_specs(filter: &[String], batch: usize) -> Vec<Spec> {
    let mut specs = Vec::new();
    for voters in [3, 5] {
        for (workload, batch, bytes, rounds) in [
            (Workload::Steady, 1, 64, 20_000),
            (Workload::Steady, batch, 64, 40_000 / batch),
            (Workload::Steady, 1, 4096, 10_000),
            (Workload::Steady, batch, 4096, 10_000 / batch),
            (Workload::Transfer, 1, 64, 2_000),
            (Workload::Failover, 1, 64, 200),
            (Workload::CatchUp, batch, 64, 20),
            (Workload::Snapshot, batch, 64, 100),
            (Workload::Fast, 1, 64, 10_000),
        ] {
            if !filter.is_empty() && !filter.iter().any(|name| name == workload.name()) {
                continue;
            }
            specs.push(Spec {
                workload,
                voters,
                batch,
                bytes,
                rounds,
            });
        }
    }
    specs
}

struct Aggregate {
    rows: Vec<Row>,
}

impl Aggregate {
    fn per_op(&self, field: impl Fn(&Row) -> f64) -> stats::Summary {
        let samples: Vec<f64> = self.rows.iter().map(|row| field(row) / row.ops).collect();
        stats::Summary::of(&samples).expect("runs")
    }
    fn band(&self) -> Option<(f64, f64)> {
        let samples: Vec<f64> = self.rows.iter().map(|row| row.ns / row.ops).collect();
        stats::band(&samples)
    }
}

fn describe(spec: &Spec) -> String {
    format!(
        "{} {}v b{} {}B",
        spec.workload.name(),
        spec.voters,
        spec.batch,
        spec.bytes
    )
}

fn number(value: f64) -> String {
    if value.is_nan() {
        "—".to_string()
    } else if value >= 100.0 {
        format!("{value:.0}")
    } else if value >= 10.0 {
        format!("{value:.1}")
    } else {
        format!("{value:.2}")
    }
}

fn table(runs: usize, filter: &[String], batch: usize, cores: &[&str]) {
    println!(
        "| workload | core | ns/op median [min–max] | band | ratio to hyper-raft | ops/s | allocs/op | reallocs/op | bytes/op | allocs/op whole loop | faults per 1k ops (minor+major) | Mach faults per 1k ops |"
    );
    println!("|---|---|---|---|---|---|---|---|---|---|---|---|");
    for spec in table_specs(filter, batch) {
        let mut aggregates: Vec<Option<Aggregate>> = cores
            .iter()
            .map(|_| Some(Aggregate { rows: Vec::new() }))
            .collect();
        for run in 0..runs {
            for (at, core) in cores.iter().enumerate() {
                let Some(aggregate) = aggregates[at].as_mut() else {
                    continue;
                };
                match spawn(core, &spec, 1_000 + run as u64) {
                    Some(row) => aggregate.rows.push(row),
                    None => aggregates[at] = None,
                }
            }
        }
        let hyper = aggregates
            .iter()
            .zip(cores)
            .find(|(_, core)| **core == "hyper")
            .and_then(|(aggregate, _)| aggregate.as_ref())
            .map(|aggregate| aggregate.per_op(|row| row.ns).median);
        for (aggregate, core) in aggregates.iter().zip(cores) {
            let Some(aggregate) = aggregate else {
                println!(
                    "| {} | {} | not implemented | | | | | | | | | |",
                    describe(&spec),
                    label(core)
                );
                continue;
            };
            let time = aggregate.per_op(|row| row.ns);
            let band = aggregate.band().map_or("—".to_string(), |(low, high)| {
                format!("{low:.2}–{high:.2}")
            });
            let ratio = hyper.map_or("—".to_string(), |hyper| {
                format!("{:.2}", time.median / hyper)
            });
            println!(
                "| {} | {} | {} [{}–{}] | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
                describe(&spec),
                label(core),
                number(time.median),
                number(time.min),
                number(time.max),
                band,
                ratio,
                number(1e9 / time.median),
                number(aggregate.per_op(|row| row.allocs).median),
                number(aggregate.per_op(|row| row.reallocs).median),
                number(aggregate.per_op(|row| row.bytes).median),
                number(aggregate.per_op(|row| row.total_allocs).median),
                number(1e3 * aggregate.per_op(|row| row.minor + row.major).median),
                number(1e3 * aggregate.per_op(|row| row.task).median),
            );
        }
    }
}

/// Per-entry time of the steady workload at every batch size from one to 256, for every core.
fn sweep(runs: usize, bytes: usize, cores: &[&str]) {
    println!(
        "| batch | {} |",
        cores
            .iter()
            .map(|core| label(core))
            .collect::<Vec<_>>()
            .join(" | ")
    );
    println!("|---|{}", "---|".repeat(cores.len()));
    for batch in [1usize, 2, 4, 8, 16, 32, 64, 128, 256] {
        let spec = Spec {
            workload: Workload::Steady,
            voters: 3,
            batch,
            bytes,
            rounds: (20_000 / batch).max(200),
        };
        let mut cells = Vec::new();
        let mut rows: Vec<Vec<f64>> = vec![Vec::new(); cores.len()];
        for run in 0..runs {
            for (at, core) in cores.iter().enumerate() {
                let row = spawn_one(core, &spec, 1_000 + run as u64, "time")
                    .expect("steady runs everywhere");
                rows[at].push(row.ns / row.ops);
            }
        }
        for samples in &rows {
            let summary = stats::Summary::of(samples).expect("runs");
            cells.push(format!(
                "{} [{}–{}]",
                number(summary.median),
                number(summary.min),
                number(summary.max)
            ));
        }
        println!("| {batch} | {} |", cells.join(" | "));
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cores: Vec<&str> = match std::env::var("CORES") {
        Ok(list) => CORES
            .iter()
            .copied()
            .filter(|core| list.split(',').any(|name| name == *core))
            .collect(),
        Err(_) => CORES.to_vec(),
    };
    match args.first().map(String::as_str) {
        Some("one") => {
            let core = &args[1];
            let spec = Spec {
                workload: Workload::parse(&args[2]).expect("a workload"),
                voters: args[3].parse().expect("voters"),
                batch: args[4].parse().expect("a batch"),
                bytes: args[5].parse().expect("bytes"),
                rounds: args[6].parse().expect("rounds"),
            };
            let seed: u64 = args[7].parse().expect("a seed");
            let counting = match args.get(8).map(String::as_str) {
                Some("count") => true,
                Some("time") | None => false,
                Some(other) => panic!("a run times or counts, not {other}"),
            };
            workload::COUNTING.store(counting, std::sync::atomic::Ordering::Relaxed);
            match measure(core, &spec, seed) {
                Some(measured) => println!("{}", Row::of(&measured).print()),
                None => println!("unsupported"),
            }
        }
        Some("table") => {
            let runs: usize = args[1].parse().expect("runs");
            let batch: usize = std::env::var("BATCH")
                .ok()
                .and_then(|batch| batch.parse().ok())
                .unwrap_or(16);
            table(runs, &args[2..], batch, &cores);
        }
        Some("sweep") => {
            let runs: usize = args[1].parse().expect("runs");
            let bytes: usize = args[2].parse().expect("bytes");
            sweep(runs, bytes, &cores);
        }
        _ => {
            eprintln!("usage: hyper-raft-compare one|table|sweep ...");
            std::process::exit(2);
        }
    }
}
