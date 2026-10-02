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
use hyper_timing::{Ballot, Exposure, Trust, poisson95};

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
    /// How late an owner's wake comes: a floor and a spread.
    late: (u64, u64),
    /// How often a node's groups write to its log on their own, if they do.
    organic: Option<u64>,
}

const LAN: World = World {
    delay: (80 * US, 60 * US),
    stall: (0.002, 20 * MS),
    loss: 0.001,
    flush: (200 * US, 400 * US),
    late: (20 * US, 60 * US),
    organic: None,
};

enum Event {
    /// A message reaches `to`.
    Arrive {
        to: usize,
        from: usize,
        bytes: Vec<u8>,
    },
    /// A node's disk makes a write durable.
    Durable {
        node: usize,
        write: Write,
        started: u64,
    },
    /// A node's groups write on their own.
    Organic { node: usize },
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
    /// The disk's flush queue: one write at a time, as one device.
    disk_busy_until: u64,
    /// Every heartbeat sent: to whom and the message, at what time.
    log: Vec<(u64, PeerId, Heartbeat)>,
    /// Every durable completion: when.
    durable: Vec<u64>,
    suspicions: Vec<Suspicion>,
}

struct Sim {
    now: u64,
    world: World,
    noise: Noise,
    nodes: Vec<Node>,
    queue: BinaryHeap<Reverse<(u64, u64)>>,
    events: BTreeMap<u64, Event>,
    next_event: u64,
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
                    disk_busy_until: 0,
                    log: Vec::new(),
                    durable: Vec::new(),
                    suspicions: Vec::new(),
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
        };
        if let Some(every) = world.organic {
            for node in 0..count {
                let at = sim.noise.below(every);
                sim.schedule(at, Event::Organic { node });
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
        let done = start + floor + self.noise.below(spread);
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
        if !self.nodes[node].alive {
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
                },
            );
        }
        if std::mem::take(&mut self.nodes[node].owner.flush) {
            self.submit(node, Write::Liveness);
        }
        let changes = std::mem::take(&mut self.nodes[node].owner.changes);
        for change in changes {
            if let Change::Suspected(suspicion) = change {
                self.nodes[node].suspicions.push(suspicion);
            }
        }
    }

    /// The election cost each node charges its detectors: the library's law over the round trips
    /// its streams measured and its flush, once a quorum's paths are measured.
    fn elect(&mut self) {
        let count = self.nodes.len();
        for node in &mut self.nodes {
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
        }
    }

    /// The next wake of each node, with its lateness drawn.
    fn next_wake(&mut self) -> Option<(u64, usize)> {
        let mut best: Option<(u64, usize)> = None;
        for (i, node) in self.nodes.iter().enumerate() {
            if !node.alive {
                continue;
            }
            if let Some(at) = node.liveness.wake()
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
        for node in 0..self.nodes.len() {
            self.poll(node);
        }
        let mut elected_at = 0;
        loop {
            let event_at = self.queue.peek().map(|Reverse((at, _))| *at);
            let wake = self.next_wake();
            let (at, is_event) = match (event_at, wake) {
                (Some(e), Some((w, _))) if e <= w => (e, true),
                (Some(e), None) => (e, true),
                (_, Some((w, _))) => (w, false),
                (None, None) => break,
            };
            if at > until {
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
            if self.now >= elected_at {
                self.elect();
                elected_at = self.now + 100 * MS;
            }
        }
    }

    fn handle(&mut self, event: Event) {
        match event {
            Event::Arrive { to, from, bytes } => {
                if !self.nodes[to].alive {
                    return;
                }
                let now = self.now;
                let n = &mut self.nodes[to];
                // Refusals are the crate's to make: a stale or unproven heartbeat is dropped.
                let _ = n
                    .liveness
                    .on_heartbeat(from as u64 + 1, &bytes, now, &mut n.owner);
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
        }
    }

    fn configured(&self) -> bool {
        self.nodes.iter().filter(|n| n.alive).all(|n| {
            (1..=self.nodes.len() as u64)
                .filter(|p| *p != n.owner.id && self.nodes[*p as usize - 1].alive)
                .all(|p| n.liveness.report(p).is_some_and(|r| r.configured))
        })
    }

    /// Runs until every live pair is configured, at most `limit`.
    fn run_until_configured(&mut self, limit: u64) {
        let step = 50 * MS;
        while !self.configured() {
            assert!(self.now < limit, "not configured by {} ms", self.now / MS);
            let until = self.now + step;
            self.run(until);
        }
    }
}

/// Live peers: every pair configures, trusts, and makes no more mistakes than Theorem 7 allows.
#[test]
fn live_peers_configure_and_keep_their_allowance() {
    for seed in 1..=8u64 {
        let mut sim = Sim::new(3, LAN, seed * 0x9E37_79B9);
        sim.run_until_configured(120_000 * MS);
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
        sim.run_until_configured(120_000 * MS);
        let settle = sim.now + 2_000 * MS;
        sim.run(settle);
        let victim = 3usize;
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
        sim.run_until_configured(120_000 * MS);
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

/// A restarted peer is judged at once by the detector in force, and its restart is a failure in
/// the MTBF's evidence.
#[test]
fn a_restarted_peer_is_trusted_again_and_counted() {
    let mut sim = Sim::new(3, LAN, 51);
    sim.run_until_configured(120_000 * MS);
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
