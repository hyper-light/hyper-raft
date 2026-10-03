//! hyper-timing measured against the timing it replaces, operation by operation, on the same
//! hardware and recorded commands (hyper-raft `CLAUDE.md` §1a, `docs/benchmarks.md`):
//!
//! ```text
//! hyper-timing-compare one <operation> <implementation> <iterations>
//! hyper-timing-compare table <runs> <iterations>
//! ```
//!
//! The operations are what a member runs every period of a five-voter group, each fed the same
//! round trips: a sample folded into a path, a path's tail read, the election timing derived from
//! the four voter paths, the quorum priority over them, the tick pace derived from them, a round's
//! budget derived, and a follower's period of the election timer. Each implementation runs the
//! operations it has:
//!
//! - **hyper**: hyper-timing, this repository's. Its election timing from the paths is L-1's law
//!   (`docs/timing.md` §2.3): the ballot, the split-vote span's search and the timing from the
//!   link's configured detector; `ballot`, `span` and `timing` are its three steps alone, `timing`
//!   from a ballot and span held since the paths last changed. Its paths' window is derived from the link's correlation time, 50 ms
//!   as macOS measured it, at the 10 ms heartbeat: eleven. Its tails take macOS's measured
//!   granularity, 45 µs.
//!
//!   The same harness built at `35d35d8`, before L-1's election law, measures that law under the
//!   name `hyper` (`docs/benchmarks.md`, "Commands for the timing").
//! - **focal**: focal-timing at `a8e95f7`, the source of hyper-timing's paths, pace and rounds.
//! - **slates**: slates-cluster's `timing.rs` at `5cce86a`, the source of hyper-timing's election
//!   timing, priority and timer.
//!
//! `one` runs one operation in this process and prints nanoseconds and allocations per call.
//! `table` runs every pair in a fresh process of its own, the implementations in a rotated order
//! each run, and prints a Markdown table of medians, with the least and the most.
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

use std::hint::black_box;
use std::process::Command;
use std::time::{Duration, Instant};

use hyper_measure::alloc::{self, Counting};
use hyper_measure::stats;

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// The group: five voters, so four paths from each.
const VOTERS: usize = 5;
/// A period, the heartbeat: 10 ms.
const HEARTBEAT_NS: u64 = 10_000_000;
/// The round trips the paths are fed, nanoseconds: a spread of WAN paths that repeats.
const ROUND_TRIPS: [u64; 8] = [
    41_000_000, 43_500_000, 39_800_000, 47_200_000, 40_100_000, 44_900_000, 42_300_000, 58_000_000,
];
/// The operations and the implementations that have each.
const OPERATIONS: [(&str, &[&str]); 10] = [
    ("sample", &["hyper", "focal", "slates"]),
    ("tail", &["hyper", "focal", "slates"]),
    ("election", &["hyper", "slates"]),
    ("ballot", &["hyper"]),
    ("span", &["hyper"]),
    ("timing", &["hyper"]),
    ("priority", &["hyper", "slates"]),
    ("pace", &["hyper", "focal"]),
    ("round", &["hyper", "focal", "slates"]),
    ("follower", &["hyper", "slates"]),
];

/// `iterations` calls of `call`, timed, then counted.
fn measure(iterations: u64, mut call: impl FnMut(u64)) -> (f64, f64) {
    for index in 0..iterations / 10 + 1 {
        call(index);
    }
    let start = Instant::now();
    for index in 0..iterations {
        call(index);
    }
    let ns = start.elapsed().as_nanos() as f64 / iterations as f64;
    alloc::begin();
    for index in 0..iterations {
        call(index);
    }
    let counts = alloc::end();
    (ns, counts.allocations as f64 / iterations as f64)
}

fn round_trip(index: u64) -> u64 {
    ROUND_TRIPS[(index as usize) % ROUND_TRIPS.len()]
}

/// Four paths fed the round trips, each from its own offset.
fn paths<P>(new: impl Fn() -> P, sample: impl Fn(&mut P, u64)) -> Vec<P> {
    (0..VOTERS as u64 - 1)
        .map(|path| {
            let mut estimate = new();
            for index in 0..ROUND_TRIPS.len() as u64 * 2 {
                sample(&mut estimate, round_trip(index + path));
            }
            estimate
        })
        .collect()
}

mod hyper {
    use super::*;
    use hyper_timing::{
        Arrivals, Ballot, Costs, ElectionTimer, ElectionTiming, PathRtt, RoundAnchors, RoundBudget,
        TickPace, configure_arrivals, quorum_priority,
    };

    /// The correlation time macOS measured at 100 µs heartbeats (`docs/timing.md` §2.6, item 6).
    const CORRELATION: Duration = Duration::from_millis(50);
    /// The granularity macOS measured at a 40 µs wait (`docs/timing.md` §2.4).
    const GRANULARITY: Duration = Duration::from_micros(45);

    pub fn run(operation: &str, iterations: u64) -> (f64, f64) {
        let heartbeat = Duration::from_nanos(HEARTBEAT_NS);
        let g = GRANULARITY.as_nanos() as u64;
        let paths = paths(
            || PathRtt::new(CORRELATION, heartbeat).unwrap(),
            |path: &mut PathRtt, rtt| path.on_sample(rtt),
        );
        let anchors = RoundAnchors {
            heartbeat_ns: HEARTBEAT_NS,
            stall_periods: 6,
            polls_per_period: 4,
            lookahead: (3, 4),
        };
        let ballot = Ballot::measure(&paths, VOTERS, Duration::ZERO, GRANULARITY).unwrap();
        let span = ballot.span(GRANULARITY).unwrap();
        // The link to the leader, as the paths see it one way, configured once.
        let link = Arrivals {
            unseen: 1e-5,
            lateness: Duration::ZERO,
            deviation: Duration::from_millis(2),
            mean_delay: ballot.latency,
        };
        let costs = Costs {
            election: span.election,
            mtbf: Duration::from_secs(30 * 86_400),
        };
        let detector = configure_arrivals(&link, &costs, GRANULARITY, GRANULARITY).unwrap();
        let timing = ElectionTiming::derive(heartbeat, &detector, &span, &ballot);
        match operation {
            "sample" => {
                let mut path = PathRtt::new(CORRELATION, heartbeat).unwrap();
                measure(iterations, |index| {
                    path.on_sample(black_box(round_trip(index)));
                    black_box(&path);
                })
            }
            "tail" => measure(iterations, |index| {
                black_box(paths[(index as usize) % paths.len()].tail_ns(g));
            }),
            "election" => measure(iterations, |_| {
                let ballot =
                    Ballot::measure(black_box(&paths), VOTERS, Duration::ZERO, GRANULARITY)
                        .unwrap();
                let span = ballot.span(GRANULARITY).unwrap();
                black_box(ElectionTiming::derive(heartbeat, &detector, &span, &ballot));
            }),
            "ballot" => measure(iterations, |_| {
                black_box(Ballot::measure(
                    black_box(&paths),
                    VOTERS,
                    Duration::ZERO,
                    GRANULARITY,
                ));
            }),
            "span" => measure(iterations, |_| {
                black_box(black_box(&ballot).span(GRANULARITY));
            }),
            "timing" => measure(iterations, |_| {
                black_box(ElectionTiming::derive(
                    heartbeat,
                    black_box(&detector),
                    black_box(&span),
                    black_box(&ballot),
                ));
            }),
            "priority" => measure(iterations, |_| {
                black_box(quorum_priority(
                    black_box(&paths).iter().map(Some),
                    VOTERS,
                    g,
                ));
            }),
            "pace" => measure(iterations, |_| {
                black_box(TickPace::derive(
                    heartbeat,
                    Duration::from_secs(1),
                    10,
                    black_box(&timing),
                ));
            }),
            "round" => measure(iterations, |index| {
                black_box(RoundBudget::derive(
                    &anchors,
                    Some(black_box(round_trip(index))),
                    1_000_000_000,
                ));
            }),
            "follower" => {
                let mut timer = ElectionTimer::new();
                measure(iterations, |index| {
                    let _ = black_box(timer.follower_period(index / 4, &timing, 1, 0));
                })
            }
            other => panic!("hyper has no {other}"),
        }
    }
}

mod focal {
    use super::*;
    use focal_timing::{PathRtt, RoundBudget, TickPace};

    pub fn run(operation: &str, iterations: u64) -> (f64, f64) {
        let paths = paths(PathRtt::default, |path: &mut PathRtt, rtt| {
            path.on_sample(rtt)
        });
        match operation {
            "sample" => {
                let mut path = PathRtt::new();
                measure(iterations, |index| {
                    path.on_sample(black_box(round_trip(index)));
                    black_box(&path);
                })
            }
            "tail" => measure(iterations, |index| {
                black_box(paths[(index as usize) % paths.len()].tail_ns());
            }),
            "pace" => measure(iterations, |_| {
                black_box(TickPace::derive(
                    Duration::from_nanos(HEARTBEAT_NS),
                    Duration::from_secs(1),
                    10,
                    black_box(&paths),
                ));
            }),
            "round" => measure(iterations, |index| {
                black_box(RoundBudget::derive(
                    Duration::from_nanos(HEARTBEAT_NS),
                    Some(Duration::from_nanos(black_box(round_trip(index)))),
                    Duration::from_secs(1),
                ));
            }),
            other => panic!("focal has no {other}"),
        }
    }
}

mod slates {
    use super::*;
    use slates_cluster::timing::{
        ElectionTimer, ElectionTiming, PathRtt, RoundAnchors, quorum_priority, round_budget,
    };
    use slates_db::register::HostId;

    pub fn run(operation: &str, iterations: u64) -> (f64, f64) {
        let paths = paths(PathRtt::default, |path: &mut PathRtt, rtt| {
            path.on_sample(rtt)
        });
        let anchors = RoundAnchors {
            heartbeat_ns: HEARTBEAT_NS,
            stall_periods: 6,
            polls_per_period: 4,
            lookahead: (3, 4),
        };
        match operation {
            "sample" => {
                let mut path = PathRtt::new();
                measure(iterations, |index| {
                    path.on_sample(black_box(round_trip(index)));
                    black_box(&path);
                })
            }
            "tail" => measure(iterations, |index| {
                black_box(paths[(index as usize) % paths.len()].tail_ns());
            }),
            "election" => measure(iterations, |_| {
                black_box(ElectionTiming::derive(HEARTBEAT_NS, black_box(&paths)));
            }),
            "priority" => measure(iterations, |_| {
                black_box(quorum_priority(black_box(&paths).iter().map(Some), VOTERS));
            }),
            "round" => measure(iterations, |index| {
                black_box(round_budget(&anchors, Some(black_box(round_trip(index)))));
            }),
            "follower" => {
                let timing = ElectionTiming::derive(HEARTBEAT_NS, &paths);
                let mut timer = ElectionTimer::new();
                measure(iterations, |index| {
                    let _ = black_box(timer.follower_period(index / 4, &timing, HostId(1), 0));
                })
            }
            other => panic!("slates has no {other}"),
        }
    }
}

fn one(args: &[String]) {
    let iterations: u64 = args[2].parse().unwrap();
    assert!(alloc::installed(), "the counting allocator is installed");
    let (ns, allocs) = match args[1].as_str() {
        "hyper" => hyper::run(&args[0], iterations),
        "focal" => focal::run(&args[0], iterations),
        "slates" => slates::run(&args[0], iterations),
        other => panic!("no implementation named {other}"),
    };
    println!("{ns} {allocs}");
}

fn run_one(operation: &str, implementation: &str, iterations: u64) -> (f64, f64) {
    let out = Command::new(std::env::current_exe().unwrap())
        .args(["one", operation, implementation, &iterations.to_string()])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    let mut fields = text.lines().last().unwrap_or("").split_whitespace();
    match (
        fields.next().and_then(|f| f.parse().ok()),
        fields.next().and_then(|f| f.parse().ok()),
    ) {
        (Some(ns), Some(allocs)) => (ns, allocs),
        _ => panic!(
            "{implementation} {operation} printed {text} {}",
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
    let iterations: u64 = args[1].parse().unwrap();
    println!(
        "{runs} runs of {iterations} calls, each in a fresh process; medians (least–most). \
         A five-voter group, a {} ms heartbeat.",
        HEARTBEAT_NS / 1_000_000
    );
    println!();
    println!("| Operation | Implementation | ns a call | Allocations a call |");
    println!("|---|---|---|---|");
    for (operation, implementations) in OPERATIONS {
        let mut points: Vec<Vec<(f64, f64)>> = vec![Vec::new(); implementations.len()];
        for run in 0..runs {
            for turn in 0..implementations.len() {
                let index = (run + turn) % implementations.len();
                points[index].push(run_one(operation, implementations[index], iterations));
            }
        }
        for (index, implementation) in implementations.iter().enumerate() {
            let ns: Vec<f64> = points[index].iter().map(|p| p.0).collect();
            let allocs: Vec<f64> = points[index].iter().map(|p| p.1).collect();
            println!(
                "| {operation} | {implementation} | {} | {} |",
                cell(&ns, 1),
                cell(&allocs, 2)
            );
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("one") => one(&args[1..]),
        Some("table") => table(&args[1..]),
        _ => panic!(
            "usage: one <operation> <implementation> <iterations> | table <runs> <iterations>"
        ),
    }
}
