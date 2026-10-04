//! slates' MLRaft on the failure path (slates `crates/cluster/tests/multilog_timed.rs` at
//! `5cce86a`), retargeted at this crate (`docs/multilog.md` §11 step 3): `n`-log groups across the
//! five published Azure regions, on hyper-sim's world and network (`docs/sim.md` S-1, S-2), driven
//! as this core's owners drive a group: each log's members elect by suspicion (timing step L-2),
//! their owner's detectors fed by one heartbeat stream for each node pair, shared by every log
//! (hyper-liveness's rule, `docs/timing.md` §2.8); each log's preferred voter (ranked by its
//! published quorum round trip) favoured by priority and handed leadership once it holds the
//! leader's log (`docs/multilog.md` §7). A keyed stream and a global stream of commands are proposed
//! at the leader of the log each routes to; a command's latency runs to its application at the
//! member that proposed it.
//!
//! **Checks.** slates' gate decided by measured margins (a gap four times another, a median twice
//! another, an expectation compared). The owner's rule is exact checks only, so each of slates'
//! claims is held as the exact fact of the mechanism it stood for, for every command of the run:
//! - "keyed commands kept flowing": with one log, no keyed command proposed after its leader's
//!   crash is applied before the log has a leader again; with five, keyed commands of the other
//!   logs proposed after log 0's leader's crash are applied before log 0 has one;
//! - "a crash of any other log's leader stalls every log's keyed commands": while log `k` has no
//!   leader, no command is applied anywhere in an epoch at or past a global proposed after the
//!   crash, the global waiting for log `k`'s barrier, and every log's commands behind theirs;
//! - "global commands wait for the barriers" and "five logs' messages": what slates measured is
//!   measured here and recorded beside slates' numbers (`docs/benchmarks.md`), not decided by a
//!   margin.
//!
//! The measurement tool (`multi_log_on_the_failure_path`, ignored) runs slates' full shape.
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

use std::collections::BTreeMap;
use std::time::Duration;

use hyper_multilog::{Limits, Route};
use hyper_raft::Timing;
use hyper_raft::proto::Message;
use hyper_raft::wire::Record;
use hyper_sim::net::{Net, NetLimits, Path, Ticket};
use hyper_sim::{
    Clock, Discipline, NodeId, Random, Record as SimRecord, Source, Step, World, twice,
};
use support::Member;

/// A millisecond, in the world's nanoseconds.
const MS: u64 = 1_000_000;
/// Format: five Azure regions, in the order of [`ROUND_TRIPS_MS`] (slates `support/azure.rs`).
const REGIONS: usize = 5;
/// Microsoft's published P50 round trips among the five regions (East US, West Europe, Japan East,
/// Southeast Asia, Brazil South), in milliseconds, source row to destination column ("Azure
/// network round-trip latency statistics", learn.microsoft.com/en-us/azure/networking/azure-network-latency,
/// page dated 2026-07-30, as slates read it on 2026-09-28; directional).
const ROUND_TRIPS_MS: [[u64; REGIONS]; REGIONS] = [
    [0, 83, 162, 224, 117],
    [85, 0, 233, 169, 185],
    [162, 234, 0, 72, 262],
    [224, 169, 72, 0, 330],
    [118, 185, 262, 331, 0],
];
/// Shape: the jitter on each one-way delay, uniform below it (slates').
const JITTER_NS: u64 = 5 * MS;
/// Shape: the period of each node pair's heartbeat stream: slates' daemon's `HEARTBEAT_NS`, the
/// period its timed simulation ticks at.
const PERIOD_NS: u64 = 100 * MS;
/// Shape: when the streams begin, after the first elections and hand-offs (slates').
const STREAM_FROM_NS: u64 = 10_000 * MS;
/// Shape: the keyed stream, twenty a second over 64 keys, and the global stream, two a second, the
/// rate of slates' groups' membership changes under churn (slates').
const KEYED_EVERY_NS: u64 = 50 * MS;
const KEYS: u64 = 64;
const GLOBAL_EVERY_NS: u64 = 500 * MS;
/// Shape: the seeds and the run the gate takes: the first elections, a crash of a log's preferred
/// voter from 20 s to 30 s, and 5 s after (slates').
const GATE_SEEDS: u64 = 2;
const GATE_DURATION_NS: u64 = 35_000 * MS;
const GATE_CRASH_NS: (u64, u64) = (20_000 * MS, 30_000 * MS);
/// The timer granularity the election law resolves its span to: the world's clocks are exact, so
/// a microsecond, finer than any span here.
const GRANULARITY: Duration = Duration::from_micros(1);
/// A bound on what a log holds past its merge that the runs never reach: the keyed stream's
/// commands over a run's whole length.
const LIMITS: Limits = Limits {
    unmerged: GATE_DURATION_NS * 2 / KEYED_EVERY_NS,
};
/// The world steps a run may take: four times the most any run of the measurement tool took
/// (89,020 steps, 70 s at five logs over 20 seeds, measured 2026-10-04), so a run that takes many
/// more has stopped converging.
const STEPS: u64 = 4 * 89_020;

/// The one-way delay from region `from` to region `to`: half the published round trip from
/// `from`, and the jitter's mean on top (slates draws `U[0, jitter)` on top; hyper-sim's path draws
/// `one_way ± jitter`, so the path is centred on its half).
fn path(from: usize, to: usize) -> Path {
    Path::reordering(
        ROUND_TRIPS_MS[from][to] * MS / 2 + JITTER_NS / 2,
        JITTER_NS / 2,
    )
}

/// The latest a message from `from` to `to` arrives after it is sent.
fn latest_ns(from: usize, to: usize) -> u64 {
    ROUND_TRIPS_MS[from][to] * MS / 2 + JITTER_NS
}

/// Which member sits in each region under `seed`: a permutation of `1..=5` (slates' `placement`,
/// Fisher–Yates over splitmix64), so that no region wins every first election.
fn placement(seed: u64) -> Vec<u64> {
    let mut hosts: Vec<u64> = (1..=REGIONS as u64).collect();
    let mut state = seed;
    for index in (1..REGIONS).rev() {
        state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut mixed = state;
        mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        mixed ^= mixed >> 31;
        let pick = (mixed % (index as u64 + 1)) as usize;
        hosts.swap(index, pick);
    }
    hosts
}

/// The published quorum round trip of region `node`: the ⌊n/2⌋-th smallest from it (slates').
fn quorum_round_trip_ms(node: usize) -> u64 {
    let mut trips: Vec<u64> = (0..REGIONS)
        .filter(|other| *other != node)
        .map(|other| ROUND_TRIPS_MS[node][other])
        .collect();
    trips.sort_unstable();
    trips[REGIONS / 2 - 1]
}

/// The timing hyper-timing's law gives the member in region `region`: the split-vote span over the
/// latest one-way delay of its paths and its quorum's round, for all but a lost leader's voters.
fn timing_of(region: usize) -> Timing {
    let latency = (0..REGIONS)
        .filter(|o| *o != region)
        .map(|o| latest_ns(region, o))
        .max()
        .unwrap();
    let round = Duration::from_nanos(quorum_round_trip_ms(region) * MS + 2 * JITTER_NS);
    let span = hyper_timing::election_span(
        REGIONS as u32,
        REGIONS as u32 - 1,
        Duration::from_nanos(latency),
        round,
        GRANULARITY,
    )
    .expect("a span");
    Timing {
        span: span.span,
        round,
        election: span.election,
    }
}

/// A crash of the voter `log` prefers over `[from_ns, until_ns)`, and its restart from what it made
/// durable.
#[derive(Clone, Copy, Debug)]
struct Crash {
    from_ns: u64,
    until_ns: u64,
    log: usize,
}

#[derive(Clone, Copy, Debug)]
struct Shape {
    logs: usize,
    seed: u64,
    duration_ns: u64,
    crash: Option<Crash>,
}

/// What the network carries: a log's Raft message, or a node pair's heartbeat (its send time).
#[derive(Clone, Debug)]
enum Payload {
    Raft(usize, Message),
    Beat(u64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ev {
    Arrive(Ticket),
    /// A member's period: it beats to every peer.
    Beat,
    /// A member's detector's freshness point for a peer: suspected if no later heartbeat came.
    Fresh {
        peer: u64,
        sent: u64,
    },
    /// The streams' next proposals are due.
    Keyed,
    Global,
    /// The crash window opens or closes.
    Crash,
    Restart,
}

/// A command proposed: when, by whom, into which log, keyed or not; and, once applied at its
/// proposer, when.
#[derive(Clone, Copy, Debug)]
struct Proposed {
    at_ns: u64,
    by: u64,
    log: usize,
    keyed: bool,
    applied_ns: Option<u64>,
}

/// What a run measured: each keyed and global command's latency, the longest time either stream
/// went without an application (the keyed stream's also for the watched log alone), the messages
/// sent, and what the exact checks need: every application's time, log, keyed-ness and epoch.
#[derive(Default, Debug)]
struct Outcome {
    keyed_ns: Vec<u64>,
    global_ns: Vec<u64>,
    keyed_gap_ns: u64,
    watched_keyed_gap_ns: u64,
    global_gap_ns: u64,
    messages: u64,
    heartbeats: u64,
    /// Every application: when, its log, keyed or not, its epoch, and when it was proposed.
    applications: Vec<(u64, usize, bool, u64, u64)>,
    /// When the crashed voter's log had a leader again, and whether the voter led it at the crash.
    led_again_ns: Option<u64>,
    led_at_crash: bool,
    /// The log-0 index of the first global proposed after the crash.
    first_global_after_crash: Option<u64>,
}

struct Sim {
    shape: Shape,
    world: World<Ev>,
    net: Net<Payload>,
    members: Vec<Member>,
    /// Region of each member.
    region: Vec<usize>,
    ranked: Vec<u64>,
    /// Per member and peer: the send time of the latest heartbeat heard, and whether suspected.
    heard: Vec<Vec<(u64, bool)>>,
    down: Vec<bool>,
    waiting: Vec<(Route, u64, u64)>,
    commands: BTreeMap<u64, Proposed>,
    next_command: u64,
    seen: Vec<BTreeMap<u64, usize>>,
    last_keyed_ns: u64,
    last_watched_ns: u64,
    last_global_ns: u64,
    outcome: Outcome,
    keys: hyper_sim::StreamId,
}

impl Sim {
    fn new(source: Source, shape: Shape) -> Self {
        let limits = hyper_sim::Limits {
            events: 1 << 16,
            nodes: REGIONS + 1,
            streams: 256,
            steps: STEPS,
            trace_words: STEPS as usize,
        };
        let mut world = World::new(source, Discipline::Ordered, limits).unwrap();
        for _ in 0..=REGIONS {
            world.node(Clock::default()).unwrap();
        }
        let hosts = placement(shape.seed);
        let mut region = vec![0; REGIONS];
        for (r, host) in hosts.iter().enumerate() {
            region[*host as usize - 1] = r;
        }
        let mut order: Vec<usize> = (0..REGIONS).collect();
        order.sort_by_key(|r| (quorum_round_trip_ms(*r), *r));
        let ranked: Vec<u64> = order.iter().map(|r| hosts[*r]).collect();
        let mut net = Net::new(NetLimits {
            flows: REGIONS * REGIONS,
            links: 0,
            nats: 0,
            link_messages: 0,
            messages: 1 << 16,
            bytes: 1 << 30,
        });
        for from in 1..=REGIONS as u64 {
            for to in 1..=REGIONS as u64 {
                if from != to {
                    let p = path(region[from as usize - 1], region[to as usize - 1]);
                    net.set_pair_path(NodeId(from as u32), NodeId(to as u32), p)
                        .unwrap();
                }
            }
        }
        let ids: Vec<u64> = (1..=REGIONS as u64).collect();
        let members = ids
            .iter()
            .map(|id| Member::open(*id, &ids, shape.logs, shape.seed ^ (id << 20), LIMITS, true))
            .collect();
        let keys = world.stream("timed.keys", &[]).unwrap();
        let mut sim = Self {
            shape,
            world,
            net,
            members,
            region,
            ranked,
            heard: vec![vec![(0, false); REGIONS + 1]; REGIONS + 1],
            down: vec![false; REGIONS + 1],
            waiting: Vec::new(),
            commands: BTreeMap::new(),
            next_command: 0,
            seen: vec![BTreeMap::new(); REGIONS + 1],
            last_keyed_ns: STREAM_FROM_NS,
            last_watched_ns: STREAM_FROM_NS,
            last_global_ns: STREAM_FROM_NS,
            outcome: Outcome::default(),
            keys,
        };
        for id in 1..=REGIONS as u64 {
            sim.prepare(id);
            // Each member's period has a phase of its own.
            let phase = sim.world.below(sim.keys, PERIOD_NS).unwrap();
            sim.world.after(phase, NodeId(id as u32), Ev::Beat).unwrap();
        }
        sim.world
            .schedule(STREAM_FROM_NS, NodeId(0), Ev::Keyed)
            .unwrap();
        sim.world
            .schedule(STREAM_FROM_NS, NodeId(0), Ev::Global)
            .unwrap();
        if let Some(crash) = shape.crash {
            sim.world
                .schedule(crash.from_ns, NodeId(0), Ev::Crash)
                .unwrap();
            sim.world
                .schedule(crash.until_ns, NodeId(0), Ev::Restart)
                .unwrap();
        }
        sim
    }

    /// A member's timing and priorities, as its owner sets them at opening.
    fn prepare(&mut self, id: u64) {
        let timing = timing_of(self.region[id as usize - 1]);
        let member = &mut self.members[id as usize - 1];
        for log in 0..member.logs {
            member
                .multi
                .node_mut(log)
                .unwrap()
                .set_timing(timing)
                .unwrap();
        }
        member.multi.spread(&self.ranked).unwrap();
    }

    fn now(&self) -> u64 {
        self.world.now()
    }

    /// After a call on member `id`: its logs woken at the clock, settled, what they send sent, its
    /// timer armed at their earliest deadline, its applications measured.
    fn settle(&mut self, id: u64) {
        let now = self.now();
        let member = &mut self.members[id as usize - 1];
        for log in 0..member.logs {
            let _ = member.multi.node_mut(log).unwrap().wake(now);
        }
        let mut out = Vec::new();
        member.settle(&mut out);
        let deadline = (0..member.logs)
            .filter_map(|log| member.multi.node(log).unwrap().deadline())
            .min();
        self.world
            .wake(NodeId(id as u32), deadline.map(|d| d.max(now)))
            .unwrap();
        for (log, message) in out {
            let to = message.to;
            let bytes = message.encoded_len();
            self.outcome.messages += 1;
            self.net
                .send(
                    &mut self.world,
                    (NodeId(id as u32), NodeId(to as u32)),
                    Payload::Raft(log, message),
                    bytes,
                    Ev::Arrive,
                )
                .unwrap();
        }
        self.measure(id);
    }

    /// The commands member `id` applied since last looked at, timed at their proposer.
    fn measure(&mut self, id: u64) {
        let now = self.now();
        let watched = self.shape.crash.map_or(0, |crash| crash.log);
        let member = &self.members[id as usize - 1];
        let mut fresh = Vec::new();
        for (log, applied) in &member.app.logs {
            let seen = self.seen[id as usize].entry(*log).or_insert(0);
            for (_, data) in applied.iter().skip(*seen) {
                fresh.push(u64::from_le_bytes(data[..8].try_into().unwrap()));
            }
            *seen = applied.len();
        }
        for command in fresh {
            let Some(proposed) = self.commands.get_mut(&command) else {
                continue;
            };
            let epoch = epoch_of(member, command);
            self.outcome.applications.push((
                now,
                proposed.log,
                proposed.keyed,
                epoch,
                proposed.at_ns,
            ));
            if proposed.by != id || proposed.applied_ns.is_some() {
                continue;
            }
            proposed.applied_ns = Some(now);
            let latency = now - proposed.at_ns;
            if proposed.keyed {
                self.outcome.keyed_ns.push(latency);
                self.outcome.keyed_gap_ns = self
                    .outcome
                    .keyed_gap_ns
                    .max(now.saturating_sub(self.last_keyed_ns));
                self.last_keyed_ns = self.last_keyed_ns.max(now);
                if proposed.log == watched {
                    self.outcome.watched_keyed_gap_ns = self
                        .outcome
                        .watched_keyed_gap_ns
                        .max(now.saturating_sub(self.last_watched_ns));
                    self.last_watched_ns = self.last_watched_ns.max(now);
                }
            } else {
                self.outcome.global_ns.push(latency);
                self.outcome.global_gap_ns = self
                    .outcome
                    .global_gap_ns
                    .max(now.saturating_sub(self.last_global_ns));
                self.last_global_ns = self.last_global_ns.max(now);
            }
        }
    }

    /// Member `id`'s period: a heartbeat to every peer on the pair's stream, and the hand-offs its
    /// logs offer.
    fn beat(&mut self, id: u64) {
        self.world
            .after(PERIOD_NS, NodeId(id as u32), Ev::Beat)
            .unwrap();
        if self.down[id as usize] {
            return;
        }
        let now = self.now();
        for peer in 1..=REGIONS as u64 {
            if peer != id {
                self.outcome.heartbeats += 1;
                self.net
                    .send(
                        &mut self.world,
                        (NodeId(id as u32), NodeId(peer as u32)),
                        Payload::Beat(now),
                        16,
                        Ev::Arrive,
                    )
                    .unwrap();
            }
        }
        let member = &mut self.members[id as usize - 1];
        for log in 0..member.logs {
            if let Some(to) = member.multi.hand_off(log) {
                let _ = member.multi.node_mut(log).unwrap().transfer_leader(to);
            }
        }
        self.settle(id);
    }

    /// A heartbeat from `peer`, sent at `sent`, reaches `id`: its detector trusts the peer again,
    /// and suspects it at the freshness point of the next one (the latest the next can arrive).
    fn heartbeat(&mut self, id: u64, peer: u64, sent: u64) {
        // Held as the send time and one, so that a heartbeat sent at zero is newer than none.
        let (latest, suspected) = self.heard[id as usize][peer as usize];
        if sent < latest {
            return;
        }
        self.heard[id as usize][peer as usize] = (sent + 1, false);
        if suspected {
            let member = &mut self.members[id as usize - 1];
            for log in 0..member.logs {
                let _ = member.multi.node_mut(log).unwrap().trust(peer);
            }
        }
        let fresh = sent
            + PERIOD_NS
            + latest_ns(self.region[peer as usize - 1], self.region[id as usize - 1]);
        self.world
            .schedule(
                fresh.max(self.now()),
                NodeId(id as u32),
                Ev::Fresh { peer, sent },
            )
            .unwrap();
        self.settle(id);
    }

    fn fresh(&mut self, id: u64, peer: u64, sent: u64) {
        let (latest, suspected) = self.heard[id as usize][peer as usize];
        if latest != sent + 1 || suspected || self.down[id as usize] {
            return;
        }
        self.heard[id as usize][peer as usize].1 = true;
        let member = &mut self.members[id as usize - 1];
        for log in 0..member.logs {
            let _ = member.multi.node_mut(log).unwrap().suspect(peer);
        }
        self.settle(id);
    }

    /// The streams' proposals due, and those waiting, at the leader of the log each routes to.
    fn propose_due(&mut self) {
        let waiting = std::mem::take(&mut self.waiting);
        for (route, at_ns, command) in waiting {
            let log = self.members[0].multi.route(route);
            let leader = (1..=REGIONS as u64)
                .find(|id| !self.down[*id as usize] && self.members[*id as usize - 1].leads(log));
            let Some(leader) = leader else {
                self.waiting.push((route, at_ns, command));
                continue;
            };
            let member = &mut self.members[leader as usize - 1];
            let index = member
                .multi
                .node(log)
                .unwrap()
                .raft
                .log()
                .last_index()
                .unwrap()
                + 1;
            if member
                .multi
                .propose(route, command.to_le_bytes().to_vec())
                .is_err()
            {
                self.waiting.push((route, at_ns, command));
                continue;
            }
            let keyed = matches!(route, Route::Key(_));
            self.commands.insert(
                command,
                Proposed {
                    at_ns,
                    by: leader,
                    log,
                    keyed,
                    applied_ns: None,
                },
            );
            if !keyed
                && self.outcome.first_global_after_crash.is_none()
                && self.shape.crash.is_some_and(|crash| at_ns >= crash.from_ns)
            {
                self.outcome.first_global_after_crash = Some(index);
            }
            self.settle(leader);
        }
    }

    fn command(&mut self, route: Route) {
        self.next_command += 1;
        self.waiting.push((route, self.now(), self.next_command));
        self.propose_due();
    }

    fn crash(&mut self, crash: Crash) {
        let victim = self.ranked[crash.log % self.ranked.len()];
        self.outcome.led_at_crash = self.members[victim as usize - 1].leads(crash.log);
        self.down[victim as usize] = true;
        self.world.wake(NodeId(victim as u32), None).unwrap();
    }

    fn restart(&mut self, crash: Crash) {
        let victim = self.ranked[crash.log % self.ranked.len()];
        self.down[victim as usize] = false;
        self.members[victim as usize - 1].restart();
        self.seen[victim as usize].clear();
        self.prepare(victim);
        self.settle(victim);
    }

    /// Notes when the crashed voter's log has a leader again.
    fn watch_leadership(&mut self) {
        let Some(crash) = self.shape.crash else {
            return;
        };
        let now = self.now();
        if now < crash.from_ns || self.outcome.led_again_ns.is_some() {
            return;
        }
        let victim = self.ranked[crash.log % self.ranked.len()];
        let led = (1..=REGIONS as u64).any(|id| {
            id != victim
                && !self.down[id as usize]
                && self.members[id as usize - 1].leads(crash.log)
        });
        if led {
            self.outcome.led_again_ns = Some(now);
        }
    }

    fn run(mut self) -> (SimRecord, Outcome) {
        let mut strategy = Random;
        loop {
            match self.world.next(&mut strategy).unwrap() {
                Step::Event { node, event } => {
                    if self.now() >= self.shape.duration_ns {
                        break;
                    }
                    let id = u64::from(node.0);
                    match event {
                        Ev::Arrive(ticket) => {
                            let delivered = self
                                .net
                                .deliver(&mut self.world, ticket, Ev::Arrive)
                                .unwrap();
                            if let Some(delivery) = delivered
                                && !self.down[id as usize]
                            {
                                let from = u64::from(delivery.from.0);
                                match delivery.payload {
                                    Payload::Beat(sent) => self.heartbeat(id, from, sent),
                                    Payload::Raft(log, message) => {
                                        let _ =
                                            self.members[id as usize - 1].multi.step(log, message);
                                        self.settle(id);
                                    }
                                }
                            }
                        }
                        Ev::Beat => self.beat(id),
                        Ev::Fresh { peer, sent } => self.fresh(id, peer, sent),
                        Ev::Keyed => {
                            self.world
                                .after(KEYED_EVERY_NS, NodeId(0), Ev::Keyed)
                                .unwrap();
                            let key = self.world.below(self.keys, KEYS).unwrap();
                            self.command(Route::Key(key));
                        }
                        Ev::Global => {
                            self.world
                                .after(GLOBAL_EVERY_NS, NodeId(0), Ev::Global)
                                .unwrap();
                            self.command(Route::Global);
                        }
                        Ev::Crash => self.crash(self.shape.crash.unwrap()),
                        Ev::Restart => self.restart(self.shape.crash.unwrap()),
                    }
                }
                Step::Wake { node } => {
                    if self.now() >= self.shape.duration_ns {
                        break;
                    }
                    let id = u64::from(node.0);
                    if !self.down[id as usize] {
                        self.settle(id);
                        self.propose_due();
                    }
                }
                Step::Idle => break,
                Step::Spent => panic!("{:?}: the step budget is spent", self.shape),
            }
            self.watch_leadership();
        }
        let outcome = std::mem::take(&mut self.outcome);
        (self.world.finish(), outcome)
    }
}

/// The epoch member's application gave `command`.
fn epoch_of(member: &Member, command: u64) -> u64 {
    let bytes = command.to_le_bytes();
    member
        .app
        .keys
        .values()
        .flat_map(|history| history.iter())
        .find(|(data, _)| data.as_slice() == bytes)
        .map_or(0, |(_, epoch)| *epoch)
}

fn run_shape(source: Source, shape: Shape) -> (SimRecord, Outcome) {
    Sim::new(source, shape).run()
}

/// The median and 99th percentile, in milliseconds (zeros when empty).
fn percentiles(values: &mut [u64]) -> (u64, u64) {
    values.sort_unstable();
    let at = |p: usize| {
        values
            .get(p * (values.len().max(1) - 1) / 100)
            .copied()
            .unwrap_or(0)
            / MS
    };
    (at(50), at(99))
}

fn median(values: &mut [u64]) -> u64 {
    values.sort_unstable();
    values[values.len() / 2]
}

/// What a set of seeds measured for one shape (slates' `Measured`).
#[derive(Debug, PartialEq, Eq)]
struct Measured {
    keyed_ms: (u64, u64),
    global_ms: (u64, u64),
    keyed_gap_ms: u64,
    watched_keyed_gap_ms: u64,
    global_gap_ms: u64,
    messages: u64,
    heartbeats: u64,
}

/// Runs `seeds` seeds of one shape, the first through the run-twice check, holding each run to
/// `check`; what they measured.
fn measure(
    seeds: u64,
    logs: usize,
    duration_ns: u64,
    crash: Option<Crash>,
    check: &dyn Fn(&Shape, &Outcome),
) -> Measured {
    let (mut keyed, mut global, mut keyed_gap, mut watched, mut global_gap) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let (mut messages, mut heartbeats) = (0, 0);
    for seed in 0..seeds {
        let shape = Shape {
            logs,
            seed,
            duration_ns,
            crash,
        };
        let mut outcome = if seed == 0 {
            let mut first = None;
            twice(seed, |source| {
                let (record, outcome) = run_shape(source, shape);
                first = Some(outcome);
                Ok::<_, String>(record)
            })
            .unwrap_or_else(|refusal| panic!("{shape:?}: {refusal}"));
            first.unwrap()
        } else {
            run_shape(Source::Seed(seed), shape).1
        };
        check(&shape, &outcome);
        keyed.push(percentiles(&mut outcome.keyed_ns));
        global.push(percentiles(&mut outcome.global_ns));
        keyed_gap.push(outcome.keyed_gap_ns / MS);
        watched.push(outcome.watched_keyed_gap_ns / MS);
        global_gap.push(outcome.global_gap_ns / MS);
        messages += outcome.messages;
        heartbeats += outcome.heartbeats;
    }
    let pick = |pairs: &[(u64, u64)], which: fn(&(u64, u64)) -> u64| {
        let mut values: Vec<u64> = pairs.iter().map(which).collect();
        median(&mut values)
    };
    Measured {
        keyed_ms: (pick(&keyed, |p| p.0), pick(&keyed, |p| p.1)),
        global_ms: (pick(&global, |p| p.0), pick(&global, |p| p.1)),
        keyed_gap_ms: median(&mut keyed_gap),
        watched_keyed_gap_ms: median(&mut watched),
        global_gap_ms: median(&mut global_gap),
        messages,
        heartbeats,
    }
}

/// slates' `keyed_expectation`: steady and with each log's preferred voter crashed in turn, and a
/// keyed command's expectation, in milliseconds times the regions, when proposed as one of the
/// regions is lost, each alike: the mean pause a crash leaves its log's keyed commands plus
/// `regions − 1` steady medians.
fn keyed_expectation(
    seeds: u64,
    logs: usize,
    duration_ns: u64,
    crash_ns: (u64, u64),
    check: &dyn Fn(&Shape, &Outcome),
) -> (u64, Measured, Vec<Measured>) {
    let steady = measure(seeds, logs, duration_ns, None, check);
    let crashed: Vec<Measured> = (0..logs)
        .map(|log| {
            measure(
                seeds,
                logs,
                duration_ns,
                Some(Crash {
                    from_ns: crash_ns.0,
                    until_ns: crash_ns.1,
                    log,
                }),
                check,
            )
        })
        .collect();
    let pauses: u64 = crashed.iter().map(|run| run.watched_keyed_gap_ms).sum();
    let expectation = pauses / logs as u64 + (REGIONS as u64 - 1) * steady.keyed_ms.0;
    (expectation, steady, crashed)
}

/// The exact facts behind slates' claims, held for every command of a run.
fn exact(shape: &Shape, outcome: &Outcome) {
    let Some(crash) = shape.crash else {
        assert!(
            !outcome.keyed_ns.is_empty() && !outcome.global_ns.is_empty(),
            "{shape:?}: both streams applied"
        );
        return;
    };
    assert!(
        outcome.led_at_crash,
        "{shape:?}: the preferred voter led its log when it crashed"
    );
    let again = outcome
        .led_again_ns
        .unwrap_or_else(|| panic!("{shape:?}: the log never led again"));
    let leaderless = |t: u64| t >= crash.from_ns && t < again;
    // Commands proposed after the crash: what was committed before it may still reach a member
    // the crashed leader's last messages had not.
    let after = |t: u64, proposed: u64| leaderless(t) && proposed >= crash.from_ns;
    let keyed_elsewhere = outcome
        .applications
        .iter()
        .filter(|(t, log, keyed, _, proposed)| after(*t, *proposed) && *keyed && *log != crash.log)
        .count();
    let keyed_there = outcome
        .applications
        .iter()
        .filter(|(t, log, keyed, _, proposed)| after(*t, *proposed) && *keyed && *log == crash.log)
        .count();
    if shape.logs == 1 {
        assert_eq!(
            keyed_there, 0,
            "{shape:?}: a keyed command applied while the one log had no leader"
        );
    } else if crash.log == 0 {
        assert!(
            keyed_elsewhere > 0,
            "{shape:?}: keyed commands of the other logs kept flowing while log 0 had no leader"
        );
    } else {
        let stalled = outcome
            .first_global_after_crash
            .expect("a global proposed while the log had no leader");
        for (t, log, _, epoch, _) in &outcome.applications {
            assert!(
                !(leaderless(*t) && *epoch >= stalled),
                "{shape:?}: log {log} applied a command in epoch {epoch} at {t} before log {} had a leader again",
                crash.log
            );
        }
    }
}

/// slates' gate (`spread_logs_keep_keyed_commands_flowing_and_global_ones_pay_for_it`) on its own
/// shape, two seeds, one log and five, each held to the exact facts of its claims; what it measured
/// printed beside them.
#[test]
fn spread_logs_keep_keyed_commands_flowing_and_global_ones_pay_for_it() {
    let (one_expected, one, one_crashed) =
        keyed_expectation(GATE_SEEDS, 1, GATE_DURATION_NS, GATE_CRASH_NS, &exact);
    let (five_expected, five, five_crashed) =
        keyed_expectation(GATE_SEEDS, 5, GATE_DURATION_NS, GATE_CRASH_NS, &exact);
    eprintln!("one log: steady {one:?}, crashed {one_crashed:?}, expected {one_expected}");
    eprintln!("five logs: steady {five:?}, crashed {five_crashed:?}, expected {five_expected}");
}

/// A measurement tool (slates'): one, two, three and five logs over the five regions, steady and
/// with each log's preferred voter crashed for twenty seconds in turn, with a keyed command's
/// expectation as a region is lost. `HYPER_MULTILOG_SEEDS=20 cargo test -p hyper-multilog --release
/// --test multilog_timed -- --ignored --exact multi_log_on_the_failure_path --nocapture`.
#[test]
#[ignore = "a measurement tool, run by hand with its environment set"]
#[allow(
    clippy::disallowed_methods,
    reason = "a measurement tool takes its seed count from the environment"
)]
fn multi_log_on_the_failure_path() {
    let Some(seeds) = std::env::var("HYPER_MULTILOG_SEEDS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
    else {
        eprintln!("skipping: set HYPER_MULTILOG_SEEDS to measure");
        return;
    };
    for logs in [1, 2, 3, 5] {
        let (expectation, steady, crashed) =
            keyed_expectation(seeds, logs, 70_000 * MS, (30_000 * MS, 50_000 * MS), &exact);
        eprintln!("{logs} logs: steady {steady:?}");
        for (log, run) in crashed.iter().enumerate() {
            eprintln!("  log {log}'s voter crashed for 20 s: {run:?}");
        }
        eprintln!(
            "  a keyed command's expectation as a region is lost: {} ms",
            expectation / REGIONS as u64
        );
    }
}
