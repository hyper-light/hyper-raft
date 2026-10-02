//! SWIM between real processes over the sealed datagram plane on real UDP sockets: the usage
//! mantle, focal and slates make of it.
//!
//! The supervisor (`a_killed_member_is_declared_dead_by_every_survivor_and_no_live_one_is`)
//! starts `NODES` copies of this test binary as member processes (`member_process`, selected by
//! `HYPER_SWIM_NODE`). Each runs the detector as the library configures it: it polls when the
//! detector asks, sends what the detector returns, and prints, every period, its view and what its
//! detector reports of each peer. The test times nothing of its own. The supervisor waits on
//! facts:
//! - every member judges every peer by a configured verdict, the pair's own or, while the pair's
//!   estimator refuses, the pool's; then it SIGKILLs one member;
//! - every survivor reports the victim dead, each within the detection bound its detector stated,
//!   with the wait it measured for evidence of its own health added where its condemnation was
//!   pending on it.
//!
//! What it asserts of live members is what the configured detectors promise (`docs/timing.md`
//! §2.7): Theorem 7 bounds the expected number of suspicions and condemnations of live members by
//! `Σβ` over every judged probe, and a run refutes that only when the 95 % lower limit of its count,
//! summed over the cluster, passes the sum of the allowances.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::disallowed_macros,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::print_stdout,
    clippy::cognitive_complexity,
    missing_docs
)]

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::UdpSocket;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use hyper_datagram::{
    AdmitAll, ExporterSecret, LENGTH_BYTES, OVERHEAD_BYTES, Plane, PlaneLimits, Role, SECRET_BYTES,
};
use hyper_swim::HostId;
use hyper_swim::codec::{Coordinate, GossipBatch, SwimMessage, gossip_capacity};
use hyper_swim::detector::{Detector, PingReq};
use hyper_swim::membership::{Liveness, MemberState};
use hyper_timing::Exposure;

/// Members in the test cluster: the victim, a prober, and two members it can ask to relay.
const NODES: u64 = 4;
/// The member the supervisor kills.
const VICTIM: u64 = NODES;
/// The path's datagram size: QUIC's minimum, which every path carries (RFC 9000 §14.1).
const DATAGRAM: usize = 1_200;
/// The normal distribution's two-sided 95 % point, for the Poisson score interval with which the
/// trace analyser and `hyper-timing`'s replay refute Theorem 7 (Brown, Cai and DasGupta 2003;
/// `docs/timing.md` §2.6).
const Z95: f64 = 1.959_963_984_540_054;

/// The 95 % score interval's lower end for a Poisson count `k`: `k + z²/2 − z√(k + z²/4)`.
fn poisson_lower(k: u64) -> f64 {
    let k = k as f64;
    (k + Z95 * Z95 / 2.0 - Z95 * (k + Z95 * Z95 / 4.0).sqrt()).max(0.0)
}

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

/// One member process: runs until it is killed, or until its supervisor is gone (its output
/// closes), when it ends.
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
    let mut detector = Detector::new(HostId(me), Exposure::new());
    for peer in (1..=NODES).filter(|peer| *peer != me) {
        let role = if me < peer {
            Role::Initiator
        } else {
            Role::Acceptor
        };
        plane
            .install_epoch(peer, 1, &secret_between(me, peer), role)
            .unwrap();
        plane.set_path(peer, DATAGRAM).unwrap();
        detector.join(HostId(peer));
    }

    // Ready, then wait for the supervisor's start: a member joins its peers only once they are up,
    // as a real member joins a peer once its transport connection to it exists.
    let mut stdout = std::io::stdout();
    if writeln!(stdout, "ready {me}")
        .and_then(|()| stdout.flush())
        .is_err()
    {
        return;
    }
    let mut start = String::new();
    if !matches!(std::io::stdin().read_line(&mut start), Ok(read) if read > 0) {
        return;
    }

    let mut member = Member {
        me,
        socket,
        plane,
        detector,
        address: Box::new(address),
        started: Instant::now(),
        gossip: 0,
        buffer: vec![0u8; DATAGRAM],
        batch: Vec::new(),
        encoded: Vec::new(),
        requests: Vec::new(),
        relaying: BTreeMap::new(),
        dead_seen: BTreeMap::new(),
    };
    member.gossip = member.gossip_room();
    let mut period = 0u64;
    loop {
        let began = member.step();
        member.note_deaths();
        if began {
            period += 1;
            if member.report(period, &mut stdout).is_err() {
                // The supervisor is gone: nothing reads this member any more.
                return;
            }
        }
        member.receive();
        member.note_deaths();
    }
}

/// One member's driver: its socket, plane and detector, and the probes it relays.
struct Member {
    me: u64,
    socket: UdpSocket,
    plane: Plane,
    detector: Detector,
    address: Box<dyn Fn(u64) -> String>,
    started: Instant,
    /// Gossip entries a message carries: what fits the datagram.
    gossip: usize,
    buffer: Vec<u8>,
    batch: Vec<(HostId, MemberState)>,
    encoded: Vec<u8>,
    requests: Vec<PingReq>,
    /// Probes this member relays: the target, the relay's nonce, who asked and with which nonce.
    /// One a target, so bounded by the membership.
    relaying: BTreeMap<u64, (u64, HostId, u64)>,
    /// When this member first saw each peer dead, and the bound its detector stated then.
    dead_seen: BTreeMap<u64, (Duration, Option<Duration>)>,
}

impl Member {
    /// Nanoseconds on this member's monotonic clock.
    fn now(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_nanos()).unwrap()
    }

    /// The gossip entries that fit the datagram beside the largest message, an acknowledgement
    /// with this member's coordinate.
    fn gossip_room(&mut self) -> usize {
        let coordinate = self.detector.coordinate().clone();
        SwimMessage::Ack {
            from: HostId(self.me),
            nonce: u64::MAX,
            boot_nonce: self.me,
            configuration_version: 0,
            standing: None,
            gossip: GossipBatch::Entries(&[]),
            coordinate: Coordinate::Held(&coordinate),
        }
        .encode_into(&mut self.encoded);
        gossip_capacity(DATAGRAM - OVERHEAD_BYTES - LENGTH_BYTES, self.encoded.len())
    }

    fn send(&mut self, to: u64, message: &SwimMessage<'_>) {
        message.encode_into(&mut self.encoded);
        // A refused message is a lost one; the detector measures losses.
        let _ = self.plane.queue(to, &self.encoded);
    }

    /// Polls the detector and sends what it asks. Whether a period began.
    fn step(&mut self) -> bool {
        let mut requests = std::mem::take(&mut self.requests);
        let ping = self.detector.poll(self.now(), &mut requests);
        for request in &requests {
            self.send(
                request.relay.0,
                &SwimMessage::PingReq {
                    from: HostId(self.me),
                    target: request.target,
                    nonce: request.nonce,
                    gossip: GossipBatch::Entries(&[]),
                },
            );
        }
        self.requests = requests;
        if let Some(ping) = ping {
            let mut batch = std::mem::take(&mut self.batch);
            self.detector
                .ping_gossip_into(ping.to, self.gossip, &mut batch);
            self.send(
                ping.to.0,
                &SwimMessage::Ping {
                    from: HostId(self.me),
                    nonce: ping.nonce,
                    boot_nonce: self.me,
                    configuration_version: 0,
                    gossip: GossipBatch::Entries(&batch),
                },
            );
            self.batch = batch;
        }
        flush(&mut self.plane, &self.socket, &*self.address);
        ping.is_some()
    }

    /// Waits for one datagram until the detector's wake (or for one datagram, when it asks no
    /// wake), and handles it. Only a timeout or a datagram ends the wait: any other error
    /// (Windows reports a reset on the next receive after a send to a closed port) skips it.
    fn receive(&mut self) {
        let timeout = match self.detector.wake() {
            Some(at) => match at.checked_sub(self.now()).filter(|left| *left > 0) {
                Some(left) => Some(Duration::from_nanos(left)),
                None => return,
            },
            None => None,
        };
        self.socket.set_read_timeout(timeout).unwrap();
        let Ok((length, _)) = self.socket.recv_from(&mut self.buffer) else {
            return;
        };
        let stamp = self.now();
        let mut buffer = std::mem::take(&mut self.buffer);
        if let Ok(opened) = self.plane.open(&mut buffer[..length], &AdmitAll) {
            for bytes in opened.messages() {
                if let Ok(message) = SwimMessage::decode(bytes) {
                    self.handle(message, stamp);
                }
            }
        }
        self.buffer = buffer;
        flush(&mut self.plane, &self.socket, &*self.address);
    }

    fn handle(&mut self, message: SwimMessage<'_>, stamp: u64) {
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
                self.detector.gossip_into(self.gossip, &mut batch);
                let coordinate = self.detector.coordinate().clone();
                self.send(
                    ack.to.0,
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
            SwimMessage::Ack {
                from,
                nonce,
                gossip,
                coordinate,
                ..
            } => {
                self.detector.apply_gossip(gossip);
                self.detector.learn_coordinate(from, coordinate);
                // An answer to a probe made for another member goes back to it.
                match self.relaying.get(&from.0) {
                    Some(&(relayed, asker, asked)) if relayed == nonce => {
                        self.relaying.remove(&from.0);
                        self.send(
                            asker.0,
                            &SwimMessage::IndirectAck {
                                from: HostId(self.me),
                                target: from,
                                nonce: asked,
                                boot_nonce: self.me,
                                gossip: GossipBatch::Entries(&[]),
                            },
                        );
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
                self.send(
                    target.0,
                    &SwimMessage::Ping {
                        from: HostId(self.me),
                        nonce: ping.nonce,
                        boot_nonce: self.me,
                        configuration_version: 0,
                        gossip: GossipBatch::Entries(&[]),
                    },
                );
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

    /// Notes, the moment this member first holds a peer dead, how long after the peer's last
    /// answer that is, and the bound its detector states: to a pending condemnation, plus, where
    /// this member's own had become pending, the wait since for an answer from another member,
    /// which it measures.
    fn note_deaths(&mut self) {
        let now = self.now();
        for peer in (1..=NODES).filter(|peer| *peer != self.me) {
            let state = self.detector.membership().state(HostId(peer)).unwrap();
            if state.liveness != Liveness::Dead || self.dead_seen.contains_key(&peer) {
                continue;
            }
            let report = self.detector.report(HostId(peer)).unwrap_or_default();
            let since =
                |at: Option<u64>| Duration::from_nanos(at.map_or(0, |at| now.saturating_sub(at)));
            let waited = since(report.pending_since_ns);
            let bound = self.detector.detection_bound(now);
            self.dead_seen.insert(
                peer,
                (
                    since(report.last_answer_ns),
                    bound.map(|bound| bound.saturating_add(waited)),
                ),
            );
        }
    }

    /// One line: `me period` then, per peer, `peer:letter:judged:own:suspicions:allowance:
    /// condemnations:allowance:dead_after_ns:dead_within_ns`.
    fn report(&mut self, period: u64, out: &mut impl Write) -> std::io::Result<()> {
        let mut line = format!("{} {period}", self.me);
        for peer in (1..=NODES).filter(|peer| *peer != self.me) {
            let state = self.detector.membership().state(HostId(peer)).unwrap();
            let report = self.detector.report(HostId(peer)).unwrap_or_default();
            let (after, within) = self.dead_seen.get(&peer).copied().unwrap_or_default();
            // Judged: a configured verdict times this member's probes of the peer, the pair's own
            // or, while the pair's estimator refuses, the pool's.
            let judged = self.detector.verdict(HostId(peer)).is_some();
            line.push_str(&format!(
                " {peer}:{}:{}:{}:{}:{}:{}:{}:{}:{}",
                liveness_letter(state.liveness),
                u8::from(judged),
                u8::from(report.configured),
                report.suspicions,
                report.suspicion_allowance,
                report.condemnations,
                report.condemnation_allowance,
                after.as_nanos(),
                within.map_or(0, |w| w.as_nanos()),
            ));
        }
        writeln!(out, "{line}")?;
        out.flush()
    }
}

fn flush(plane: &mut Plane, socket: &UdpSocket, address: &dyn Fn(u64) -> String) {
    plane.flush(|peer, datagram: Result<&[u8], hyper_datagram::Refusal>| {
        // A datagram the plane refused, or the socket did not take, is a lost one.
        if let Ok(datagram) = datagram {
            let _ = socket.send_to(datagram, address(peer));
        }
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

/// The member processes, killed when the supervisor ends however it ends.
struct Members(BTreeMap<u64, Child>);

impl Drop for Members {
    fn drop(&mut self) {
        for child in self.0.values_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// What a member last reported of one peer.
#[derive(Clone, Copy, Debug, Default)]
struct Seen {
    letter: char,
    judged: bool,
    own: bool,
    suspicions: u64,
    suspicion_allowance: f64,
    condemnations: u64,
    condemnation_allowance: f64,
    dead_after: Duration,
    dead_within: Duration,
}

fn parse(line: &str) -> Option<(u64, BTreeMap<u64, Seen>)> {
    let mut fields = line.split(' ');
    let member = fields.next()?.parse().ok()?;
    fields.next()?.parse::<u64>().ok()?;
    let mut peers = BTreeMap::new();
    for field in fields {
        let parts: Vec<&str> = field.split(':').collect();
        let [peer, letter, judged, own, s, sa, c, ca, after, within] = parts[..] else {
            return None;
        };
        peers.insert(
            peer.parse().ok()?,
            Seen {
                letter: letter.chars().next()?,
                judged: judged == "1",
                own: own == "1",
                suspicions: s.parse().ok()?,
                suspicion_allowance: sa.parse().ok()?,
                condemnations: c.parse().ok()?,
                condemnation_allowance: ca.parse().ok()?,
                dead_after: Duration::from_nanos(after.parse().ok()?),
                dead_within: Duration::from_nanos(within.parse().ok()?),
            },
        );
    }
    Some((member, peers))
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
    let mut members = Members(
        (1..=NODES)
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
            .collect(),
    );
    let (lines, receiver) = std::sync::mpsc::channel::<String>();
    for child in members.0.values_mut() {
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
    drop(lines);

    // Start every member together once all have bound their sockets.
    let mut ready = 0;
    while ready < NODES {
        let line = receiver.recv().expect("a member ended before it was ready");
        // libtest prints "test member_process ... " before the body runs, without a newline.
        if line.contains("ready ") {
            ready += 1;
        }
    }
    for child in members.0.values_mut() {
        writeln!(child.stdin.as_mut().unwrap(), "start").unwrap();
    }

    let mut latest: BTreeMap<u64, BTreeMap<u64, Seen>> = BTreeMap::new();
    let mut killed = false;
    let judged_by = |latest: &BTreeMap<u64, BTreeMap<u64, Seen>>| {
        latest.len() == NODES as usize
            && latest
                .values()
                .all(|peers| peers.values().all(|seen| seen.judged))
    };
    let detected = |latest: &BTreeMap<u64, BTreeMap<u64, Seen>>| {
        (1..NODES).all(|member| {
            latest
                .get(&member)
                .and_then(|peers| peers.get(&VICTIM))
                .is_some_and(|seen| seen.letter == 'D')
        })
    };
    while !(killed && detected(&latest)) {
        let line = receiver.recv().expect("a member stopped reporting");
        let Some((member, peers)) = parse(&line) else {
            continue;
        };
        if killed && member == VICTIM {
            continue;
        }
        latest.insert(member, peers);
        if !killed && judged_by(&latest) {
            let mut victim = members.0.remove(&VICTIM).unwrap();
            victim.kill().unwrap();
            victim.wait().unwrap();
            killed = true;
        }
    }
    drop(members);

    for member in 1..NODES {
        let seen = latest[&member][&VICTIM];
        assert!(
            seen.dead_after <= seen.dead_within,
            "member {member} saw the victim dead {:?} after its last answer, past its stated \
             bound of {:?}",
            seen.dead_after,
            seen.dead_within
        );
    }
    let (mut suspicions, mut suspicion_allowance) = (0u64, 0.0f64);
    let (mut condemnations, mut condemnation_allowance) = (0u64, 0.0f64);
    for peers in latest.values() {
        for (peer, seen) in peers {
            if *peer == VICTIM {
                continue;
            }
            suspicions += seen.suspicions;
            suspicion_allowance += seen.suspicion_allowance;
            condemnations += seen.condemnations;
            condemnation_allowance += seen.condemnation_allowance;
        }
    }
    let own = latest
        .values()
        .flat_map(BTreeMap::values)
        .filter(|seen| seen.own)
        .count();
    println!(
        "pairs judged by their own estimator at the end: {own} of {}",
        NODES * (NODES - 1)
    );
    println!(
        "detection, from the victim's last answer: {:?}; suspicions of live members {suspicions} \
         (Theorem 7 allows {suspicion_allowance:.3}); condemnations {condemnations} (allows \
         {condemnation_allowance:.3})",
        (1..NODES)
            .map(|member| {
                let seen = latest[&member][&VICTIM];
                (member, seen.dead_after, seen.dead_within)
            })
            .collect::<Vec<_>>()
    );
    // Theorem 7 bounds the expected count: a run refutes it only when the count's 95 % lower
    // limit passes the allowance, the rule the replay and the trace analyser apply.
    assert!(
        poisson_lower(suspicions) <= suspicion_allowance,
        "{suspicions} suspicions of live members refute the {suspicion_allowance} the configured \
         detectors allow"
    );
    assert!(
        poisson_lower(condemnations) <= condemnation_allowance,
        "{condemnations} condemnations of live members refute the {condemnation_allowance} the \
         configured detectors allow"
    );
}
