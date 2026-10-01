//! SWIM between real processes over the sealed datagram plane on real UDP sockets: the usage
//! mantle, focal and slates make of it.
//!
//! The supervisor (`a_killed_member_is_declared_dead_by_every_survivor_and_no_live_one_is`)
//! starts `NODES` copies of this test binary as member processes (`member_process`, selected by
//! `HYPER_SWIM_NODE`). Each runs the detector each period over its own socket and prints its view
//! on stdout. The supervisor kills one with SIGKILL once the members have settled, then waits on
//! the fact it needs: every survivor reports the killed member dead and no live member suspected,
//! within a bound in periods derived from the detector's timing.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::disallowed_macros,
    clippy::cast_possible_truncation,
    clippy::print_stdout,
    clippy::cognitive_complexity,
    missing_docs
)]

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::UdpSocket;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use hyper_datagram::{AdmitAll, ExporterSecret, Plane, PlaneLimits, Role, SECRET_BYTES};
use hyper_swim::HostId;
use hyper_swim::codec::{Coordinate, GossipBatch, SwimMessage};
use hyper_swim::detector::{Detector, DetectorTiming, PingReq};
use hyper_swim::membership::{Liveness, MemberState};

/// Members in the test cluster.
const NODES: u64 = 4;
/// The member the supervisor kills.
const VICTIM: u64 = NODES;
/// The protocol period. A test parameter: loopback round trips are well under a millisecond, so
/// any period of at least three of them satisfies node.md §3.5; this one leaves the processes idle
/// most of the time.
const PERIOD: Duration = Duration::from_millis(40);
/// The detector's timing for the test, each a test parameter of the shape the owner derives.
const TIMING: DetectorTiming = DetectorTiming {
    suspicion_periods: 3,
    // ⌈λ·ln(n+1)⌉ with λ = 3 and n = 4 (SWIM §4.4's infection-style dissemination).
    gossip_transmits: 5,
    health_max: 2,
    suspicion_min: 2,
    confirmations_expected: 2,
};
/// Gossip entries piggybacked per message.
const GOSSIP_PER_MESSAGE: usize = 8;
/// Members asked to probe a target that did not answer directly (SWIM §4.1's k): every other
/// member, which in a cluster this small is every relay there is.
const INDIRECT_FANOUT: usize = NODES as usize;
/// The longest a member may take, after the victim dies, to report it dead, in periods:
/// - a round to probe it (`NODES`);
/// - the widest suspicion window (`suspicion_periods × (health_max + 1)`);
/// - a gossip transmission to every member (`gossip_transmits × NODES`).
const DETECTION_BOUND_PERIODS: u64 = NODES
    + TIMING.suspicion_periods as u64 * (TIMING.health_max as u64 + 1)
    + TIMING.gossip_transmits as u64 * NODES;
/// Periods the supervisor lets the members settle before it kills the victim: every member
/// probed twice.
const SETTLE_PERIODS: u64 = 2 * NODES;
/// The most periods a member runs, and the supervisor waits, before the test fails: four times
/// everything the test needs, so only a member that stopped reporting runs out.
const RUN_PERIODS: u64 = 4 * (SETTLE_PERIODS + DETECTION_BOUND_PERIODS + NODES);

fn secret_between(a: u64, b: u64) -> ExporterSecret {
    // Stands for the QUIC exporter both ends of a connection compute: one secret per pair.
    let (low, high) = (a.min(b), a.max(b));
    let mut bytes = [0u8; SECRET_BYTES];
    bytes[..8].copy_from_slice(&low.to_le_bytes());
    bytes[8..16].copy_from_slice(&high.to_le_bytes());
    ExporterSecret::new(bytes)
}

fn liveness_letter(liveness: Liveness) -> char {
    match liveness {
        Liveness::Alive => 'A',
        Liveness::Suspect => 'S',
        Liveness::Dead => 'D',
    }
}

/// One member process: runs until it is killed.
#[test]
fn member_process() {
    let Ok(me) = std::env::var("HYPER_SWIM_NODE") else {
        return;
    };
    let me: u64 = me.parse().unwrap();
    let ports: Vec<u16> = std::env::var("HYPER_SWIM_PORTS")
        .unwrap()
        .split(',')
        .map(|port| port.parse().unwrap())
        .collect();
    let address = move |id: u64| format!("127.0.0.1:{}", ports[(id - 1) as usize]);
    let socket = UdpSocket::bind(address(me)).unwrap();

    let limits = PlaneLimits {
        max_peers: NODES as usize,
        epochs_per_peer: 2,
        window_limit: 1_024,
    };
    let mut plane = Plane::new(me, limits).unwrap();
    let mut detector = Detector::new(HostId(me), TIMING);
    for peer in (1..=NODES).filter(|peer| *peer != me) {
        let role = if me < peer {
            Role::Initiator
        } else {
            Role::Acceptor
        };
        plane
            .install_epoch(peer, 1, &secret_between(me, peer), role)
            .unwrap();
        plane.set_path(peer, 1_200).unwrap();
        detector.join(HostId(peer));
    }

    // Ready, then wait for the supervisor's start: a member joins its peers only once they are up,
    // as a real member joins a peer once its transport connection to it exists.
    let mut stdout = std::io::stdout();
    writeln!(stdout, "ready {me}").unwrap();
    stdout.flush().unwrap();
    let mut start = String::new();
    std::io::stdin().read_line(&mut start).unwrap();

    let mut member = Member {
        me,
        socket,
        plane,
        detector,
        address: Box::new(address),
        nonce: 0,
        buffer: vec![0u8; 2_048],
        batch: Vec::new(),
        encoded: Vec::new(),
        requests: Vec::new(),
        relaying: BTreeMap::new(),
    };
    let mut due = Instant::now();
    for period in 0..RUN_PERIODS {
        // The member's own lag: how late this period began against when it was due, as a
        // starved process's would be (node.md §3.5); the detector dilates its suspicion by it.
        let began = Instant::now();
        member.detector.observe_self_lag(
            u64::try_from(began.saturating_duration_since(due).as_nanos()).unwrap(),
            u64::try_from(PERIOD.as_nanos()).unwrap(),
        );
        let deadline = began + PERIOD;
        due = deadline;
        member.probe();
        // SWIM §4.1: a probe unanswered by half the period is retried through other members.
        member.receive_until(deadline - PERIOD / 2);
        member.probe_indirectly();
        member.receive_until(deadline);
        let view: String = (1..=NODES)
            .filter(|peer| *peer != me)
            .map(|peer| {
                let state = member.detector.membership().state(HostId(peer)).unwrap();
                format!("{peer}{}", liveness_letter(state.liveness))
            })
            .collect::<Vec<_>>()
            .join(" ");
        writeln!(stdout, "{me} {period} {view}").unwrap();
        stdout.flush().unwrap();
    }
}

/// One member's driver: its socket, plane and detector, and the probes it relays.
struct Member {
    me: u64,
    socket: UdpSocket,
    plane: Plane,
    detector: Detector,
    address: Box<dyn Fn(u64) -> String>,
    nonce: u64,
    buffer: Vec<u8>,
    batch: Vec<(HostId, MemberState)>,
    encoded: Vec<u8>,
    requests: Vec<PingReq>,
    /// Probes this member makes for others: the target, and who asked with which nonce. One
    /// a target, so bounded by the membership.
    relaying: BTreeMap<u64, (HostId, u64)>,
}

impl Member {
    fn send(&mut self, to: u64, message: &SwimMessage<'_>) {
        message.encode_into(&mut self.encoded);
        let _ = self.plane.queue(to, &self.encoded);
    }

    /// The period's direct probe.
    fn probe(&mut self) {
        if let Some(ping) = self.detector.tick() {
            self.nonce += 1;
            let mut batch = std::mem::take(&mut self.batch);
            self.detector
                .ping_gossip_into(ping.to, GOSSIP_PER_MESSAGE, &mut batch);
            self.send(
                ping.to.0,
                &SwimMessage::Ping {
                    from: HostId(self.me),
                    nonce: self.nonce,
                    boot_nonce: self.me,
                    configuration_version: 0,
                    gossip: GossipBatch::Entries(&batch),
                },
            );
            self.batch = batch;
        }
        flush(&mut self.plane, &self.socket, &*self.address);
    }

    /// Asks every other member to probe a target that has not answered.
    fn probe_indirectly(&mut self) {
        let mut requests = std::mem::take(&mut self.requests);
        self.detector
            .request_indirect_into(INDIRECT_FANOUT, &mut requests);
        for request in &requests {
            self.send(
                request.relay.0,
                &SwimMessage::PingReq {
                    from: HostId(self.me),
                    target: request.target,
                    nonce: self.nonce,
                    gossip: GossipBatch::Entries(&[]),
                },
            );
        }
        self.requests = requests;
        flush(&mut self.plane, &self.socket, &*self.address);
    }

    /// Reads and answers until `until`. Only the timeout ends it: any other error (Windows
    /// reports a reset on the next receive after a send to a closed port) skips that datagram.
    fn receive_until(&mut self, until: Instant) {
        while let Some(remaining) = until.checked_duration_since(Instant::now()) {
            if remaining.is_zero() {
                break;
            }
            self.socket.set_read_timeout(Some(remaining)).unwrap();
            let length = match self.socket.recv_from(&mut self.buffer) {
                Ok((length, _)) => length,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    break;
                }
                Err(_) => continue,
            };
            let mut buffer = std::mem::take(&mut self.buffer);
            if let Ok(opened) = self.plane.open(&mut buffer[..length], &AdmitAll) {
                // The messages borrow the datagram, not the plane, so answers queue as they are
                // read.
                for bytes in opened.messages() {
                    self.handle(SwimMessage::decode(bytes).unwrap());
                }
            }
            self.buffer = buffer;
            flush(&mut self.plane, &self.socket, &*self.address);
        }
    }

    fn handle(&mut self, message: SwimMessage<'_>) {
        match message {
            SwimMessage::Ping {
                from,
                nonce,
                gossip,
                ..
            } => {
                self.detector.apply_gossip_from(from, gossip);
                let mut batch = std::mem::take(&mut self.batch);
                self.detector.gossip_into(GOSSIP_PER_MESSAGE, &mut batch);
                let coordinate = self.detector.coordinate().clone();
                self.send(
                    from.0,
                    &SwimMessage::Ack {
                        from: HostId(self.me),
                        nonce,
                        boot_nonce: self.me,
                        configuration_version: 0,
                        standing: None,
                        gossip: GossipBatch::Entries(&batch),
                        coordinate: Coordinate::Held(&coordinate),
                    },
                );
                self.batch = batch;
            }
            SwimMessage::Ack { from, gossip, .. } => {
                self.detector.apply_gossip_from(from, gossip);
                // An answer to a probe made for another member goes back to it.
                if let Some((asker, nonce)) = self.relaying.remove(&from.0) {
                    self.send(
                        asker.0,
                        &SwimMessage::IndirectAck {
                            from: HostId(self.me),
                            target: from,
                            nonce,
                            boot_nonce: self.me,
                            gossip: GossipBatch::Entries(&[]),
                        },
                    );
                }
                self.detector.on_ack(from);
            }
            SwimMessage::PingReq {
                from,
                target,
                nonce,
                gossip,
            } => {
                self.detector.apply_gossip_from(from, gossip);
                self.relaying.insert(target.0, (from, nonce));
                self.send(
                    target.0,
                    &SwimMessage::Ping {
                        from: HostId(self.me),
                        nonce,
                        boot_nonce: self.me,
                        configuration_version: 0,
                        gossip: GossipBatch::Entries(&[]),
                    },
                );
            }
            SwimMessage::IndirectAck {
                target,
                gossip,
                from,
                ..
            } => {
                self.detector.apply_gossip_from(from, gossip);
                self.detector.on_indirect_ack(target);
            }
        }
    }
}

fn flush(plane: &mut Plane, socket: &UdpSocket, address: &dyn Fn(u64) -> String) {
    plane.flush(|peer, datagram: Result<&[u8], hyper_datagram::Refusal>| {
        socket.send_to(datagram.unwrap(), address(peer)).unwrap();
    });
}

/// Free loopback ports, one per member: bound, read and released.
fn free_ports() -> Vec<u16> {
    let sockets: Vec<UdpSocket> = (0..NODES)
        .map(|_| UdpSocket::bind("127.0.0.1:0").unwrap())
        .collect();
    sockets
        .iter()
        .map(|socket| socket.local_addr().unwrap().port())
        .collect()
}

#[test]
fn a_killed_member_is_declared_dead_by_every_survivor_and_no_live_one_is() {
    if std::env::var("HYPER_SWIM_NODE").is_ok() {
        return;
    }
    let ports = free_ports();
    let ports = ports
        .iter()
        .map(u16::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let mut members: BTreeMap<u64, Child> = (1..=NODES)
        .map(|id| {
            let child = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "member_process",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env("HYPER_SWIM_NODE", id.to_string())
                .env("HYPER_SWIM_PORTS", &ports)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();
            (id, child)
        })
        .collect();
    let (lines, receiver) = std::sync::mpsc::channel::<String>();
    for child in members.values_mut() {
        let stdout = child.stdout.take().unwrap();
        let lines = lines.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if lines.send(line).is_err() {
                    break;
                }
            }
        });
    }

    // Start every member together once all have bound their sockets.
    let mut ready = 0;
    while ready < NODES {
        let line = receiver
            .recv_timeout(PERIOD * 1_000)
            .expect("members never became ready");
        // libtest prints "test member_process ... " before the body runs, without a newline.
        if line.contains("ready ") {
            ready += 1;
        }
    }
    for child in members.values_mut() {
        writeln!(child.stdin.as_mut().unwrap(), "start").unwrap();
    }

    // Views as reported: member → (period, view of each peer).
    let mut views: BTreeMap<u64, (u64, BTreeMap<u64, char>)> = BTreeMap::new();
    let mut killed_at: Option<u64> = None;
    let mut detected: BTreeMap<u64, u64> = BTreeMap::new();
    let wait = PERIOD * u32::try_from(RUN_PERIODS).unwrap();
    let started = Instant::now();
    while detected.len() < (NODES - 1) as usize {
        let line = receiver
            .recv_timeout(wait.saturating_sub(started.elapsed()))
            .expect("members stopped reporting");
        let mut fields = line.split(' ');
        let (Some(member), Some(period)) = (fields.next(), fields.next()) else {
            continue;
        };
        let (Ok(member), Ok(period)) = (member.parse::<u64>(), period.parse::<u64>()) else {
            continue;
        };
        let view: BTreeMap<u64, char> = fields
            .filter_map(|entry| {
                let (peer, state) = entry.split_at(entry.len() - 1);
                Some((peer.parse().ok()?, state.chars().next()?))
            })
            .collect();
        for (peer, state) in &view {
            if *peer != VICTIM {
                assert_eq!(
                    *state, 'A',
                    "member {member} saw live member {peer} as {state} in period {period}"
                );
            }
        }
        views.insert(member, (period, view.clone()));
        if killed_at.is_none() && period >= SETTLE_PERIODS && member != VICTIM {
            let mut victim = members.remove(&VICTIM).unwrap();
            victim.kill().unwrap();
            victim.wait().unwrap();
            killed_at = Some(period);
        }
        if let Some(killed) = killed_at
            && member != VICTIM
            && view.get(&VICTIM) == Some(&'D')
        {
            detected
                .entry(member)
                .or_insert(period.saturating_sub(killed));
        }
    }
    for child in members.values_mut() {
        child.kill().unwrap();
        child.wait().unwrap();
    }
    for (member, periods) in &detected {
        assert!(
            *periods <= DETECTION_BOUND_PERIODS,
            "member {member} took {periods} periods, past the bound of {DETECTION_BOUND_PERIODS}"
        );
    }
    println!(
        "detection, in periods after the kill: {detected:?} (bound {DETECTION_BOUND_PERIODS})"
    );
}
