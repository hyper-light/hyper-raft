//! mantle's range replica on this shell (mantle's D-1): its `Replica` over hyper-log's group handle
//! with its engine and layer as the state machine, at the mantle commit that switched it and over
//! the snapshots mantle vendors there, driven as mantle's node is to drive a range (mantle
//! `docs/design/node.md` §2.2): a member is driven when a message arrived for it, when its log
//! answered one of its writes and woke it, or when its last drive said there is more, in the order
//! it became due; what a drive gives out is delivered at once. Its own settings, as mantle opens a
//! range: elections on ticks, no quiet write.
use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, sync_channel};
use std::task::Waker;
use std::time::{Duration, Instant};

use mantle_d1_hyper_block::block::BlockFile;
use mantle_d1_hyper_log::{Config as LogConfig, Log, Waits};
use mantle_meta_d1::apply::Layer;
use mantle_meta_d1::engine::{Engine, Model};
use mantle_meta_d1::name;
use mantle_meta_d1::session::Rules;
use mantle_meta_d1::wire::Entry;
use mantle_range_d1::{
    ConfState, GroupStore, Message, Output, Range, Replica, ReplicaError, Settings,
};

/// The range's group, as mantle's group test names it.
const GROUP: u128 = 0x0072_616e_6765;

/// The same log as the other sides (`mantle::log_config`).
fn log_config() -> LogConfig {
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

/// The other sides' settings (`mantle::SETTINGS`).
const SETTINGS: Settings = Settings {
    election_tick: 10,
    heartbeat_tick: 2,
    max_size_per_msg: 1 << 16,
    max_inflight_msgs: 16,
    max_uncommitted_size: 1 << 20,
    max_committed_size_per_ready: 1 << 22,
    max_entry_bytes: 1 << 16,
};

/// The other sides' session rules (`workload::RULES`).
const RULES: Rules = Rules {
    lifetime_ns: 3_600_000_000_000,
    max_sessions: 1 << 20,
    max_answers: 16,
    max_answer_bytes: usize::MAX,
    expiries_per_entry: 8,
};

/// The engine of a cell's first Name range (`workload::first_range`).
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

struct Member<F: BlockFile + 'static> {
    replica: Replica<GroupStore<F>, Model>,
    waker: Waker,
    queued: bool,
}

pub struct Group<F: BlockFile + 'static> {
    // Dropped before the logs, whose owners the stores reach.
    members: Vec<Member<F>>,
    logs: Vec<Log<F>>,
    woken: Receiver<usize>,
    /// Members due a drive, in the order they became due.
    queue: VecDeque<usize>,
    out: Output,
    wire: VecDeque<Message>,
    epoch: Instant,
    /// How long each of the leader's writes took, its submission to the drive that took its answer.
    writes: Vec<Duration>,
    /// Waits for a log's answer by polling, not blocking (`HYPER_DURABLE_SPIN`): what the wake
    /// itself costs, against mantle's shell, whose harness blocks on the log's own wait.
    spin: bool,
}

impl<F: BlockFile + 'static> Group<F> {
    pub fn open(members: u64, mut device: impl FnMut(u64) -> F) -> Self {
        let (tell, woken) = sync_channel(4096);
        let range = Range {
            layer: Layer::Name,
            rules: RULES,
            boot: ConfState {
                voters: (1..=members).collect(),
                ..ConfState::default()
            },
            settings: SETTINGS,
        };
        let mut logs = Vec::new();
        let mut all = Vec::new();
        for id in 1..=members {
            let log = Log::create(device(id), log_config(), 0x6c6f67 + u128::from(id)).unwrap();
            let store = mantle_range_d1::claim(&log, GROUP, &range).unwrap();
            let replica = Replica::open(id, store, first_range(), &range, id).unwrap();
            let (waker, _) =
                hyper_measure::wake::waker(usize::try_from(id - 1).unwrap(), tell.clone());
            all.push(Member {
                replica,
                waker,
                queued: false,
            });
            logs.push(log);
        }
        let mut group = Self {
            members: all,
            logs,
            woken,
            queue: VecDeque::new(),
            out: Output::default(),
            wire: VecDeque::new(),
            epoch: Instant::now(),
            writes: Vec::new(),
            spin: std::env::var_os("HYPER_DURABLE_SPIN").is_some(),
        };
        group.members[0].replica.campaign().unwrap();
        group.schedule(0);
        for _ in 0..100_000 {
            group.turn(&mut |_| {});
            let leads = group.members[0].replica.is_leader();
            if leads && group.wire.is_empty() && group.queue.is_empty() {
                return group;
            }
            group.wait();
        }
        panic!("mantle D-1's group never elected member 1");
    }

    fn schedule(&mut self, at: usize) {
        if let Some(m) = self.members.get_mut(at)
            && !m.queued
        {
            m.queued = true;
            self.queue.push_back(at);
        }
    }

    fn now(&self) -> u64 {
        u64::try_from(self.epoch.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }

    /// Drives every member due once, as the owner's turn does, and delivers what they gave out.
    fn turn(&mut self, answered: &mut dyn FnMut(bool)) {
        for _ in 0..self.queue.len() {
            let Some(at) = self.queue.pop_front() else {
                break;
            };
            let now = self.now();
            let member = &mut self.members[at];
            member.queued = false;
            self.out.clear();
            let driven = member
                .replica
                .drive(now, &member.waker, &mut self.out)
                .unwrap();
            if driven.more {
                member.queued = true;
                self.queue.push_back(at);
            }
            self.wire.extend(self.out.messages.drain(..));
            if at == 0 {
                if let Some((submitted, taken)) = driven.flushed {
                    self.writes
                        .push(Duration::from_nanos(taken.saturating_sub(submitted)));
                }
                answered(!self.out.answers.is_empty());
            }
        }
        while let Some(m) = self.wire.pop_front() {
            let Some(to) = usize::try_from(m.to).ok().and_then(|to| to.checked_sub(1)) else {
                continue;
            };
            let Some(member) = self.members.get_mut(to) else {
                continue;
            };
            match member.replica.step(m) {
                Ok(()) | Err(ReplicaError::Refused(_) | ReplicaError::Stalled) => {}
                Err(e) => panic!("mantle D-1 step: {e}"),
            }
            self.schedule(to);
        }
    }

    /// With nothing due, waits for a log to answer a write.
    fn wait(&mut self) {
        if self.queue.is_empty() && self.spin {
            // Polled for at most a second, then waited for as before.
            let until = Instant::now() + Duration::from_secs(1);
            while Instant::now() < until {
                if let Ok(at) = self.woken.try_recv() {
                    self.schedule(at);
                    break;
                }
                std::hint::spin_loop();
            }
        }
        if self.queue.is_empty()
            && let Ok(at) = self.woken.recv()
        {
            self.schedule(at);
        }
        while let Ok(at) = self.woken.try_recv() {
            self.schedule(at);
        }
    }

    pub fn commit(&mut self, entry: &Entry) {
        self.members[0].replica.propose(entry).unwrap();
        self.schedule(0);
        for _ in 0..1_000_000 {
            let mut done = false;
            self.turn(&mut |answered| done |= answered);
            if done {
                return;
            }
            self.wait();
        }
        panic!("mantle D-1's group never applied an entry");
    }

    /// The leader's writes' times since the last call.
    pub fn take_writes(&mut self) -> Vec<Duration> {
        std::mem::take(&mut self.writes)
    }

    pub fn flushes(&self) -> u64 {
        self.logs.iter().map(|l| l.flushed().0).sum()
    }
}
