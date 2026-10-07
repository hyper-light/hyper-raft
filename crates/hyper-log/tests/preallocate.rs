//! A slot the file grows by is written whole with zeros before its first frame, under that frame's
//! flush (`docs/durable.md` §6.3), so every later frame in it is an overwrite. Zeros past the last
//! frame read as no frame, as the file's end does; a power cut anywhere in a growing frame's writes
//! leaves a log that opens with what was acknowledged and nothing else. A writer idle under
//! `Waits::Measured` writes the next slot so ahead of its frame, under the owner's admission and
//! flushed before the slot is used; a power cut at any step of that opens clean too.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::disallowed_macros,
    clippy::disallowed_types
)]

use std::sync::{Arc, Mutex};

use hyper_block::block::BlockFile;
use hyper_block::buf::Alignment;
use hyper_block::sim::{Crash, Fault, SimFile};
use hyper_log::format::{FRAME_MAGIC, FrameHeader, SEGMENT_MAGIC, SegmentHeader};
use hyper_log::{Config, Entries, Entry, Growth, Log, LogError, Update, Waits, With};

const ID: u128 = 0x7072_6561_6c6c_6f63;
const BLOCK: usize = 4096;
const SEGMENT: u64 = 16 * BLOCK as u64;
/// An entry a frame holds alone and two of which no segment does: every second frame opens a slot.
const ENTRY: usize = 36 * 1024;

fn config() -> Config {
    Config {
        segment_bytes: SEGMENT,
        max_segments: 16,
        max_groups: 4,
        group_entries: 1 << 16,
        group_bytes: 1 << 26,
        group_cache: 1 << 10,
        queue_submissions: 16,
        waits: Waits::Never,
    }
}

/// `config`, under which the writer fills a slot ahead while idle.
fn measured() -> Config {
    Config {
        waits: Waits::Measured,
        ..config()
    }
}

/// Waits until `log` has filled a slot ahead `fills` times, or failed to `failures` times. Each
/// look is a round trip to the owner, so the wait is charged to the owner's progress.
fn settled(log: &Log<SimFile>, fills: u64, failures: u64) {
    for _ in 0..1_000_000 {
        let stats = log.stats(None).unwrap();
        if stats.fills >= fills && stats.fill_failures >= failures {
            return;
        }
        std::thread::yield_now();
    }
    panic!("the log never filled {fills} slots ahead and failed {failures}");
}

/// The file's writes and flushes so far: what a power cut counts.
fn ops(log: &Log<SimFile>) -> u64 {
    log.with_file(|f| {
        let s = f.stats().unwrap();
        s.writes + s.syncs
    })
    .unwrap()
}

fn sim(seed: u64) -> SimFile {
    SimFile::new(
        Alignment::new(BLOCK).unwrap(),
        Alignment::new(512).unwrap(),
        seed,
    )
    .unwrap()
}

fn entry(index: u64, size: usize) -> Update {
    Update {
        entries: Some(Entries {
            first: index,
            entries: vec![Entry {
                term: 1,
                bytes: vec![(index % 251) as u8; size],
            }],
        }),
        ..Update::default()
    }
}

/// A block of zeros is no frame and no segment: neither magic has a zero byte, and each header is
/// read only where its magic is. So recovery reads a zeroed tail as the end, never as a damaged
/// frame.
#[test]
fn a_zeroed_block_is_no_header() {
    assert!(FRAME_MAGIC.iter().all(|&b| b != 0));
    assert!(SEGMENT_MAGIC.iter().all(|&b| b != 0));
    let zeros = vec![0u8; BLOCK];
    assert!(FrameHeader::decode(&zeros).is_none());
    assert!(SegmentHeader::decode(&zeros).is_none());
}

/// The file spans its first slot from its creation, does not grow while frames fill a slot, and
/// grows by exactly a segment when a frame opens the next: frames inside a slot are overwrites.
#[test]
fn frames_inside_a_slot_overwrite_and_a_new_slot_grows_the_file_by_a_segment() {
    let log = Log::create(sim(1), config(), ID).unwrap();
    let len = |log: &Log<SimFile>| log.with_file(|f| f.len()).unwrap().unwrap();
    let created = len(&log);
    assert_eq!(
        created,
        2 * SEGMENT,
        "the persist area and the first slot, whole"
    );
    // Small entries fill the first slot without growing the file.
    let mut index = 1;
    while len(&log) == created {
        log.write(1, entry(index, 1024)).unwrap();
        index += 1;
        assert!(index < 1_000, "the file never grew");
        if len(&log) != created {
            break;
        }
    }
    assert_eq!(len(&log), created + SEGMENT, "a new slot, whole");
    // And reads back across both slots after a reopen.
    let file = log.close().unwrap();
    let (log, _) = Log::open(file, config(), ID).unwrap();
    for i in 1..index {
        assert_eq!(
            log.entries(1, i, i + 1, u64::MAX).unwrap()[0].bytes.len(),
            1024
        );
    }
    drop(log.close().unwrap());
}

/// Power is cut at each of a growing frame's operations (the zeros' write, the frame's, its
/// record's, the flush), under each way a crash keeps what was not flushed. The log opens with
/// every entry it acknowledged; the one it did not is gone or whole, as a write whose answer was
/// lost may be, never damaged; the slot's zeros read as no frame; and it takes writes again.
#[test]
fn a_power_cut_anywhere_in_a_growing_frame_leaves_what_was_acknowledged() {
    for ops in 0..4 {
        for (seed, crash) in [Crash::Random, Crash::KeepAll, Crash::LoseAll]
            .into_iter()
            .enumerate()
        {
            let log = Log::create(sim(10 + seed as u64), config(), ID).unwrap();
            // The first entry fills most of slot 0; the second opens slot 1.
            log.write(1, entry(1, ENTRY)).unwrap();
            log.with_file(move |f| f.inject(Fault::PowerCut { ops }).unwrap())
                .unwrap();
            assert!(matches!(
                log.write(1, entry(2, ENTRY)),
                Err(LogError::Fenced)
            ));
            log.with_file(move |f| f.crash(crash).unwrap()).unwrap();
            let file = log.close().unwrap();
            file.clear_faults().unwrap();
            let (log, _) = Log::open(file, config(), ID).unwrap();
            let view = log.view(1).unwrap().unwrap();
            assert!(
                view.last == 1 || view.last == 2,
                "ops {ops}, {crash:?}: last {}",
                view.last
            );
            for (index, byte) in (1..=view.last).zip([1u8, 2]) {
                assert_eq!(
                    log.entries(1, index, index + 1, u64::MAX).unwrap()[0].bytes,
                    vec![byte; ENTRY],
                    "ops {ops}, {crash:?}: entry {index}"
                );
            }
            log.write(1, entry(2, ENTRY)).unwrap();
            log.write(1, entry(3, ENTRY)).unwrap();
            let file = log.close().unwrap();
            let (log, _) = Log::open(file, config(), ID).unwrap();
            assert_eq!(
                log.view(1).unwrap().unwrap().last,
                3,
                "ops {ops}, {crash:?}"
            );
            drop(log.close().unwrap());
        }
    }
}

/// An idle writer fills the next slot ahead: a new log spans its persist area and two slots once
/// idle, and the frame that opens the filled slot writes no zeros of its own, so its bytes and the
/// next fill's come to less than two segments; the file then spans one slot more, filled ahead in
/// turn.
#[test]
fn an_idle_writer_fills_the_next_slot_and_its_frame_writes_no_zeros() {
    let log = Log::create(sim(20), measured(), ID).unwrap();
    let len = |log: &Log<SimFile>| log.with_file(|f| f.len()).unwrap().unwrap();
    settled(&log, 1, 0);
    assert_eq!(
        len(&log),
        3 * SEGMENT,
        "the persist area, slot 0 and slot 1 filled"
    );
    log.write(1, entry(1, ENTRY)).unwrap();
    let before = log.stats(None).unwrap();
    // Opens slot 1, filled; the writer then fills slot 2.
    log.write(1, entry(2, ENTRY)).unwrap();
    settled(&log, 2, 0);
    let after = log.stats(None).unwrap();
    assert_eq!(len(&log), 4 * SEGMENT);
    let written = after.bytes - before.bytes;
    assert!(
        written > SEGMENT && written < 2 * SEGMENT,
        "{written} bytes: the frame wrote zeros of its own"
    );
    let file = log.close().unwrap();
    let (log, recovery) = Log::open(file, measured(), ID).unwrap();
    assert!(recovery.damaged.is_empty() && recovery.restored.is_empty());
    assert_eq!(log.view(1).unwrap().unwrap().last, 2);
    drop(log.close().unwrap());
}

/// Power is cut at each step of a fill ahead (its zeros' write, its flush) and just after it,
/// under each way a crash keeps what was not flushed. The fill's failure fences nothing: the log
/// goes on answering. Reopened, it holds every entry it acknowledged, nothing is damaged or
/// restored, the slot's zeros or what is left of them read as no frame, and it opens the slot and
/// the ones after it as it writes again.
#[test]
fn a_power_cut_anywhere_in_a_fill_ahead_opens_clean() {
    let fill = fill_starts_after();
    for step in 0..3 {
        for (seed, crash) in [Crash::Random, Crash::KeepAll, Crash::LoseAll]
            .into_iter()
            .enumerate()
        {
            cut_in_fill(30 + seed as u64, fill + step, step < 2, crash);
        }
    }
}

/// The writes and flushes from before entry 2 to the start of the fill of slot 2 that follows
/// it: the fill's are the last two.
fn fill_starts_after() -> u64 {
    let dry = Log::create(sim(30), measured(), ID).unwrap();
    settled(&dry, 1, 0);
    dry.write(1, entry(1, ENTRY)).unwrap();
    let start = ops(&dry);
    dry.write(1, entry(2, ENTRY)).unwrap();
    settled(&dry, 2, 0);
    let fill = ops(&dry) - start - 2;
    drop(dry.close().unwrap());
    fill
}

/// Cuts power after `cut` writes and flushes from before entry 2, inside the fill that follows it
/// where `fails`, just past it where not; crashes the file as `crash` says; and reopens it.
fn cut_in_fill(seed: u64, cut: u64, fails: bool, crash: Crash) {
    let at = format!("cut {cut}, {crash:?}");
    let log = Log::create(sim(seed), measured(), ID).unwrap();
    settled(&log, 1, 0);
    log.write(1, entry(1, ENTRY)).unwrap();
    log.with_file(move |f| f.inject(Fault::PowerCut { ops: cut }).unwrap())
        .unwrap();
    log.write(1, entry(2, ENTRY)).unwrap();
    if fails {
        settled(&log, 1, 1);
    } else {
        settled(&log, 2, 0);
    }
    // The failed fill fenced nothing.
    assert_eq!(log.view(1).unwrap().unwrap().last, 2, "{at}");
    log.with_file(move |f| f.crash(crash).unwrap()).unwrap();
    let file = log.close().unwrap();
    file.clear_faults().unwrap();
    let (log, recovery) = Log::open(file, measured(), ID).unwrap();
    assert!(
        recovery.damaged.is_empty() && recovery.restored.is_empty(),
        "{at}: {recovery:?}"
    );
    assert_eq!(log.view(1).unwrap().unwrap().last, 2, "{at}");
    for index in 3..=6 {
        log.write(1, entry(index, ENTRY)).unwrap();
    }
    let file = log.close().unwrap();
    let (log, recovery) = Log::open(file, measured(), ID).unwrap();
    assert!(recovery.damaged.is_empty(), "{at}");
    assert_eq!(log.view(1).unwrap().unwrap().last, 6, "{at}");
    for index in 1..=6u64 {
        assert_eq!(
            log.entries(1, index, index + 1, u64::MAX).unwrap()[0].bytes,
            vec![(index % 251) as u8; ENTRY],
            "{at}: entry {index}"
        );
    }
    drop(log.close().unwrap());
}

/// What a test's gate admitted, committed, released and was told at open.
#[derive(Debug, Default)]
struct Ledger {
    told: Vec<u64>,
    pending: u64,
    committed: u64,
}

struct Recorded(Arc<Mutex<Ledger>>);

impl Growth for Recorded {
    fn held(&mut self, bytes: u64) {
        self.0.lock().unwrap().told.push(bytes);
    }
    fn admit(&mut self, bytes: u64) -> bool {
        self.0.lock().unwrap().pending += bytes;
        true
    }
    fn commit(&mut self, bytes: u64) {
        let mut l = self.0.lock().unwrap();
        l.pending -= bytes;
        l.committed += bytes;
    }
    fn release(&mut self, bytes: u64) {
        self.0.lock().unwrap().pending -= bytes;
    }
}

fn recorded() -> (Arc<Mutex<Ledger>>, With) {
    let ledger = Arc::new(Mutex::new(Ledger::default()));
    let with = With {
        growth: Some(Box::new(Recorded(ledger.clone()))),
        ..With::default()
    };
    (ledger, with)
}

/// A fill ahead takes the slot its owner admitted and commits it once flushed: what the gate
/// committed is the file's length, through fills and the frames that open the filled slots; once
/// the log ends nothing stays pending; and the log reopened tells its owner the filled slot past
/// the last too, so a restart counts it once.
#[test]
fn a_fill_ahead_commits_the_slot_its_owner_admitted() {
    let (ledger, with) = recorded();
    let log = Log::create_with(sim(40), measured(), ID, with).unwrap();
    settled(&log, 1, 0);
    let len = |log: &Log<SimFile>| log.with_file(|f| f.len()).unwrap().unwrap();
    let committed = |l: &Arc<Mutex<Ledger>>| l.lock().unwrap().committed;
    // Each read alone: the owner asks the gate on its own thread.
    let file_len = len(&log);
    assert_eq!(committed(&ledger), file_len);
    for index in 1..=4 {
        log.write(1, entry(index, ENTRY)).unwrap();
    }
    settled(&log, 3, 0);
    let file_len = len(&log);
    assert_eq!(committed(&ledger), file_len);
    let file = log.close().unwrap();
    let len = file.len().unwrap();
    {
        let l = ledger.lock().unwrap();
        assert_eq!(l.pending, 0, "an admission outlived the log");
        assert_eq!(l.committed, len);
    }
    let (ledger, with) = recorded();
    let (log, recovery) = Log::open_with(file, measured(), ID, with).unwrap();
    assert!(recovery.damaged.is_empty());
    assert_eq!(ledger.lock().unwrap().told, vec![len]);
    drop(log.close().unwrap());
}

/// A fill ahead fails alone (the volume ends at the file's end) and fences nothing: frames inside
/// the slots the file has go on being written and acknowledged, the fill is not tried again, and
/// the frame that opens the slot zeroes it under its own flush, failing and fencing there as a
/// growing frame on a full volume does. Reopened, the log holds what it acknowledged.
#[test]
fn a_failed_fill_ahead_fences_nothing() {
    let log = Log::create(sim(50), measured(), ID).unwrap();
    settled(&log, 1, 0);
    log.write(1, entry(1, ENTRY)).unwrap();
    let len = log.with_file(|f| f.len()).unwrap().unwrap();
    log.with_file(move |f| f.inject(Fault::Capacity { len }).unwrap())
        .unwrap();
    // Opens slot 1, filled; the fill of slot 2 then fails.
    log.write(1, entry(2, ENTRY)).unwrap();
    settled(&log, 1, 1);
    log.write(1, entry(3, 1024)).unwrap();
    let stats = log.stats(None).unwrap();
    assert_eq!((stats.fills, stats.fill_failures), (1, 1), "tried once");
    assert!(matches!(
        log.write(1, entry(4, ENTRY)),
        Err(LogError::Fenced)
    ));
    let file = log.close().unwrap();
    file.clear_faults().unwrap();
    let (log, recovery) = Log::open(file, measured(), ID).unwrap();
    assert!(recovery.damaged.is_empty());
    let last = log.view(1).unwrap().unwrap().last;
    assert!(last == 3 || last == 4, "last {last}");
    drop(log.close().unwrap());
}
