//! hyper-swim measured against the detector it replaces, on one workload, the same hardware and
//! recorded commands (hyper-raft `CLAUDE.md` §1a, `docs/benchmarks.md`):
//!
//! ```text
//! hyper-swim-compare one <hyper|slates> <members> <quiet|churning> <periods>
//! hyper-swim-compare table <runs> <periods> [members]
//! ```
//!
//! The workload is `crates/hyper-swim/benches/allocs.rs`'s: `N` detectors in one process run
//! whole periods as a member's driver does. Each ticks, sends its probe target a ping carrying
//! gossip, the target applies it and answers with an acknowledgement carrying its own gossip and
//! coordinate, and the prober applies that, credits the probe and folds the round trip into its
//! coordinate. Every message goes through the wire codec. Quiet has no membership changes;
//! churning has one member refute a suspicion every period, so its new incarnation spreads.
//!
//! - **hyper**: hyper-swim, reusing its gossip batch and encode buffers across periods.
//! - **slates**: slates-cluster's `Detector` and `SwimMessage` at `5cce86a`, the source hyper-swim
//!   was ported from, driven through its own API, which hands out owned batches and messages.
//!
//! `one` runs one point in this process and prints it as one line: nanoseconds and allocations
//! per member per period. `table` runs every point of each detector in a fresh process of its own,
//! the detectors in a rotated order each run, and prints a Markdown table of each row's medians,
//! with the least and the most.
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

mod hyper;
mod slates;

use std::process::Command;
use std::time::Instant;

use hyper_measure::alloc::{self, Counting};
use hyper_measure::stats;

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// The detectors, by the name the command line gives them.
const DETECTORS: [&str; 2] = ["hyper", "slates"];
/// Gossip entries a message carries at most, as hyper-swim's `tests/cluster.rs` sends.
pub const GOSSIP_PER_MESSAGE: usize = 8;
/// A round trip to fold into the coordinate, in seconds: any constant serves, the work is the
/// same.
pub const RTT: f64 = 0.000_2;

/// The transmit budget λ·ln(n+1) for λ = 3 (SWIM §4.4).
pub fn transmits(members: usize) -> u32 {
    (3.0 * ((members + 1) as f64).ln()).ceil() as u32
}

/// Periods run before measuring: twice what the joins take to drain. Each member holds a report
/// of every member, sent `transmits` times, and sends at most two batches a period.
pub fn warm(members: usize) -> u64 {
    let reports = (members * transmits(members) as usize) as u64;
    2 * reports.div_ceil(2 * GOSSIP_PER_MESSAGE as u64)
}

/// A detector cluster the comparison drives.
pub trait Cluster {
    fn new(members: usize) -> Self;
    /// One period of every member, with probe token `nonce`.
    fn period(&mut self, nonce: u64);
    /// Has the member `at` picks hear itself suspected, so it refutes.
    fn churn(&mut self, at: u64);
    /// Members every detector holds alive.
    fn alive(&self) -> usize;
}

/// What one point measured, per member per period.
#[derive(Clone, Copy, Debug, Default)]
struct Point {
    ns: f64,
    allocs: f64,
}

fn measure<C: Cluster>(members: usize, churning: bool, periods: u64) -> Point {
    let mut cluster = C::new(members);
    let mut nonce = 0;
    for _ in 0..warm(members) {
        nonce += 1;
        cluster.period(nonce);
    }
    let run = |cluster: &mut C, nonce: &mut u64| {
        for _ in 0..periods {
            *nonce += 1;
            if churning {
                cluster.churn(*nonce);
            }
            cluster.period(*nonce);
        }
    };
    let start = Instant::now();
    run(&mut cluster, &mut nonce);
    let elapsed = start.elapsed().as_nanos() as f64;
    alloc::begin();
    run(&mut cluster, &mut nonce);
    let counts = alloc::end();
    assert_eq!(cluster.alive(), members, "every member stays alive");
    let n = (periods * members as u64) as f64;
    Point {
        ns: elapsed / n,
        allocs: counts.allocations as f64 / n,
    }
}

fn one(args: &[String]) {
    let members: usize = args[1].parse().unwrap();
    let churning = match args[2].as_str() {
        "quiet" => false,
        "churning" => true,
        other => panic!("no workload named {other}"),
    };
    let periods: u64 = args[3].parse().unwrap();
    assert!(alloc::installed(), "the counting allocator is installed");
    let point = match args[0].as_str() {
        "hyper" => measure::<hyper::Members>(members, churning, periods),
        "slates" => measure::<slates::Members>(members, churning, periods),
        other => panic!("no detector named {other}"),
    };
    println!("{} {}", point.ns, point.allocs);
}

fn run_one(detector: &str, members: usize, workload: &str, periods: u64) -> Point {
    let out = Command::new(std::env::current_exe().unwrap())
        .args([
            "one",
            detector,
            &members.to_string(),
            workload,
            &periods.to_string(),
        ])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().last().unwrap_or("");
    let mut fields = line.split_whitespace().map(str::parse::<f64>);
    match (fields.next(), fields.next()) {
        (Some(Ok(ns)), Some(Ok(allocs))) => Point { ns, allocs },
        _ => panic!(
            "{detector} printed {text} {}",
            String::from_utf8_lossy(&out.stderr)
        ),
    }
}

/// A column's median with its least and most, as `median (least–most)`.
fn cell(samples: &[f64], digits: usize) -> String {
    let summary = stats::Summary::of(samples).unwrap();
    format!(
        "{:.digits$} ({:.digits$}–{:.digits$})",
        summary.median, summary.min, summary.max
    )
}

fn table(args: &[String]) {
    let runs: usize = args[0].parse().unwrap();
    let periods: u64 = args[1].parse().unwrap();
    let sizes: Vec<usize> = args.get(2).map_or_else(
        || vec![16, 64, 256],
        |list| list.split(',').map(|s| s.parse().unwrap()).collect(),
    );
    println!("{runs} runs of {periods} periods, each in a fresh process; medians (least–most).");
    println!();
    println!(
        "| Members | Workload | Detector | ns a member a period | Allocations a member a period |"
    );
    println!("|---|---|---|---|---|");
    for members in sizes {
        for workload in ["quiet", "churning"] {
            let mut points: Vec<Vec<Point>> = vec![Vec::new(); DETECTORS.len()];
            for run in 0..runs {
                for turn in 0..DETECTORS.len() {
                    let index = (run + turn) % DETECTORS.len();
                    points[index].push(run_one(DETECTORS[index], members, workload, periods));
                }
            }
            for (index, detector) in DETECTORS.iter().enumerate() {
                let ns: Vec<f64> = points[index].iter().map(|p| p.ns).collect();
                let allocs: Vec<f64> = points[index].iter().map(|p| p.allocs).collect();
                println!(
                    "| {members} | {workload} | {detector} | {} | {} |",
                    cell(&ns, 0),
                    cell(&allocs, 2),
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
        _ => panic!(
            "usage: one <hyper|slates> <members> <quiet|churning> <periods> | table <runs> <periods> [members]"
        ),
    }
}
