//! Node pairs under a deterministic simulation: one clock, a network with seeded delays, stalls
//! and losses, a disk per node with seeded flush times, and owners that wake late by a seeded
//! amount. The owners run the crate as a real one does (poll at its wake, feed what arrives and what
//! becomes durable, make the liveness write it asks for) and compute the election cost from the
//! library's own law over the round trips the streams measure; the tests assert what the crate
//! promises and derive no bound of their own.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::disallowed_macros,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cognitive_complexity,
    missing_docs
)]

use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap};
use std::time::Duration;

use hyper_liveness::{
    Change, Heartbeat, Liveness, MAX_BYTES, Output, PeerId, Refusal, Settings, Suspicion, Write,
};
use hyper_timing::{Ballot, Exposure, Trust, WINDOW_LIMIT, poisson95};

const MS: u64 = 1_000_000;
const US: u64 = 1_000;

/// A xorshift stream (Marsaglia 2003): deterministic test noise.
#[derive(Clone)]
struct Noise(u64);

impl Noise {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    /// Uniform in `[0, span)`.
    fn below(&mut self, span: u64) -> u64 {
        self.next() % span.max(1)
    }
    fn chance(&mut self, p: f64) -> bool {
        ((self.next() >> 11) as f64 / (1u64 << 53) as f64) < p
    }
}

/// How the simulated world behaves.
#[derive(Clone, Copy, Debug)]
struct World {
    /// One-way delay: a floor and a uniform spread.
    delay: (u64, u64),
    /// The chance a message meets a stall, and the stall's length bound.
    stall: (f64, u64),
    /// The chance a message is lost.
    loss: f64,
    /// A flush: a floor and a uniform spread.
    flush: (u64, u64),
    /// The chance a flush meets a stall of the device, and the stall's length bound.
    flush_stall: (f64, u64),
    /// How late an owner's wake comes: a floor and a spread.
    late: (u64, u64),
    /// How often a node's groups write to its log on their own, if they do.
    organic: Option<u64>,
    /// Host freezes, if any: each node is frozen (wakes, sends, reads and completions held; the
    /// kernel still stamps what arrives) for up to the bound, about once a period. The stalls of
    /// the traces are the hosts' (`docs/timing.md` §2.6): a frozen sender's heartbeats leave late
    /// together, which makes those at a short interval correlated over the freeze.
    freeze: Option<(u64, u64)>,
}

const LAN: World = World {
    delay: (80 * US, 60 * US),
    stall: (0.002, 20 * MS),
    loss: 0.001,
    flush: (200 * US, 400 * US),
    flush_stall: (0.0, 0),
    late: (20 * US, 60 * US),
    organic: None,
    freeze: None,
};

/// A LAN whose hosts freeze for up to 50 ms about every 250 ms: macOS's measured correlation time
/// under load (`docs/timing.md` §2.6, item 6), with heartbeats at a floor of under a millisecond,
/// a hundred of them in each freeze.
const FROZEN: World = World {
    freeze: Some((250 * MS, 50 * MS)),
    ..LAN
};

/// A node whose groups write every few milliseconds to a device that stalls one flush in fifty for
/// up to 60 ms, as a loaded disk does (`docs/timing.md` §2.6: flushes stalled up to 117 and 139 ms):
/// the mean flush, and the floor `E[flush] + G` with it, moves by more than `G` with each stall.
const BUSY: World = World {
    organic: Some(2 * MS),
    flush_stall: (0.02, 60 * MS),
    ..LAN
};

/// A Windows host: a timed wait ends on the next 15.625 ms clock interrupt (Microsoft,
/// `timeBeginPeriod`, `docs/research/timing.md`), so a wake is up to that late, and a full flush
/// (`FlushFileBuffers`) takes 10 to 30 ms, as the members of hyper-durable-e2e measured their floors
/// `E[flush] + G` at 20 to 32 ms, with `G` at 2 to 8 ms, on the windows-2025 and windows-11-arm
/// runners (`docs/timing.md` §2.9, "On real detectors").
const WINDOWS: World = World {
    late: (0, 15_625 * US),
    flush: (10 * MS, 20 * MS),
    ..LAN
};

enum Event {
    /// A message reaches `to`, received by its kernel at `stamp`.
    Arrive {
        to: usize,
        from: usize,
        bytes: Vec<u8>,
        stamp: u64,
    },
    /// A node's disk makes a write durable.
    Durable {
        node: usize,
        write: Write,
        started: u64,
    },
    /// A node's groups write on their own.
    Organic { node: usize },
    /// A node's host freezes.
    Freeze { node: usize },
}

struct Owner {
    id: PeerId,
    sent: Vec<(PeerId, Vec<u8>)>,
    flush: bool,
    changes: Vec<Change>,
}

impl Output for Owner {
    fn heartbeat(&mut self, peer: PeerId, message: &[u8]) {
        self.sent.push((peer, message.to_vec()));
    }
    fn flush(&mut self) {
        self.flush = true;
    }
    fn change(&mut self, change: Change) {
        self.changes.push(change);
    }
}

struct Node {
    liveness: Liveness,
    owner: Owner,
    alive: bool,
    disk_stalled: bool,
    /// The host is frozen until this time.
    frozen_until: u64,
    /// The heartbeats taken when the node last charged its detectors an election.
    charged: u64,
    /// The disk's flush queue: one write at a time, as one device.
    disk_busy_until: u64,
    /// Every heartbeat sent: to whom and the message, at what time.
    log: Vec<(u64, PeerId, Heartbeat)>,
    /// Every durable completion: when.
    durable: Vec<u64>,
    suspicions: Vec<Suspicion>,
    /// The peers whose restart the node's stream reported.
    restarts: Vec<PeerId>,
}

struct Sim {
    now: u64,
    world: World,
    noise: Noise,
    nodes: Vec<Node>,
    queue: BinaryHeap<Reverse<(u64, u64)>>,
    events: BTreeMap<u64, Event>,
    next_event: u64,
    /// When each node first held each pair configured: `(node, peer)` to the time.
    configured_at: BTreeMap<(usize, PeerId), u64>,
}

impl Sim {
    fn new(count: usize, world: World, seed: u64) -> Self {
        let nodes = (0..count)
            .map(|i| {
                let id = i as u64 + 1;
                let mut liveness = Liveness::new(Settings {
                    local: id,
                    boot: seed ^ id,
                    max_peers: count,
                    history: Exposure::new(),
                })
                .unwrap();
                for peer in 1..=count as u64 {
                    if peer != id {
                        liveness.attach(peer).unwrap();
                    }
                }
                Node {
                    liveness,
                    owner: Owner {
                        id,
                        sent: Vec::new(),
                        flush: false,
                        changes: Vec::new(),
                    },
                    alive: true,
                    disk_stalled: false,
                    frozen_until: 0,
                    charged: 0,
                    disk_busy_until: 0,
                    log: Vec::new(),
                    durable: Vec::new(),
                    suspicions: Vec::new(),
                    restarts: Vec::new(),
                }
            })
            .collect();
        let mut sim = Self {
            now: 0,
            world,
            noise: Noise(seed | 1),
            nodes,
            queue: BinaryHeap::new(),
            events: BTreeMap::new(),
            next_event: 0,
            configured_at: BTreeMap::new(),
        };
        if let Some(every) = world.organic {
            for node in 0..count {
                let at = sim.noise.below(every);
                sim.schedule(at, Event::Organic { node });
            }
        }
        if let Some((every, _)) = world.freeze {
            for node in 0..count {
                let at = sim.noise.below(2 * every);
                sim.schedule(at, Event::Freeze { node });
            }
        }
        sim
    }

    fn schedule(&mut self, at: u64, event: Event) {
        let key = self.next_event;
        self.next_event += 1;
        self.queue.push(Reverse((at, key)));
        self.events.insert(key, event);
    }

    /// A write submitted on `node`'s disk now, durable after its flush.
    fn submit(&mut self, node: usize, write: Write) {
        if self.nodes[node].disk_stalled {
            return;
        }
        let (floor, spread) = self.world.flush;
        let start = self.now.max(self.nodes[node].disk_busy_until);
        let mut done = start + floor + self.noise.below(spread);
        if self.noise.chance(self.world.flush_stall.0) {
            done += self.noise.below(self.world.flush_stall.1);
        }
        self.nodes[node].disk_busy_until = done;
        let started = self.now;
        self.schedule(
            done,
            Event::Durable {
                node,
                write,
                started,
            },
        );
    }

    /// Polls `node` and carries out what it asked.
    fn poll(&mut self, node: usize) {
        if !self.nodes[node].alive || self.nodes[node].frozen_until > self.now {
            return;
        }
        let now = self.now;
        let n = &mut self.nodes[node];
        n.liveness.poll(now, &mut n.owner);
        self.drain(node);
    }

    fn drain(&mut self, node: usize) {
        let sent = std::mem::take(&mut self.nodes[node].owner.sent);
        for (peer, bytes) in sent {
            let beat = Heartbeat::decode(&bytes).unwrap();
            self.nodes[node].log.push((self.now, peer, beat));
            if self.noise.chance(self.world.loss) {
                continue;
            }
            let (floor, spread) = self.world.delay;
            let mut delay = floor + self.noise.below(spread);
            if self.noise.chance(self.world.stall.0) {
                delay += self.noise.below(self.world.stall.1);
            }
            self.schedule(
                self.now + delay,
                Event::Arrive {
                    to: peer as usize - 1,
                    from: node,
                    bytes,
                    stamp: self.now + delay,
                },
            );
        }
        if std::mem::take(&mut self.nodes[node].owner.flush) {
            self.submit(node, Write::Liveness);
        }
        let changes = std::mem::take(&mut self.nodes[node].owner.changes);
        for change in changes {
            match change {
                Change::Suspected(suspicion) => self.nodes[node].suspicions.push(suspicion),
                Change::Restarted { peer, .. } => self.nodes[node].restarts.push(peer),
                Change::Trusted { .. } => {}
            }
        }
    }

    /// The election cost each node charges its detectors: the library's law over the round trips
    /// its streams measured and its flush, once a quorum's paths are measured. Charged again on the
    /// detectors' own doubling schedule: once the heartbeats a node has taken have doubled since
    /// it last charged them, as the estimates the law reads have renewed.
    fn elect(&mut self) {
        let count = self.nodes.len();
        for node in &mut self.nodes {
            let taken: u64 = (1..=count as u64)
                .filter_map(|peer| node.liveness.report(peer))
                .map(|report| report.taken)
                .sum();
            if taken == 0 || taken < 2 * node.charged {
                continue;
            }
            let (Some(granularity), Some(durable)) =
                (node.liveness.granularity(), node.liveness.flush_mean())
            else {
                continue;
            };
            let peers: Vec<PeerId> = (1..=count as u64).filter(|p| *p != node.owner.id).collect();
            let paths: Vec<_> = peers
                .iter()
                .filter_map(|peer| node.liveness.round_trip(*peer).copied())
                .collect();
            let Some(span) = Ballot::measure(paths.iter(), count, durable, granularity)
                .and_then(|ballot| ballot.span(granularity))
            else {
                continue;
            };
            for peer in peers {
                node.liveness.set_election(peer, span.election).unwrap();
            }
            node.charged = taken;
        }
    }

    /// The next wake of each node, with its lateness drawn.
    fn next_wake(&mut self) -> Option<(u64, usize)> {
        let mut best: Option<(u64, usize)> = None;
        for (i, node) in self.nodes.iter().enumerate() {
            if !node.alive {
                continue;
            }
            if let Some(at) = node.liveness.wake().map(|at| at.max(node.frozen_until))
                && best.is_none_or(|(b, _)| at < b)
            {
                best = Some((at, i));
            }
        }
        best.map(|(at, i)| {
            let (floor, spread) = self.world.late;
            (at.max(self.now) + floor + self.noise.below(spread), i)
        })
    }

    /// Runs until `until`, polling every node first.
    fn run(&mut self, until: u64) {
        self.run_while(|sim| sim.now < until, Some(until));
    }

    /// Runs while `keep` holds of the world, an event or a wake at a time, polling every node
    /// first; never past `until`, where one is given.
    fn run_while(&mut self, keep: impl Fn(&Self) -> bool, until: Option<u64>) {
        for node in 0..self.nodes.len() {
            self.poll(node);
        }
        while keep(self) {
            let event_at = self.queue.peek().map(|Reverse((at, _))| *at);
            let wake = self.next_wake();
            let (at, is_event) = match (event_at, wake) {
                (Some(e), Some((w, _))) if e <= w => (e, true),
                (Some(e), None) => (e, true),
                (_, Some((w, _))) => (w, false),
                (None, None) => break,
            };
            if let Some(until) = until
                && at > until
            {
                self.now = until;
                break;
            }
            self.now = at;
            if is_event {
                let Reverse((_, key)) = self.queue.pop().unwrap();
                let event = self.events.remove(&key).unwrap();
                self.handle(event);
            } else if let Some((_, node)) = wake {
                self.poll(node);
            }
            self.elect();
            self.note_configured();
        }
    }

    /// Notes the pairs each live node has newly configured.
    fn note_configured(&mut self) {
        let count = self.nodes.len() as u64;
        for (index, node) in self.nodes.iter().enumerate().filter(|(_, n)| n.alive) {
            for peer in (1..=count).filter(|p| *p != node.owner.id) {
                if !self.configured_at.contains_key(&(index, peer))
                    && node.liveness.report(peer).is_some_and(|r| r.configured)
                {
                    self.configured_at.insert((index, peer), self.now);
                }
            }
        }
    }

    fn handle(&mut self, event: Event) {
        // A frozen host takes what happens to it once it thaws, in order.
        let held = match &event {
            Event::Arrive { to, .. } => Some(*to),
            Event::Durable { node, .. } => Some(*node),
            Event::Organic { .. } | Event::Freeze { .. } => None,
        };
        if let Some(node) = held
            && self.nodes[node].frozen_until > self.now
        {
            let until = self.nodes[node].frozen_until;
            self.schedule(until, event);
            return;
        }
        match event {
            Event::Arrive {
                to,
                from,
                bytes,
                stamp,
            } => {
                if !self.nodes[to].alive {
                    return;
                }
                let now = self.now;
                let n = &mut self.nodes[to];
                // Refusals are the crate's to make: a stale or unproven heartbeat is dropped.
                let _ = n
                    .liveness
                    .on_heartbeat(from as u64 + 1, &bytes, stamp, &mut n.owner);
                n.liveness.poll(now, &mut n.owner);
                self.drain(to);
            }
            Event::Durable {
                node,
                write,
                started,
            } => {
                if !self.nodes[node].alive || self.nodes[node].disk_stalled {
                    return;
                }
                let now = self.now;
                let n = &mut self.nodes[node];
                n.durable.push(now);
                n.liveness.on_durable(write, started, now);
                n.liveness.poll(now, &mut n.owner);
                self.drain(node);
            }
            Event::Organic { node } => {
                if let Some(every) = self.world.organic {
                    self.submit(node, Write::Log);
                    let next = self.now + every / 2 + self.noise.below(every);
                    self.schedule(next, Event::Organic { node });
                }
            }
            Event::Freeze { node } => {
                if let Some((every, longest)) = self.world.freeze {
                    let until = self.now + self.noise.below(longest);
                    self.nodes[node].frozen_until = self.nodes[node].frozen_until.max(until);
                    let next = until + self.noise.below(2 * every);
                    self.schedule(next, Event::Freeze { node });
                }
            }
        }
    }

    fn configured(&self) -> bool {
        self.nodes.iter().filter(|n| n.alive).all(|n| {
            (1..=self.nodes.len() as u64)
                .filter(|p| *p != n.owner.id && self.nodes[*p as usize - 1].alive)
                .all(|p| n.liveness.report(p).is_some_and(|r| r.configured))
        })
    }

    /// The live pairs that have taken more heartbeats than any window holds without a
    /// configuration of their own: a link whose estimator has not measured what it needs in
    /// `WINDOW_LIMIT` heartbeats, the longest window the drift bound lets any link average
    /// (`hyper_timing::link`), is one whose correlation no window of it can resolve, the failure
    /// `docs/timing.md` §2.9 found.
    fn unresolved(&self) -> Vec<(PeerId, PeerId, u64)> {
        let count = self.nodes.len() as u64;
        let mut stuck = Vec::new();
        for node in self.nodes.iter().filter(|n| n.alive) {
            for peer in (1..=count).filter(|p| *p != node.owner.id) {
                if !self.nodes[peer as usize - 1].alive {
                    continue;
                }
                if let Some(report) = node.liveness.report(peer)
                    && !report.configured
                    && report.taken > WINDOW_LIMIT
                {
                    stuck.push((node.owner.id, peer, report.taken));
                }
            }
        }
        stuck
    }

    /// Runs until every live pair is configured; a pair that takes more heartbeats than any window
    /// holds unconfigured fails it (`unresolved`).
    fn run_until_configured(&mut self) {
        self.run_while(
            |sim| {
                let stuck = sim.unresolved();
                assert!(stuck.is_empty(), "unconfigured links: {stuck:?}");
                !sim.configured()
            },
            None,
        );
    }
}

/// Live peers: every pair configures, trusts, and makes no more mistakes than Theorem 7 allows.
#[test]
fn live_peers_configure_and_keep_their_allowance() {
    for seed in 1..=8u64 {
        let mut sim = Sim::new(3, LAN, seed * 0x9E37_79B9);
        sim.run_until_configured();
        let end = sim.now + 60_000 * MS;
        sim.run(end);
        let (mut suspicions, mut allowance) = (0u64, 0.0f64);
        for node in &sim.nodes {
            for peer in (1..=3u64).filter(|p| *p != node.owner.id) {
                let report = node.liveness.report(peer).unwrap();
                assert!(report.configured, "seed {seed}");
                suspicions += report.suspicions;
                allowance += report.allowance;
                assert!(report.taken > 0 && report.unproven == 0, "{report:?}");
            }
        }
        assert!(
            poisson95(suspicions).0 <= allowance,
            "seed {seed}: {suspicions} suspicions of live peers refute the allowance {allowance}"
        );
    }
}

/// Every heartbeat sent carries a flush made durable after the previous heartbeat to that peer
/// was due, newer than the previous one's, and none leaves without one.
#[test]
fn no_heartbeat_leaves_without_a_newer_flush() {
    for (seed, organic) in [(3u64, None), (5, Some(7 * MS)), (7, Some(300 * US))] {
        let world = World { organic, ..LAN };
        let mut sim = Sim::new(3, world, seed);
        sim.run(5_000 * MS);
        for node in &sim.nodes {
            let mut previous: BTreeMap<PeerId, Heartbeat> = BTreeMap::new();
            for (at, peer, beat) in &node.log {
                let due = beat.sent_ns - beat.late_ns;
                let durable = beat.sent_ns - beat.flush_age_ns;
                assert_eq!(beat.sent_ns, *at);
                assert!(
                    durable + beat.interval_ns > due,
                    "the flush came after the previous was due"
                );
                assert!(
                    node.durable.contains(&durable),
                    "a flush reported to the stream"
                );
                if let Some(before) = previous.get(peer) {
                    assert!(beat.flushes > before.flushes);
                    assert!(beat.seq > before.seq);
                }
                previous.insert(*peer, *beat);
            }
            assert!(!node.log.is_empty());
        }
    }
}

/// A killed peer is suspected by every survivor, within the bound each states from the peer's
/// last heartbeat's schedule (one clock in the simulation, so the peer's schedule is on it).
#[test]
fn a_killed_peer_is_suspected_within_the_stated_bound() {
    for seed in 11..=26u64 {
        let mut sim = Sim::new(4, LAN, seed);
        sim.run_until_configured();
        let settle = sim.now + 2_000 * MS;
        sim.run(settle);
        let victim = 3usize;
        // Killed while every survivor trusts it: one that suspected it by a mistake just before
        // would hold that suspicion through the kill and make no new one to measure.
        let trusted = |sim: &Sim| {
            (0..3).all(|node| {
                matches!(
                    sim.nodes[node].liveness.trust(victim as u64 + 1),
                    Some(Trust::Trusted { .. })
                )
            })
        };
        while !trusted(&sim) {
            let next = sim.now + MS;
            sim.run(next);
        }
        let killed_at = sim.now;
        sim.nodes[victim].alive = false;
        let last_sent: Vec<u64> = (0..4usize)
            .map(|node| {
                sim.nodes[victim]
                    .log
                    .iter()
                    .filter(|(_, peer, _)| *peer == node as u64 + 1)
                    .map(|(at, _, _)| *at)
                    .max()
                    .unwrap_or(0)
            })
            .collect();
        let end = sim.now + 10_000 * MS;
        sim.run(end);
        for (node, last_sent) in last_sent.iter().enumerate().take(3) {
            let found = sim.nodes[node]
                .suspicions
                .iter()
                .find(|s| s.peer == victim as u64 + 1 && s.at_ns >= killed_at)
                .copied()
                .unwrap_or_else(|| panic!("seed {seed}: node {node} never suspected the victim"));
            let last = found.last.unwrap();
            assert_eq!(
                last.sent_ns, *last_sent,
                "the last heartbeat sent is the last taken"
            );
            let bound = found
                .detection
                .expect("every heartbeat in the window was echoed");
            assert!(
                found.at_ns - last.due_ns <= bound.as_nanos() as u64,
                "seed {seed}: suspected {} ns after the last schedule, past the stated {bound:?}",
                found.at_ns - last.due_ns
            );
            assert_eq!(
                sim.nodes[node].liveness.trust(victim as u64 + 1),
                Some(Trust::Suspected)
            );
        }
    }
}

/// A node whose disk stops completing flushes stops heartbeating, and every peer suspects it
/// within the bound it states, while the node itself is still running and receiving.
#[test]
fn a_stalled_disk_is_suspected_as_a_crash_is() {
    for seed in 31..=38u64 {
        let mut sim = Sim::new(
            3,
            World {
                organic: Some(5 * MS),
                ..LAN
            },
            seed,
        );
        sim.run_until_configured();
        let settle = sim.now + 2_000 * MS;
        sim.run(settle);
        let stalled = 2usize;
        let stalled_at = sim.now;
        sim.nodes[stalled].disk_stalled = true;
        let end = sim.now + 10_000 * MS;
        sim.run(end);
        // At most the heartbeats an in-flight flush had proved left after the stall.
        let late_sends = sim.nodes[stalled]
            .log
            .iter()
            .filter(|(at, _, beat)| {
                *at > stalled_at && beat.sent_ns - beat.flush_age_ns > stalled_at
            })
            .count();
        assert_eq!(
            late_sends, 0,
            "seed {seed}: a heartbeat left on a flush made after the stall"
        );
        for node in 0..2 {
            let found = sim.nodes[node]
                .suspicions
                .iter()
                .find(|s| s.peer == stalled as u64 + 1 && s.at_ns >= stalled_at)
                .copied()
                .unwrap_or_else(|| panic!("seed {seed}: node {node} never suspected the stall"));
            let last = found.last.unwrap();
            let bound = found.detection.unwrap();
            assert!(found.at_ns - last.due_ns <= bound.as_nanos() as u64);
        }
        // The stalled node still hears its peers.
        assert!(
            sim.nodes[stalled].liveness.suspected().next().is_none(),
            "seed {seed}: the stalled node suspected a live peer"
        );
    }
}

/// The stream is the pair's, not the group's: a thousand groups shared send what one does, and a
/// pair whose last group goes sends nothing.
#[test]
fn groups_share_one_stream_and_an_unshared_pair_is_silent() {
    let sent = |groups: u32| {
        let mut sim = Sim::new(2, LAN, 41);
        for node in &mut sim.nodes {
            let peer = 3 - node.owner.id;
            for _ in 1..groups {
                node.liveness.attach(peer).unwrap();
            }
        }
        sim.run(3_000 * MS);
        (sim.nodes[0].log.len(), sim.nodes[1].log.len())
    };
    assert_eq!(sent(1), sent(1_000));
    let mut sim = Sim::new(2, LAN, 43);
    sim.run(1_000 * MS);
    for node in &mut sim.nodes {
        let peer = 3 - node.owner.id;
        node.liveness.detach(peer).unwrap();
        assert_eq!(node.liveness.report(peer), None);
    }
    let before = sim.nodes[0].log.len();
    sim.run(3_000 * MS);
    assert_eq!(
        sim.nodes[0].log.len(),
        before,
        "nothing shared, nothing sent"
    );
    assert_eq!(sim.nodes[0].liveness.wake(), None);
}

/// A restarted peer is reported restarted (`Change::Restarted`, for the core's `restarted`), is
/// judged at once by the detector in force, and its restart is a failure in the MTBF's evidence.
#[test]
fn a_restarted_peer_is_trusted_again_and_counted() {
    let mut sim = Sim::new(3, LAN, 51);
    sim.run_until_configured();
    let mtbf = sim.nodes[0].liveness.mtbf().unwrap();
    let victim = 2usize;
    sim.nodes[victim].alive = false;
    let end = sim.now + 3_000 * MS;
    sim.run(end);
    assert_eq!(sim.nodes[0].liveness.trust(3), Some(Trust::Suspected));
    // A new run: a new process, the same node.
    let mut liveness = Liveness::new(Settings {
        local: 3,
        boot: 0xB007,
        max_peers: 3,
        history: Exposure::new(),
    })
    .unwrap();
    liveness.attach(1).unwrap();
    liveness.attach(2).unwrap();
    sim.nodes[victim].liveness = liveness;
    sim.nodes[victim].alive = true;
    sim.nodes[victim].disk_busy_until = sim.now;
    sim.poll(victim);
    let end = sim.now + 2_000 * MS;
    sim.run(end);
    assert!(matches!(
        sim.nodes[0].liveness.trust(3),
        Some(Trust::Trusted { .. })
    ));
    for node in 0..2 {
        assert_eq!(
            sim.nodes[node].restarts,
            vec![3],
            "node {node} saw the restart once"
        );
    }
    assert!(sim.nodes[0].liveness.report(3).unwrap().configured);
    assert!(
        sim.nodes[0].liveness.mtbf().unwrap() < mtbf + Duration::from_secs(60),
        "the restart counted"
    );
}

/// A heartbeat whose proof does not hold, or that is stale, from a stranger or from this node, is
/// refused.
#[test]
fn heartbeats_without_their_proof_are_refused() {
    let mut node = Liveness::new(Settings {
        local: 1,
        boot: 1,
        max_peers: 1,
        history: Exposure::new(),
    })
    .unwrap();
    node.attach(2).unwrap();
    assert_eq!(node.attach(3), Err(Refusal::TooManyPeers));
    assert_eq!(node.attach(1), Err(Refusal::FromSelf));
    let mut owner = Owner {
        id: 1,
        sent: Vec::new(),
        flush: false,
        changes: Vec::new(),
    };
    let beat = Heartbeat {
        boot: 9,
        seq: 0,
        interval_ns: 10 * MS,
        floor_ns: MS,
        ask_ns: 0,
        sent_ns: 50 * MS,
        late_ns: MS,
        flushes: 4,
        flush_age_ns: 2 * MS,
        echo: None,
    };
    let mut out = [0u8; MAX_BYTES];
    let send =
        |node: &mut Liveness, owner: &mut Owner, beat: Heartbeat, out: &mut [u8; MAX_BYTES]| {
            let bytes = beat.encode(out).to_vec();
            node.on_heartbeat(2, &bytes, 60 * MS, owner)
        };
    // No granularity measured yet: the proof is checked and the echo kept, nothing estimated.
    assert_eq!(
        send(&mut node, &mut owner, beat, &mut out),
        Err(Refusal::Unmeasured)
    );
    let same = Heartbeat { seq: 1, ..beat };
    assert_eq!(
        send(&mut node, &mut owner, same, &mut out),
        Err(Refusal::Unproven),
        "no newer flush"
    );
    let old = Heartbeat {
        seq: 1,
        flushes: 5,
        flush_age_ns: 12 * MS,
        ..beat
    };
    assert_eq!(
        send(&mut node, &mut owner, old, &mut out),
        Err(Refusal::Unproven),
        "a flush from before the previous heartbeat was due"
    );
    assert_eq!(node.report(2).unwrap().unproven, 2);
    let bytes = beat.encode(&mut out).to_vec();
    assert_eq!(
        node.on_heartbeat(3, &bytes, 0, &mut owner),
        Err(Refusal::UnknownPeer)
    );
    assert_eq!(
        node.on_heartbeat(1, &bytes, 0, &mut owner),
        Err(Refusal::FromSelf)
    );
    assert_eq!(
        node.on_heartbeat(2, &bytes[..9], 0, &mut owner),
        Err(Refusal::Truncated)
    );
    assert_eq!(node.detach(3), Err(Refusal::UnknownPeer));
}

/// Problem 1 and item 10 of `docs/timing.md` (§2.9, §3): every link configures, or a crash on it
/// is suspected within the bound its suspicion states. On a LAN, and on one whose hosts freeze for
/// tens of milliseconds at a time, which makes the heartbeats at a sub-millisecond floor too
/// correlated for any window to measure (`LinkEstimator::independent_interval`), seeds from zero:
/// - before any kill, every pair configures, none taking more heartbeats unconfigured than any
///   window holds (`Sim::unresolved`);
/// - one node is killed after a share of the heartbeats it sent before every pair had configured
///   in the same seed's run, drawn from the seed between none and twice as many: before it sent
///   any, while the links were young, and after; every survivor then suspects it, each suspicion
///   within the bound it states, from the victim's last heartbeat's schedule (one clock here), or,
///   for a victim never heard, from the start.
#[test]
fn every_link_configures_or_suspects_a_crash_within_its_bound() {
    let mut slowest = (0u64, 0u64, "");
    let mut judged_by = [0u64; 3];
    for (name, world) in [("lan", LAN), ("frozen", FROZEN), ("busy", BUSY)] {
        for seed in 0..32u64 {
            // The heartbeats the victim sends before every pair is configured, in this seed's run.
            let mut twin = Sim::new(4, world, seed);
            twin.run_until_configured();
            let victim = 3usize;
            let configured_after = twin.nodes[victim].log.len() as u64;
            for node in &twin.nodes {
                for peer in (1..=4u64).filter(|p| *p != node.owner.id) {
                    let taken = node.liveness.report(peer).unwrap().taken;
                    if taken > slowest.0 {
                        slowest = (taken, seed, name);
                    }
                }
            }
            let mut draw = Noise(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
            let kill_after = draw.below(2 * configured_after + 1);
            let mut sim = Sim::new(4, world, seed);
            sim.run_while(
                |sim| (sim.nodes[victim].log.len() as u64) < kill_after,
                None,
            );
            let killed_at = sim.now;
            sim.nodes[victim].alive = false;
            let last_due: Vec<Option<u64>> = (0..3usize)
                .map(|node| {
                    sim.nodes[victim]
                        .log
                        .iter()
                        .filter(|(_, peer, _)| *peer == node as u64 + 1)
                        .map(|(_, _, beat)| beat.sent_ns - beat.late_ns)
                        .max()
                })
                .collect();
            sim.run_while(
                |sim| {
                    let stuck = sim.unresolved();
                    assert!(stuck.is_empty(), "{name} seed {seed}: {stuck:?}");
                    (0..3).any(|node| {
                        sim.nodes[node].liveness.trust(victim as u64 + 1) != Some(Trust::Suspected)
                    })
                },
                None,
            );
            for (node, last_due) in last_due.iter().enumerate() {
                let suspicion = sim.nodes[node]
                    .suspicions
                    .iter()
                    .rfind(|s| s.peer == victim as u64 + 1)
                    .copied()
                    .unwrap_or_else(|| panic!("{name} seed {seed}: {node} holds no suspicion"));
                let bound = suspicion.detection.map(|d| d.as_nanos() as u64);
                match (suspicion.last, bound) {
                    (Some(last), Some(bound)) => {
                        // Its own configuration's or the node's evidence's margin: either states its bound,
                        // from the last heartbeat taken (a later one may have been lost).
                        assert!(Some(last.due_ns) <= *last_due, "{name} seed {seed}");
                        assert!(
                            suspicion.at_ns - last.due_ns <= bound,
                            "{name} seed {seed}: node {node} suspected {} ns past the last \
                             schedule, past its stated {bound}",
                            suspicion.at_ns - last.due_ns
                        );
                        let own = sim.nodes[node]
                            .liveness
                            .report(victim as u64 + 1)
                            .unwrap()
                            .configured;
                        judged_by[usize::from(!own)] += 1;
                    }
                    (None, Some(bound)) => {
                        // Never heard: from the start, an interval and the evidence's margin.
                        assert!(suspicion.at_ns <= bound, "{name} seed {seed}");
                        judged_by[2] += 1;
                    }
                    (Some(_), None) => {
                        // A heartbeat in the window carried no echo: a link killed in its first
                        // heartbeats, before the peer heard back. Suspected all the same.
                        assert!(suspicion.at_ns >= killed_at.min(suspicion.at_ns));
                        judged_by[1] += 1;
                    }
                    (None, None) => panic!("{name} seed {seed}: a suspicion with no bound"),
                }
            }
        }
    }
    println!(
        "most heartbeats a link took to configure: {} ({} seed {}); suspicions of the killed node \
         by its own detector {}, by the node's evidence's margin {}, never heard {}",
        slowest.0, slowest.2, slowest.1, judged_by[0], judged_by[1], judged_by[2]
    );
}

/// Item 10 of `docs/timing.md` §3: a peer from which no heartbeat ever comes (dead before its
/// first) is suspected by every other, once its node's pool can give a margin, within the bound
/// the suspicion states from the start: one interval at the node's own floor and the margin of the node's evidence.
#[test]
fn a_peer_never_heard_from_is_suspected() {
    for seed in 0..16u64 {
        let mut sim = Sim::new(4, LAN, seed);
        sim.nodes[3].alive = false;
        sim.run_while(
            |sim| {
                let stuck = sim.unresolved();
                assert!(stuck.is_empty(), "seed {seed}: {stuck:?}");
                (0..3).any(|node| sim.nodes[node].liveness.trust(4) != Some(Trust::Suspected))
            },
            None,
        );
        for node in 0..3 {
            let suspicion = sim.nodes[node]
                .suspicions
                .iter()
                .find(|s| s.peer == 4)
                .copied()
                .unwrap();
            assert_eq!(suspicion.last, None);
            let bound = suspicion.detection.expect("a bound from the start");
            assert!(suspicion.at_ns <= bound.as_nanos() as u64, "seed {seed}");
            assert_eq!(sim.nodes[node].liveness.report(4).unwrap().taken, 0);
        }
    }
}

/// Item 10 of `docs/timing.md` §3, with three nodes: a peer that dies in its links' first
/// heartbeats, before any pair has its own evidence, leaves each survivor one live link, whose
/// configuration lengthens its interval to its best (seconds, on Windows' timer), and a pool fed at
/// that rate. Every survivor suspects it no later than the first poll once both its freshness point
/// has passed and the node holds a link's own evidence (a pair it has configured): the young link
/// is judged by the widest behaviour the node has measured (`docs/timing.md` §2.8, "Judged before
/// its own evidence"). In four worlds, Windows' timer among them, 32 seeds each, the victim killed
/// after a share, drawn from the seed, of the heartbeats it sent before any pair configured in the
/// same seed's run.
#[test]
fn a_peer_dead_before_its_links_have_evidence_is_suspected_once_a_sibling_has_its_own() {
    for (name, world) in [
        ("lan", LAN),
        ("frozen", FROZEN),
        ("busy", BUSY),
        ("windows", WINDOWS),
    ] {
        let mut noticed = Vec::new();
        for seed in 0..32u64 {
            let victim = 2usize;
            let mut twin = Sim::new(3, world, seed);
            twin.run_while(|sim| sim.configured_at.is_empty(), None);
            let young = twin.nodes[victim].log.len() as u64;
            let mut draw = Noise(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
            let kill_after = 1 + draw.below(young.saturating_sub(1).max(1));
            let mut sim = Sim::new(3, world, seed);
            sim.run_while(
                |sim| (sim.nodes[victim].log.len() as u64) < kill_after,
                None,
            );
            assert!(
                sim.configured_at.is_empty(),
                "{name} seed {seed}: killed after a pair configured"
            );
            let killed_at = sim.now;
            sim.nodes[victim].alive = false;
            sim.run_while(
                |sim| {
                    let stuck = sim.unresolved();
                    assert!(stuck.is_empty(), "{name} seed {seed}: {stuck:?}");
                    (0..2).any(|node| {
                        sim.nodes[node].liveness.trust(victim as u64 + 1) != Some(Trust::Suspected)
                    })
                },
                None,
            );
            // A poll comes at most a wake's lateness, or a freeze, past what it waits for.
            let late = world.late.0 + world.late.1 + world.freeze.map_or(0, |(_, longest)| longest);
            for node in 0..2usize {
                let suspicion = sim.nodes[node]
                    .suspicions
                    .iter()
                    .rfind(|s| s.peer == victim as u64 + 1)
                    .copied()
                    .unwrap_or_else(|| panic!("{name} seed {seed}: {node} holds no suspicion"));
                if let (Some(last), Some(bound)) = (suspicion.last, suspicion.detection) {
                    assert!(
                        suspicion.at_ns - last.due_ns <= bound.as_nanos() as u64,
                        "{name} seed {seed}: node {node} past its stated bound"
                    );
                }
                let sibling = (1 - node) as u64 + 1;
                // Suspected before its sibling configured: the pool had its evidence first.
                let evidence = sim
                    .configured_at
                    .get(&(node, sibling))
                    .copied()
                    .unwrap_or(u64::MAX);
                assert!(
                    suspicion.noticed_ns <= suspicion.at_ns.max(evidence).saturating_add(late),
                    "{name} seed {seed}: node {node} noticed {} ms after the kill, its freshness \
                     point {} ms and its sibling's configuration {} ms after it",
                    suspicion.noticed_ns.saturating_sub(killed_at) / MS,
                    suspicion.at_ns.saturating_sub(killed_at) / MS,
                    evidence.saturating_sub(killed_at) / MS,
                );
                // A suspicion held from before the kill (a mistake) counts as at the kill.
                noticed.push(suspicion.noticed_ns.saturating_sub(killed_at));
            }
        }
        noticed.sort_unstable();
        println!(
            "{name}: survivors noticed the victim's death {} ms after the kill at the median, {} ms \
             at the 90th percentile, {} ms at the most",
            noticed[noticed.len() / 2] / MS,
            noticed[noticed.len() * 9 / 10] / MS,
            noticed[noticed.len() - 1] / MS
        );
    }
}
