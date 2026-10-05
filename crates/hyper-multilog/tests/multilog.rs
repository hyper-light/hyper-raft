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
use hyper_raft::proto::{Entry, EntryType, Message};
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
                Applied::Refused { .. } | Applied::Resized { .. } => {
                    panic!("refused or resized: {applied:?}")
                }
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

/// What one step's actions weigh: a step draws one in proportion to its weight.
#[derive(Clone, Copy, Debug)]
struct Weights {
    deliver: u64,
    heartbeat: u64,
    propose: u64,
    barriers: u64,
    drop: u64,
    duplicate: u64,
    time_out: u64,
    tick: u64,
    crash: u64,
    isolate: u64,
    image: u64,
    resize: u64,
}

impl Weights {
    fn total(&self) -> u64 {
        self.deliver
            + self.heartbeat
            + self.propose
            + self.barriers
            + self.drop
            + self.duplicate
            + self.time_out
            + self.tick
            + self.crash
            + self.isolate
            + self.image
            + self.resize
    }
}

/// What a step does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Act {
    Deliver,
    Heartbeat,
    Propose,
    Barriers,
    Drop,
    Duplicate,
    TimeOut,
    Tick,
    Crash,
    Isolate,
    Image,
    Resize,
}

/// The explorer's scale: each number stated where [`SCALE`] gives it.
#[derive(Clone, Copy, Debug)]
struct Scale {
    /// The steps one seeded history runs.
    steps: usize,
    /// A history alternates adversarial and calm stretches of this many steps.
    stretch: usize,
    /// The commands a history proposes at most, keyed and global.
    proposals: u64,
    /// The keys the commands write.
    keys: u64,
    /// One command in this many is global.
    global_every: u64,
    /// What the network holds in flight, the oldest lost past it and counted.
    in_flight: usize,
    weights: Weights,
}

/// The scale the explorer runs at, measured by [`measure_the_scale`] (2026-10-05): from slates'
/// shape (3,000 steps, stretches of 200, 96 proposals, 12 keys, one global in 6, its hundred
/// actions, four image actions and one resize), each number in the order listed went down to the
/// least at which, with every number before it at its own least, every path [`coverage`] claims
/// holds its floor over [`WILKS_SEEDS`] seeds of every shape and the planted defect is caught. The
/// network's bound is the most messages any of those histories held in flight at once, with no
/// bound: no message is lost to it, only to the explorer's own drops.
const SCALE: Scale = Scale {
    steps: 431,
    stretch: 75,
    proposals: 18,
    keys: 10,
    global_every: 6,
    in_flight: 142,
    weights: Weights {
        deliver: 40,
        heartbeat: 1,
        propose: 6,
        barriers: 6,
        drop: 1,
        duplicate: 1,
        time_out: 3,
        tick: 7,
        crash: 1,
        isolate: 1,
        image: 1,
        resize: 1,
    },
};

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
    /// Images taken at a canonical cut, every log compacted to them.
    pub images_taken: u64,
    /// Images installed from a log's snapshot, ahead of the member's state.
    pub images_installed: u64,
    /// Indexes the leaders committed by a fast quorum (a group on the fast track).
    pub fast_committed: u64,
    /// Fast proposals another entry took the index of, each proposed again by its member.
    pub displaced: u64,
    /// Resizes applied, by every member, all told.
    pub resizes: u64,
    /// Keyed commands refused for their log after a resize routed their key elsewhere.
    pub misrouted: u64,
    /// The most messages the network held in flight at once, over every history.
    pub most_in_flight: u64,
    /// The fewest commands any log the group began with gave the members to apply, over a
    /// history's members: the explorer exercises every log, or this says which it left idle.
    pub least_log_applied: u64,
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
        self.images_taken += other.images_taken;
        self.images_installed += other.images_installed;
        self.fast_committed += other.fast_committed;
        self.displaced += other.displaced;
        self.resizes += other.resizes;
        self.misrouted += other.misrouted;
        self.most_in_flight = self.most_in_flight.max(other.most_in_flight);
        self.least_log_applied += other.least_log_applied;
    }
}

/// One history's world: the members, the network, the checks' records.
struct Cluster {
    world: World<Ev>,
    net: Net<Payload>,
    members: Vec<Member>,
    streams: Streams,
    isolated: Option<u64>,
    /// Per (log, generation, term), the leader seen.
    leaders: BTreeMap<(usize, u32, u64), u64>,
    /// Per member, log and generation, whether it led after the last step.
    leading: BTreeSet<(u64, usize, u32)>,
    /// Per (log, generation, index), the entry first seen committed there.
    committed: BTreeMap<(usize, u32, u64), (EntryType, Vec<u8>)>,
    /// The history every member's application must be a prefix of: per key, the longest seen.
    reference: History,
    proposals: u64,
    counters: Counters,
    /// Counts the members had folded in before their last restart.
    folded: Vec<support::Counts>,
    /// For each member, how far into each key's history it has been checked since its restart:
    /// what it applies again below the reference's length is a replay, matched.
    checked: Vec<BTreeMap<Option<u64>, usize>>,
    /// The planted defect, where a test plants it: member 1's application is a merge that passes
    /// barriers as nothing, with what it applied.
    mutant: Option<(Merge, support::App)>,
    /// Whether the group runs the fast track: each command is proposed by it.
    fast: bool,
    /// Whether members propose to change the count of logs, to between one and one more than the
    /// group began with.
    resize: bool,
    /// The count of logs the group began with.
    logs: usize,
    /// Fast commits counted by members before their last restart.
    fast_folded: u64,
    scale: Scale,
}

/// The world's streams the explorer draws from, one a source.
struct Streams {
    action: StreamId,
    member: StreamId,
    log: StreamId,
    command: StreamId,
}

impl Cluster {
    fn new(
        source: Source,
        voters: u64,
        logs: usize,
        fast: bool,
        resize: bool,
        scale: Scale,
    ) -> Self {
        let scale = if resize {
            scale
        } else {
            Scale {
                weights: Weights {
                    resize: 0,
                    ..scale.weights
                },
                ..scale
            }
        };
        let world_steps = scale.steps as u64 * WORLD_STEPS_PER_ACTION;
        let limits = hyper_sim::Limits {
            // A message in flight is one arrival, and a duplicate of it one more.
            events: 2 * scale.in_flight,
            nodes: voters as usize,
            streams: 16 + (voters * voters) as usize * 3,
            steps: world_steps,
            trace_words: (scale.steps as u64 * WORDS_PER_ACTION) as usize,
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
            messages: scale.in_flight,
            bytes: scale.in_flight * support::MESSAGE,
        });
        net.set_path(Path::NONE);
        let ids: Vec<u64> = (1..=voters).collect();
        let members = ids
            .iter()
            .map(|id| {
                let mut member = if fast {
                    Member::new_fast(*id, &ids, logs, *id, ROOMY)
                } else {
                    Member::new(*id, &ids, logs, *id, ROOMY)
                };
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
            checked: vec![BTreeMap::new(); voters as usize],
            mutant: None,
            fast,
            resize,
            logs,
            fast_folded: 0,
            scale,
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
        self.propose_displaced(id);
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

    /// A log some member holds now: members whose merges are at another count hold others.
    fn pick_log(&mut self) -> usize {
        let logs = self.members.iter().map(|member| member.logs).max().unwrap() as u64;
        self.draw(self.streams.log, logs) as usize
    }

    /// Member `at` proposes to change the count of logs (`docs/multilog.md` §3.5).
    fn propose_resize(&mut self, at: u64) {
        let logs = self.draw(self.streams.log, self.logs as u64 + 1) as usize + 1;
        if self.members[at as usize - 1]
            .multi
            .propose_resize(logs)
            .is_err()
        {
            self.counters.proposals_refused += 1;
        }
        self.settle(at);
    }

    /// A command, keyed to one of [`KEYS`] or, one in [`GLOBAL_EVERY`], global, proposed at a
    /// member drawn: appended where it leads the log the command routes to, forwarded otherwise.
    fn propose(&mut self) {
        if self.proposals >= self.scale.proposals {
            return;
        }
        let route = if self.draw(self.streams.command, self.scale.global_every) == 0 {
            Route::Global
        } else {
            Route::Key(self.draw(self.streams.command, self.scale.keys))
        };
        let at = self.pick_member();
        self.proposals += 1;
        let command = self.proposals.to_le_bytes().to_vec();
        let member = &mut self.members[at as usize - 1];
        let proposed = if self.fast {
            member.propose_fast(route, command)
        } else {
            member.propose(route, command)
        };
        if !proposed {
            self.counters.proposals_refused += 1;
        }
        self.settle(at);
    }

    /// What member `id` proposed by the fast track and another entry took the index of, it
    /// proposes again, as an owner does; a proposal refused again is counted refused.
    fn propose_displaced(&mut self, id: u64) {
        let member = &mut self.members[id as usize - 1];
        for (_, entry) in std::mem::take(&mut member.displaced) {
            self.counters.displaced += 1;
            let (route, command) = match entry::read(&entry) {
                entry::Stated::Keyed { key, command } => (Route::Key(key), command.to_vec()),
                entry::Stated::Global(command) => (Route::Global, command.to_vec()),
                other => panic!("member {id} proposed {other:?} by the fast track"),
            };
            if !member.propose_fast(route, command) {
                self.counters.proposals_refused += 1;
            }
        }
    }

    /// Member `at` proposes the barriers it owes.
    fn barriers(&mut self, at: u64) {
        self.members[at as usize - 1].barriers();
        self.settle(at);
    }

    /// Member `at` times out in `log`: it campaigns, by pre-vote.
    fn time_out(&mut self, at: u64, log: usize) {
        if let Some(node) = self.members[at as usize - 1].multi.node_mut(log) {
            let _ = node.campaign();
        }
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
        folded.images += counts.images;
        folded.installed += counts.installed;
        folded.resizes += counts.resizes;
        folded.misrouted += counts.misrouted;
        self.fast_folded += fast_committed(member);
        // What it waited on to propose again is lost with it.
        member.displaced.clear();
        member.restart();
        member.counts = support::Counts::default();
        self.counters.crashes += 1;
        self.checked[at as usize - 1] = member
            .app
            .keys
            .iter()
            .map(|(key, history)| (*key, history.len()))
            .collect();
        if at == 1
            && let Some(mutant) = self.mutant.as_mut()
        {
            *mutant = (Merge::new(member.logs).unwrap(), support::App::default());
        }
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

    /// The action a step draws, in proportion to [`Scale::weights`].
    fn act(&mut self) -> Act {
        let w = self.scale.weights;
        let mut roll = self.draw(self.streams.action, w.total());
        for (act, weight) in [
            (Act::Deliver, w.deliver),
            (Act::Heartbeat, w.heartbeat),
            (Act::Propose, w.propose),
            (Act::Barriers, w.barriers),
            (Act::Drop, w.drop),
            (Act::Duplicate, w.duplicate),
            (Act::TimeOut, w.time_out),
            (Act::Tick, w.tick),
            (Act::Crash, w.crash),
            (Act::Isolate, w.isolate),
            (Act::Image, w.image),
            (Act::Resize, w.resize),
        ] {
            if roll < weight {
                return act;
            }
            roll -= weight;
        }
        Act::Deliver
    }

    fn step(&mut self, calm: bool) {
        self.counters.most_in_flight = self
            .counters
            .most_in_flight
            .max(self.world.pending() as u64);
        if calm && self.isolated.is_some() {
            self.isolate(None);
        }
        let act = self.act();
        // A calm stretch loses, duplicates and crashes nothing, and partitions no one.
        if calm && matches!(act, Act::Drop | Act::Duplicate | Act::Crash | Act::Isolate) {
            return;
        }
        match act {
            Act::Deliver => {
                for _ in 0..self.members.len() {
                    if self.world.pending() == 0 {
                        break;
                    }
                    self.deliver_one();
                }
            }
            Act::Heartbeat => {
                let log = self.pick_log();
                self.heartbeat(log);
            }
            Act::Propose => self.propose(),
            Act::Barriers => {
                let at = self.pick_member();
                self.barriers(at);
            }
            Act::Drop => {
                if self.world.pending() > 0 {
                    self.drop_one();
                }
            }
            Act::Duplicate => {
                if self.world.pending() > 0 {
                    self.duplicate_one();
                }
            }
            Act::TimeOut => {
                let (at, log) = (self.pick_member(), self.pick_log());
                let leader_known = self.members[at as usize - 1]
                    .multi
                    .node(log)
                    .is_some_and(|node| node.raft.leader_id() != 0);
                if calm && leader_known {
                    return;
                }
                self.time_out(at, log);
            }
            Act::Tick => self.tick_all(),
            Act::Crash => {
                let at = self.pick_member();
                self.crash(at);
            }
            Act::Isolate => {
                let at = if self.isolated.is_none() {
                    Some(self.pick_member())
                } else {
                    None
                };
                self.isolate(at);
            }
            Act::Image => {
                let at = self.pick_member();
                if self.mutant.is_none() {
                    self.members[at as usize - 1].image_at_next_global = true;
                }
            }
            Act::Resize => {
                if self.resize {
                    let at = self.pick_member();
                    self.propose_resize(at);
                }
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
            let generation = member.multi.generation(log).unwrap();
            if raft.state() == StateRole::Leader {
                let previous = *self
                    .leaders
                    .entry((log, generation, raft.term()))
                    .or_insert(id);
                assert_eq!(
                    previous,
                    id,
                    "{at}: two leaders of term {} in log {log}",
                    raft.term()
                );
                if self.leading.insert((id, log, generation)) {
                    self.counters.elections_won += 1;
                }
            } else {
                self.leading.remove(&(id, log, generation));
            }
            let disk = &member.multi.node(log).unwrap().store().0;
            let commit = raft.log().committed().min(disk.last_index());
            // What a committed entry states is one at each index (state machine safety). Its term
            // may differ between members: a later leader's election takes an entry the fast track
            // committed again under its own term, and a member that committed it keeps the stamp
            // it had (`docs/models/README.md`, `LogMatching`; `docs/raft.md` §3.5).
            for entry in disk.entries.iter().filter(|entry| entry.index <= commit) {
                let first = self
                    .committed
                    .entry((log, generation, entry.index))
                    .or_insert_with(|| (entry.entry_type, entry.data.clone()));
                assert_eq!(
                    first,
                    &(entry.entry_type, entry.data.clone()),
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
        let keys = match (&mut self.mutant, id) {
            (Some((merge, app)), 1) => {
                merge
                    .advance(&Unbarriered(member), u64::MAX, &mut |applied| {
                        if let Applied::Command(command) = applied {
                            app.apply(&command);
                        }
                        Flow::Continue
                    })
                    .unwrap();
                app.keys.clone()
            }
            _ => member.app.keys.clone(),
        };
        let checked = &mut self.checked[id as usize - 1];
        for (key, history) in &keys {
            let reference = self.reference.entry(*key).or_default();
            let from = checked.get(key).copied().unwrap_or(0).min(history.len());
            for (position, applied) in history.iter().enumerate().skip(from) {
                match reference.get(position) {
                    Some(seen) => {
                        assert_eq!(
                            seen, applied,
                            "{at}: member {id} applied a different command or epoch for key {key:?} at its {position}th"
                        );
                        if restarted {
                            self.counters.replays_matched += 1;
                        }
                    }
                    None => reference.push(applied.clone()),
                }
            }
            checked.insert(*key, history.len());
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
            counters.images_taken += member.counts.images + folded.images;
            counters.images_installed += member.counts.installed + folded.installed;
            counters.resizes += member.counts.resizes + folded.resizes;
            counters.misrouted += member.counts.misrouted + folded.misrouted;
        }
        counters.overflowed = self.net.stats().dropped_capacity;
        counters.least_log_applied = (0..self.logs)
            .map(|log| {
                self.members
                    .iter()
                    .map(|member| member.app.logs.get(&(log as u64)).map_or(0, Vec::len) as u64)
                    .sum::<u64>()
            })
            .min()
            .unwrap_or(0);
        counters.fast_committed =
            self.fast_folded + self.members.iter().map(fast_committed).sum::<u64>();
        counters
    }
}

/// What a member's leaders committed by a fast quorum, all its logs told.
fn fast_committed(member: &Member) -> u64 {
    (0..member.logs)
        .map(|log| member.multi.node(log).unwrap().raft.fast_stats().committed)
        .sum()
}

/// One seeded history of `voters` voters holding `logs` logs each, on the fast track where
/// `fast`; its record and its counters.
fn explore_one(
    source: Source,
    voters: u64,
    logs: usize,
    shape: Shape,
    seed: u64,
) -> (SimRecord, Counters) {
    let mut cluster = Cluster::new(source, voters, logs, shape.fast, shape.resize, shape.scale);
    for step in 0..shape.scale.steps {
        let calm = (step / shape.scale.stretch) % 2 == 1;
        cluster.step(calm);
        cluster.check(&format!("seed {seed} step {step}"));
    }
    let counters = cluster.totals();
    (cluster.world.finish(), counters)
}

/// Explores `seeds` histories of `voters` voters holding `logs` logs each, the first through the
/// run-twice check (`docs/sim.md` §3.9); what they counted.
/// What a campaign's groups do beside the classic track: the fast track, resizes.
#[derive(Clone, Copy, Debug)]
struct Shape {
    fast: bool,
    resize: bool,
    scale: Scale,
}

fn explore(voters: u64, logs: usize, shape: Shape, seeds: u64) -> Counters {
    let mut total = Counters::default();
    let base = (voters << 32)
        ^ ((logs as u64) << 40)
        ^ (u64::from(shape.fast) << 48)
        ^ (u64::from(shape.resize) << 49);
    let mut first = None;
    twice(base, |source| {
        let (record, counters) = explore_one(source, voters, logs, shape, 0);
        first = Some(counters);
        Ok::<_, String>(record)
    })
    .unwrap_or_else(|refusal| panic!("{voters} voters, {logs} logs: {refusal}"));
    total.add(&first.unwrap());
    for seed in 1..seeds {
        let (_, counters) = explore_one(Source::Seed(base ^ seed), voters, logs, shape, seed);
        total.add(&counters);
    }
    total
}

/// How often a run must reach a path (`docs/sim.md` §4.4): a common path more than once a seed,
/// a rare one at least once a campaign. Which paths are common was read off the counts measured
/// at both scales (`docs/benchmarks.md`, "The multilog explorer").
#[derive(Clone, Copy, Debug)]
enum Floor {
    Common,
    Rare,
}

/// Every named path the explorer claims, with its floor; on the fast track, its own two besides.
fn coverage(counted: &Counters, shape: Shape) -> Vec<(&'static str, u64, Floor)> {
    let mut paths = vec![
        ("elections won", counted.elections_won, Floor::Common),
        (
            "keyed commands applied",
            counted.keyed_applied,
            Floor::Common,
        ),
        (
            "global commands applied",
            counted.globals_applied,
            Floor::Common,
        ),
        (
            "barriers proposed",
            counted.barriers_proposed,
            Floor::Common,
        ),
        (
            "members crashed and restarted",
            counted.crashes,
            Floor::Common,
        ),
        (
            "restarted members' replays matched",
            counted.replays_matched,
            Floor::Common,
        ),
        (
            "commands applied from the least used log",
            counted.least_log_applied,
            Floor::Common,
        ),
        ("messages dropped", counted.dropped, Floor::Common),
        ("messages duplicated", counted.duplicated, Floor::Common),
        (
            "proposals refused",
            counted.proposals_refused,
            Floor::Common,
        ),
        (
            "images taken and every log compacted",
            counted.images_taken,
            Floor::Common,
        ),
        (
            "images installed from a log's snapshot",
            counted.images_installed,
            Floor::Rare,
        ),
    ];
    if shape.resize {
        paths.push(("resizes applied", counted.resizes, Floor::Common));
        paths.push((
            "keyed commands refused as a resize moved their key",
            counted.misrouted,
            Floor::Common,
        ));
    }
    if shape.fast {
        paths.push((
            "indexes committed by a fast quorum",
            counted.fast_committed,
            Floor::Rare,
        ));
        paths.push((
            "fast proposals displaced and proposed again",
            counted.displaced,
            Floor::Common,
        ));
    }
    paths
}

/// Explores three voters with three logs and five with two, and holds every floor.
fn explore_and_check_coverage(seeds: u64) {
    if let Err(fault) = coverage_holds(SCALE, seeds, true) {
        panic!("{fault}");
    }
}

/// Explores every shape at `scale` over `seeds` and holds every path to its floor: the floor
/// missed, if one is.
fn coverage_holds(scale: Scale, seeds: u64, print: bool) -> Result<(), String> {
    let classic = Shape {
        fast: false,
        resize: false,
        scale,
    };
    for (voters, logs, shape) in [(3, 3), (5, 2)].into_iter().flat_map(|(voters, logs)| {
        [
            classic,
            Shape {
                fast: true,
                ..classic
            },
            Shape {
                resize: true,
                ..classic
            },
        ]
        .map(move |shape| (voters, logs, shape))
    }) {
        let counted = explore(voters, logs, shape, seeds);
        if print {
            eprintln!(
                "explored {voters} voters x {logs} logs x {seeds} seeds, {shape:?}: {counted:?}"
            );
        }
        for (path, count, floor) in coverage(&counted, shape) {
            let least = match floor {
                Floor::Common => seeds + 1,
                Floor::Rare => 1,
            };
            if count < least {
                return Err(format!(
                    "{voters} voters, {logs} logs, fast {}, resize {}: {path}: {count} over {seeds} seeds, below its floor {least}",
                    shape.fast, shape.resize
                ));
            }
        }
    }
    Ok(())
}

/// Whether the planted defect is caught at `seed` and `scale`: member 1's merge passes barriers.
fn mutant_caught(scale: Scale, seed: u64) -> bool {
    std::panic::catch_unwind(|| {
        let mut cluster = Cluster::new(Source::Seed(seed), 3, 3, false, false, scale);
        cluster.mutant = Some((Merge::new(3).unwrap(), support::App::default()));
        for step in 0..scale.steps {
            let calm = (step / scale.stretch) % 2 == 1;
            cluster.step(calm);
            cluster.check(&format!("seed {seed} step {step}"));
        }
    })
    .is_err()
}

/// slates (§3.6, T-8.13's counterpart for MLRaft): "the merge applies every key's commands in one
/// order and each in one epoch, and the global commands in one order, on every node and across
/// every restart, under loss, duplication, reordering, partitions and crash-restarts; each log keeps
/// Raft's own safety." Over [`WILKS_SEEDS`] seeds of every shape: no history of them fails, so at
/// least 95% of histories at this scale keep it, with 95% confidence. The planted defect is caught
/// at every one of the same seeds (`measure_the_scale`), so its per-seed catch rate needs no more.
#[test]
fn the_multi_log_merges_alike_under_an_adversarial_network() {
    explore_and_check_coverage(WILKS_SEEDS);
}

/// Member 1's logs as the planted defect reads them: every barrier passed as nothing, so its merge
/// applies what follows a barrier without waiting for the global it names.
struct Unbarriered<'a>(&'a Member);

impl Logs for Unbarriered<'_> {
    fn count(&self) -> usize {
        self.0.logs
    }
    fn through(&self, log: usize) -> u64 {
        self.0.multi.node(log).unwrap().given_to_apply()
    }
    fn walk(
        &self,
        log: usize,
        from: u64,
        through: u64,
        visit: &mut dyn FnMut(&Entry) -> bool,
    ) -> Result<(), StorageError> {
        let store = self.0.multi.node(log).unwrap().store();
        hyper_raft::Storage::any_entry(store, from, through + 1, &mut |entry| {
            if matches!(entry::read(entry), entry::Stated::Barrier(_)) {
                visit(&Entry {
                    index: entry.index,
                    term: entry.term,
                    ..Entry::default()
                })
            } else {
                visit(entry)
            }
        })
        .map(|_| ())
    }
}

/// The planted defect (`docs/sim.md` §4.6; slates' mutation): a member whose merge does not wait
/// at barriers. The explorer's history check catches it within the workspace's seeds.
#[test]
fn a_member_that_does_not_wait_at_barriers_is_caught() {
    let caught = std::panic::catch_unwind(|| {
        for seed in 0..WILKS_SEEDS {
            let mut cluster = Cluster::new(Source::Seed(seed), 3, 3, false, false, SCALE);
            cluster.mutant = Some((Merge::new(3).unwrap(), support::App::default()));
            for step in 0..SCALE.steps {
                let calm = (step / SCALE.stretch) % 2 == 1;
                cluster.step(calm);
                cluster.check(&format!("seed {seed} step {step}"));
            }
        }
    });
    let message = caught.expect_err("the planted defect went uncaught");
    let said = message
        .downcast_ref::<String>()
        .cloned()
        .unwrap_or_default();
    eprintln!("caught: {said}");
    assert!(said.contains("a different command or epoch"), "{said}");
}

/// Each scale's number measured (`docs/multilog.md` §11): the planted defect's catch rate, the most
/// messages in flight, and each number swept down to the least at which every path holds its floor
/// and the planted defect is caught, the others held. Run in release with `--ignored --nocapture`.
#[test]
#[ignore = "a measurement, in release"]
fn measure_the_scale() {
    std::panic::set_hook(Box::new(|_| {}));
    let seeds = WILKS_SEEDS;
    let caught = (0..WILKS_SEEDS)
        .filter(|seed| mutant_caught(SCALE, *seed))
        .count();
    println!("the planted defect: caught at {caught} of {WILKS_SEEDS} seeds");

    let holds = |scale: Scale| {
        // A history whose check fails at a scale is no pass, and is said: a scale that finds a
        // defect is no smaller scale, it is a finding.
        match std::panic::catch_unwind(|| coverage_holds(scale, seeds, false)) {
            Ok(Ok(())) => (0..seeds).any(|seed| mutant_caught(scale, seed)),
            Ok(Err(floor)) => {
                println!("  {floor}");
                false
            }
            Err(panic) => {
                let said = panic
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).to_owned()))
                    .unwrap_or_default();
                println!("  a history failed at {scale:?}: {said}");
                false
            }
        }
    };
    assert!(holds(SCALE), "the scale itself holds");
    type Set = fn(&mut Scale, u64);
    let numbers: [(&str, u64, Set); 17] = [
        ("steps", SCALE.steps as u64, |s, v| s.steps = v as usize),
        ("stretch", SCALE.stretch as u64, |s, v| {
            s.stretch = v as usize
        }),
        ("proposals", SCALE.proposals, |s, v| s.proposals = v),
        ("keys", SCALE.keys, |s, v| s.keys = v),
        ("global_every", SCALE.global_every, |s, v| {
            s.global_every = v
        }),
        ("deliver", SCALE.weights.deliver, |s, v| {
            s.weights.deliver = v
        }),
        ("heartbeat", SCALE.weights.heartbeat, |s, v| {
            s.weights.heartbeat = v
        }),
        ("propose", SCALE.weights.propose, |s, v| {
            s.weights.propose = v
        }),
        ("barriers", SCALE.weights.barriers, |s, v| {
            s.weights.barriers = v
        }),
        ("drop", SCALE.weights.drop, |s, v| s.weights.drop = v),
        ("duplicate", SCALE.weights.duplicate, |s, v| {
            s.weights.duplicate = v
        }),
        ("time_out", SCALE.weights.time_out, |s, v| {
            s.weights.time_out = v
        }),
        ("tick", SCALE.weights.tick, |s, v| s.weights.tick = v),
        ("crash", SCALE.weights.crash, |s, v| s.weights.crash = v),
        ("isolate", SCALE.weights.isolate, |s, v| {
            s.weights.isolate = v
        }),
        ("image", SCALE.weights.image, |s, v| s.weights.image = v),
        ("resize", SCALE.weights.resize, |s, v| s.weights.resize = v),
    ];
    // In the order listed, each number goes down to the least at which the criterion holds with
    // every number before it already at its own least: the scale found holds by construction.
    let mut found = SCALE;
    for (name, at, set) in numbers {
        // By bisection between a failing low and a passing high: the criterion is taken to hold
        // from its boundary up, which the run confirms at the boundary.
        let mut one = found;
        set(&mut one, 1);
        if holds(one) {
            found = one;
            println!("{name}: holds down to 1 (from {at})");
            continue;
        }
        let (mut low, mut high) = (1u64, at);
        while high - low > 1 {
            let middle = low + (high - low) / 2;
            let mut scale = found;
            set(&mut scale, middle);
            if holds(scale) {
                high = middle;
            } else {
                low = middle;
            }
        }
        set(&mut found, high);
        println!("{name}: boundary {high} (fails at {low}; from {at})");
    }
    println!("the scale found: {found:?}");
    let roomy = Scale {
        in_flight: 1 << 16,
        ..found
    };
    let mut most = 0;
    for (fast, resize) in [(false, false), (true, false), (false, true)] {
        for (voters, logs) in [(3, 3), (5, 2)] {
            let counted = explore(
                voters,
                logs,
                Shape {
                    fast,
                    resize,
                    scale: roomy,
                },
                seeds,
            );
            most = most.max(counted.most_in_flight);
        }
    }
    println!("most in flight at the scale found: {most}");
}

/// The seeds a measurement of the scale runs: Wilks's 59, the least n with 1 - 0.95^n >= 0.95
/// (one-sided 95/95).
const WILKS_SEEDS: u64 = 59;
