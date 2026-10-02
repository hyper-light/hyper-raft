//! A member of this core driven ahead of its persistence (core step R-4,
//! `docs/durable.md` §2.1): it takes a `Ready` and issues its write while
//! earlier writes are out, a write becomes durable on its disk when the
//! schedule says, and its owner hears of what became durable later still.
//! Each of those is a step of the schedule ([`Step`]), so a schedule
//! interleaves them with everything else, and a crash between any two loses
//! exactly what was not durable.
//!
//! The member is held to the invariants of `docs/durable.md` §3 the core
//! keeps, against its disk as it is at each step: what a write holds is
//! what the member held when it took the `Ready` (I7); a message waiting
//! for a write leaves only once the disk holds the term, vote and entries
//! it speaks for (I1, I2); a leader's own messages leave at once only while
//! its term and vote are durable (I1); what is given to apply is committed
//! and durable (I4). What a leader counts and commits is held to its
//! voters' disks by the cluster (`Cluster::check_durable`, I3).
use std::collections::VecDeque;

use hyper_raft::proto::{Entry, HardState, Message, MessageType, Snapshot};

use super::{
    App, Disk, Led, New, Output, Replica, Settings, Step, Store, View, apply_to, canonical,
    cluster::holds, committed_of,
};

/// What the schedules reached, so that a test proves it ran what it claims.
#[derive(Clone, Copy, Debug, Default)]
pub struct Coverage {
    /// `Ready`s taken.
    pub taken: u64,
    /// Taken while an earlier write was out.
    pub behind: u64,
    /// Refused at the bound of writes out.
    pub refused: u64,
    /// Writes made durable.
    pub durable: u64,
    /// Notices the core was given.
    pub notices: u64,
    /// Notices of more than one write.
    pub several: u64,
    /// Notices whose messages waited for the next `Ready`.
    pub held_back: u64,
    /// Writes out when their member stopped: lost.
    pub lost: u64,
}
impl Coverage {
    pub fn add(&mut self, other: Self) {
        self.taken += other.taken;
        self.behind += other.behind;
        self.refused += other.refused;
        self.durable += other.durable;
        self.notices += other.notices;
        self.several += other.several;
        self.held_back += other.held_back;
        self.lost += other.lost;
    }
}

/// One write a member issued.
struct Write {
    number: u64,
    snapshot: Option<Snapshot>,
    entries: Vec<Entry>,
    proposals: Vec<Entry>,
    hard_state: Option<HardState>,
    /// What waits for this write to be durable.
    messages: Vec<Message>,
    /// What the member held when it took the `Ready`, from the last entry
    /// both committed and durable on: once this write is durable its disk
    /// holds the same (I7).
    from: u64,
    terms: Vec<u64>,
    vote: (u64, u64),
}

pub struct Lagged {
    node: New,
    depth: usize,
    /// Writes issued and not yet durable, oldest first.
    out: VecDeque<Write>,
    /// Writes durable whose owner has not heard of them yet.
    durable: VecDeque<Write>,
    output: Output,
    coverage: Coverage,
}

/// A message is held to what the disk holds when it may leave (I1, I2).
fn check_message(disk: &Disk, message: &Message, member: u64) {
    let kind = message.msg_type;
    // A pre-vote is asked for a term the member does not take.
    if matches!(
        kind,
        MessageType::MsgRequestPreVote | MessageType::MsgRequestPreVoteResponse
    ) {
        return;
    }
    let hard = disk.hard_state;
    assert!(
        hard.term >= message.term,
        "member {member}: {kind:?} of term {} left with term {} durable",
        message.term,
        hard.term
    );
    // A message of an older term than the disk's was superseded by what the
    // member did since: it is as late as the network may make any message
    // (thesis §3.3), and the disk holds a later promise.
    let current = hard.term == message.term;
    match kind {
        MessageType::MsgRequestVote if current => assert_eq!(
            (hard.term, hard.vote),
            (message.term, member),
            "member {member}: a vote asked for before its own was durable"
        ),
        MessageType::MsgRequestVoteResponse if current && !message.reject => assert_eq!(
            (hard.term, hard.vote),
            (message.term, message.to),
            "member {member}: a vote given before it was durable"
        ),
        MessageType::MsgAppendResponse if current && !message.reject => {
            assert!(
                message.index <= disk.last_index(),
                "member {member}: acknowledged {} with {} durable",
                message.index,
                disk.last_index()
            );
        }
        _ if current && kind == hyper_raft::fast::FAST_VOTE => {
            for entry in &message.entries {
                // Once the log reaches the index, what it holds there was the
                // leader's, and supersedes what the member held beside it.
                assert!(
                    holds(disk, entry) || disk.last_index() >= entry.index,
                    "member {member}: said it holds {} before its disk did",
                    entry.index
                );
            }
        }
        _ => {}
    }
}

impl Lagged {
    fn take(&mut self) -> bool {
        let raw = &mut self.node.raw;
        if !raw.has_ready() {
            return false;
        }
        if raw.in_flight() >= self.depth {
            self.coverage.refused += 1;
            return false;
        }
        let behind = raw.in_flight() > 0;
        let in_place = self.node.in_place;
        let mut ready = if in_place {
            raw.ready_in_place()
        } else {
            raw.ready()
        }
        .expect("a ready");
        let log = raw.raft.log();
        let from = log.committed().min(log.persisted());
        let last = log.last_index().unwrap();
        let terms = (from..=last)
            .map(|index| log.term(index).unwrap())
            .collect();
        let vote = (raw.raft.term(), raw.raft.vote());
        let (snapshot, entries) = if in_place {
            let persist = raw.to_persist();
            (persist.snapshot.cloned(), persist.entries.to_vec())
        } else {
            (ready.snapshot().cloned(), ready.entries().to_vec())
        };
        let id = raw.raft.id();
        let disk = &raw.store().0;
        for message in ready.messages() {
            // I1: a leader's own messages leave at once only with its term
            // and vote durable.
            assert_eq!(
                (disk.hard_state.term, disk.hard_state.vote),
                vote,
                "member {id}: {:?} sent at once before its term was durable",
                message.msg_type
            );
        }
        for message in ready.messages().iter().chain(ready.persisted_messages()) {
            assert_eq!(
                message.entries.capacity(),
                message.entries.len(),
                "member {id}: a page with spare room"
            );
        }
        let write = Write {
            number: ready.number(),
            snapshot,
            entries,
            proposals: ready.proposals().to_vec(),
            hard_state: ready.hard_state().copied(),
            messages: ready.take_persisted_messages(),
            from,
            terms,
            vote,
        };
        self.output.messages.extend(ready.take_messages());
        self.output.reads.extend(
            ready
                .take_read_states()
                .into_iter()
                .map(|read| (read.index, read.request_ctx)),
        );
        self.output
            .displaced
            .extend(ready.displaced().iter().map(super::said));
        let committed = committed_of(
            &self.node.raw,
            in_place,
            ready.take_committed_entries(),
            ready.committed_range(),
        );
        self.apply(committed);
        self.node.raw.advance_issued(ready).expect("issued");
        self.node
            .raw
            .advance_apply_to(self.node.app.index)
            .expect("applied");
        self.out.push_back(write);
        self.coverage.taken += 1;
        self.coverage.behind += u64::from(behind);
        true
    }

    /// I4: what is given to apply is committed, and durable here. I5 is the
    /// shell's (`docs/durable.md` §4.1, the commit fence): a change of
    /// configuration is applied only on a commit its disk records, or a
    /// member that stops reopens under the configuration before it. The
    /// shell is not built yet, so its rule is kept here at its strictest, as
    /// the synchronous members' harness keeps it: the disk records the
    /// commit through what is applied before it is applied.
    fn apply(&mut self, committed: Vec<Entry>) {
        let raw = &mut self.node.raw;
        for entry in &committed {
            assert!(
                entry.index <= raw.raft.log().committed() && holds(&raw.store().0, entry),
                "member {}: {} given to apply before it was committed and durable",
                raw.raft.id(),
                entry.index
            );
        }
        if let Some(last) = committed.last() {
            let disk = &mut raw.store_mut().0;
            disk.hard_state.commit = disk.hard_state.commit.max(last.index);
        }
        apply_to(
            &mut self.node.raw,
            &mut self.node.app,
            committed,
            &mut self.output,
        );
    }

    fn make_durable(&mut self) -> bool {
        let Some(write) = self.out.pop_front() else {
            return false;
        };
        let id = self.node.raw.raft.id();
        let disk = &mut self.node.raw.store_mut().0;
        if let Some(snapshot) = &write.snapshot {
            disk.install(snapshot);
        }
        disk.append(&write.entries);
        disk.proposals.extend(write.proposals.iter().cloned());
        disk.trim_proposals();
        if let Some(hard) = write.hard_state {
            // The commit a store records never goes back: a compaction since
            // the `Ready` was taken recorded a later one (`Disk::compact`).
            disk.hard_state = HardState {
                commit: hard.commit.max(disk.hard_state.commit),
                ..hard
            };
        }
        // I7: the disk holds what the member held when it took the `Ready`.
        assert_eq!(
            (disk.hard_state.term, disk.hard_state.vote),
            write.vote,
            "member {id}: write {} left another term or vote durable",
            write.number
        );
        let last = write.from + write.terms.len() as u64 - 1;
        assert_eq!(
            disk.last_index().max(disk.snapshot_index()),
            last,
            "member {id}: write {} left another log durable",
            write.number
        );
        for (at, term) in write.terms.iter().enumerate() {
            let index = write.from + at as u64;
            if index >= disk.snapshot_index() {
                assert_eq!(
                    disk.term(index),
                    Some(*term),
                    "member {id}: write {} left another entry durable at {index}",
                    write.number
                );
            }
        }
        for message in &write.messages {
            check_message(disk, message, id);
        }
        self.durable.push_back(write);
        self.coverage.durable += 1;
        true
    }

    fn notify(&mut self) -> bool {
        let Some(number) = self.durable.back().map(|write| write.number) else {
            return false;
        };
        if self.durable.len() > 1 {
            self.coverage.several += 1;
        }
        for write in self.durable.drain(..) {
            self.output.messages.extend(write.messages);
            if let Some(snapshot) = write.snapshot {
                let metadata = snapshot.metadata.clone().unwrap_or_default();
                self.output.snapshots.push((metadata.index, metadata.term));
                self.node.app = App::decode(&snapshot.data);
            }
        }
        let raw = &mut self.node.raw;
        let mut light = if self.node.in_place {
            raw.on_persist_keeping(number, |store, kept| {
                // What the member gives up is what its disk holds.
                if let Some(snapshot) = &kept.snapshot {
                    assert!(
                        store.0.snapshot_index()
                            >= snapshot
                                .metadata
                                .as_ref()
                                .map_or(0, |metadata| metadata.index)
                    );
                }
                for entry in &kept.entries {
                    assert!(holds(&store.0, entry), "kept {} not held", entry.index);
                }
            })
        } else {
            raw.on_persist(number)
        }
        .expect("persisted");
        let id = raw.raft.id();
        let leads = raw.raft.state() == hyper_raft::StateRole::Leader;
        let messages = light.take_messages();
        if messages.is_empty() && !raw.raft.messages().is_empty() {
            self.coverage.held_back += 1;
        }
        for message in &messages {
            if leads {
                // A pre-vote's answer names the term asked about, which no
                // member takes by it.
                assert!(
                    raw.store().0.hard_state.term >= message.term
                        || message.msg_type == MessageType::MsgRequestPreVoteResponse,
                    "member {id}: {:?} of term {} sent at once with term {} durable (term {} now)",
                    message.msg_type,
                    message.term,
                    raw.store().0.hard_state.term,
                    raw.raft.term()
                );
            } else {
                check_message(&raw.store().0, message, id);
            }
        }
        self.output.messages.extend(messages);
        let committed = committed_of(
            raw,
            self.node.in_place,
            light.take_committed_entries(),
            light.committed_range(),
        );
        self.apply(committed);
        self.node
            .raw
            .advance_apply_to(self.node.app.index)
            .expect("applied");
        self.coverage.notices += 1;
        true
    }
}

impl Replica for Lagged {
    const LAGGED: bool = true;
    fn open(id: u64, store: Store, settings: &Settings, seed: u64) -> Self {
        Self {
            node: New::open(id, store, settings, seed),
            depth: settings.depth,
            out: VecDeque::new(),
            durable: VecDeque::new(),
            output: Output::default(),
            coverage: Coverage::default(),
        }
    }
    fn id(&self) -> u64 {
        self.node.id()
    }
    fn store(&self) -> &Store {
        self.node.store()
    }
    fn store_mut(&mut self) -> &mut Store {
        self.node.store_mut()
    }
    fn tick(&mut self) -> bool {
        self.node.tick()
    }
    fn step(&mut self, message: Message) -> bool {
        self.node.step(message)
    }
    fn propose(&mut self, data: Vec<u8>) -> bool {
        self.node.propose(data)
    }
    fn propose_fast(&mut self, data: Vec<u8>) -> Option<u64> {
        self.node.propose_fast(data)
    }
    fn propose_change(&mut self, change: &hyper_raft::proto::ConfChangeV2) -> bool {
        self.node.propose_change(change)
    }
    fn campaign(&mut self) -> bool {
        self.node.campaign()
    }
    fn ping(&mut self) {
        self.node.ping();
    }
    fn transfer(&mut self, to: u64) {
        self.node.transfer(to);
    }
    fn read(&mut self, context: Vec<u8>) {
        self.node.read(context);
    }
    fn unreachable(&mut self, member: u64) {
        self.node.unreachable(member);
    }
    fn snapshot_status(&mut self, member: u64, arrived: bool) {
        self.node.snapshot_status(member, arrived);
    }
    fn set_priority(&mut self, priority: i64) {
        self.node.set_priority(priority);
    }
    fn set_window(&mut self, member: u64, bytes: u64) {
        self.node.set_window(member, bytes);
    }
    fn set_timeout(&mut self, ticks: usize) {
        self.node.set_timeout(ticks);
    }
    fn drain(&mut self) -> Output {
        let mut output = std::mem::take(&mut self.output);
        output.messages = canonical(output.messages);
        output
    }
    fn persist(&mut self, step: Step) -> bool {
        match step {
            Step::Take => self.take(),
            Step::Durable => self.make_durable(),
            Step::Notify => self.notify(),
            Step::Flush => {
                let mut any = false;
                // Each round takes, writes or hears of something, and what
                // there is to take is bounded by what the member was given.
                for _ in 0..10_000 {
                    let mut moved = false;
                    while self.take() {
                        moved = true;
                    }
                    while self.make_durable() {
                        moved = true;
                    }
                    moved |= self.notify();
                    if !moved {
                        return any;
                    }
                    any = true;
                }
                panic!("member {}: its writes never settled", self.id());
            }
        }
    }
    fn busy(&self) -> bool {
        self.node.raw.has_ready() || !self.out.is_empty() || !self.durable.is_empty()
    }
    fn coverage(&self) -> Coverage {
        Coverage {
            lost: self.out.len() as u64,
            ..self.coverage
        }
    }
    fn led(&self) -> Option<Led> {
        let raft = &self.node.raw.raft;
        if raft.state() != hyper_raft::StateRole::Leader {
            return None;
        }
        let log = raft.log();
        let entry_at = |index: u64| {
            if index == 0 || index < log.first_index().ok()? {
                return None;
            }
            log.slice(index, index + 1, u64::MAX)
                .ok()?
                .into_iter()
                .next()
        };
        Some(Led {
            term: raft.term(),
            committed: entry_at(log.committed()),
            own: raft
                .tracker()
                .get(raft.id())
                .and_then(|own| entry_at(own.matched)),
        })
    }
    fn view(&self) -> View {
        self.node.view()
    }
    fn app(&self) -> App {
        self.node.app()
    }
}
