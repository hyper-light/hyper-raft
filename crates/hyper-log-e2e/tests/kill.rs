//! hyper-log in real use: a writer process (`hyper-log-writer`) appends to a log in a real file,
//! fully flushed, and is killed with SIGKILL mid-append, again and again; after each kill the
//! log is opened in this process and checked against every append the writer acknowledged:
//! - the open recovers, with no group reported damaged;
//! - every group holds every entry the writer acknowledged, up to its last acknowledged, with
//!   the bytes it wrote, from where its log starts on (an append not acknowledged may have
//!   landed or not, never in part);
//! - a group's start is never behind the start it had when its last acknowledged append was
//!   answered, and its hard state's commit never behind that append.
//!
//! Then the writer is started again on the same file and goes on from where each group's log
//! ends, until the last cycle lets it finish and close the log.
//!
//! The kill comes once the writer has acknowledged a number of appends drawn for the cycle:
//! the writer submits continuously, so the kill lands while a frame is being written, flushed
//! or confirmed. Every wait is on the fact it needs: a line of the writer's output, or its exit.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::disallowed_macros,
    clippy::cast_possible_truncation,
    clippy::cognitive_complexity,
    missing_docs
)]

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};

use hyper_log::Log;
use hyper_log_e2e::{ID, config, open, payload};

const WRITER: &str = env!("CARGO_BIN_EXE_hyper-log-writer");
const TMP: &str = env!("CARGO_TARGET_TMPDIR");

/// Groups the writer appends to: enough that a frame carries several updates.
const GROUPS: usize = 8;
/// Writer processes killed in one run, before the last, which finishes.
const KILLS: u64 = 24;
/// Rounds a writer would run unkilled: past what any kill lets it reach.
const ROUNDS: u64 = 1 << 20;
/// Rounds the last writer runs, to its end.
const LAST_ROUNDS: u64 = 40;

/// What the writers acknowledged of one group: its last entry, and its start then.
#[derive(Debug, Clone, Copy, Default)]
struct Acked {
    last: u64,
    start: u64,
}

/// A file the test makes in its own target directory, under its process id, removed when the
/// test lets it go: whatever the test does with it, it leaves nothing behind (a run left one of
/// each file a run, on a disk with little room).
struct Scratch(PathBuf);

impl std::ops::Deref for Scratch {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for Scratch {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    #[expect(
        clippy::disallowed_methods,
        reason = "a test removes the file it made in its own target directory"
    )]
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "a test removes the file it made in its own target directory"
)]
fn fresh(name: &str) -> Scratch {
    let path = PathBuf::from(TMP).join(format!("log-{}-{name}", std::process::id()));
    let _ = std::fs::remove_file(&path);
    Scratch(path)
}

fn writer(path: &Path, rounds: u64) -> (Child, BufReader<ChildStdout>) {
    let mut child = Command::new(WRITER)
        .arg(path)
        .arg(GROUPS.to_string())
        .arg(rounds.to_string())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let out = BufReader::new(child.stdout.take().unwrap());
    (child, out)
}

/// Records one line of the writer's output: an acknowledgement, or `done`.
fn heard(line: &str, acked: &mut BTreeMap<u128, Acked>) -> bool {
    let fields: Vec<&str> = line.split_whitespace().collect();
    match fields.as_slice() {
        ["ack", group, index, start] => {
            let a = acked.entry(group.parse().unwrap()).or_default();
            let index: u64 = index.parse().unwrap();
            assert!(index > a.last, "acknowledged {index} after {}", a.last);
            a.last = index;
            a.start = start.parse().unwrap();
            true
        }
        ["done"] => false,
        other => panic!("the writer said {other:?}"),
    }
}

/// Opens the log the killed writer left and checks it against what it acknowledged.
fn check(path: &Path, acked: &BTreeMap<u128, Acked>, cycle: u64) {
    let (log, recovery) = Log::open(open(path, false).unwrap(), config(GROUPS), ID).unwrap();
    assert!(
        recovery.damaged.is_empty(),
        "cycle {cycle}: damaged {recovery:?}"
    );
    for (&group, a) in acked {
        let view = log.view(group).unwrap().unwrap();
        assert!(
            view.last >= a.last,
            "cycle {cycle}: group {group} holds through {} of {} acknowledged",
            view.last,
            a.last
        );
        assert!(
            view.start.index >= a.start,
            "cycle {cycle}: group {group}'s start {} went back past {}",
            view.start.index,
            a.start
        );
        let hard = view.hard_state.unwrap();
        assert!(
            hard.commit >= a.last,
            "cycle {cycle}: group {group}'s commit {} behind {}",
            hard.commit,
            a.last
        );
        let first = view.start.index + 1;
        let entries = log.entries(group, first, view.last + 1, u64::MAX).unwrap();
        assert_eq!(entries.len() as u64, view.last + 1 - first, "cycle {cycle}");
        for (index, entry) in (first..).zip(&entries) {
            assert_eq!(entry.term, 1);
            assert!(
                entry.bytes == payload(group, index),
                "cycle {cycle}: group {group} entry {index} has the wrong bytes"
            );
        }
    }
    log.close().unwrap();
}

#[test]
fn a_writer_killed_mid_append_loses_nothing_it_acknowledged() {
    let path = fresh("kill");
    let mut acked: BTreeMap<u128, Acked> = BTreeMap::new();
    // SplitMix64 of the cycle: how many acknowledgements each writer gives before its kill.
    let draw = |cycle: u64| {
        let mut z = cycle.wrapping_add(1).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        1 + (z ^ (z >> 31)) % (12 * GROUPS as u64)
    };
    let mut kills_mid_append = 0u64;
    for cycle in 0..KILLS {
        let (mut child, mut out) = writer(&path, ROUNDS);
        let target = draw(cycle);
        let mut heard_now = 0u64;
        let mut line = String::new();
        while heard_now < target {
            line.clear();
            assert!(
                out.read_line(&mut line).unwrap() > 0,
                "cycle {cycle}: the writer ended"
            );
            assert!(
                heard(&line, &mut acked),
                "cycle {cycle}: the writer finished"
            );
            heard_now += 1;
        }
        // SIGKILL on Unix, TerminateProcess on Windows: no unwinding, no close, no flush.
        child.kill().unwrap();
        let status = child.wait().unwrap();
        assert!(
            !status.success(),
            "cycle {cycle}: the writer was not killed"
        );
        // What it printed before it died was acknowledged too.
        loop {
            line.clear();
            if out.read_line(&mut line).unwrap() == 0 {
                break;
            }
            if line.trim().is_empty() {
                continue;
            }
            heard(&line, &mut acked);
            kills_mid_append += 1;
        }
        check(&path, &acked, cycle);
    }
    let (mut child, mut out) = writer(&path, LAST_ROUNDS);
    let mut line = String::new();
    loop {
        line.clear();
        assert!(
            out.read_line(&mut line).unwrap() > 0,
            "the last writer ended unfinished"
        );
        if !heard(&line, &mut acked) {
            break;
        }
    }
    assert!(child.wait().unwrap().success());
    check(&path, &acked, KILLS);
    let total: u64 = acked.values().map(|a| a.last).sum();
    println!(
        "{KILLS} writers killed, {total} appends acknowledged across {GROUPS} groups, \
         {kills_mid_append} acknowledgements read after a kill"
    );
    assert!(total > LAST_ROUNDS * GROUPS as u64);
}
