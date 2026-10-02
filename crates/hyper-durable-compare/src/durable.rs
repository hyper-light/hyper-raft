//! hyper-durable: `Replica`s over hyper-log's group handle (`GroupStore`) with mantle's engine and
//! layer as the state machine (`RangeMachine`, `docs/durable.md` §11's D-1 shape), held by one
//! `Owner` and driven by its turns, the log's answers waking it. Readies are taken ahead of their
//! persistence to the log's depth.
use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, sync_channel};
use std::task::Waker;
use std::sync::OnceLock;
use std::time::Instant;

use hyper_block::block::BlockFile;
use hyper_durable::{
    EntryRef, Fatal, Fault, GroupStore, Handle, LogStore, Output, Owner, Point, Replica,
    ReplicaError, Settings, StateMachine, StoreView, Unbounded, Write,
};
use hyper_log::{Config as LogConfig, Log, Waits};
use hyper_raft::Config;
use hyper_raft::StorageError;
use hyper_raft::proto::{ConfState, Message};
use mantle_meta::apply::apply_entry;
use mantle_meta::engine::{Engine, Model, Rows};
use mantle_meta::wire::Entry;

use crate::workload::{LAYER, RULES, first_range};

/// The range's group, as mantle's group test names it.
const GROUP: u128 = 0x0072_616e_6765;

/// The same log as mantle's side (`mantle::log_config`), of this repository's hyper-log.
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

/// A range's engine and layer as a state machine: `apply` is mantle's `apply_entry`.
pub struct RangeMachine {
    engine: Model,
    configuration: ConfState,
    applied: Point,
    persisted: Point,
}

impl StateMachine for RangeMachine {
    /// The answers an entry made: the run waits for its own entry's.
    type Answer = usize;

    fn apply(&mut self, entry: &EntryRef<'_>, answers: &mut Vec<usize>) -> Result<(), Fatal> {
        if entry.data.is_empty() {
            self.engine
                .apply(entry.index, &[])
                .map_err(|_| Fatal("the engine"))?;
        } else {
            let batch =
                Entry::decode(entry.data).map_err(|_| Fatal("an entry that does not decode"))?;
            let made = apply_entry(&mut self.engine, entry.index, &batch, LAYER, &RULES)
                .map_err(|_| Fatal("the layer"))?;
            answers.push(made.len());
        }
        self.applied = Point {
            index: entry.index,
            term: entry.term,
        };
        Ok(())
    }
    fn apply_change(&mut self, at: Point, configuration: &ConfState) -> Result<(), Fatal> {
        self.engine
            .apply(at.index, &[])
            .map_err(|_| Fatal("the engine"))?;
        self.configuration = configuration.clone();
        self.applied = at;
        Ok(())
    }
    fn durable(&self) -> Point {
        self.persisted
    }
    fn configuration(&self) -> &ConfState {
        &self.configuration
    }
    fn acts_at_start(&self, _: &EntryRef<'_>) -> bool {
        false
    }
    fn image(&mut self, _: &mut Vec<u8>) -> Result<Point, Fatal> {
        Err(Fatal("the run compacts nothing"))
    }
    fn install(&mut self, _: &[u8], _: Point, _: &ConfState) -> Result<(), Fatal> {
        Err(Fatal("the run compacts nothing"))
    }
    fn persist(&mut self) -> Result<(), Fatal> {
        self.engine.persist().map_err(|_| Fatal("the engine"))?;
        self.persisted = self.applied;
        Ok(())
    }
}

/// hyper-log's group handle at a depth the run chooses (`HYPER_DURABLE_DEPTH`, the log's own
/// when unset): how much of a difference the readies taken ahead make.
pub struct Depth<F: BlockFile + 'static>(GroupStore<F>, usize);

impl<F: BlockFile + 'static> LogStore for Depth<F> {
    fn depth(&self) -> usize {
        self.1
    }
    fn view(&self) -> Result<StoreView, Fault> {
        self.0.view()
    }
    fn bounds(&self) -> Result<(Point, u64), StorageError> {
        self.0.bounds()
    }
    fn term(&self, index: u64) -> Result<u64, StorageError> {
        self.0.term(index)
    }
    fn entries(
        &self,
        low: u64,
        high: u64,
        max: u64,
        into: &mut Vec<hyper_raft::proto::Entry>,
    ) -> Result<(), StorageError> {
        self.0.entries(low, high, max, into)
    }
    fn visit(
        &self,
        low: u64,
        high: u64,
        page: u64,
        visit: &mut dyn FnMut(EntryRef<'_>) -> bool,
    ) -> Result<(), StorageError> {
        self.0.visit(low, high, page, visit)
    }
    fn proposals(&self, into: &mut Vec<hyper_raft::proto::Entry>) -> Result<(), StorageError> {
        self.0.proposals(into)
    }
    fn room(&self) -> bool {
        self.0.room()
    }
    fn submit(&mut self, write: &Write<'_>, waker: &Waker) -> Result<(), Fault> {
        self.0.submit(write, waker)
    }
    fn poll(&mut self) -> Option<Result<(), Fault>> {
        self.0.poll()
    }
    fn write_now(&mut self, write: &Write<'_>) -> Result<(), Fault> {
        self.0.write_now(write)
    }
}

type Member<F> = Replica<Depth<F>, RangeMachine, Unbounded>;

pub struct Group<F: BlockFile + 'static> {
    // Dropped before the logs, whose owners the handles reach.
    owner: Owner<Depth<F>, RangeMachine, Unbounded>,
    logs: Vec<Log<F>>,
    handles: Vec<Handle>,
    woken: Receiver<usize>,
    out: Output<usize>,
    wire: VecDeque<Message>,
    slots: Vec<&'static hyper_measure::wake::Slot>,
    /// Waits for every write out before the next turn (`HYPER_DURABLE_ROUNDS`).
    rounds: bool,
    /// The leader answered in a turn the wait took.
    answered: bool,
}

/// mantle's group test's settings, as the core takes them.
fn settings(id: u64) -> Settings {
    let s = crate::mantle::SETTINGS;
    Settings {
        core: Config {
            election_tick: s.election_tick,
            heartbeat_tick: s.heartbeat_tick,
            max_size_per_msg: s.max_size_per_msg,
            max_inflight_msgs: s.max_inflight_msgs,
            max_uncommitted_size: s.max_uncommitted_size,
            max_committed_size_per_ready: s.max_committed_size_per_ready,
            check_quorum: true,
            pre_vote: true,
            seed: id,
            ..Config::new(id)
        },
        quiet: std::time::Duration::from_millis(10),
    }
}

impl<F: BlockFile + 'static> Group<F> {
    pub fn open(members: u64, mut device: impl FnMut(u64) -> F) -> Self {
        let (tell, woken) = sync_channel(4096);
        let made: Vec<(Waker, &'static hyper_measure::wake::Slot)> = (0..members)
            .map(|slot| hyper_measure::wake::waker(slot as usize, tell.clone()))
            .collect();
        let slots = made.iter().map(|(_, s)| *s).collect();
        let wakers: Vec<Waker> = made.into_iter().map(|(w, _)| w).collect();
        let mut owner = Owner::new(wakers);
        let mut logs = Vec::new();
        let mut handles = Vec::new();
        for id in 1..=members {
            let log = Log::create(device(id), log_config(), 0x6c6f67 + u128::from(id)).unwrap();
            let store = GroupStore::claim(&log, GROUP).unwrap();
            let depth = std::env::var("HYPER_DURABLE_DEPTH")
                .ok()
                .and_then(|d| d.parse().ok())
                .unwrap_or_else(|| store.depth());
            let store = Depth(store, depth);
            let machine = RangeMachine {
                engine: first_range(),
                configuration: ConfState {
                    voters: (1..=members).collect(),
                    ..ConfState::default()
                },
                applied: Point::default(),
                persisted: Point::default(),
            };
            let replica: Member<F> =
                Replica::open(&settings(id), store, machine, Unbounded).unwrap();
            handles.push(owner.insert(replica).map_err(|_| ()).unwrap());
            logs.push(log);
        }
        let mut group = Self {
            owner,
            logs,
            handles,
            woken,
            out: Output::default(),
            wire: VecDeque::new(),
            slots,
            rounds: std::env::var_os("HYPER_DURABLE_ROUNDS").is_some(),
            answered: false,
        };
        let first = group.handles[0];
        group.owner.get_mut(first).unwrap().campaign().unwrap();
        group.owner.schedule(first);
        for _ in 0..100_000 {
            group.turn(&mut |_| {});
            let leads = group.owner.get(first).unwrap().is_leader();
            if leads && group.wire.is_empty() && !group.owner.has_work() {
                return group;
            }
            group.wait();
        }
        panic!("the group never elected member 1");
    }

    /// One turn of the owner and the messages it gave out delivered.
    fn turn(&mut self, answered: &mut dyn FnMut(bool)) {
        let leader = self.handles[0];
        let wire = &mut self.wire;
        self.owner
            .turn(now(), &mut self.out, |h, driven, out| {
                driven.unwrap();
                wire.extend(out.messages.drain(..));
                if h == leader {
                    answered(!out.answers.is_empty());
                }
            });
        while let Some(m) = self.wire.pop_front() {
            let Some(&to) = self.handles.get(usize::try_from(m.to).unwrap() - 1) else {
                continue;
            };
            if let Some(r) = self.owner.get_mut(to) {
                match r.step(m) {
                    Ok(()) | Err(ReplicaError::Refused(_) | ReplicaError::Stalled) => {}
                    Err(e) => panic!("step: {e}"),
                }
            }
            self.owner.schedule(to);
        }
    }

    /// With nothing to do, waits for the log to answer a write.
    fn wait(&mut self) {
        if self.rounds {
            // As mantle's node waits: for every write out before the next turn.
            while self
                .handles
                .iter()
                .any(|&h| self.owner.get(h).is_some_and(|r| r.in_flight() > 0))
            {
                if let Ok(slot) = self.woken.recv() {
                    self.owner.woken(slot);
                }
                let mut done = false;
                self.turn(&mut |answered| done |= answered);
                self.answered |= done;
            }
            return;
        }
        if !self.owner.has_work() {
            if let Ok(slot) = self.woken.recv() {
                self.owner.woken(slot);
            }
        }
        while let Ok(slot) = self.woken.try_recv() {
            self.owner.woken(slot);
        }
    }

    pub fn commit(&mut self, entry: &Entry) {
        let leader = self.handles[0];
        let data = entry.encode().unwrap();
        self.owner
            .get_mut(leader)
            .unwrap()
            .propose(Vec::new(), data)
            .unwrap();
        self.owner.schedule(leader);
        for _ in 0..1_000_000 {
            let mut done = std::mem::take(&mut self.answered);
            self.turn(&mut |answered| done |= answered);
            if done {
                return;
            }
            self.wait();
            if std::mem::take(&mut self.answered) {
                return;
            }
        }
        panic!("the group never applied an entry");
    }

    /// The writes every member made, by kind, and the wakes the logs' answers made.
    pub fn diagnosis(&self) -> (hyper_durable::Writes, u64) {
        let mut all = hyper_durable::Writes::default();
        for &h in &self.handles {
            if let Some(r) = self.owner.get(h) {
                let w = r.writes();
                all.readies += w.readies;
                all.empty += w.empty;
                all.fenced += w.fenced;
                all.quiet += w.quiet;
                all.starts += w.starts;
            }
        }
        (all, self.slots.iter().map(|s| s.wakes()).sum())
    }

    pub fn flushes(&self) -> u64 {
        self.logs.iter().map(|l| l.flushed().0).sum()
    }
}

/// The owner's clock in nanoseconds since the first reading, as the shell takes it.
fn now() -> u64 {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    let epoch = *EPOCH.get_or_init(Instant::now);
    u64::try_from(epoch.elapsed().as_nanos()).unwrap_or(u64::MAX)
}
