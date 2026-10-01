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

use hyper_measure::{alloc, faults};
use hyper_swim::HostId;
use hyper_swim::codec::{Coordinate, GossipBatch, SwimMessage};
use hyper_swim::detector::{Detector, DetectorTiming};
use hyper_swim::membership::{Liveness, MemberState};

#[global_allocator]
static ALLOCATOR: alloc::Counting = alloc::Counting;

/// Periods counted per row, after the warm-up periods.
const PERIODS: u64 = 400;
/// Gossip entries a message carries at most, as `tests/cluster.rs` sends.
const GOSSIP_PER_MESSAGE: usize = 8;
/// A round trip to fold into the coordinate, in seconds: any constant serves, the count is the
/// same.
const RTT: f64 = 0.000_2;

/// The transmit budget λ·ln(n+1) for λ = 3 (SWIM §4.4).
fn transmits(members: usize) -> u32 {
    (3.0 * ((members + 1) as f64).ln()).ceil() as u32
}

/// Periods run before counting: twice what the joins take to drain. Each member holds a report of
/// every member, sent `transmits` times, and sends at most two batches a period (its ping and the
/// acknowledgement of the ping it receives).
fn warm(members: usize) -> u64 {
    let reports = (members * transmits(members) as usize) as u64;
    2 * reports.div_ceil(2 * GOSSIP_PER_MESSAGE as u64)
}

fn timing(members: usize) -> DetectorTiming {
    // The cluster test's timing.
    let transmits = transmits(members);
    DetectorTiming {
        suspicion_periods: 6,
        gossip_transmits: transmits,
        health_max: 8,
        suspicion_min: 2,
        confirmations_expected: 3,
    }
}

fn cluster(members: usize) -> Vec<Detector> {
    (0..members as u64)
        .map(|id| {
            let mut detector = Detector::new(HostId(id), timing(members));
            for peer in 0..members as u64 {
                detector.join(HostId(peer));
            }
            detector
        })
        .collect()
}

/// What a member's driver holds across periods: the gossip batch it fills and the bytes it encodes.
#[derive(Default)]
struct Buffers {
    batch: Vec<(HostId, MemberState)>,
    ping: Vec<u8>,
    ack: Vec<u8>,
}

fn period(detectors: &mut [Detector], buffers: &mut Buffers, nonce: u64) {
    for prober in 0..detectors.len() {
        let Some(ping) = detectors[prober].tick() else {
            continue;
        };
        let target = ping.to.0 as usize;
        detectors[prober].ping_gossip_into(ping.to, GOSSIP_PER_MESSAGE, &mut buffers.batch);
        let message = SwimMessage::Ping {
            from: HostId(prober as u64),
            nonce,
            boot_nonce: 1,
            configuration_version: 1,
            gossip: GossipBatch::Entries(&buffers.batch),
        };
        message.encode_into(&mut buffers.ping);
        let SwimMessage::Ping { from, gossip, .. } = SwimMessage::decode(&buffers.ping).unwrap()
        else {
            unreachable!()
        };
        let answering = &mut detectors[target];
        answering.apply_gossip_from(from, gossip);
        answering.gossip_into(GOSSIP_PER_MESSAGE, &mut buffers.batch);
        let ack = SwimMessage::Ack {
            from: ping.to,
            nonce,
            boot_nonce: 1,
            configuration_version: 1,
            standing: None,
            gossip: GossipBatch::Entries(&buffers.batch),
            coordinate: Coordinate::Held(answering.coordinate()),
        };
        ack.encode_into(&mut buffers.ack);
        let SwimMessage::Ack {
            from,
            gossip,
            coordinate,
            ..
        } = SwimMessage::decode(&buffers.ack).unwrap()
        else {
            unreachable!()
        };
        let probing = &mut detectors[prober];
        probing.apply_gossip_from(from, gossip);
        probing.on_ack(from);
        probing.learn_coordinate(from, coordinate);
        probing.observe_rtt(from, RTT);
    }
}

/// One member hears itself suspected and refutes, so a new incarnation spreads.
fn churn(detectors: &mut [Detector], at: u64) {
    let member = (at as usize) % detectors.len();
    let detector = &mut detectors[member];
    let incarnation = detector.membership().local_incarnation();
    detector.apply(
        HostId(member as u64),
        MemberState {
            liveness: Liveness::Suspect,
            incarnation,
        },
    );
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
    let mut detectors = cluster(members);
    let mut buffers = Buffers::default();
    let mut nonce = 0;
    for _ in 0..warm(members) {
        nonce += 1;
        period(&mut detectors, &mut buffers, nonce);
    }
    let n = PERIODS * members as u64;
    let cost = counted(n, || {
        for _ in 0..PERIODS {
            nonce += 1;
            period(&mut detectors, &mut buffers, nonce);
        }
    });
    row("quiet", members, &cost);
    let cost = counted(n, || {
        for _ in 0..PERIODS {
            nonce += 1;
            churn(&mut detectors, nonce);
            period(&mut detectors, &mut buffers, nonce);
        }
    });
    row("churning", members, &cost);
    for detector in &detectors {
        assert_eq!(
            detector.membership().alive().count(),
            members,
            "every member stays alive"
        );
    }
}

fn main() {
    assert!(alloc::installed(), "the counting allocator is installed");
    println!(
        "hyper-swim: allocations, reallocations, bytes asked and minor faults per member per \
         period ({PERIODS} periods, after twice the joins' drain)"
    );
    println!(
        "  {:<12} {:>8} {:>10} {:>10} {:>10} {:>10}",
        "", "members", "allocs", "reallocs", "bytes", "faults"
    );
    for members in [4usize, 16, 64, 256] {
        point(members);
    }
}
