//! slates-cluster's detector at `5cce86a`, driven through its own API: the same period, with the
//! owned batches, coordinates and messages that API hands out.

use slates_cluster::detector::{Detector, DetectorTiming};
use slates_cluster::membership::{Liveness, MemberState};
use slates_cluster::swim::SwimMessage;
use slates_db::register::HostId;

use crate::{Cluster, GOSSIP_PER_MESSAGE, RTT, transmits};

pub struct Members {
    detectors: Vec<Detector>,
}

impl Cluster for Members {
    fn new(members: usize) -> Self {
        let timing = DetectorTiming {
            suspicion_periods: 6,
            gossip_transmits: transmits(members),
            health_max: 8,
            suspicion_min: 2,
            confirmations_expected: 3,
        };
        let detectors = (0..members as u64)
            .map(|id| {
                let mut detector = Detector::new(HostId(id), timing);
                for peer in 0..members as u64 {
                    detector.join(HostId(peer));
                }
                detector
            })
            .collect();
        Self { detectors }
    }

    fn period(&mut self, nonce: u64) {
        let detectors = &mut self.detectors;
        for prober in 0..detectors.len() {
            let Some(ping) = detectors[prober].tick() else {
                continue;
            };
            let target = ping.to.0 as usize;
            let bytes = SwimMessage::Ping {
                from: HostId(prober as u64),
                nonce,
                boot_nonce: 1,
                configuration_version: 1,
                gossip: detectors[prober].ping_gossip(ping.to, GOSSIP_PER_MESSAGE),
            }
            .encode();
            let SwimMessage::Ping { from, gossip, .. } = SwimMessage::decode(&bytes).unwrap()
            else {
                unreachable!()
            };
            let answering = &mut detectors[target];
            answering.apply_gossip_from(from, &gossip);
            let bytes = SwimMessage::Ack {
                from: ping.to,
                nonce,
                boot_nonce: 1,
                configuration_version: 1,
                standing: None,
                gossip: answering.gossip(GOSSIP_PER_MESSAGE),
                coordinate: answering.coordinate(),
            }
            .encode();
            let SwimMessage::Ack {
                from,
                gossip,
                coordinate,
                ..
            } = SwimMessage::decode(&bytes).unwrap()
            else {
                unreachable!()
            };
            let probing = &mut detectors[prober];
            probing.apply_gossip_from(from, &gossip);
            probing.on_ack(from);
            probing.learn_coordinate(from, coordinate);
            probing.observe_rtt(from, RTT);
        }
    }

    fn churn(&mut self, at: u64) {
        let member = (at as usize) % self.detectors.len();
        let detector = &mut self.detectors[member];
        let incarnation = detector.membership().local_incarnation();
        detector.apply(
            HostId(member as u64),
            MemberState {
                liveness: Liveness::Suspect,
                incarnation,
            },
        );
    }

    fn alive(&self) -> usize {
        self.detectors
            .iter()
            .map(|detector| detector.membership().alive().len())
            .min()
            .unwrap_or(0)
    }
}
