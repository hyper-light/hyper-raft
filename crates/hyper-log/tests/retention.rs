//! A group's own bound comes before the file's: on a file whose budget holds every group's
//! retention, a group that takes many small updates is refused `Backlog`, never `Full`, however
//! small its updates, down to one entry an update. Found by mantle (its KIND cell, 2026-11-07): a
//! founder's file filled at 135,503,872 bytes, 30,896 frames of about one block each, while its
//! groups held a few thousand entries of a few hundred bytes, none near its bound; its writes were
//! refused `Full`, and a group's bound, counted in payload bytes, was never reached.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::disallowed_macros,
    clippy::cast_possible_truncation
)]

use hyper_block::buf::Alignment;
use hyper_block::sim::SimFile;
use hyper_log::{Config, Entries, Entry, Facts, HardState, Log, LogError, Start, Update, Waits};

const ID: u128 = 0x7265_7465_6e74;
const BLOCK: usize = 4096;
/// An entry's payload: a few hundred bytes, mantle's small writes.
const SMALL: usize = 300;

fn sim(seed: u64) -> SimFile {
    SimFile::new(
        Alignment::new(BLOCK).unwrap(),
        Alignment::new(512).unwrap(),
        seed,
    )
    .unwrap()
}

/// How a test's disk budget is sized.
#[derive(Clone, Copy, Debug)]
enum Budget {
    /// Every group's retention in payload bytes at once, as mantle sized its file: the persist
    /// area's segment, and the groups' retained bytes in whole segments.
    Payload,
    /// What holds every group's retention at once as the file stores it (`Config::disk_for`).
    Stored,
}

/// A node's facts for `groups` groups, each retaining a mebibyte of payload, on a disk budget of
/// the payload the groups retain at once.
fn facts(groups: usize) -> Facts {
    sized(groups, Budget::Payload)
}

/// A node's facts for `groups` groups, each retaining a mebibyte of payload, on a `budget`.
fn sized(groups: usize, budget: Budget) -> Facts {
    let mut facts = Facts {
        align: Alignment::new(BLOCK).unwrap(),
        sealed: false,
        largest_entry: 64 << 10,
        disk_bytes: 1 << 34,
        max_groups: groups,
        cadence_entries: 1 << 20,
        cadence_bytes: 1 << 19,
        uncommitted_entries: 0,
        uncommitted_bytes: 0,
        cache_bytes: 1 << 26,
    };
    facts.disk_bytes = match budget {
        Budget::Payload => {
            let unbounded = Config::derive(&facts).unwrap();
            let retained = unbounded.group_bytes * groups as u64;
            let segments = retained.div_ceil(unbounded.segment_bytes).max(3);
            (segments + 1) * unbounded.segment_bytes
        }
        Budget::Stored => Config::disk_for(&facts).unwrap(),
    };
    facts
}

/// One entry of `size` bytes at `index`, as its own update.
fn one(index: u64, size: usize) -> Update {
    Update {
        entries: Some(Entries {
            first: index,
            entries: vec![Entry {
                term: 1,
                bytes: vec![index as u8; size],
            }],
        }),
        ..Update::default()
    }
}

/// The log on `facts`' derived configuration, its writer never waiting, so each update written
/// alone is a frame of its own: the most a payload byte can take of the file.
fn log(facts: &Facts, seed: u64) -> (Log<SimFile>, Config) {
    let mut config = Config::derive(facts).unwrap();
    config.waits = Waits::Never;
    (Log::create(sim(seed), config, ID).unwrap(), config)
}

/// The entries of `size` bytes a group's bounds admit.
fn admitted(config: &Config, size: usize) -> u64 {
    config.group_entries.min(config.group_bytes / size as u64)
}

/// Writes one-entry updates of `size` bytes to each of `groups` groups in turn until every one is
/// refused, never compacting; then holds that each was refused `Backlog`, its own bound, never
/// `Full`, having taken all its bounds admit.
fn every_group_is_refused_backlog(groups: usize, budget: Budget, size: usize, seed: u64) {
    let facts = sized(groups, budget);
    let (log, config) = log(&facts, seed);
    let mut next = vec![1u64; groups];
    let mut refused: Vec<Option<LogError>> = (0..groups).map(|_| None).collect();
    while refused.iter().any(Option::is_none) {
        for at in 0..groups {
            if refused[at].is_some() {
                continue;
            }
            match log.write(at as u128 + 1, one(next[at], size)) {
                Ok(()) => next[at] += 1,
                Err(error) => refused[at] = Some(error),
            }
        }
    }
    for (at, refused) in refused.iter().enumerate() {
        let group = at as u128 + 1;
        assert!(
            matches!(refused, Some(LogError::Backlog(g)) if *g == group),
            "{groups} groups of {size}-byte entries on a {budget:?} budget: group {group} refused {refused:?} after {} \
             entries, its bounds {} entries and {} bytes, the file {} segments of {}",
            next[at] - 1,
            config.group_entries,
            config.group_bytes,
            config.max_segments,
            config.segment_bytes
        );
        assert_eq!(
            next[at] - 1,
            admitted(&config, size),
            "group {group}'s entries taken"
        );
    }
    log.close().unwrap();
}

/// Do: one group on a file whose budget holds its retention in payload bytes, as mantle sized its
/// file, written one-entry updates of a few hundred bytes, each a frame of its own, never
/// compacted. Expect: it is refused `Backlog`, its own bound, never `Full`, having taken all its
/// bounds admit. Before: refused `Full` after 2,471 of the 3,495 entries its bound admitted.
#[test]
fn one_group_of_one_entry_updates_is_refused_backlog_never_full() {
    every_group_is_refused_backlog(1, Budget::Payload, SMALL, 1);
}

/// Do: four groups, every one the configuration admits, on the budget `Config::disk_for` names
/// for their retention at once, written one-entry updates in turn, never compacted. Expect: each
/// is refused `Backlog`, never `Full`, each having taken all its bounds admit.
#[test]
fn every_group_at_once_of_one_entry_updates_is_refused_backlog_never_full() {
    every_group_is_refused_backlog(4, Budget::Stored, SMALL, 2);
}

/// Do: as above, one group on the payload's budget and four on `Config::disk_for`'s, with
/// one-entry updates of every size from one byte to the largest entry, among them sizes just past
/// half a segment's room, which fill a segment alone. Expect: every group refused `Backlog`, never
/// `Full`, having taken all its bounds admit.
#[test]
fn groups_of_one_entry_updates_of_any_size_are_refused_backlog_never_full() {
    let room = Config::derive(&facts(1)).unwrap().segment_bytes as usize - BLOCK;
    let sizes = [
        1,
        17,
        100,
        SMALL,
        1_000,
        4_000,
        4_100,
        9_000,
        30_000,
        room / 2 + 1,
        room / 2 + 200,
        64 << 10,
    ];
    for (seed, size) in sizes.into_iter().enumerate() {
        every_group_is_refused_backlog(1, Budget::Payload, size, seed as u64 + 10);
        every_group_is_refused_backlog(4, Budget::Stored, size, seed as u64 + 30);
    }
}

/// Do: the least file, three segments (`Config::derive`'s floor), one group whose own bounds the
/// file never reaches, written one-entry updates with their hard state, as a replica writes them,
/// until the file refuses one `Full`; the group then compacts to its last entry, as an owner does
/// when its log is refused for room, and writes again; ten times, for entries of a few sizes.
/// Expect: after each compaction the next write is taken. Found by mantle (2026-11-07): a leader
/// whose log was compacted to one entry (start 286, last 287) was refused `Full` for good, its
/// file's tail one frame of one live record, which the sweep never moved.
#[test]
fn a_least_file_takes_writes_again_once_its_group_compacts() {
    for (seed, size) in [1usize, SMALL, 3_000].into_iter().enumerate() {
        let facts = facts(1);
        let mut config = Config::derive(&facts).unwrap();
        config.max_segments = 3;
        config.group_entries = u64::MAX;
        config.group_bytes = u64::MAX;
        config.waits = Waits::Never;
        let log = Log::create(sim(seed as u64 + 40), config, ID).unwrap();
        let mut next = 1u64;
        let write = |log: &Log<SimFile>, index: u64| {
            let mut update = one(index, size);
            update.hard_state = Some(HardState {
                term: 1,
                commit: index,
                ..HardState::default()
            });
            log.write(1, update)
        };
        for round in 0..10 {
            let taken = next;
            let refused = loop {
                match write(&log, next) {
                    Ok(()) => next += 1,
                    Err(error) => break error,
                }
            };
            assert!(
                matches!(refused, LogError::Full),
                "{size}-byte entries, round {round}: refused {refused:?}"
            );
            assert!(
                next > taken,
                "{size}-byte entries, round {round}: nothing taken since the last compaction"
            );
            let compact = Update {
                start: Some(Start {
                    index: next - 2,
                    term: 1,
                }),
                ..Update::default()
            };
            log.write(1, compact).unwrap();
            write(&log, next).unwrap_or_else(|refused| {
                panic!("{size}-byte entries, round {round}: compacted to its last entry, the next write refused {refused:?}")
            });
            next += 1;
        }
        log.close().unwrap();
    }
}

/// Do: the least file, three segments, one group whose own bounds the file never reaches. Its
/// first frame is its hard state alone, one block, and its next an entry too large for the rest of
/// that segment, which opens the next: the oldest segment is one block's frame holding one live
/// record. Then entries of a few hundred bytes, each a frame of its own; whenever the file refuses
/// one `Full`, the group compacts to its last entry, as an owner does when its log is refused for
/// room; twenty times. Expect: each compaction and the write after it are taken. Before: the
/// oldest segment, one live record in one block's frame, was never swept (its copy, one block, was
/// no smaller than its frames); once the newest segment filled with what the group then let go,
/// nothing freed the third, and every write, the compaction's too, was refused `Full` for good, as
/// mantle's leader was with its log compacted to one entry (start 286, last 287; mantle's
/// docs/measurements/2026-11-07-full-log-restart.md).
#[test]
fn a_least_file_whose_oldest_segment_holds_one_small_record_takes_writes_once_its_group_compacts() {
    let facts = facts(1);
    let mut config = Config::derive(&facts).unwrap();
    config.max_segments = 3;
    config.group_entries = u64::MAX;
    config.group_bytes = u64::MAX;
    config.waits = Waits::Never;
    let room = config.segment_bytes as usize - BLOCK;
    let log = Log::create(sim(60), config, ID).unwrap();
    let state = Update {
        hard_state: Some(HardState {
            term: 1,
            ..HardState::default()
        }),
        ..Update::default()
    };
    log.write(1, state).unwrap();
    // An entry whose frame fills a segment's room but for a block: past what the first segment
    // has left after its one-block frame.
    let large = room - 4 * 64 - BLOCK / 2;
    log.write(1, one(1, large)).unwrap();
    let mut next = 2u64;
    let mut compactions = 0;
    while compactions < 20 {
        match log.write(1, one(next, SMALL)) {
            Ok(()) => next += 1,
            Err(LogError::Full) => {
                let compact = Update {
                    start: Some(Start {
                        index: next - 2,
                        term: 1,
                    }),
                    ..Update::default()
                };
                log.write(1, compact).unwrap_or_else(|refused| {
                    panic!(
                        "compaction {compactions}, to entry {}: refused {refused:?}",
                        next - 2
                    )
                });
                log.write(1, one(next, SMALL)).unwrap_or_else(|refused| {
                    panic!("compaction {compactions}: the next write refused {refused:?}")
                });
                next += 1;
                compactions += 1;
            }
            Err(other) => panic!("entry {next}: refused {other:?}"),
        }
    }
    log.close().unwrap();
}
