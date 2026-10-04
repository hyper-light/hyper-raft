//! hyper-swim's first simulation (`docs/sim.md` §9, step S-2): the cluster test's members on
//! hyper-sim's world and network instead of processes and sockets.
//!
//! Five members, each with a clock of its own (an offset, a rate within RFC 5905's 15 ppm, timers
//! late by a drawn amount), send their messages encoded as on the plane, carried by the network's
//! LAN path, each arrival an event of the world. Once every member judges every peer by a verdict
//! (the pair's own or the pool's), one member is killed: its arrivals are dropped and its timer
//! disarmed. Every survivor then holds it dead within the detection bound its detector stated, with
//! the wait it measured for evidence of its own health added where its condemnation was pending on
//! it, and no member holds a live one dead. Every seed's run replays from its seed, so each check
//! is exact for the seeds named; the run-twice check refuses a run whose digests differ.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::disallowed_macros,
    clippy::cast_possible_truncation,
    clippy::cognitive_complexity,
    clippy::panic_in_result_fn,
    missing_docs
)]

use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::time::Duration;

use hyper_sim::net::{Net, NetLimits, Path, Ticket};
use hyper_sim::{
    Clock, Discipline, Lateness, Limits, NodeId, Random, Record, SimError, Source, Step, World,
    twice,
};
use hyper_swim::HostId;
use hyper_swim::codec::{Coordinate, GossipBatch, SwimMessage, gossip_capacity};
use hyper_swim::detector::{Detector, PingReq};
use hyper_swim::membership::{Liveness, MemberState};
use hyper_timing::Exposure;

/// Members in the cluster.
const NODES: u32 = 5;
/// The member killed.
const VICTIM: u32 = NODES - 1;
/// The path's datagram size: QUIC's minimum, which every path carries (RFC 9000 §14.1).
const DATAGRAM: usize = 1_200;
/// RFC 5905's frequency tolerance, the most a member's clock runs fast or slow (§7.2).
const PHI_PPM: u64 = 15;

/// The steps a run may take: four times the most any seed took, 1,240 of seeds 0 to 15 (measured
/// 2026-10-04), so a run that takes many more has stopped converging.
const STEPS: u64 = 4 * 1_240;
/// The decisions a step makes at most: the strategy's pick of a tie, a timer's lateness, and a
/// delay for each message a poll sends, a probe and a ping-request to each other member.
const DECISIONS_PER_STEP: u64 = 2 + NODES as u64;

const LIMITS: Limits = Limits {
    events: 1_024,
    nodes: NODES as usize,
    streams: 64,
    steps: STEPS,
    trace_words: (STEPS * DECISIONS_PER_STEP) as usize,
};

const NET: NetLimits = NetLimits {
    flows: (NODES * NODES) as usize,
    links: 0,
    nats: 0,
    link_messages: 0,
    messages: 1_024,
    bytes: 1_024 * DATAGRAM,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ev {
    Arrive(Ticket),
}

fn host(node: NodeId) -> HostId {
    HostId(u64::from(node.0) + 1)
}

fn node_of(host: HostId) -> NodeId {
    NodeId(u32::try_from(host.0 - 1).unwrap())
}

/// A death a member came to hold: how long after the peer's last answer, and the bound its detector
/// stated then (none while its probes judged nothing).
#[derive(Clone, Copy, Debug)]
struct Noted {
    after: Duration,
    within: Option<Duration>,
}

/// One member's driver, as the cluster test's: its detector and the probes it relays.
struct Member {
    detector: Detector,
    gossip: usize,
    view_room: usize,
    requests: Vec<PingReq>,
    batch: Vec<(HostId, MemberState)>,
    encoded: Vec<u8>,
    /// The probes this member relays: the target, the relay's nonce, who asked and with which nonce.
    relaying: BTreeMap<u64, (u64, HostId, u64)>,
    /// Each peer's death as this member came to hold it.
    deaths: BTreeMap<u64, Noted>,
    alive: bool,
}

struct Sim {
    world: World<Ev>,
    net: Net<Vec<u8>>,
    members: Vec<Member>,
    /// Encoded messages to send, held so the members' borrows end first.
    outbox: Vec<(NodeId, NodeId, Vec<u8>)>,
}

impl Member {
    fn new(me: NodeId) -> Self {
        let members = NonZeroUsize::new(NODES as usize).unwrap();
        // The world's clocks read whole nanoseconds.
        let mut detector =
            Detector::new(host(me), Exposure::new(), members, Duration::from_nanos(1));
        for peer in (0..NODES).map(NodeId).filter(|peer| *peer != me) {
            detector.join(host(peer)).unwrap();
        }
        let mut member = Self {
            detector,
            gossip: 0,
            view_room: 0,
            requests: Vec::new(),
            batch: Vec::new(),
            encoded: Vec::new(),
            relaying: BTreeMap::new(),
            deaths: BTreeMap::new(),
            alive: true,
        };
        member.gossip = member.room(me, true);
        member.view_room = member.room(me, false);
        member
    }

    /// The gossip entries that fit the datagram beside the largest message (an acknowledgement
    /// with this member's coordinate), or the members a chunk of its view carries.
    fn room(&mut self, me: NodeId, gossip: bool) -> usize {
        let coordinate = *self.detector.coordinate();
        let message = if gossip {
            SwimMessage::Ack {
                from: host(me),
                nonce: u64::MAX,
                boot_nonce: u64::from(me.0),
                configuration_version: 0,
                standing: None,
                gossip: GossipBatch::Entries(&[]),
                coordinate: Coordinate::Held(&coordinate),
            }
        } else {
            SwimMessage::Sync {
                from: host(me),
                boot_nonce: u64::from(me.0),
                digest: 0,
                pull: true,
                gossip: GossipBatch::Entries(&[]),
            }
        };
        message.encode_into(&mut self.encoded);
        gossip_capacity(DATAGRAM, self.encoded.len())
    }

    fn queue(
        &mut self,
        outbox: &mut Vec<(NodeId, NodeId, Vec<u8>)>,
        me: NodeId,
        to: HostId,
        message: &SwimMessage<'_>,
    ) {
        message.encode_into(&mut self.encoded);
        outbox.push((me, node_of(to), self.encoded.clone()));
    }

    /// Polls the detector at `now` and queues what it asks: the relays, the probe, the view's
    /// chunks.
    fn step(&mut self, me: NodeId, now: u64, outbox: &mut Vec<(NodeId, NodeId, Vec<u8>)>) {
        let mut requests = std::mem::take(&mut self.requests);
        let ping = self.detector.poll(now, &mut requests);
        for request in &requests {
            let message = SwimMessage::PingReq {
                from: host(me),
                target: request.target,
                nonce: request.nonce,
                gossip: GossipBatch::Entries(&[]),
            };
            self.queue(outbox, me, request.relay, &message);
        }
        self.requests = requests;
        let mut batch = std::mem::take(&mut self.batch);
        if let Some(ping) = ping {
            self.detector
                .ping_gossip_into(ping.to, self.gossip, &mut batch);
            let message = SwimMessage::Ping {
                from: host(me),
                nonce: ping.nonce,
                boot_nonce: u64::from(me.0),
                configuration_version: 0,
                gossip: GossipBatch::Entries(&batch),
            };
            self.queue(outbox, me, ping.to, &message);
        }
        while let Some(chunk) = self.detector.sync_into(self.view_room, &mut batch) {
            let message = SwimMessage::Sync {
                from: host(me),
                boot_nonce: u64::from(me.0),
                digest: chunk.digest,
                pull: chunk.pull,
                gossip: GossipBatch::Entries(&batch),
            };
            self.queue(outbox, me, chunk.to, &message);
        }
        self.batch = batch;
    }

    /// Hands a message that arrived at `stamp` to the detector, queuing what it answers.
    fn handle(
        &mut self,
        me: NodeId,
        message: SwimMessage<'_>,
        stamp: u64,
        outbox: &mut Vec<(NodeId, NodeId, Vec<u8>)>,
    ) {
        match message {
            SwimMessage::Ping {
                from,
                nonce,
                gossip,
                ..
            } => {
                self.detector.apply_gossip(gossip);
                let ack = self.detector.on_ping(from);
                let mut batch = std::mem::take(&mut self.batch);
                self.detector.ack_gossip_into(from, self.gossip, &mut batch);
                let coordinate = *self.detector.coordinate();
                let message = SwimMessage::Ack {
                    from: host(me),
                    nonce,
                    boot_nonce: u64::from(me.0),
                    configuration_version: 0,
                    standing: None,
                    gossip: GossipBatch::Entries(&batch),
                    coordinate: Coordinate::Held(&coordinate),
                };
                self.queue(outbox, me, ack.to, &message);
                self.batch = batch;
            }
            SwimMessage::Ack {
                from,
                nonce,
                gossip,
                coordinate,
                ..
            } => {
                self.detector.apply_gossip(gossip);
                self.detector.learn_coordinate(from, coordinate);
                match self.relaying.get(&from.0) {
                    Some(&(relayed, asker, asked)) if relayed == nonce => {
                        self.relaying.remove(&from.0);
                        let message = SwimMessage::IndirectAck {
                            from: host(me),
                            target: from,
                            nonce: asked,
                            boot_nonce: u64::from(me.0),
                            gossip: GossipBatch::Entries(&[]),
                        };
                        self.queue(outbox, me, asker, &message);
                    }
                    _ => self.detector.on_ack(from, nonce, stamp),
                }
            }
            SwimMessage::PingReq {
                from,
                target,
                nonce,
                gossip,
            } => {
                self.detector.apply_gossip(gossip);
                let ping = self.detector.on_ping_req(target);
                self.relaying.insert(target.0, (ping.nonce, from, nonce));
                let message = SwimMessage::Ping {
                    from: host(me),
                    nonce: ping.nonce,
                    boot_nonce: u64::from(me.0),
                    configuration_version: 0,
                    gossip: GossipBatch::Entries(&[]),
                };
                self.queue(outbox, me, target, &message);
            }
            SwimMessage::Sync {
                from,
                digest,
                pull,
                gossip,
                ..
            } => {
                self.detector.on_sync(from, digest, pull, gossip);
            }
            SwimMessage::IndirectAck {
                target,
                nonce,
                gossip,
                ..
            } => {
                self.detector.apply_gossip(gossip);
                self.detector.on_indirect_ack(target, nonce, stamp);
            }
        }
    }

    /// Notes each death this member comes to hold, as the cluster test does.
    fn note_deaths(&mut self, me: NodeId, now: u64) {
        for peer in (0..NODES).map(NodeId).filter(|peer| *peer != me) {
            let held = self
                .detector
                .membership()
                .state(host(peer))
                .map(|state| state.liveness);
            let id = host(peer).0;
            match held {
                Some(Liveness::Dead) if !self.deaths.contains_key(&id) => {
                    let report = self.detector.report(host(peer)).unwrap_or_default();
                    let since = |at: Option<u64>| {
                        Duration::from_nanos(at.map_or(0, |at| now.saturating_sub(at)))
                    };
                    let waited = since(report.pending_since_ns);
                    let bound = self.detector.detection_bound(now);
                    self.deaths.insert(
                        id,
                        Noted {
                            after: since(report.last_answer_ns),
                            within: bound.map(|bound| bound.saturating_add(waited)),
                        },
                    );
                }
                Some(Liveness::Alive | Liveness::Suspect) => {
                    self.deaths.remove(&id);
                }
                _ => {}
            }
        }
    }
}

impl Sim {
    fn new(source: Source) -> Self {
        let mut world = World::new(source, Discipline::Ordered, LIMITS).unwrap();
        // Each member's clock: an offset, a rate within ±PHI and timers late by up to 60 µs, drawn
        // through the world from a stream of the member's own, so a run is its seed or its trace.
        for id in 0..NODES {
            let clocks = world.stream("clock", &[u64::from(id)]).unwrap();
            let rate = i32::try_from(world.below(clocks, 2 * PHI_PPM + 1).unwrap()).unwrap()
                - PHI_PPM as i32;
            let clock = Clock {
                offset_ns: world.below(clocks, 1 << 40).unwrap(),
                rate_ppm: rate,
                wall_ns: 0,
                lateness: Lateness {
                    floor_ns: 1_000,
                    spread_ns: 60_000,
                },
            };
            assert_eq!(world.node(clock).unwrap(), NodeId(id));
        }
        let mut net = Net::new(NET);
        net.set_path(Path::LAN);
        let members = (0..NODES).map(|id| Member::new(NodeId(id))).collect();
        let mut sim = Self {
            world,
            net,
            members,
            outbox: Vec::new(),
        };
        for id in 0..NODES {
            sim.step(NodeId(id));
        }
        sim
    }

    /// Polls member `node`, sends what it queued and arms its timer at its detector's wake.
    fn step(&mut self, node: NodeId) {
        let now = self.world.monotonic(node).unwrap();
        let member = &mut self.members[node.0 as usize];
        member.step(node, now, &mut self.outbox);
        member.note_deaths(node, now);
        let wake = member.detector.wake();
        self.world.wake(node, wake).unwrap();
        self.flush();
    }

    fn flush(&mut self) {
        for (from, to, bytes) in std::mem::take(&mut self.outbox) {
            let length = bytes.len();
            self.net
                .send(&mut self.world, (from, to), bytes, length, Ev::Arrive)
                .unwrap();
        }
    }

    /// Runs until `done`, within the world's step budget.
    fn run(&mut self, what: &str, done: impl Fn(&Self) -> bool) {
        let mut strategy = Random;
        while !done(self) {
            match self.world.next(&mut strategy).unwrap() {
                Step::Wake { node } => {
                    if self.members[node.0 as usize].alive {
                        self.step(node);
                    }
                }
                Step::Event {
                    node,
                    event: Ev::Arrive(ticket),
                } => {
                    let delivered = self
                        .net
                        .deliver(&mut self.world, ticket, Ev::Arrive)
                        .unwrap();
                    if let Some(delivery) = delivered
                        && self.members[node.0 as usize].alive
                    {
                        let stamp = self.world.monotonic(node).unwrap();
                        let member = &mut self.members[node.0 as usize];
                        if let Ok(message) = SwimMessage::decode(&delivery.payload) {
                            member.handle(node, message, stamp, &mut self.outbox);
                        }
                        self.step(node);
                    }
                }
                Step::Idle => panic!("{what}: nothing pending"),
                Step::Spent => panic!("{what}: the step budget is spent"),
            }
        }
    }

    /// Whether every live member judges every live peer by a verdict, the pair's own or the
    /// pool's.
    fn every_pair_judged(&self) -> bool {
        self.live().all(|(me, member)| {
            self.live()
                .filter(|(peer, _)| *peer != me)
                .all(|(peer, _)| member.detector.verdict(host(peer)).is_some())
        })
    }

    fn live(&self) -> impl Iterator<Item = (NodeId, &Member)> {
        self.members
            .iter()
            .enumerate()
            .filter(|(_, member)| member.alive)
            .map(|(id, member)| (NodeId(id as u32), member))
    }

    fn kill(&mut self, victim: NodeId) {
        self.members[victim.0 as usize].alive = false;
        self.world.wake(victim, None).unwrap();
    }

    /// Whether every survivor holds `victim` dead.
    fn held_dead(&self, victim: NodeId) -> bool {
        self.live()
            .all(|(_, member)| member.deaths.contains_key(&host(victim).0))
    }
}

/// One seed's run: judged, a member killed, every survivor holding it dead. The survivors' deaths
/// noted, and the digest of the run.
/// One run from `source`: judged, a member killed, every survivor holding it dead within its
/// bound and none holding a live one dead. The run's record.
fn run(source: Source) -> Result<Record, SimError> {
    let seed = match &source {
        Source::Seed(seed) => format!("seed {seed}"),
        Source::Trace(_) => "the trace".to_owned(),
    };
    let mut sim = Sim::new(source);
    sim.run("every pair judged", Sim::every_pair_judged);
    for (me, member) in sim.live() {
        assert!(
            member.deaths.is_empty(),
            "{seed}: member {me:?} holds a live member dead"
        );
    }
    let victim = NodeId(VICTIM);
    sim.kill(victim);
    sim.run("the victim held dead by every survivor", |sim| {
        sim.held_dead(victim)
    });
    let mut noted = Vec::new();
    for (me, member) in sim.live() {
        for peer in member.deaths.keys() {
            assert_eq!(
                *peer,
                host(victim).0,
                "{seed}: member {me:?} holds live member {peer} dead"
            );
        }
        let death = member.deaths[&host(victim).0];
        let within = death
            .within
            .unwrap_or_else(|| panic!("{seed}: member {me:?} stated no bound"));
        assert!(
            death.after <= within,
            "{seed}: member {me:?} held the victim dead {:?} after its last answer, past its \
             stated bound {within:?}",
            death.after
        );
        assert!(
            death.after > Duration::ZERO,
            "{seed}: held dead after its last answer"
        );
        noted.push((me.0, death));
    }
    assert_eq!(noted.len(), (NODES - 1) as usize);
    Ok(sim.world.finish())
}

#[test]
fn a_killed_member_is_held_dead_by_every_survivor_within_its_bound_and_no_live_one_is() {
    // Every seed through the run-twice check (docs/sim.md §3.9): twice from its seed and once from
    // its trace, to one digest.
    for seed in 0..16 {
        twice(seed, run).unwrap_or_else(|refusal| panic!("seed {seed}: {refusal}"));
    }
}
