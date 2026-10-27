//! slates' consensus retention publication against a transition through this shell (`docs/durable.md`
//! §14 item 8): what a group's record costs to make durable as its log grows.
//!
//! slates publishes a node's retained Raft state on the acknowledgement path of every vote, append and
//! configuration change: it clones the state (`RaftNode::saved`), encodes it (`SavedRaft::to_bytes`)
//! and hashes the bytes with BLAKE3 (slates `crates/cluster/tests/publication_cost.rs` at `ec5e0df`,
//! whose workload this is: a lone leader, commands of 64 bytes). Through this shell the same group's
//! store is the publication (`RamStore`, slates' shape: each write durable as it is submitted, depth
//! one), and a transition is the shell's whole path for one command: its proposal, the drive that
//! takes the `Ready` and submits its write, and the drive that takes the write's answer and applies.
use std::task::Waker;
use std::time::{Duration, Instant};

use hyper_durable::{
    EntryRef, Fatal, Output, Point, RamStore, Replica, Settings, StateMachine, Unbounded,
};
use hyper_raft::Config;
use hyper_raft::proto::{ConfChangeV2, ConfState};
use slates_cluster::raft::RaftNode;
use slates_db::register::HostId;
use slates_wire::Wire;

/// slates' measured command: a record's encoded configuration command is tens of bytes
/// (`publication_cost.rs`'s `COMMAND_BYTES`).
const COMMAND_BYTES: usize = 64;

/// One publication of slates' retained state at a log of `entries` commands: the clone, the
/// encoding and the hash, and the bytes published.
pub fn slates_publication(entries: u64, runs: usize) -> Vec<(Duration, Duration, Duration, usize)> {
    let mut leader = RaftNode::new(HostId(1), vec![HostId(1)]);
    let _ = leader.start_election().expect("a lone node elects itself");
    for appended in 0..entries {
        let mut command = vec![0u8; COMMAND_BYTES];
        command[..8].copy_from_slice(&appended.to_le_bytes());
        assert!(leader.append_command(command));
    }
    (0..runs)
        .map(|_| {
            let started = Instant::now();
            let saved = leader.saved();
            let cloned = started.elapsed();
            let started = Instant::now();
            let bytes = saved.to_bytes();
            let encoded = started.elapsed();
            let started = Instant::now();
            let hash = blake3::hash(&bytes);
            let hashed = started.elapsed();
            assert_ne!(hash.as_bytes(), &[0u8; 32]);
            (cloned, encoded, hashed, bytes.len())
        })
        .collect()
}

/// A group's state machine that keeps nothing: what is measured is the shell's path and its store.
struct Nothing {
    configuration: ConfState,
}

impl StateMachine for Nothing {
    type Answer = ();

    fn apply(&mut self, entry: &EntryRef<'_>, answers: &mut Vec<()>) -> Result<(), Fatal> {
        if !entry.data.is_empty() {
            answers.push(());
        }
        Ok(())
    }
    fn apply_change(&mut self, _: Point, _: &ConfChangeV2, c: &ConfState) -> Result<(), Fatal> {
        self.configuration = c.clone();
        Ok(())
    }
    fn durable(&self) -> Point {
        Point::default()
    }
    fn configuration(&self) -> &ConfState {
        &self.configuration
    }
    fn acts_at_start(&self, _: &EntryRef<'_>) -> bool {
        false
    }
    fn image(&mut self, _: &mut Vec<u8>) -> Result<(Point, ConfState), Fatal> {
        Err(Fatal("the run compacts nothing"))
    }
    fn image_bytes(&self) -> Option<u64> {
        None
    }
    fn install(&mut self, _: &[u8], _: Point, _: &ConfState) -> Result<(), Fatal> {
        Err(Fatal("the run compacts nothing"))
    }
    fn persist(&mut self) -> Result<(), Fatal> {
        Ok(())
    }
}

/// A sole voter's settings: slates' groups elect on ticks today; a lone leader never needs to.
fn settings() -> Settings {
    let limits = hyper_raft::Limits::derive(hyper_raft::Stated {
        message: (1 << 16) + hyper_raft::wire::MESSAGE_RECORD_FIXED_BYTES,
        members: 1,
        memory: 64 << 20,
        depth: 1,
    })
    .expect("the run's statement gives its bounds");
    Settings {
        core: Config {
            check_quorum: true,
            pre_vote: true,
            ..Config::new(1, limits)
        },
        quiet: Duration::from_millis(10),
        elections: hyper_raft::Elections::Suspicion,
        disk: hyper_durable::Disk::Flushed,
    }
}

/// Drives `r` until its writes are answered and nothing is left to do now.
fn settle(r: &mut Replica<RamStore, Nothing, Unbounded>, now: &mut u64, out: &mut Output<()>) {
    // A counted bound: a sole voter on a store that answers at once rests within a few drives.
    for _ in 0..1_000 {
        *now += 1;
        out.clear();
        let driven = r
            .drive(*now, Waker::noop(), out)
            .expect("the leader drives");
        if !driven.more && driven.out == 0 {
            return;
        }
    }
    panic!("the leader never rested");
}

/// One transition through the shell at a log of `entries` commands, `runs` times: a command's
/// proposal, its write and its answer.
pub fn shell_transition(entries: u64, runs: usize) -> Vec<Duration> {
    let machine = Nothing {
        configuration: ConfState {
            voters: vec![1],
            ..ConfState::default()
        },
    };
    let mut r =
        Replica::open(&settings(), RamStore::new(), machine, Unbounded).expect("a member opens");
    let mut now = 0u64;
    let mut out = Output::default();
    r.campaign().expect("a sole voter campaigns");
    settle(&mut r, &mut now, &mut out);
    assert!(r.is_leader());
    let command = |n: u64| {
        let mut c = vec![0u8; COMMAND_BYTES];
        c[..8].copy_from_slice(&n.to_le_bytes());
        c
    };
    for n in 0..entries {
        r.propose(Vec::new(), command(n))
            .expect("the leader takes it");
        settle(&mut r, &mut now, &mut out);
    }
    let first = r.applied().index;
    let times = (0..runs)
        .map(|k| {
            let started = Instant::now();
            r.propose(Vec::new(), command(entries + k as u64))
                .expect("the leader takes it");
            settle(&mut r, &mut now, &mut out);
            started.elapsed()
        })
        .collect();
    // Each timed transition applied its command: the store made it durable and the shell applied it.
    assert_eq!(r.applied().index, first + runs as u64);
    assert_eq!(r.durable_commit(), r.applied().index);
    times
}
