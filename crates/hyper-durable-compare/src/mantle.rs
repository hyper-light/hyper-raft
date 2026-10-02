//! mantle's own shell: its range `Replica` at origin/dev `1c179e8`, over the hyper-log mantle
//! vendors there, driven as mantle's node drives it: every member begins its ready (its leader's
//! messages out before its own write, its update submitted), the messages are delivered, and every
//! member waits for its write (`begin`, then `wait_persisted`: mantle `crates/range/src/replica.rs`,
//! audit §5.1). One ready of a member is out at a time.
use std::collections::VecDeque;

use mantle_hyper_block::block::BlockFile;
use mantle_hyper_log::{Config as LogConfig, Log, Waits};
use mantle_meta::engine::Model;
use mantle_meta::wire::Entry;
use mantle_range::{ConfState, Message, Range, Replica, ReplicaError, Settings};

use crate::workload::{LAYER, RULES, first_range};

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

/// mantle's group test's settings.
pub const SETTINGS: Settings = Settings {
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
}

impl<F: BlockFile + 'static> Group<F> {
    /// A group of `members` on the devices `device` makes, member 1 elected.
    pub fn open(members: u64, mut device: impl FnMut(u64) -> F) -> Self {
        let range = Range {
            layer: LAYER,
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
        };
        group.nodes[0].replica.campaign().unwrap();
        for _ in 0..10_000 {
            group.round(&mut |_| {});
            if group.nodes[0].replica.is_leader() && group.wire.is_empty() {
                return group;
            }
        }
        panic!("mantle's group never elected member 1");
    }

    /// One round: every member begins, the messages arrive, every member waits for its write.
    fn round(&mut self, applied: &mut dyn FnMut(bool)) {
        for (at, node) in self.nodes.iter_mut().enumerate() {
            let out = node.replica.begin().unwrap();
            self.wire.extend(out.messages);
            if at == 0 {
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
                Err(e) => panic!("mantle step: {e}"),
            }
        }
        for node in &mut self.nodes {
            node.replica.wait_persisted();
        }
    }

    /// Proposes `entry` at the leader and drives until the leader applied it.
    pub fn commit(&mut self, entry: &Entry) {
        loop {
            match self.nodes[0].replica.propose(entry) {
                Ok(()) => break,
                Err(ReplicaError::Stalled) => self.round(&mut |_| {}),
                Err(e) => panic!("mantle propose: {e}"),
            }
        }
        for _ in 0..100_000 {
            let mut done = false;
            self.round(&mut |answered| done |= answered);
            if done {
                return;
            }
        }
        panic!("mantle's group never applied an entry");
    }

    /// Frames every member's log wrote and flushed.
    pub fn flushes(&self) -> u64 {
        self.nodes.iter().map(|n| n.log.flushed().0).sum()
    }
}
