//! mantle's own shell at its step 1 (`85b9c2d`): its range `Replica` as at `1c179e8`, over the
//! hyper-raft, hyper-log and hyper-block snapshots of hyper-raft `687244f`, the log this
//! repository's shell runs on, so beside the D-1 side the shells differ and the log does not.
//! Driven as `mantle.rs` drives `1c179e8`'s: every member begins its ready, the messages are
//! delivered, and every member waits for its write (`begin`, then `wait_persisted`). One ready of
//! a member is out at a time.
use std::collections::VecDeque;
use std::time::{Duration, Instant};

use mantle_meta_s1::apply::Layer;
use mantle_meta_s1::engine::{Engine, Model};
use mantle_meta_s1::name;
use mantle_meta_s1::session::Rules;
use mantle_meta_s1::wire::Entry;
use mantle_range_s1::{ConfState, Message, Range, Replica, ReplicaError, Settings};
use mantle_s1_hyper_block::block::BlockFile;
use mantle_s1_hyper_log::{Config as LogConfig, Log, Waits};

/// The range's group, as mantle's group test names it.
const GROUP: u128 = 0x0072_616e_6765;

/// mantle's group test's log (`crates/range/tests/group.rs`), the writer's measured waits.
pub fn log_config() -> LogConfig {
    LogConfig {
        segment_bytes: 64 * 4096,
        max_segments: 64,
        max_groups: 16,
        group_entries: 1 << 18,
        group_bytes: 1 << 26,
        group_cache: 1 << 16,
        queue_submissions: 64,
        waits: Waits::Measured,
    }
}

/// The workload's session rules (`workload::RULES`), in this side's types.
const RULES: Rules = Rules {
    lifetime_ns: 3_600_000_000_000,
    max_sessions: 1 << 20,
    max_answers: 16,
    max_answer_bytes: usize::MAX,
    expiries_per_entry: 8,
};

/// The engine of a cell's first Name range (`workload::first_range`), in this side's types.
fn first_range() -> Model {
    let mut m = Model::default();
    m.install(0, name::first(1).expect("the first range"))
        .expect("installed");
    m.persist().expect("persisted");
    m
}

/// The workload's entry in this side's types: the same bytes, decoded.
pub fn entry(of: &mantle_meta::wire::Entry) -> Entry {
    Entry::decode(&of.encode().expect("encodes")).expect("decodes")
}

/// mantle's group test's settings.
const SETTINGS: Settings = Settings {
    election_tick: 10,
    heartbeat_tick: 2,
    max_size_per_msg: 1 << 16,
    max_inflight_msgs: 16,
    max_uncommitted_size: 1 << 20,
    max_committed_size_per_ready: 1 << 22,
    max_entry_bytes: 1 << 16,
};

struct Node<F: BlockFile + 'static> {
    // Dropped before the log, whose owner its handle reaches.
    replica: Replica<F, Model>,
    log: Log<F>,
}

pub struct Group<F: BlockFile + 'static> {
    nodes: Vec<Node<F>>,
    wire: VecDeque<Message>,
    /// How long each of the leader's writes took: its `begin` to its `wait_persisted`'s return.
    writes: Vec<Duration>,
}

impl<F: BlockFile + 'static> Group<F> {
    /// A group of `members` on the devices `device` makes, member 1 elected.
    pub fn open(members: u64, mut device: impl FnMut(u64) -> F) -> Self {
        let range = Range {
            layer: Layer::Name,
            rules: RULES,
            boot: ConfState {
                voters: (1..=members).collect(),
                ..ConfState::default()
            },
            settings: SETTINGS,
        };
        let nodes = (1..=members)
            .map(|id| {
                let log = Log::create(device(id), log_config(), 0x6c6f67 + u128::from(id)).unwrap();
                let replica = Replica::open(id, GROUP, &log, first_range(), &range, id).unwrap();
                Node { replica, log }
            })
            .collect();
        let mut group = Self {
            nodes,
            wire: VecDeque::new(),
            writes: Vec::new(),
        };
        group.nodes[0].replica.campaign().unwrap();
        for _ in 0..10_000 {
            group.round(&mut |_| {});
            if group.nodes[0].replica.is_leader() && group.wire.is_empty() {
                return group;
            }
        }
        panic!("mantle step 1's group never elected member 1");
    }

    /// One round: every member begins, the messages arrive, every member waits for its write.
    fn round(&mut self, applied: &mut dyn FnMut(bool)) {
        let mut submitted = None;
        for (at, node) in self.nodes.iter_mut().enumerate() {
            let out = node.replica.begin().unwrap();
            self.wire.extend(out.messages);
            if at == 0 {
                submitted = out.persisting.then(Instant::now);
                applied(out.applied.iter().any(|a| !a.answers.is_empty()));
            }
        }
        while let Some(m) = self.wire.pop_front() {
            let to = usize::try_from(m.to).unwrap() - 1;
            match self.nodes[to].replica.step(m) {
                Ok(())
                | Err(
                    ReplicaError::Refused(_)
                    | ReplicaError::Stalled
                    | ReplicaError::MessagesHeld { .. },
                ) => {}
                Err(e) => panic!("mantle step 1 step: {e}"),
            }
        }
        for node in &mut self.nodes {
            node.replica.wait_persisted();
        }
        if let Some(at) = submitted {
            self.writes.push(at.elapsed());
        }
    }

    /// Proposes `entry` at the leader and drives until the leader applied it.
    pub fn commit(&mut self, entry: &Entry) {
        loop {
            match self.nodes[0].replica.propose(entry) {
                Ok(()) => break,
                Err(ReplicaError::Stalled) => self.round(&mut |_| {}),
                Err(e) => panic!("mantle step 1 propose: {e}"),
            }
        }
        for _ in 0..100_000 {
            let mut done = false;
            self.round(&mut |answered| done |= answered);
            if done {
                return;
            }
        }
        panic!("mantle step 1's group never applied an entry");
    }

    /// The leader's writes' times since the last call.
    pub fn take_writes(&mut self) -> Vec<Duration> {
        std::mem::take(&mut self.writes)
    }

    /// Frames every member's log wrote and flushed.
    pub fn flushes(&self) -> u64 {
        self.nodes.iter().map(|n| n.log.flushed().0).sum()
    }
}
