//! What a protocol period costs the allocator and the page tables: `cargo bench -p hyper-swim
//! --bench allocs` (docs/benchmarks.md, "hyper-swim").
//!
//! `N` detectors in one process run whole periods as a member's driver does (`tests/cluster.rs`):
//! each ticks, sends its probe target a ping carrying gossip, the target applies it and answers
//! with an acknowledgement carrying its own gossip and coordinate, and the prober applies that,
//! credits the probe and folds the round trip into its coordinate. Every message goes through the
//! wire codec. The count is per member per period, after the cluster has converged: quiet (no
//! membership changes) and churning (one member refutes a suspicion every period, so its new
//! incarnation spreads through the gossip).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::disallowed_macros,
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::indexing_slicing,
    clippy::unreachable,
    missing_docs
)]

use std::num::NonZeroUsize;

use hyper_datagram::{LENGTH_BYTES, OVERHEAD_BYTES};
use hyper_measure::{alloc, faults};
use hyper_swim::HostId;
use hyper_swim::codec::{Coordinate, GossipBatch, SwimMessage, gossip_capacity};
use hyper_swim::coordinates::NetworkCoordinate;
use hyper_swim::detector::{Detector, Ping, PingReq};
use hyper_swim::membership::{Liveness, MemberState};
use hyper_timing::Exposure;

#[global_allocator]
static ALLOCATOR: alloc::Counting = alloc::Counting;

/// Periods counted per row, after the warm-up.
const PERIODS: u64 = 400;
/// The path's datagram size: QUIC's minimum (RFC 9000 §14.1), as `tests/cluster.rs` sets it.
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
fn gossip_per_message() -> usize {
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
        from,
        gossip,
        nonce,
        ..
    } = SwimMessage::decode(&buffers.ping).unwrap()
    else {
        unreachable!()
    };
    let answering = &mut members[target].detector;
    answering.apply_gossip(gossip);
    let ack = answering.on_ping(from);
    answering.ack_gossip_into(from, buffers.gossip, &mut buffers.batch);
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
            (0..n).filter(|peer| *peer != id as u64).all(|peer| {
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

/// One member hears itself suspected and refutes, so a new incarnation spreads.
fn churn(members: &mut [Member], at: u64) {
    let member = (at as usize) % members.len();
    let detector = &mut members[member].detector;
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

struct Cost {
    allocations: f64,
    reallocations: f64,
    bytes: f64,
    faults: f64,
}

fn counted(n: u64, work: impl FnOnce()) -> Cost {
    let before = faults::read().unwrap();
    alloc::begin();
    work();
    let counts = alloc::end();
    let after = faults::read().unwrap();
    let n = n as f64;
    Cost {
        allocations: counts.allocations as f64 / n,
        reallocations: counts.reallocations as f64 / n,
        bytes: counts.bytes as f64 / n,
        faults: after.since(&before).minor as f64 / n,
    }
}

fn row(what: &str, members: usize, cost: &Cost) {
    println!(
        "  {what:<12} {members:>8} {:>10.2} {:>10.2} {:>10.1} {:>10.4}",
        cost.allocations, cost.reallocations, cost.bytes, cost.faults
    );
}

fn point(members: usize) {
    let mut cluster = cluster(members);
    let mut buffers = Buffers::new();
    warm(&mut cluster, &mut buffers);
    let n = PERIODS * members as u64;
    let cost = counted(n, || {
        for _ in 0..PERIODS {
            period(&mut cluster, &mut buffers);
        }
    });
    row("quiet", members, &cost);
    let mut at = 0;
    let cost = counted(n, || {
        for _ in 0..PERIODS {
            at += 1;
            churn(&mut cluster, at);
            period(&mut cluster, &mut buffers);
        }
    });
    row("churning", members, &cost);
    for member in &cluster {
        assert_eq!(
            member.detector.membership().alive().count(),
            members,
            "every member stays alive"
        );
    }
}

fn main() {
    assert!(alloc::installed(), "the counting allocator is installed");
    println!(
        "hyper-swim: allocations, reallocations, bytes asked and minor faults per member per \
         period ({PERIODS} periods, after every pair is configured); {} gossip entries a message",
        gossip_per_message()
    );
    println!(
        "  {:<12} {:>8} {:>10} {:>10} {:>10} {:>10}",
        "", "members", "allocs", "reallocs", "bytes", "faults"
    );
    for members in [4usize, 16, 64, 256] {
        point(members);
    }
}
