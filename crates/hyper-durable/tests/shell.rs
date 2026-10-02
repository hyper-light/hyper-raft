//! The shell's rules one at a time, and every bound of `docs/durable.md` §6 at its edge: the
//! writes out, the entries behind the fence, the reads, the snapshot reports kept while
//! stalled, the budget, the arena; what opening repairs (§4.3); the unwind boundary; and the
//! threads an owner's replicas cost, which do not grow with them.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::disallowed_macros,
    clippy::cast_possible_truncation,
    clippy::cognitive_complexity,
    clippy::unreachable,
    clippy::panic_in_result_fn
)]

mod support;

use std::sync::OnceLock;
use std::task::Waker;
use std::time::Instant;

use hyper_durable::{
    Budget, Bytes, Cause, EntryRef, Fatal, Fault, LogStore, OpenError, Output, Owner, Point,
    RamStore, Replica, ReplicaError, Settings, StateMachine, Unbounded, Write,
};
use hyper_raft::proto::{
    ConfChangeSingle, ConfChangeTransition, ConfChangeType, ConfChangeV2, ConfState, Entry,
    HardState, Message, MessageType,
};
use support::cluster::settings;
use support::{Kv, SimStore};

fn voters(ids: &[u64]) -> ConfState {
    ConfState {
        voters: ids.to_vec(),
        ..ConfState::default()
    }
}

fn waker() -> &'static Waker {
    Waker::noop()
}

type Sim<B = Unbounded> = Replica<SimStore, Kv, B>;

/// A sole voter on a store of `depth`, elected: it commits alone.
fn sole<B: Budget>(depth: usize, budget: B, tune: impl FnOnce(&mut Settings)) -> Sim<B> {
    let mut s = settings(1, 7);
    tune(&mut s);
    let mut r = Replica::open(
        &s,
        SimStore::new(depth),
        Kv::new(voters(&[1]), false),
        budget,
    )
    .unwrap();
    r.campaign().unwrap();
    pump(&mut r);
    assert!(r.is_leader());
    r
}

/// Elects member 1 of three by hand: it campaigns, and the test answers its pre-votes and then
/// its votes as if from the other two.
fn elect_by_hand(r: &mut Sim) {
    r.campaign().unwrap();
    for asked in [MessageType::MsgRequestPreVote, MessageType::MsgRequestVote] {
        let answer = match asked {
            MessageType::MsgRequestPreVote => MessageType::MsgRequestPreVoteResponse,
            _ => MessageType::MsgRequestVoteResponse,
        };
        let out = pump(r);
        for m in out.messages {
            if m.msg_type == asked {
                r.step(Message {
                    msg_type: answer,
                    from: m.to,
                    to: 1,
                    term: m.term,
                    ..Message::default()
                })
                .unwrap();
            }
        }
    }
    pump(r);
    assert!(r.is_leader());
}

/// Drives `r` and makes every write durable until nothing is out and nothing more to do.
fn pump<B: Budget>(r: &mut Sim<B>) -> Output<(u64, Vec<u8>)> {
    let mut all = Output::default();
    let mut out = Output::default();
    for _ in 0..10_000 {
        out.clear();
        let driven = r.drive(now(), waker(), &mut out).unwrap();
        all.messages.append(&mut out.messages);
        all.answers.append(&mut out.answers);
        all.reads.append(&mut out.reads);
        let mut made = false;
        while r.log_mut().make_durable() {
            made = true;
        }
        if !made && !driven.more && driven.out == 0 && r.log_mut().unanswered() == 0 {
            return all;
        }
    }
    panic!("the replica never rested");
}

fn change(kind: ConfChangeType, node_id: u64) -> ConfChangeV2 {
    ConfChangeV2 {
        transition: ConfChangeTransition::Auto,
        changes: vec![ConfChangeSingle {
            change_type: kind,
            node_id,
        }],
        context: Vec::new(),
    }
}

/// The core takes `Ready`s ahead of their persistence up to the store's depth, and the shell
/// keeps at most one write of its own beside them; past them it takes none, and nothing
/// refuses: the proposals wait in the core.
#[test]
fn the_writes_out_never_pass_the_stores_depth() {
    for depth in [1, 2, 3] {
        let mut r = sole(depth, Unbounded, |_| {});
        let mut out = Output::default();
        let mut deepest = 0;
        for i in 0..64u64 {
            r.propose(Vec::new(), i.to_le_bytes().to_vec()).unwrap();
            out.clear();
            r.drive(now(), waker(), &mut out).unwrap();
            deepest = deepest.max(r.in_flight());
            assert!(r.core().in_flight() <= depth, "depth {depth}");
            assert!(r.in_flight() <= depth + 1, "depth {depth}");
        }
        assert_eq!(deepest, depth, "depth {depth}: the pipeline never filled");
        pump(&mut r);
        assert_eq!(r.machine().now.entries.len(), 65, "depth {depth}");
    }
}

/// A sole voter states the commit of its own entries in the write that holds them (focal F17's
/// `sole_commit`): once that write is durable the commit is logged, with no write of its own.
#[test]
fn a_sole_voter_logs_its_commit_in_the_write_of_its_entries() {
    let mut r = sole(3, Unbounded, |_| {});
    let submits = r.log_mut().events.submits;
    r.propose(Vec::new(), b"x".to_vec()).unwrap();
    pump(&mut r);
    let last = r.applied().index;
    assert_eq!(r.log_mut().disk.hard.commit, last);
    assert_eq!(
        r.log_mut().events.submits,
        submits + 1,
        "a write of its own for the commit"
    );
    // Many entries, one write each, and never a write of the commit alone (§4.1).
    let before = r.writes();
    for i in 0..32u64 {
        r.propose(Vec::new(), i.to_le_bytes().to_vec()).unwrap();
        pump(&mut r);
    }
    let after = r.writes();
    assert_eq!(after.readies - before.readies, 32);
    assert_eq!((after.fenced, after.quiet), (before.fenced, before.quiet));
}

/// A change waits behind the fence until a write states its commit; entries committed after it
/// wait with it, at most a page from each write out and the page of the `Ready` that gave it
/// (`docs/durable.md` §6); the replica takes no `Ready` meanwhile.
#[test]
fn a_change_waits_behind_the_fence_and_what_waits_is_bounded() {
    // A member that does not decide alone: three voters of whom two are elsewhere, driven by
    // hand with acknowledgements sent as if from them.
    let depth = 3;
    let page = 256u64;
    let mut s = settings(1, 9);
    s.core.max_committed_size_per_ready = page;
    let mut r: Sim = Replica::open(
        &s,
        SimStore::new(depth),
        Kv::new(voters(&[1, 2, 3]), false),
        Unbounded,
    )
    .unwrap();
    elect_by_hand(&mut r);
    let ack = |r: &mut Sim| {
        let index = r.core().raft.log().last_index().unwrap();
        let term = r.term();
        r.step(Message {
            msg_type: MessageType::MsgAppendResponse,
            from: 2,
            to: 1,
            term,
            index,
            ..Message::default()
        })
        .unwrap();
    };
    ack(&mut r);
    pump(&mut r);
    // The change, then entries behind it, all committed by member 2's acknowledgement while
    // the leader's own writes are out.
    r.change(Vec::new(), &change(ConfChangeType::AddLearnerNode, 4))
        .unwrap();
    for i in 0..40u64 {
        r.propose(Vec::new(), vec![i as u8; 32]).unwrap();
        let mut out = Output::default();
        r.drive(now(), waker(), &mut out).unwrap();
    }
    let mut held = None;
    for _ in 0..200 {
        ack(&mut r);
        let mut out = Output::default();
        r.drive(now(), waker(), &mut out).unwrap();
        if let Some(range) = r.behind_fence() {
            held = Some(range);
            let entry = 32 + hyper_raft::wire::ENTRY_FIXED_BYTES as u64;
            let bytes = (range.1 - range.0) * entry;
            assert!(
                bytes <= (depth as u64 + 1) * (page + entry),
                "{range:?}: {bytes} bytes behind the fence"
            );
            assert!(
                r.configuration().learners.is_empty(),
                "applied behind the fence"
            );
            assert!(!r.core().has_ready() || r.in_flight() > 0 || r.log_mut().pending() > 0);
        }
        r.log_mut().make_durable();
    }
    assert!(held.is_some(), "nothing waited behind the fence");
    pump(&mut r);
    assert_eq!(r.configuration().learners, vec![4]);
    assert!(r.durable_commit() >= r.applied().index);
}

/// A write refused for room stalls the replica: inputs are refused `Stalled`, its campaigns held
/// whatever its detectors say, snapshot reports kept (one a member, members only), and the
/// refused writes are made again, in order, once the log has room; nothing was lost.
#[test]
fn a_write_refused_for_room_stalls_the_replica_until_it_is_made_again() {
    let mut r = sole(3, Unbounded, |s| s.core.limits.pending_reads = 8);
    for i in 0..3u64 {
        r.propose(Vec::new(), i.to_le_bytes().to_vec()).unwrap();
        let mut out = Output::default();
        r.drive(now(), waker(), &mut out).unwrap();
    }
    r.log_mut().refuse = Some(Fault::Room("the group's retained bound"));
    r.log_mut().make_durable();
    r.log_mut().full = true;
    let mut out = Output::default();
    let driven = r.drive(now(), waker(), &mut out).unwrap();
    assert!(r.is_stalled());
    assert_eq!(
        driven.stalled,
        Some(Fault::Room("the group's retained bound"))
    );
    assert!(out.messages.is_empty() && out.answers.is_empty());
    assert_eq!(
        r.propose(Vec::new(), b"no".to_vec()),
        Err(ReplicaError::Stalled)
    );
    assert_eq!(r.step(Message::default()), Err(ReplicaError::Stalled));
    assert_eq!(r.read(b"r".to_vec()), Err(ReplicaError::Stalled));
    assert_eq!(r.campaign(), Err(ReplicaError::Stalled));
    let term = r.term();
    r.suspect(2).unwrap();
    for _ in 0..100 {
        let mut out = Output::default();
        let driven = r.drive(now(), waker(), &mut out).unwrap();
        assert_eq!(driven.wake, None, "a stalled member is due for nothing");
    }
    assert_eq!(r.term(), term, "a stalled member campaigned");
    for member in [1, 9, 1] {
        r.report_snapshot(member, true).unwrap();
    }
    r.log_mut().full = false;
    // Nothing is made again until room may have been freed: the owner says so.
    let mut out = Output::default();
    r.drive(now(), waker(), &mut out).unwrap();
    assert!(r.is_stalled());
    r.resume();
    pump(&mut r);
    assert!(!r.is_stalled());
    assert_eq!(r.machine().now.entries.len(), 4);
    r.propose(Vec::new(), b"after".to_vec()).unwrap();
    pump(&mut r);
    assert_eq!(r.machine().now.entries.len(), 5);
}

/// Reads past the core's bound are refused, counting those the replica holds for its apply.
#[test]
fn reads_past_the_bound_are_refused() {
    let mut r = sole(1, Unbounded, |s| s.core.limits.pending_reads = 4);
    for i in 0..4u8 {
        r.read(vec![i]).unwrap();
    }
    assert!(matches!(
        r.read(vec![9]),
        Err(ReplicaError::Refused(hyper_raft::Error::Capacity(_)))
    ));
    let out = pump(&mut r);
    assert_eq!(out.reads.len(), 4);
    r.read(vec![10]).unwrap();
}

/// The budget refuses an input it cannot hold, and nothing changes; what it holds follows the
/// replica's resident bytes.
#[test]
fn the_budget_refuses_what_it_cannot_hold_and_follows_what_is_held() {
    let mut r = sole(2, Bytes::new(1 << 20), |_| {});
    let held = r.charged();
    assert!(held > 0);
    let last = r.core().raft.log().last_index().unwrap();
    assert!(matches!(
        r.propose(Vec::new(), vec![0; 2 << 20]),
        Err(ReplicaError::Budget(_))
    ));
    assert_eq!(r.core().raft.log().last_index().unwrap(), last);
    r.propose(Vec::new(), vec![0; 1024]).unwrap();
    pump(&mut r);
    assert_eq!(r.machine().now.entries.len(), 2);
}

/// A state machine that unwinds on an entry.
struct Boom(Kv);
impl StateMachine for Boom {
    type Answer = (u64, Vec<u8>);
    fn apply(
        &mut self,
        entry: &EntryRef<'_>,
        answers: &mut Vec<Self::Answer>,
    ) -> Result<(), Fatal> {
        if entry.data == b"boom" {
            panic!("the application unwound");
        }
        self.0.apply(entry, answers)
    }
    fn apply_change(&mut self, at: Point, c: &ConfState) -> Result<(), Fatal> {
        self.0.apply_change(at, c)
    }
    fn durable(&self) -> Point {
        self.0.durable()
    }
    fn configuration(&self) -> &ConfState {
        self.0.configuration()
    }
    fn acts_at_start(&self, entry: &EntryRef<'_>) -> bool {
        self.0.acts_at_start(entry)
    }
    fn image(&mut self, into: &mut Vec<u8>) -> Result<Point, Fatal> {
        self.0.image(into)
    }
    fn install(&mut self, image: &[u8], at: Point, c: &ConfState) -> Result<(), Fatal> {
        self.0.install(image, at, c)
    }
    fn persist(&mut self) -> Result<(), Fatal> {
        self.0.persist()
    }
}

/// An unwind of the state machine fences the replica and is reported, never propagated.
#[test]
fn an_unwind_inside_a_call_fences_the_replica() {
    let mut r = Replica::open(
        &settings(1, 3),
        RamStore::new(),
        Boom(Kv::new(voters(&[1]), false)),
        Unbounded,
    )
    .unwrap();
    r.campaign().unwrap();
    let mut out = Output::default();
    for _ in 0..8 {
        r.drive(now(), waker(), &mut out).unwrap();
    }
    r.propose(Vec::new(), b"boom".to_vec()).unwrap();
    let mut fenced = None;
    for _ in 0..8 {
        if let Err(e) = r.drive(now(), waker(), &mut out) {
            fenced = Some(e);
            break;
        }
    }
    assert_eq!(fenced, Some(ReplicaError::Fenced(Cause::Unwound)));
    assert_eq!(r.suspect(2), Err(ReplicaError::Fenced(Cause::Unwound)));
}

fn entries(first: u64, terms: &[u64]) -> Vec<Entry> {
    terms
        .iter()
        .enumerate()
        .map(|(i, &term)| Entry {
            index: first + i as u64,
            term,
            data: vec![1],
            ..Entry::default()
        })
        .collect()
}

fn store_with(entries: &[Entry], commit: u64) -> RamStore {
    let mut store = RamStore::new();
    store
        .write_now(&Write {
            entries: Some(hyper_durable::Entries { first: 1, entries }),
            hard_state: Some(HardState {
                term: 2,
                vote: 1,
                commit,
            }),
            ..Write::default()
        })
        .unwrap();
    store
}

fn machine_at(point: Point) -> Kv {
    let mut kv = Kv::new(voters(&[1, 2, 3]), false);
    kv.now.applied = point;
    kv.durable.applied = point;
    kv
}

/// §4.3, case 1: a snapshot the state machine installed and the log never recorded: the log
/// starts there, holds nothing past it, and records it committed.
#[test]
fn an_install_the_log_never_recorded_is_finished_at_open() {
    let store = store_with(&entries(1, &[1, 1, 2, 2, 2]), 3);
    let r: Replica<RamStore, Kv> = Replica::open(
        &settings(1, 1),
        store,
        machine_at(Point { index: 8, term: 2 }),
        Unbounded,
    )
    .unwrap();
    let view = r.core().store().log().view().unwrap();
    assert_eq!((view.start, view.last), (Point { index: 8, term: 2 }, 8));
    assert_eq!(view.hard_state.commit, 8);
    assert_eq!(view.hard_state.term, 2, "the term and vote are kept");
}

/// §4.3, case 2: a state machine past the log's commit, within its entries: the commit rises to
/// it, and the entries stay.
#[test]
fn a_state_machine_past_the_logs_commit_raises_it() {
    let store = store_with(&entries(1, &[1, 1, 2, 2, 2]), 2);
    let r: Replica<RamStore, Kv> = Replica::open(
        &settings(1, 1),
        store,
        machine_at(Point { index: 4, term: 2 }),
        Unbounded,
    )
    .unwrap();
    let view = r.core().store().log().view().unwrap();
    assert_eq!(
        (view.start.index, view.last, view.hard_state.commit),
        (0, 5, 4)
    );
    assert_eq!(r.applied().index, 4);
}

/// §4.3, case 3: a state machine past the log's last entry (a lost last frame, or a leader that
/// applied before its own write was durable): the log starts at the state machine's point, whose
/// term the state machine reports.
#[test]
fn a_state_machine_past_the_logs_last_entry_moves_its_start() {
    let store = store_with(&entries(1, &[1, 1, 2]), 2);
    let r: Replica<RamStore, Kv> = Replica::open(
        &settings(1, 1),
        store,
        machine_at(Point { index: 5, term: 2 }),
        Unbounded,
    )
    .unwrap();
    let view = r.core().store().log().view().unwrap();
    assert_eq!(
        (view.start, view.last, view.hard_state.commit),
        (Point { index: 5, term: 2 }, 5, 5)
    );
}

/// I8: a log that starts past what the state machine holds durably does not open.
#[test]
fn a_log_that_starts_past_the_state_machine_does_not_open() {
    let mut store = store_with(&entries(1, &[1, 1, 2, 2, 2]), 5);
    store
        .write_now(&Write {
            start: Some(Point { index: 4, term: 2 }),
            ..Write::default()
        })
        .unwrap();
    let opened: Result<Replica<RamStore, Kv>, _> = Replica::open(
        &settings(1, 1),
        store,
        machine_at(Point { index: 2, term: 1 }),
        Unbounded,
    );
    assert!(matches!(
        opened,
        Err(OpenError::StartPastMachine {
            start: 4,
            machine: 2
        })
    ));
}

/// A marked member (its log may lack entries it acknowledged) takes no part in elections: it
/// refuses to campaign, drops a vote asked by a candidate behind its mark, and sends no request
/// for votes of its own, though it opened knowing no leader and its detectors suspect every
/// peer: its campaigns are held, and it is due for nothing.
#[test]
fn a_marked_member_takes_no_part_in_elections() {
    let mut store = SimStore::new(1);
    store
        .write_now(&Write {
            entries: Some(hyper_durable::Entries {
                first: 1,
                entries: &entries(1, &[1, 1]),
            }),
            hard_state: Some(HardState {
                term: 1,
                vote: 1,
                commit: 1,
            }),
            ..Write::default()
        })
        .unwrap();
    store.mark = Some(Point { index: 5, term: 1 });
    let mut r: Sim = Replica::open(
        &settings(2, 1),
        store,
        Kv::new(voters(&[1, 2, 3]), false),
        Unbounded,
    )
    .unwrap();
    assert_eq!(r.mark(), Some(Point { index: 5, term: 1 }));
    assert_eq!(r.campaign(), Err(ReplicaError::Marked));
    r.step(Message {
        msg_type: MessageType::MsgRequestVote,
        from: 3,
        to: 2,
        term: 2,
        log_term: 1,
        index: 3,
        ..Message::default()
    })
    .unwrap();
    r.set_timing(hyper_raft::Timing {
        span: std::time::Duration::from_millis(1),
        round: std::time::Duration::from_millis(1),
    })
    .unwrap();
    r.suspect(1).unwrap();
    r.suspect(3).unwrap();
    let start = now();
    let mut out = Output::default();
    for step in 0..100u64 {
        out.clear();
        let at = start + step * 1_000_000;
        let driven = r.drive(at, waker(), &mut out).unwrap();
        assert_eq!(driven.wake, None, "a marked member is due for nothing");
    }
    let out = pump(&mut r);
    assert!(
        out.messages.iter().all(|m| !matches!(
            m.msg_type,
            MessageType::MsgRequestVote
                | MessageType::MsgRequestVoteResponse
                | MessageType::MsgRequestPreVote
        )),
        "{:?}",
        out.messages
    );
}

/// The flushes that made a term or vote durable feed hyper-timing's fold.
#[test]
fn a_votes_flush_is_folded() {
    let r = sole(2, Unbounded, |_| {});
    assert!(r.flushes().flushes() >= 1);
    assert!(r.last_durable().is_some());
}

/// The owner drives each queued replica once a turn, for one `Ready`, and queues again one with
/// more: every replica moves each turn, whatever the others hold. The arena refuses past its
/// slots, and a handle to an emptied slot finds nothing.
#[test]
fn the_owner_gives_each_replica_one_ready_a_turn() {
    let wakers: Vec<Waker> = (0..3).map(|_| Waker::noop().clone()).collect();
    let mut owner: Owner<RamStore, Kv, Unbounded> = Owner::new(wakers);
    let mut handles = Vec::new();
    for _ in 0..3 {
        let mut r = Replica::open(
            &settings(1, 5),
            RamStore::new(),
            Kv::new(voters(&[1]), false),
            Unbounded,
        )
        .unwrap();
        r.campaign().unwrap();
        handles.push(owner.insert(r).map_err(|_| ()).unwrap());
    }
    let extra = Replica::open(
        &settings(1, 5),
        RamStore::new(),
        Kv::new(voters(&[1]), false),
        Unbounded,
    )
    .unwrap();
    assert!(owner.insert(extra).is_err());
    let mut out = Output::default();
    for _ in 0..50 {
        owner.turn(now(), &mut out, |_, d, _| {
            d.unwrap();
        });
        for &h in &handles {
            owner.schedule(h);
        }
    }
    // One replica is given far more to do: each turn still drives every one once.
    for i in 0..200u64 {
        owner
            .get_mut(handles[0])
            .unwrap()
            .propose(Vec::new(), i.to_le_bytes().to_vec())
            .unwrap();
    }
    owner
        .get_mut(handles[1])
        .unwrap()
        .propose(Vec::new(), b"one".to_vec())
        .unwrap();
    for &h in &handles {
        owner.schedule(h);
    }
    let mut driven = [0u32; 3];
    for _ in 0..4 {
        owner.turn(now(), &mut out, |h, d, _| {
            d.unwrap();
            driven[h.slot()] += 1;
        });
        // A RamStore's answer wakes the slot; the noop waker cannot, so the test does.
        for &h in &handles {
            owner.woken(h.slot());
        }
    }
    assert!(driven.iter().all(|&d| d == 4), "{driven:?}");
    let applied: Vec<u64> = handles
        .iter()
        .map(|&h| owner.get(h).unwrap().applied().index)
        .collect();
    assert!(
        applied[1] > 1,
        "the light replica waited behind the heavy one: {applied:?}"
    );
    let removed = owner.remove(handles[2]).unwrap();
    drop(removed);
    assert!(owner.get(handles[2]).is_none());
    let again = Replica::open(
        &settings(1, 5),
        RamStore::new(),
        Kv::new(voters(&[1]), false),
        Unbounded,
    )
    .unwrap();
    let h = owner.insert(again).map_err(|_| ()).unwrap();
    assert_eq!(h.slot(), handles[2].slot());
    assert!(
        owner.get(handles[2]).is_none(),
        "a stale handle found the slot's new replica"
    );
}

/// A leader of three, elected by hand, whose followers' answers are stepped in by the test.
fn led(depth: usize, ahead: bool) -> Sim {
    let mut s = settings(1, 9);
    s.core.apply_unpersisted = ahead;
    let mut r: Sim = Replica::open(
        &s,
        SimStore::new(depth),
        Kv::new(voters(&[1, 2, 3]), false),
        Unbounded,
    )
    .unwrap();
    elect_by_hand(&mut r);
    r
}

fn acknowledge(r: &mut Sim, from: u64) {
    let index = r.core().raft.log().last_index().unwrap();
    let term = r.term();
    r.step(Message {
        msg_type: MessageType::MsgAppendResponse,
        from,
        to: 1,
        term,
        index,
        ..Message::default()
    })
    .unwrap();
}

/// §4.2 (core step R-6, `Config::apply_unpersisted`): a leader whose followers hold an entry of
/// its term before its own disk does applies it on the commit and answers its caller then, its
/// disk the slowest of the quorum; without it, the answer waits for its own write.
#[test]
fn a_leader_applies_its_own_term_before_its_write_is_durable() {
    for ahead in [false, true] {
        let mut r = led(3, ahead);
        r.propose(Vec::new(), b"fast".to_vec()).unwrap();
        let mut out = Output::default();
        r.drive(now(), waker(), &mut out).unwrap();
        acknowledge(&mut r, 2);
        acknowledge(&mut r, 3);
        out.clear();
        r.drive(now(), waker(), &mut out).unwrap();
        let answered = out.answers.iter().any(|(_, data)| data == b"fast");
        let disk = r.log_mut().disk.last();
        assert_eq!(
            answered,
            ahead,
            "ahead {ahead}: applied {:?}, disk through {disk}",
            r.applied()
        );
        if ahead {
            assert!(r.applied().index > disk);
        }
        pump(&mut r);
        assert!(r.machine().now.entries.iter().any(|(_, _, d)| d == b"fast"));
    }
}

/// The owner's clock in nanoseconds: the test reads the host's monotonic clock at its edge, as an
/// owner does, and hands the shell nanoseconds since the first reading.
fn now() -> u64 {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    let epoch = *EPOCH.get_or_init(Instant::now);
    u64::try_from(epoch.elapsed().as_nanos()).unwrap_or(u64::MAX)
}
