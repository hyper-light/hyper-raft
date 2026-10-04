//! slates' MLRaft tests (slates `crates/cluster/src/multilog.rs`'s unit tests and
//! `crates/cluster/tests/multilog.rs`, read at `5cce86a`), each as slates states it, retargeted at
//! this crate (`docs/multilog.md` §11 step 3): the codec, the routing, one log in its own order, a
//! global waiting for every log's barrier, replicas learning commits in any order, three logs led
//! apart merging alike on every voter, each log handing off to its preferred voter; and slates'
//! explorer, every member's real logs over one adversarial network, on hyper-sim's world and
//! network (`docs/sim.md` S-1, S-2).
//!
//! The explorer, as slates states it: any message delivered in any order, dropped or duplicated;
//! any member timing out in any log, crashing and restarting from what it made durable; a member cut
//! off by a partition; commands proposed to any log, keyed and global; barriers proposed when the
//! adversary lets them. Checked after every step:
//! - **per log**, as Raft's own: one leader per term, ever, and no two members committing
//!   different entries at one index, a member that crashed and restarted among them;
//! - **the merge**: every member's application (each key's commands in order with the epoch each
//!   saw, and the global commands in order) is a prefix of one history, the first seen, across the
//!   members and across a member's restarts (a restarted member applies everything again, and must
//!   reach the same); and nothing is refused.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::disallowed_macros,
    clippy::cognitive_complexity,
    clippy::cast_possible_truncation,
    clippy::needless_range_loop,
    missing_docs,
    unreachable_pub
)]

mod support;

use std::collections::{BTreeMap, BTreeSet};

use hyper_multilog::{Applied, Command, Flow, Limits, Logs, Merge, Route, entry, log_of};
use hyper_raft::proto::{Entry, Message};
use hyper_raft::wire::Record;
use hyper_raft::{StateRole, StorageError};
use hyper_sim::net::{Net, NetLimits, Path, Ticket};
use hyper_sim::{Discipline, Random, Record as SimRecord, Source, Step, StreamId, World, twice};
use support::{Group, Member};

/// A bound on what a log holds past its merge no test here reaches.
const ROOMY: Limits = Limits { unmerged: 1 << 20 };

/// The first key that routes to `log` of `logs`.
fn key_in(log: usize, logs: usize) -> u64 {
    (0..).find(|key| log_of(*key, logs) == log).unwrap()
}

// --- slates' unit tests (`crates/cluster/src/multilog.rs`, `mod tests`) ---

/// slates: "the codec: every entry round-trips, the empty command is Raft's no-op, and bytes no
/// multi-log writes — a truncated key or barrier, an unknown tag — decode as a no-op, never as
/// another entry." Here such bytes are refused, alike on every member (`docs/multilog.md` §2.2),
/// which applies nothing, as a no-op does; slates' own hostile vectors are among them.
#[test]
fn entries_round_trip_and_foreign_bytes_are_no_ops() {
    let read = |data: Vec<u8>| {
        let entry = Entry {
            data,
            ..Entry::default()
        };
        format!("{:?}", entry::read(&entry))
    };
    assert_eq!(read(entry::global(vec![7]).unwrap()), "Global([7])");
    assert_eq!(read(entry::global(Vec::new()).unwrap()), "Global([])");
    assert_eq!(
        read(entry::keyed_command(vec![9], u64::MAX).unwrap()),
        format!("Keyed {{ key: {}, command: [9] }}", u64::MAX)
    );
    assert_eq!(read(entry::barrier_naming(42).unwrap()), "Barrier(42)");
    assert_eq!(read(Vec::new()), "Own");
    // slates' hostile bytes (its prefix tags 1 keyed and 2 barrier), and this format's own.
    for hostile in [
        vec![1, 1, 2, 3],
        vec![2, 1, 2, 3],
        vec![2, 0, 0, 0, 0, 0, 0, 0, 1, 9],
        vec![0xff],
        vec![1, 2, 3, entry::KEYED],
        vec![1, 2, 3, entry::BARRIER],
    ] {
        assert_eq!(read(hostile.clone()), "Malformed", "{hostile:?}");
    }
}

/// slates: "routing: a key goes to one log, the same every time, and keys spread over every
/// log." slates held each log of three to more than 900 of keys 0 to 2,999; the counts are exact
/// facts of the function, so they are held exactly, and a change of the routing, which is part of
/// the format, fails here.
#[test]
fn keys_route_to_one_log_each_and_spread_over_all() {
    let mut hits = [0usize; 3];
    for key in 0..3_000u64 {
        let log = log_of(key, 3);
        assert_eq!(log, log_of(key, 3));
        hits[log] += 1;
    }
    assert_eq!(
        hits,
        [999, 986, 1_015],
        "every log takes keys, as the format routes them"
    );
    assert_eq!(log_of(12_345, 1), 0, "one log takes every key");
}

/// The logs as a merge reads them: each committed through its length.
struct Whole(Vec<Vec<Entry>>);

impl Whole {
    fn of(logs: &[Vec<Vec<u8>>]) -> Self {
        Self(
            logs.iter()
                .map(|log| {
                    log.iter()
                        .enumerate()
                        .map(|(at, data)| Entry {
                            index: at as u64 + 1,
                            term: 1,
                            data: data.clone(),
                            ..Entry::default()
                        })
                        .collect()
                })
                .collect(),
        )
    }
}

/// The first `through[k]` entries of each log.
struct Prefix<'a>(&'a Whole, &'a [usize]);

impl Logs for Prefix<'_> {
    fn count(&self) -> usize {
        self.0.0.len()
    }
    fn through(&self, log: usize) -> u64 {
        self.1[log] as u64
    }
    fn walk(
        &self,
        log: usize,
        from: u64,
        through: u64,
        visit: &mut dyn FnMut(&Entry) -> bool,
    ) -> Result<(), StorageError> {
        for index in from..=through {
            if visit(&self.0.0[log][index as usize - 1]) {
                break;
            }
        }
        Ok(())
    }
}

/// What a merge applies over the first `through` of each log: `(log, key, command, epoch)`.
fn advance(
    merge: &mut Merge,
    logs: &Whole,
    through: &[usize],
) -> Vec<(usize, Option<u64>, Vec<u8>, u64)> {
    let mut out = Vec::new();
    merge
        .advance(&Prefix(logs, through), u64::MAX, &mut |applied| {
            match applied {
                Applied::Command(Command {
                    log,
                    key,
                    data,
                    epoch,
                    ..
                }) => {
                    out.push((log, key, data.to_vec(), epoch));
                }
                Applied::Refused { .. } => panic!("refused {applied:?}"),
            }
            Flow::Continue
        })
        .unwrap();
    out
}

fn keyed(key: u64, command: u8) -> Vec<u8> {
    entry::keyed_command(vec![command], key).unwrap()
}

fn global(command: u8) -> Vec<u8> {
    entry::global(vec![command]).unwrap()
}

/// slates (R8): "with one log there are no barriers, and the merge is log 0 in order, each command
/// in the epoch the last global command before it opened."
#[test]
fn one_log_applies_in_its_own_order() {
    let logs = Whole::of(&[vec![keyed(1, 1), global(2), Vec::new(), keyed(1, 3)]]);
    let mut merge = Merge::new(1).unwrap();
    let seen: Vec<(Option<u64>, Vec<u8>, u64)> = advance(&mut merge, &logs, &[4])
        .into_iter()
        .map(|(_, key, command, epoch)| (key, command, epoch))
        .collect();
    assert_eq!(
        seen,
        vec![
            (Some(1), vec![1], 0),
            (None, vec![2], 0),
            (Some(1), vec![3], 2)
        ]
    );
}

/// slates: "a global command waits until every other log has a barrier naming it, so the keyed
/// commands a log ordered before its barrier are applied before it, and those after, after it."
/// Keys are each in the log they route to (slates' keys 10 and 20 route elsewhere here).
#[test]
fn a_global_command_waits_for_every_logs_barrier() {
    let (k1, k2) = (key_in(1, 3), key_in(2, 3));
    let logs = Whole::of(&[
        vec![global(1)],
        vec![
            keyed(k1, 2),
            entry::barrier_naming(1).unwrap(),
            keyed(k1, 3),
        ],
        vec![keyed(k2, 4), entry::barrier_naming(1).unwrap()],
    ]);
    let mut merge = Merge::new(3).unwrap();
    let first = advance(&mut merge, &logs, &[1, 1, 1]);
    assert_eq!(first.len(), 2, "the two keyed commands before any barrier");
    assert!(first.iter().all(|applied| applied.3 == 0), "at epoch 0");
    assert!(
        advance(&mut merge, &logs, &[1, 3, 1]).is_empty(),
        "log 2 has no barrier yet"
    );
    let rest: Vec<(usize, Vec<u8>, u64)> = advance(&mut merge, &logs, &[1, 3, 2])
        .into_iter()
        .map(|(log, _, command, epoch)| (log, command, epoch))
        .collect();
    assert_eq!(
        rest,
        vec![(0, vec![1], 0), (1, vec![3], 1)],
        "the global, then what log 1 ordered after it"
    );
}

/// slates: "determinism: replicas that learn the same logs' commits in different orders apply
/// each key's commands in one order, each in the same epoch, and the global commands in one order —
/// the state is the same everywhere." slates' 64 seeds of its arrival draw, and its expected
/// histories; its keys 3, 6, 1 and 2 stood in logs they do not route to here, so each is replaced
/// by the first key routing to its log (log 0's two keys stay two keys).
#[test]
fn replicas_learning_commits_in_any_order_reach_one_state() {
    let (a, b) = (
        key_in(0, 3),
        (0..).filter(|k| log_of(*k, 3) == 0).nth(1).unwrap(),
    );
    let (one, two) = (key_in(1, 3), key_in(2, 3));
    let logs = Whole::of(&[
        vec![keyed(a, 1), global(2), keyed(a, 3), global(4), keyed(b, 5)],
        vec![
            keyed(one, 6),
            entry::barrier_naming(2).unwrap(),
            keyed(one, 7),
            entry::barrier_naming(4).unwrap(),
            keyed(one, 8),
        ],
        vec![
            entry::barrier_naming(4).unwrap(),
            keyed(two, 9),
            keyed(two, 10),
        ],
    ]);
    let lengths: Vec<usize> = logs.0.iter().map(Vec::len).collect();
    let summary = |applied: Vec<(usize, Option<u64>, Vec<u8>, u64)>| {
        let mut keys: BTreeMap<Option<u64>, Vec<(Vec<u8>, u64)>> = BTreeMap::new();
        for (_, key, command, epoch) in applied {
            keys.entry(key).or_default().push((command, epoch));
        }
        keys
    };
    let mut reference = None;
    for seed in 0..64u64 {
        let mut through = vec![0usize; 3];
        let mut merge = Merge::new(3).unwrap();
        let mut applied = Vec::new();
        let mut draw = seed;
        while through.iter().zip(&lengths).any(|(at, length)| at < length) {
            draw = draw
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let log = ((draw >> 33) % 3) as usize;
            if through[log] < lengths[log] {
                through[log] += 1;
                applied.extend(advance(&mut merge, &logs, &through));
            }
        }
        let summary = summary(applied);
        assert_eq!(
            *reference.get_or_insert_with(|| summary.clone()),
            summary,
            "seed {seed}"
        );
    }
    let reference = reference.unwrap();
    assert_eq!(
        reference[&None],
        vec![(vec![2], 0), (vec![4], 2)],
        "both globals, in log 0's order"
    );
    assert_eq!(
        reference[&Some(one)],
        vec![(vec![6], 0), (vec![7], 2), (vec![8], 4)]
    );
    assert_eq!(reference[&Some(two)], vec![(vec![9], 4), (vec![10], 4)]);
}

/// The member that leads `log`.
fn leader_of(group: &mut Group, log: usize) -> u64 {
    (1..=group.members.len() as u64)
        .find(|id| group.member(*id).leads(log))
        .unwrap()
}

/// slates: "over real Raft logs: three logs, each led by a different voter. Keyed and global
/// commands are proposed at their logs' leaders, each log's leader appends its barrier once log 0's
/// global commands reach it, and every voter applies each key's commands in one order and each in
/// one epoch." slates counted one barrier appended in each other log; here every member proposes
/// what it owes and the leader keeps one (`docs/multilog.md` §3.1), so each log holds one.
#[test]
fn three_logs_led_apart_merge_alike_on_every_voter() {
    let mut group = Group::new(3, 3, 29, ROOMY);
    for (log, leader) in [(0, 1), (1, 2), (2, 3)] {
        group.elect(log, leader);
    }
    let propose_in_every_other_log = |group: &mut Group, command: u8| {
        for log in 1..3 {
            let leader = leader_of(group, log);
            assert!(
                group
                    .member(leader)
                    .propose(Route::Key(key_in(log, 3)), vec![command])
            );
        }
    };
    propose_in_every_other_log(&mut group, 1);
    assert!(group.member(1).propose(Route::Global, vec![2]));
    assert!(group.member(1).propose(Route::Key(key_in(0, 3)), vec![3]));
    group.quiet();
    for log in 1..3 {
        let leader = leader_of(&mut group, log);
        let barriers = group
            .member(leader)
            .multi
            .node(log)
            .unwrap()
            .store()
            .0
            .entries
            .iter()
            .filter(|entry| matches!(entry::read(entry), entry::Stated::Barrier(_)))
            .count();
        assert_eq!(barriers, 1, "one barrier after the global in log {log}");
    }
    propose_in_every_other_log(&mut group, 4);
    group.quiet();
    let outcomes: Vec<_> = group
        .members
        .iter()
        .map(|member| member.app.keys.clone())
        .collect();
    assert!(
        outcomes.windows(2).all(|pair| pair[0] == pair[1]),
        "{outcomes:?}"
    );
    assert_eq!(
        outcomes[0][&None][0].1, 0,
        "the global applied before any other"
    );
    for log in 1..3 {
        let history = &outcomes[0][&Some(key_in(log, 3))];
        assert_eq!(history.len(), 2);
        assert!(
            history[0].1 < history[1].1,
            "the second command, after the barrier, saw the global: {history:?}"
        );
    }
}

/// slates: "leaders spread by priority and transfer: with one voter leading all three logs, each
/// log whose preferred voter is another hands off to it once its leader has led the priority
/// windows — so the logs end led apart." Here the hand-off is offered once the preferred voter
/// holds the leader's whole log, and the owner takes it (`docs/multilog.md` §7).
#[test]
fn each_log_hands_off_to_its_preferred_voter() {
    let mut group = Group::new(3, 3, 31, ROOMY);
    for log in 0..3 {
        group.elect(log, 1);
    }
    for member in &mut group.members {
        member.multi.spread(&[1, 2, 3]).unwrap();
    }
    // A round of heartbeats, so the leader hears each follower hold its log.
    for _ in 0..2 {
        group.tick();
    }
    let targets: Vec<Option<u64>> = (0..3)
        .map(|log| group.member(1).multi.hand_off(log))
        .collect();
    assert_eq!(
        targets,
        vec![None, Some(2), Some(3)],
        "log 0 stays with 1; logs 1 and 2 go to 2 and 3"
    );
    for (log, to) in targets.iter().enumerate() {
        if let Some(to) = to {
            group
                .member(1)
                .multi
                .node_mut(log)
                .unwrap()
                .transfer_leader(*to)
                .unwrap();
        }
    }
    group.quiet();
    for _ in 0..2 {
        group.tick();
    }
    let leaders: Vec<u64> = (0..3).map(|log| leader_of(&mut group, log)).collect();
    assert_eq!(leaders, vec![1, 2, 3], "the logs end led apart");
}

// --- slates' explorer (`crates/cluster/tests/multilog.rs`) on hyper-sim ---

/// Shape: the seeds each shape is explored under at full scale, and in the workspace's debug run
/// (slates' own).
const SEEDS_FULL: u64 = 200;
const SEEDS_QUICK: u64 = 16;
/// Shape: the steps one seeded history runs (slates').
const STEPS: usize = 3_000;
/// Shape: a history alternates adversarial and calm stretches of this many steps, as slates'
/// explorers do, so elections settle and commands commit between the faults (slates measured five
/// voters committing 108 entries over 400 seeds without them).
const STRETCH: usize = 200;
/// Shape: what the network holds in flight, the oldest lost past it and counted (slates').
const IN_FLIGHT_BOUND: usize = 512;
/// Shape: the commands a history proposes at most, keyed and global (slates').
const PROPOSALS_BOUND: u64 = 96;
/// Shape: the keys the commands write: few, so every key collects a history across several epochs
/// (slates').
const KEYS: u64 = 12;
/// Shape: one command in this many is global (slates').
const GLOBAL_EVERY: u64 = 6;
/// Shape: the actions one step chooses among (slates').
const ACTIONS: u64 = 100;
/// Shape: the world steps one action may take: a network tick delivers a message for each member,
/// a drop or a duplicate one.
const WORLD_STEPS_PER_ACTION: u64 = 5;
/// Shape: the words of trace one action records at most: its action, its member and log draws, a
/// pick for each world step, and a proposal's two.
const WORDS_PER_ACTION: u64 = 4 + WORLD_STEPS_PER_ACTION + 2;

/// What the network carries: a log's number and its member's message.
type Payload = (usize, Message);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ev {
    Arrive(Ticket),
}

/// One member's application since its last restart, as the harness's state machine holds it.
type History = BTreeMap<Option<u64>, Vec<(Vec<u8>, u64)>>;

/// What an exploration counted: the non-vacuity evidence (slates' counters).
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Counters {
    pub elections_won: u64,
    pub keyed_applied: u64,
    pub globals_applied: u64,
    pub barriers_proposed: u64,
    pub crashes: u64,
    pub replays_matched: u64,
    pub dropped: u64,
    pub duplicated: u64,
    pub overflowed: u64,
    /// Proposals refused: no log's member could take them (no leader known, a transfer, a bound).
    pub proposals_refused: u64,
}

impl Counters {
    fn add(&mut self, other: &Self) {
        self.elections_won += other.elections_won;
        self.keyed_applied += other.keyed_applied;
        self.globals_applied += other.globals_applied;
        self.barriers_proposed += other.barriers_proposed;
        self.crashes += other.crashes;
        self.replays_matched += other.replays_matched;
        self.dropped += other.dropped;
        self.duplicated += other.duplicated;
        self.overflowed += other.overflowed;
        self.proposals_refused += other.proposals_refused;
    }
}

/// One history's world: the members, the network, the checks' records.
struct Cluster {
    world: World<Ev>,
    net: Net<Payload>,
    members: Vec<Member>,
    streams: Streams,
    isolated: Option<u64>,
    /// Per (log, term), the leader seen.
    leaders: BTreeMap<(usize, u64), u64>,
    /// Per member and log, whether it led after the last step.
    leading: BTreeSet<(u64, usize)>,
    /// Per (log, index), the entry first seen committed there.
    committed: BTreeMap<(usize, u64), (u64, Vec<u8>)>,
    /// The history every member's application must be a prefix of: per key, the longest seen.
    reference: History,
    proposals: u64,
    counters: Counters,
    /// Counts the members had folded in before their last restart.
    folded: Vec<support::Counts>,
}

/// The world's streams the explorer draws from, one a source.
struct Streams {
    action: StreamId,
    member: StreamId,
    log: StreamId,
    command: StreamId,
}

impl Cluster {
    fn new(source: Source, voters: u64, logs: usize) -> Self {
        let world_steps = STEPS as u64 * WORLD_STEPS_PER_ACTION;
        let limits = hyper_sim::Limits {
            events: 2 * IN_FLIGHT_BOUND,
            nodes: voters as usize,
            streams: 16 + (voters * voters) as usize * 3,
            steps: world_steps,
            trace_words: (STEPS as u64 * WORDS_PER_ACTION) as usize,
        };
        let mut world = World::new(source, Discipline::Free, limits).unwrap();
        for _ in 0..voters {
            world.node(hyper_sim::Clock::default()).unwrap();
        }
        let streams = Streams {
            action: world.stream("explore.action", &[]).unwrap(),
            member: world.stream("explore.member", &[]).unwrap(),
            log: world.stream("explore.log", &[]).unwrap(),
            command: world.stream("explore.command", &[]).unwrap(),
        };
        let mut net = Net::new(NetLimits {
            flows: (voters * voters) as usize,
            links: 0,
            nats: 0,
            link_messages: 0,
            messages: IN_FLIGHT_BOUND,
            bytes: IN_FLIGHT_BOUND * support::MESSAGE,
        });
        net.set_path(Path::NONE);
        let ids: Vec<u64> = (1..=voters).collect();
        let members = ids
            .iter()
            .map(|id| {
                let mut member = Member::new(*id, &ids, logs, *id, ROOMY);
                member.auto_barriers = false;
                member
            })
            .collect();
        Self {
            world,
            net,
            members,
            streams,
            isolated: None,
            leaders: BTreeMap::new(),
            leading: BTreeSet::new(),
            committed: BTreeMap::new(),
            reference: History::new(),
            proposals: 0,
            counters: Counters::default(),
            folded: vec![support::Counts::default(); voters as usize],
        }
    }

    fn draw(&mut self, stream: StreamId, bound: u64) -> u64 {
        self.world.below(stream, bound).unwrap()
    }

    fn node(id: u64) -> hyper_sim::NodeId {
        hyper_sim::NodeId(id as u32 - 1)
    }

    /// Settles member `id` and sends what it sends.
    fn settle(&mut self, id: u64) {
        let mut out = Vec::new();
        self.members[id as usize - 1].settle(&mut out);
        for (log, message) in out {
            let to = message.to;
            let bytes = message.encoded_len();
            let fate = self
                .net
                .send(
                    &mut self.world,
                    (Self::node(id), Self::node(to)),
                    (log, message),
                    bytes,
                    Ev::Arrive,
                )
                .unwrap();
            if matches!(fate, hyper_sim::net::Fate::Dropped(_)) {
                self.counters.dropped += 1;
            }
        }
    }

    /// Delivers the arrival the world picks: a member steps it and settles.
    fn deliver_one(&mut self) {
        match self.world.next(&mut Random).unwrap() {
            Step::Event {
                event: Ev::Arrive(ticket),
                ..
            } => {
                if let Some(delivery) = self
                    .net
                    .deliver(&mut self.world, ticket, Ev::Arrive)
                    .unwrap()
                {
                    let to = u64::from(delivery.to.0) + 1;
                    let (log, message) = delivery.payload;
                    let _ = self.members[to as usize - 1].multi.step(log, message);
                    self.settle(to);
                }
            }
            Step::Wake { .. } | Step::Idle | Step::Spent => {}
        }
    }

    /// Takes the arrival the world picks and loses it.
    fn drop_one(&mut self) {
        if let Step::Event {
            event: Ev::Arrive(ticket),
            ..
        } = self.world.next(&mut Random).unwrap()
            && self
                .net
                .deliver(&mut self.world, ticket, Ev::Arrive)
                .unwrap()
                .is_some()
        {
            self.counters.dropped += 1;
        }
    }

    /// Delivers the arrival the world picks, and sends a copy of it again.
    fn duplicate_one(&mut self) {
        let Step::Event {
            event: Ev::Arrive(ticket),
            ..
        } = self.world.next(&mut Random).unwrap()
        else {
            return;
        };
        let Some(delivery) = self
            .net
            .deliver(&mut self.world, ticket, Ev::Arrive)
            .unwrap()
        else {
            return;
        };
        let (log, message) = delivery.payload.clone();
        let bytes = message.encoded_len();
        self.net
            .send(
                &mut self.world,
                (delivery.from, delivery.to),
                (log, message),
                bytes,
                Ev::Arrive,
            )
            .unwrap();
        self.counters.duplicated += 1;
        let to = u64::from(delivery.to.0) + 1;
        let (log, message) = delivery.payload;
        let _ = self.members[to as usize - 1].multi.step(log, message);
        self.settle(to);
    }

    fn pick_member(&mut self) -> u64 {
        let voters = self.members.len() as u64;
        self.draw(self.streams.member, voters) + 1
    }

    fn pick_log(&mut self) -> usize {
        let logs = self.members[0].logs as u64;
        self.draw(self.streams.log, logs) as usize
    }

    /// A command, keyed to one of [`KEYS`] or, one in [`GLOBAL_EVERY`], global, proposed at a
    /// member drawn: appended where it leads the log the command routes to, forwarded otherwise.
    fn propose(&mut self) {
        if self.proposals >= PROPOSALS_BOUND {
            return;
        }
        let route = if self.draw(self.streams.command, GLOBAL_EVERY) == 0 {
            Route::Global
        } else {
            Route::Key(self.draw(self.streams.command, KEYS))
        };
        let at = self.pick_member();
        self.proposals += 1;
        let command = self.proposals.to_le_bytes().to_vec();
        if !self.members[at as usize - 1].propose(route, command) {
            self.counters.proposals_refused += 1;
        }
        self.settle(at);
    }

    /// Member `at` proposes the barriers it owes.
    fn barriers(&mut self, at: u64) {
        self.members[at as usize - 1].barriers();
        self.settle(at);
    }

    /// Member `at` times out in `log`: it campaigns, by pre-vote.
    fn time_out(&mut self, at: u64, log: usize) {
        let _ = self.members[at as usize - 1]
            .multi
            .node_mut(log)
            .unwrap()
            .campaign();
        self.settle(at);
    }

    /// Every member's every log ticks once: leaders beat and judge their quorum, followers age
    /// their leases and timers.
    fn tick_all(&mut self) {
        for id in 1..=self.members.len() as u64 {
            let member = &mut self.members[id as usize - 1];
            for log in 0..member.logs {
                let _ = member.multi.node_mut(log).unwrap().tick();
            }
            self.settle(id);
        }
    }

    /// `log`'s leaders, if any, beat to every other member.
    fn heartbeat(&mut self, log: usize) {
        for id in 1..=self.members.len() as u64 {
            if self.members[id as usize - 1].leads(log) {
                let _ = self.members[id as usize - 1]
                    .multi
                    .node_mut(log)
                    .unwrap()
                    .ping();
                self.settle(id);
            }
        }
    }

    /// Member `at` crashes and restarts from what it made durable: its logs reopen, its merge
    /// starts over at its image's cut, its application is rebuilt from there.
    fn crash(&mut self, at: u64) {
        let member = &mut self.members[at as usize - 1];
        let counts = member.counts;
        let folded = &mut self.folded[at as usize - 1];
        folded.keyed_applied += counts.keyed_applied;
        folded.globals_applied += counts.globals_applied;
        folded.barriers_proposed += counts.barriers_proposed;
        folded.refused += counts.refused;
        member.restart();
        member.counts = support::Counts::default();
        self.counters.crashes += 1;
        self.settle(at);
    }

    fn isolate(&mut self, at: Option<u64>) {
        self.net.heal();
        self.isolated = at;
        if let Some(at) = at {
            for other in 1..=self.members.len() as u64 {
                if other != at {
                    self.net.partition(Self::node(at), Self::node(other), true);
                    self.net.partition(Self::node(other), Self::node(at), true);
                }
            }
        }
    }

    fn step(&mut self, calm: bool) {
        if calm && self.isolated.is_some() {
            self.isolate(None);
        }
        let roll = self.draw(self.streams.action, ACTIONS);
        if calm && matches!(roll, 60..=65 | 90..=94) {
            return;
        }
        match roll {
            0..=39 => {
                for _ in 0..self.members.len() {
                    if self.world.pending() == 0 {
                        break;
                    }
                    self.deliver_one();
                }
            }
            40..=47 => {
                let log = self.pick_log();
                self.heartbeat(log);
            }
            48..=55 => self.propose(),
            56..=59 => {
                let at = self.pick_member();
                self.barriers(at);
            }
            60..=62 => {
                if self.world.pending() > 0 {
                    self.drop_one();
                }
            }
            63..=65 => {
                if self.world.pending() > 0 {
                    self.duplicate_one();
                }
            }
            66..=79 => {
                let (at, log) = (self.pick_member(), self.pick_log());
                let leader_known = self.members[at as usize - 1]
                    .multi
                    .node(log)
                    .unwrap()
                    .raft
                    .leader_id()
                    != 0;
                if calm && leader_known {
                    return;
                }
                self.time_out(at, log);
            }
            80..=89 => self.tick_all(),
            90..=92 => {
                let at = self.pick_member();
                self.crash(at);
            }
            93..=94 => {
                let at = if self.isolated.is_none() {
                    Some(self.pick_member())
                } else {
                    None
                };
                self.isolate(at);
            }
            _ => {
                let at = self.pick_member();
                self.barriers(at);
            }
        }
    }

    /// The checks after every step: Raft's own per log, and the merge's history.
    fn check(&mut self, at: &str) {
        for index in 0..self.members.len() {
            let id = index as u64 + 1;
            self.check_logs(id, at);
            self.check_history(id, at);
        }
    }

    fn check_logs(&mut self, id: u64, at: &str) {
        let member = &self.members[id as usize - 1];
        for log in 0..member.logs {
            let raft = &member.multi.node(log).unwrap().raft;
            if raft.state() == StateRole::Leader {
                let previous = *self.leaders.entry((log, raft.term())).or_insert(id);
                assert_eq!(
                    previous,
                    id,
                    "{at}: two leaders of term {} in log {log}",
                    raft.term()
                );
                if self.leading.insert((id, log)) {
                    self.counters.elections_won += 1;
                }
            } else {
                self.leading.remove(&(id, log));
            }
            let disk = &member.multi.node(log).unwrap().store().0;
            let commit = raft.log().committed().min(disk.last_index());
            for entry in disk.entries.iter().filter(|entry| entry.index <= commit) {
                let first = self
                    .committed
                    .entry((log, entry.index))
                    .or_insert_with(|| (entry.term, entry.data.clone()));
                assert_eq!(
                    first,
                    &(entry.term, entry.data.clone()),
                    "{at}: log {log} committed two entries at {}",
                    entry.index
                );
            }
        }
    }

    fn check_history(&mut self, id: u64, at: &str) {
        let member = &self.members[id as usize - 1];
        assert_eq!(
            member.counts.refused, 0,
            "{at}: member {id} refused an entry"
        );
        let restarted = self.counters.crashes > 0;
        for (key, history) in &member.app.keys {
            let reference = self.reference.entry(*key).or_default();
            for (position, applied) in history.iter().enumerate() {
                match reference.get(position) {
                    Some(seen) => {
                        assert_eq!(
                            seen, applied,
                            "{at}: member {id} applied a different command or epoch for key {key:?} at its {position}th"
                        );
                    }
                    None => reference.push(applied.clone()),
                }
            }
            if restarted && history.len() < reference.len() {
                self.counters.replays_matched += 1;
            }
        }
    }

    /// The counters, with every member's applications summed.
    fn totals(&self) -> Counters {
        let mut counters = self.counters;
        for (member, folded) in self.members.iter().zip(&self.folded) {
            counters.keyed_applied += member.counts.keyed_applied + folded.keyed_applied;
            counters.globals_applied += member.counts.globals_applied + folded.globals_applied;
            counters.barriers_proposed +=
                member.counts.barriers_proposed + folded.barriers_proposed;
        }
        counters.overflowed = self.net.stats().dropped_capacity;
        counters
    }
}

/// One seeded history of `voters` voters holding `logs` logs each; its record and its counters.
fn explore_one(source: Source, voters: u64, logs: usize, seed: u64) -> (SimRecord, Counters) {
    let mut cluster = Cluster::new(source, voters, logs);
    for step in 0..STEPS {
        let calm = (step / STRETCH) % 2 == 1;
        cluster.step(calm);
        cluster.check(&format!("seed {seed} step {step}"));
    }
    let counters = cluster.totals();
    (cluster.world.finish(), counters)
}

/// Explores `seeds` histories of `voters` voters holding `logs` logs each, the first through the
/// run-twice check (`docs/sim.md` §3.9); what they counted.
fn explore(voters: u64, logs: usize, seeds: u64) -> Counters {
    let mut total = Counters::default();
    let base = (voters << 32) ^ ((logs as u64) << 40);
    let mut first = None;
    twice(base, |source| {
        let (record, counters) = explore_one(source, voters, logs, 0);
        first = Some(counters);
        Ok::<_, String>(record)
    })
    .unwrap_or_else(|refusal| panic!("{voters} voters, {logs} logs: {refusal}"));
    total.add(&first.unwrap());
    for seed in 1..seeds {
        let (_, counters) = explore_one(Source::Seed(base ^ seed), voters, logs, seed);
        total.add(&counters);
    }
    total
}

/// Explores three voters with three logs and five with two, and holds every non-vacuity floor:
/// each path reached more than once a seed, slates' floor.
fn explore_and_check_coverage(seeds: u64) {
    for (voters, logs) in [(3, 3), (5, 2)] {
        let counted = explore(voters, logs, seeds);
        eprintln!(
            "explored {voters} voters x {logs} logs x {seeds} seeds x {STEPS} steps: {counted:?}"
        );
        let floors = [
            (counted.elections_won, "elections were won"),
            (counted.keyed_applied, "keyed commands were applied"),
            (counted.globals_applied, "global commands were applied"),
            (counted.barriers_proposed, "barriers were proposed"),
            (counted.crashes, "members crashed and restarted"),
            (
                counted.replays_matched,
                "restarted members replayed their application",
            ),
        ];
        for (count, path) in floors {
            assert!(
                count > seeds,
                "{voters} voters, {logs} logs: {path} ({count} over {seeds} seeds)"
            );
        }
    }
}

/// slates (§3.6, T-8.13's counterpart for MLRaft): "the merge applies every key's commands in one
/// order and each in one epoch, and the global commands in one order, on every node and across
/// every restart, under loss, duplication, reordering, partitions and crash-restarts; each log keeps
/// Raft's own safety." The workspace's scale ([`SEEDS_QUICK`]).
#[test]
fn the_multi_log_merges_alike_under_an_adversarial_network() {
    explore_and_check_coverage(SEEDS_QUICK);
}

/// The same at full scale ([`SEEDS_FULL`]), run in release with `--ignored`.
#[test]
#[ignore = "full scale: run in release"]
fn the_multi_log_merges_alike_at_full_scale() {
    explore_and_check_coverage(SEEDS_FULL);
}
