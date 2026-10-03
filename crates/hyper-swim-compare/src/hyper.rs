//! hyper-swim, driven as `crates/hyper-swim/benches/allocs.rs` drives it: each member on its own
//! simulated clock, polled at the wake its detector asks, measuring the round trips it is
//! answered in. A cluster is built and run until every pair is configured by its own estimator.

use std::num::NonZeroUsize;

use hyper_datagram::{LENGTH_BYTES, OVERHEAD_BYTES};
use hyper_swim::HostId;
use hyper_swim::codec::{Coordinate, GossipBatch, SwimMessage, gossip_capacity};
use hyper_swim::coordinates::NetworkCoordinate;
use hyper_swim::detector::{Detector, Ping, PingReq};
use hyper_swim::membership::{Liveness, MemberState};
use hyper_timing::Exposure;

use crate::Cluster;

/// The path's datagram size: QUIC's minimum (RFC 9000 §14.1), as hyper-swim's `tests/cluster.rs`
/// sets it.
const DATAGRAM: usize = 1_200;
/// The workload's round trip: 200 µs, a LAN's.
const RTT_NS: u64 = 200_000;
/// The workload's jitter: up to half the round trip more, from a xorshift stream.
const JITTER_NS: u64 = RTT_NS / 2;
/// How late each wake comes: Linux's default timer slack, 50 µs (`PR_SET_TIMERSLACK(2const)`).
const LATE_NS: u64 = 50_000;

/// One member: its detector and its own clock.
struct Member {
    detector: Detector,
    now: u64,
    /// The acknowledgement in flight: when it lands.
    landing: Option<u64>,
}

/// Gossip entries a message carries: what the datagram holds beside an acknowledgement, as
/// `tests/cluster.rs` derives it.
pub fn gossip_per_message() -> usize {
    let mut bare = Vec::new();
    SwimMessage::Ack {
        from: HostId(0),
        nonce: u64::MAX,
        boot_nonce: 1,
        configuration_version: 1,
        standing: None,
        gossip: GossipBatch::Entries(&[]),
        coordinate: Coordinate::Held(&NetworkCoordinate::origin()),
    }
    .encode_into(&mut bare);
    gossip_capacity(DATAGRAM - OVERHEAD_BYTES - LENGTH_BYTES, bare.len())
}

/// What a member's driver holds across periods: the gossip batch it fills, the bytes it encodes,
/// the relays it is asked for, and the workload's noise.
struct Buffers {
    gossip: usize,
    batch: Vec<(HostId, MemberState)>,
    ping: Vec<u8>,
    ack: Vec<u8>,
    requests: Vec<PingReq>,
    noise: u64,
}

impl Buffers {
    fn new() -> Self {
        Self {
            gossip: gossip_per_message(),
            batch: Vec::new(),
            ping: Vec::new(),
            ack: Vec::new(),
            requests: Vec::new(),
            noise: 0x2545_F491_4F6C_DD1D,
        }
    }

    /// A xorshift step (Marsaglia 2003).
    fn round_trip(&mut self) -> u64 {
        self.noise ^= self.noise << 13;
        self.noise ^= self.noise >> 7;
        self.noise ^= self.noise << 17;
        RTT_NS + self.noise % JITTER_NS
    }
}

fn cluster(members: usize) -> Vec<Member> {
    (0..members as u64)
        .map(|id| {
            let mut detector = Detector::new(
                HostId(id),
                Exposure::new(),
                NonZeroUsize::new(members).unwrap(),
            );
            for peer in 0..members as u64 {
                detector.join(HostId(peer)).unwrap();
            }
            Member {
                detector,
                now: 1,
                landing: None,
            }
        })
        .collect()
}

/// Advances member `prober` to its next period and runs the probe it starts: the ping through
/// the codec to its target, which applies its gossip and answers, and the acknowledgement back.
fn step(members: &mut [Member], prober: usize, buffers: &mut Buffers) {
    let ping = loop {
        let member = &mut members[prober];
        let at = match (member.detector.wake(), member.landing) {
            (Some(wake), _) => wake + LATE_NS,
            (None, Some(landing)) => landing,
            (None, None) => member.now,
        };
        member.now = member.now.max(at);
        if let Some(ping) = member.detector.poll(member.now, &mut buffers.requests) {
            break ping;
        }
    };
    exchange(members, prober, ping, buffers);
}

fn exchange(members: &mut [Member], prober: usize, ping: Ping, buffers: &mut Buffers) {
    let target = ping.to.0 as usize;
    let landing = members[prober].now + buffers.round_trip();
    members[prober]
        .detector
        .ping_gossip_into(ping.to, buffers.gossip, &mut buffers.batch);
    SwimMessage::Ping {
        from: HostId(prober as u64),
        nonce: ping.nonce,
        boot_nonce: 1,
        configuration_version: 1,
        gossip: GossipBatch::Entries(&buffers.batch),
    }
    .encode_into(&mut buffers.ping);
    let SwimMessage::Ping {
        from, gossip, nonce, ..
    } = SwimMessage::decode(&buffers.ping).unwrap()
    else {
        unreachable!()
    };
    let answering = &mut members[target].detector;
    answering.apply_gossip(gossip);
    let ack = answering.on_ping(from);
    answering.gossip_into(buffers.gossip, &mut buffers.batch);
    SwimMessage::Ack {
        from: ping.to,
        nonce,
        boot_nonce: 1,
        configuration_version: 1,
        standing: None,
        gossip: GossipBatch::Entries(&buffers.batch),
        coordinate: Coordinate::Held(answering.coordinate()),
    }
    .encode_into(&mut buffers.ack);
    let SwimMessage::Ack {
        from,
        gossip,
        coordinate,
        nonce,
        ..
    } = SwimMessage::decode(&buffers.ack).unwrap()
    else {
        unreachable!()
    };
    let probing = &mut members[prober];
    probing.detector.apply_gossip(gossip);
    probing.detector.learn_coordinate(from, coordinate);
    probing.detector.on_ack(from, nonce, landing);
    probing.landing = Some(landing);
    let _ = ack;
}

/// One period of every member.
fn period(members: &mut [Member], buffers: &mut Buffers) {
    for prober in 0..members.len() {
        step(members, prober, buffers);
    }
}

/// Runs whole rounds until every member's every pair is configured by its own estimator: the
/// fact the counted periods start from.
fn warm(members: &mut [Member], buffers: &mut Buffers) {
    let n = members.len() as u64;
    let configured = |members: &[Member]| {
        members.iter().enumerate().all(|(id, member)| {
            (0..n)
                .filter(|peer| *peer != id as u64)
                .all(|peer| {
                    member
                        .detector
                        .report(HostId(peer))
                        .is_some_and(|report| report.configured)
                })
        })
    };
    while !configured(members) {
        for _ in 0..n {
            period(members, buffers);
        }
    }
    // A further round for every report the configuration's gossip queued to drain.
    for _ in 0..n {
        period(members, buffers);
    }
}

pub struct Members {
    members: Vec<Member>,
    buffers: Buffers,
}

impl Cluster for Members {
    fn new(members: usize) -> Self {
        let mut members = cluster(members);
        let mut buffers = Buffers::new();
        warm(&mut members, &mut buffers);
        Self { members, buffers }
    }

    fn period(&mut self, _nonce: u64) {
        period(&mut self.members, &mut self.buffers);
    }

    fn churn(&mut self, at: u64) {
        let member = (at as usize) % self.members.len();
        let detector = &mut self.members[member].detector;
        let incarnation = detector.membership().local_incarnation();
        detector
            .apply(
                HostId(member as u64),
                MemberState {
                    liveness: Liveness::Suspect,
                    incarnation,
                },
            )
            .unwrap();
    }

    fn alive(&self) -> usize {
        self.members
            .iter()
            .map(|member| member.detector.membership().alive().count())
            .min()
            .unwrap_or(0)
    }
}
