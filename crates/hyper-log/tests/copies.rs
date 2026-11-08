//! A frame no other frame follows at once is answered at its own flush, its copy written in that
//! flush (docs/durable.md §6.2). These tests cut the power at every byte boundary of every write
//! of such a frame's flush, its predecessor answered, and damage frames that were answered, on the
//! simulated file, and assert exact properties of each open:
//!
//! - every frame that was answered is in the log after the open, whole;
//! - a frame whose own bytes and whose copy both tore is the torn tail and is cut, nothing
//!   reported damaged;
//! - a frame whose own bytes no longer read and whose copy does is written back from the copy
//!   and reported (`Recovery::copied`), never cut as torn;
//! - a damaged frame with an answered frame after it is reported damaged, never cut.
//!
//! The model is the one the confirmation kept before (docs/durable.md §6.2): damage at rest reaches
//! one of the two places a frame is held, its bytes in the segment or its copy in the persist area,
//! not both, as PAR §3.3.4 takes an item and its identifier stored apart.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::disallowed_macros,
    clippy::disallowed_methods,
    clippy::cast_possible_truncation,
    clippy::cognitive_complexity
)]

use hyper_block::block::BlockFile;
use hyper_block::buf::{AlignedBuf, Alignment};
use hyper_block::sim::SimFile;
use hyper_log::{Config, Entries, Entry, Log, LogError, Recovery, Refused, Update, Waits};

const ID: u128 = 0x636f_7069_6573_2121;
const BLOCK: usize = 512;
/// The persist area is one segment; its two record slots are a block each at four groups.
const SEGMENT: u64 = 32 * BLOCK as u64;
const RECORDS: u64 = 2 * BLOCK as u64;

fn config() -> Config {
    Config {
        segment_bytes: SEGMENT,
        // Room for twenty entries too large for a copy, none compacted.
        max_segments: 32,
        max_groups: 4,
        group_entries: 1 << 16,
        group_bytes: 1 << 24,
        group_cache: 1 << 10,
        queue_submissions: 256,
        waits: Waits::Measured,
    }
}

fn sim() -> SimFile {
    SimFile::new(
        Alignment::new(BLOCK).unwrap(),
        Alignment::new(BLOCK).unwrap(),
        11,
    )
    .unwrap()
}

/// The bytes of entry `index` of `len` bytes: each byte names the index, so a frame torn and
/// read whole would show.
fn bytes_of(index: u64, len: usize) -> Vec<u8> {
    (0..len)
        .map(|at| (index as u8).wrapping_mul(31).wrapping_add(at as u8))
        .collect()
}

/// Writes entry `index` of group 1 and waits for its answer.
fn write(log: &Log<SimFile>, index: u64, len: usize) {
    log.write(
        1,
        Update {
            entries: Some(Entries {
                first: index,
                entries: vec![Entry {
                    term: 1,
                    bytes: bytes_of(index, len),
                }],
            }),
            ..Update::default()
        },
    )
    .unwrap();
}

/// A file holding `image`, durable.
fn file_of(image: &[u8]) -> SimFile {
    let file = sim();
    let mut buf = AlignedBuf::zeroed(image.len(), Alignment::new(BLOCK).unwrap()).unwrap();
    buf.extend_from_slice(image).unwrap();
    file.write_all_at(buf.as_slice(), 0).unwrap();
    file.sync_data().unwrap();
    file
}

fn closed(log: Log<SimFile>) -> SimFile {
    log.close().unwrap()
}

/// Opens `image`, and checks that entries `1..=answered` are all held, whole.
fn open_holding(image: &[u8], answered: u64, len: usize) -> (Log<SimFile>, Recovery, u64) {
    let (log, recovery) = Log::open(file_of(image), config(), ID).unwrap();
    let last = log.view(1).unwrap().unwrap().last;
    assert!(
        last >= answered,
        "an answered entry was lost: last {last}, answered {answered}, {recovery:?}"
    );
    let held = log.entries(1, 1, last + 1, u64::MAX).unwrap();
    assert_eq!(held.len() as u64, last);
    for (at, entry) in held.iter().enumerate() {
        assert_eq!(
            entry.bytes,
            bytes_of(at as u64 + 1, len),
            "entry {} does not read whole",
            at + 1
        );
    }
    (log, recovery, last)
}

/// What one flush wrote: the runs of bytes `after` differs from `before` in, runs closer than a
/// sector merged, each with what it is part of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Part {
    Record,
    Copy,
    Frame,
}

fn runs(before: &[u8], after: &[u8]) -> Vec<(usize, usize, Part)> {
    assert_eq!(before.len(), after.len(), "the flush grew the file");
    let mut out: Vec<(usize, usize, Part)> = Vec::new();
    for at in 0..after.len() {
        if before[at] == after[at] {
            continue;
        }
        let part = if (at as u64) < RECORDS {
            Part::Record
        } else if (at as u64) < SEGMENT {
            Part::Copy
        } else {
            Part::Frame
        };
        match out.last_mut() {
            Some((_, end, p)) if *p == part && at - *end < BLOCK => *end = at + 1,
            _ => out.push((at, at + 1, part)),
        }
    }
    out
}

/// `before` with the parts of one flush laid over it: run `cut` up to its byte `upto`, and each
/// other run whole where `others` has its bit set.
fn image(
    before: &[u8],
    after: &[u8],
    runs: &[(usize, usize, Part)],
    cut: usize,
    upto: usize,
    others: u32,
) -> Vec<u8> {
    let mut out = before.to_vec();
    let mut other = 0;
    for (i, &(from, to, _)) in runs.iter().enumerate() {
        let end = if i == cut {
            from + upto
        } else {
            let whole = others & (1 << other) != 0;
            other += 1;
            if whole { to } else { from }
        };
        out[from..end].copy_from_slice(&after[from..end]);
    }
    out
}

/// Whether every run of `part` is whole in `image`.
fn whole(image: &[u8], after: &[u8], runs: &[(usize, usize, Part)], part: Part) -> bool {
    runs.iter()
        .filter(|r| r.2 == part)
        .all(|&(from, to, _)| image[from..to] == after[from..to])
}

/// `answered` entries of `len` bytes, each answered before the next was written, then entry
/// `answered + 1`: the durable images before and after its flush.
fn one_more(answered: u64, len: usize) -> (Vec<u8>, Vec<u8>) {
    let log = Log::create(sim(), config(), ID).unwrap();
    for index in 1..=answered {
        write(&log, index, len);
    }
    let file = closed(log);
    let before = file.durable_image().unwrap();
    let (log, _) = Log::open(file, config(), ID).unwrap();
    // The open wrote (it clears the copies' headers): the image the next flush starts from.
    let file = closed(log);
    let before_flush = file.durable_image().unwrap();
    assert_eq!(before_flush.len(), before.len());
    let (log, _) = Log::open(file_of(&before_flush), config(), ID).unwrap();
    write(&log, answered + 1, len);
    let after = closed(log).durable_image().unwrap();
    (before_flush, after)
}

/// Do: cut the power at every byte boundary of each write of a lone frame's flush (its frame,
/// its record, its copy), each other write of that flush landed or not, after three frames
/// answered. Expect: the three answered entries survive every cut; the fourth is held exactly
/// when its frame landed whole or its copy did, written back from the copy in the second case;
/// otherwise it is the torn tail, cut, and nothing is reported damaged.
#[test]
fn every_cut_of_a_lone_frames_flush_keeps_the_answered_and_cuts_only_the_torn() {
    for len in [24usize, 700] {
        let (before, after) = one_more(3, len);
        let runs = runs(&before, &after);
        assert!(
            runs.iter().any(|r| r.2 == Part::Copy),
            "the lone frame wrote no copy: {runs:?}"
        );
        assert!(runs.iter().any(|r| r.2 == Part::Frame), "{runs:?}");
        let others = 1u32 << (runs.len() - 1);
        let mut opened = 0;
        for cut in 0..runs.len() {
            let (from, to, _) = runs[cut];
            for upto in 0..=(to - from) {
                for set in 0..others {
                    let img = image(&before, &after, &runs, cut, upto, set);
                    let (_, recovery, last) = open_holding(&img, 3, len);
                    let frame = whole(&img, &after, &runs, Part::Frame);
                    let copy = whole(&img, &after, &runs, Part::Copy);
                    assert!(
                        recovery.damaged.is_empty(),
                        "a crash reported damage: {recovery:?}"
                    );
                    if frame {
                        assert_eq!(last, 4, "a whole frame was cut");
                        assert_eq!(recovery.copied, 0);
                    } else if copy {
                        assert_eq!(last, 4, "a frame whose copy landed was cut");
                        assert_eq!(recovery.copied, 1);
                    } else {
                        assert_eq!(last, 3, "a torn frame with a torn copy was kept");
                        assert_eq!(recovery.copied, 0);
                    }
                    opened += 1;
                }
            }
        }
        assert!(opened > 0);
    }
}

/// Do: answer a frame, then corrupt each byte of its bytes in the segment in turn, one at a time,
/// nothing written after it. Expect: every open holds it whole, written back from its copy and
/// reported, never cut as a torn tail (PAR §3.3.3's last entry, which a quorum may have
/// acknowledged).
#[test]
fn an_answered_last_frame_damaged_anywhere_is_restored_from_its_copy() {
    let len = 300;
    let (before, after) = one_more(2, len);
    let runs = runs(&before, &after);
    for &(from, to, part) in &runs {
        for at in from..to {
            let mut img = after.clone();
            img[at] ^= 0x40;
            let (_, recovery, last) = open_holding(&img, 3, len);
            assert_eq!(last, 3);
            match part {
                Part::Frame => assert_eq!(recovery.copied, 1, "byte {at} of the frame"),
                Part::Copy | Part::Record => assert_eq!(recovery.copied, 0, "byte {at}"),
            }
            assert!(recovery.damaged.is_empty(), "{recovery:?}");
        }
    }
}

/// Do: answer two frames, damage the first's bytes, and cut the power at every byte boundary of
/// each write of a third frame's flush. Expect: the damaged frame, with an answered frame after it,
/// is reported damaged in every open, never cut; the log refuses to open rather than drop it.
#[test]
fn damage_before_an_answered_frame_is_reported_whatever_the_next_flush_left() {
    let len = 40;
    // Where entry 1's frame lies: the frame run of its flush, the same in every log so made.
    let (before1, after1) = one_more(0, len);
    let (first_from, first_to, _) = *runs(&before1, &after1)
        .iter()
        .find(|r| r.2 == Part::Frame)
        .unwrap();
    let (_, after2) = one_more(1, len);
    let (before, after) = {
        let (log, _) = Log::open(file_of(&after2), config(), ID).unwrap();
        let file = closed(log);
        let before = file.durable_image().unwrap();
        let (log, _) = Log::open(file_of(&before), config(), ID).unwrap();
        write(&log, 3, len);
        (before, closed(log).durable_image().unwrap())
    };
    let third = runs(&before, &after);
    let others = 1u32 << (third.len() - 1);
    for cut in 0..third.len() {
        let (f, t, _) = third[cut];
        for upto in 0..=(t - f) {
            for set in 0..others {
                let mut img = image(&before, &after, &third, cut, upto, set);
                img[(first_from + first_to) / 2] ^= 0x01;
                match Log::try_open(file_of(&img), config(), ID) {
                    Err(Refused {
                        error: LogError::Damaged(_),
                        ..
                    }) => {}
                    Err(Refused { error, .. }) => panic!("refused, not as damage: {error:?}"),
                    Ok((log, recovery)) => {
                        let last = log.view(1).unwrap().unwrap().last;
                        assert!(last >= 2, "an answered frame was cut: {recovery:?}");
                    }
                }
            }
        }
    }
}

/// Do: write twenty entries one at a time, each waited for, on the simulated file, and count the
/// durable operations its frames made: flushes (less those of fills ahead of a slot, which no
/// append waits on, docs/durable.md §6.3) and writes made durable on their own; then the same with
/// entries too large for a copy. Expect: one an append with copies, where the confirmation made
/// two; two without (an entry larger than a copy slot is confirmed as before).
#[test]
fn a_lone_append_costs_one_flush() {
    let count = 20u64;
    let durable_ops = |len: usize| {
        let log = Log::create(sim(), config(), ID).unwrap();
        write(&log, 1, len);
        let before = log.stats(None).unwrap();
        for index in 2..=count + 1 {
            write(&log, index, len);
        }
        let after = log.stats(None).unwrap();
        assert_eq!(after.frames - before.frames, count, "one frame an append");
        (after.flushes - before.flushes) - (after.fills - before.fills)
            + (after.durable_writes - before.durable_writes)
    };
    assert_eq!(durable_ops(64), count, "one flush an append");
    // A copy slot holds (32 blocks less two) / 2 = 15 blocks, a block of it the header.
    assert_eq!(
        durable_ops(15 * BLOCK),
        2 * count,
        "two an append without a copy"
    );
}
