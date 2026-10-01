//! What one append and one fetch cost the allocator and the page tables: `cargo bench -p
//! hyper-log --bench allocs [-- DIR]` (docs/benchmarks.md, "Allocations").
//!
//! A log on a real file in DIR (the target directory's scratch by default) takes closed-loop
//! appends from `R` replicas, each to its own group: one round submits one entry for every
//! replica and waits for all of them, so every frame carries the round. The updates are built
//! before the count begins, so the count is the log's own: the submission, the owner's and the
//! device's work, the answer, and the drop of what the log held. The count is the process's,
//! every thread's together (`hyper_measure::alloc::begin_process`), since the log runs threads of
//! its own. Fetches are counted the same way: a group's entries still in memory, the same
//! entries read back from the file after a reopen, a term and a view.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::disallowed_macros,
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::indexing_slicing
)]

use std::path::{Path, PathBuf};

use hyper_block::buf::Alignment;
use hyper_block::file::{CachingRequest, DeviceFile};
use hyper_block::scratch::Scratch;
use hyper_log::{Config, Entries, Entry, Log, Update, Waits};
use hyper_measure::{alloc, faults};

#[global_allocator]
static ALLOCATOR: alloc::Counting = alloc::Counting;

/// Rounds counted per row, after the warm-up rounds.
const ROUNDS: u64 = 200;
const WARM: u64 = 20;

fn config(replicas: usize) -> Config {
    Config {
        segment_bytes: 16 << 20,
        max_segments: 64,
        max_groups: replicas,
        group_entries: 1 << 20,
        group_bytes: 1 << 30,
        group_cache: 1 << 16,
        queue_submissions: replicas * 2,
        waits: Waits::Measured,
    }
}

fn open(path: &Path) -> DeviceFile {
    DeviceFile::open(
        path,
        false,
        CachingRequest::PreferDirect,
        Alignment::new(4096).unwrap(),
    )
    .unwrap()
}

fn updates(replicas: usize, size: usize, from: u64, rounds: u64) -> Vec<Vec<Update>> {
    (from..from + rounds)
        .map(|index| {
            (0..replicas)
                .map(|_| Update {
                    entries: Some(Entries {
                        first: index,
                        entries: vec![Entry {
                            term: 1,
                            bytes: vec![0x5a; size],
                        }],
                    }),
                    ..Update::default()
                })
                .collect()
        })
        .collect()
}

fn rounds(log: &Log<DeviceFile>, batches: Vec<Vec<Update>>) {
    for batch in batches {
        let pending: Vec<_> = batch
            .into_iter()
            .enumerate()
            .map(|(g, u)| log.submit(g as u128, u).unwrap())
            .collect();
        for p in pending {
            p.wait().unwrap();
        }
    }
}

struct Cost {
    allocations: f64,
    reallocations: f64,
    bytes: f64,
    faults: f64,
}

fn per(counts: alloc::Counts, faults: u64, n: u64) -> Cost {
    let n = n as f64;
    Cost {
        allocations: counts.allocations as f64 / n,
        reallocations: counts.reallocations as f64 / n,
        bytes: counts.bytes as f64 / n,
        faults: faults as f64 / n,
    }
}

fn counted(n: u64, work: impl FnOnce()) -> Cost {
    let before = faults::read().unwrap();
    alloc::begin_process();
    work();
    let counts = alloc::end_process();
    let after = faults::read().unwrap();
    per(counts, after.since(&before).minor, n)
}

fn row(what: &str, replicas: usize, size: usize, cost: &Cost) {
    println!(
        "  {what:<22} {replicas:>8} {size:>7} {:>10.2} {:>10.2} {:>12.0} {:>10.3}",
        cost.allocations, cost.reallocations, cost.bytes, cost.faults
    );
}

fn point(dir: &Path, replicas: usize, size: usize) {
    let scratch = Scratch::create(dir, ".hyper-log-allocs").unwrap();
    let log = Log::create(open(scratch.path()), config(replicas), 1).unwrap();
    rounds(&log, updates(replicas, size, 1, WARM));
    let batches = updates(replicas, size, WARM + 1, ROUNDS);
    let appends = ROUNDS * replicas as u64;
    let cost = counted(appends, || rounds(&log, batches));
    row("append", replicas, size, &cost);
    let last = WARM + ROUNDS;
    let cost = counted(replicas as u64, || {
        for g in 0..replicas {
            let got = log.entries(g as u128, last, last + 1, u64::MAX).unwrap();
            assert_eq!(got.len(), 1);
        }
    });
    row("fetch, in memory", replicas, size, &cost);
    let cost = counted(replicas as u64, || {
        for g in 0..replicas {
            assert_eq!(log.term(g as u128, last).unwrap(), 1);
        }
    });
    row("term", replicas, size, &cost);
    let cost = counted(replicas as u64, || {
        for g in 0..replicas {
            assert!(log.view(g as u128).unwrap().is_some());
        }
    });
    row("view", replicas, size, &cost);
    drop(log);
    let (log, _) = Log::open(open(scratch.path()), config(replicas), 1).unwrap();
    let cost = counted(replicas as u64, || {
        for g in 0..replicas {
            let got = log.entries(g as u128, last, last + 1, u64::MAX).unwrap();
            assert_eq!(got.len(), 1);
        }
    });
    row("fetch, from the file", replicas, size, &cost);
}

fn main() {
    assert!(alloc::installed(), "the counting allocator is installed");
    let dir = std::env::args()
        .skip(1)
        .find(|a| !a.starts_with('-'))
        .map_or_else(|| PathBuf::from(env!("CARGO_TARGET_TMPDIR")), PathBuf::from);
    println!(
        "hyper-log: allocations, reallocations, bytes asked and minor faults per operation, \
         every thread's ({ROUNDS} rounds after {WARM})"
    );
    println!(
        "  {:<22} {:>8} {:>7} {:>10} {:>10} {:>12} {:>10}",
        "", "replicas", "entry", "allocs", "reallocs", "bytes", "faults"
    );
    for replicas in [1usize, 16] {
        for size in [128usize, 1 << 10, 16 << 10] {
            point(&dir, replicas, size);
        }
    }
}
