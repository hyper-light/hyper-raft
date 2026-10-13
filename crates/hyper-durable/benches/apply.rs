//! What a leader gains by applying its own term's committed entries before its own write of them is
//! durable (`docs/durable.md` §4.2, §14 item 2): commit-to-answer latency with it and without, on a
//! leader whose disk is the slowest of its quorum, and on a disk the members share.
//!
//! ```text
//! cargo bench -p hyper-durable --bench apply -- DIR [CLIENTS] [LOADS] [ROUNDS] [ENTRIES]
//! ```
//!
//! Three voters on hyper-log at the log's depth, driven as `support` states, in two shapes:
//!
//! - **Slowest leader**: the leader's log on a scratch file under `DIR`, the followers' on
//!   hyper-block's simulated device, whose flush costs nothing: a disk per member, the leader's
//!   the slowest by the whole of its flush, the bound of what applying ahead can gain.
//! - **Shared disk**: every member's log a scratch file under `DIR`, on the one disk.
//!
//! Each shape runs closed loop (`CLIENTS`, `1,16` by default) and open loop (`LOADS`, percentages of
//! the most entries a second the shape's closed loop reached, `50,90` by default), with the leader
//! applying ahead and without, `ROUNDS` times (3 by default) in turn, the order turned each round,
//! each run on fresh logs measuring `ENTRIES` (by default the run length a p99.9 needs for an upper
//! end at 95% coverage, shared among the rounds). Latencies are pooled over the rounds and stated
//! with their intervals (`docs/tails.md` §3.3); the rest are medians over the rounds.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::disallowed_macros,
    clippy::cast_precision_loss,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation
)]

mod support;

use std::path::{Path, PathBuf};

use support::{Device, HEAD, MEMBERS, Mode, Run, Shape, Wakes};

/// The log's depth: a group's writes out at once, as the shell runs them.
const DEPTH: usize = 3;

/// The two shapes, the leader's device first.
const SHAPES: [(&str, [Device; MEMBERS as usize]); 2] = [
    ("slowest leader", [Device::File, Device::Sim, Device::Sim]),
    ("shared disk", [Device::File, Device::File, Device::File]),
];

/// Both ways of one point, `rounds` times in turn; returns the most entries a second either
/// reached.
fn point(
    wakes: &Wakes,
    dir: &Path,
    (name, devices): (&str, [Device; MEMBERS as usize]),
    mode: Mode,
    rounds: usize,
    entries: u64,
) -> f64 {
    let before = support::load();
    let mut runs: [Vec<Run>; 2] = [Vec::new(), Vec::new()];
    for round in 0..rounds {
        let seed = 0x6170_706c ^ ((round as u64) << 8);
        for k in 0..2 {
            let at = (k + round) % 2;
            let shape = Shape {
                apply_ahead: at == 1,
                devices,
                ..Shape::on(Device::File, DEPTH)
            };
            runs[at].push(support::run(wakes, dir, shape, mode, entries, seed));
        }
    }
    println!(
        "\n**{name}, {}; {entries} entries a run, {rounds} rounds; load {before} → {}**\n",
        mode.label(),
        support::load()
    );
    println!("| leader applies | {HEAD}\n|---|---|---|---|---|---|---|---|---|");
    let mut most = 0.0f64;
    for (at, way) in ["after its own write", "ahead of its own write"]
        .iter()
        .enumerate()
    {
        let (row, rate) = support::row(&runs[at]);
        most = most.max(rate);
        println!("| {way} {row}");
    }
    let p50s = |runs: &[Run]| -> Vec<f64> { runs.iter().map(support::run_p50).collect() };
    let rates = |runs: &[Run]| -> Vec<f64> { runs.iter().map(|r| r.entries_per_second).collect() };
    println!(
        "\n- ahead against after: each run's p50 {}; entries a second {}",
        support::verdict(&p50s(&runs[0]), &p50s(&runs[1])),
        support::verdict(&rates(&runs[0]), &rates(&runs[1]))
    );
    most
}

fn main() {
    let args: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| a != "--bench")
        .collect();
    let dir = PathBuf::from(args.first().expect("a directory for the logs' files"));
    let clients: Vec<usize> = support::list(args.get(1), &[1, 16]);
    let loads: Vec<u64> = support::list(args.get(2), &[50, 90]);
    let rounds: usize = args.get(3).map_or(3, |r| r.parse().unwrap());
    let entries: u64 = args
        .get(4)
        .map_or_else(|| support::default_entries(rounds), |n| n.parse().unwrap());
    let wakes = Wakes::new();
    for shape in SHAPES {
        let mut capacity = 0.0f64;
        for &c in &clients {
            capacity = capacity.max(point(&wakes, &dir, shape, Mode::Closed(c), rounds, entries));
        }
        for &l in &loads {
            let rate = capacity * l as f64 / 100.0;
            point(&wakes, &dir, shape, Mode::Open(rate), rounds, entries);
        }
    }
}
