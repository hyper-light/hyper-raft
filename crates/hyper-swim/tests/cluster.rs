//! SWIM between real processes over the sealed datagram plane on real UDP sockets: the usage
//! mantle, focal and slates make of it.
//!
//! The supervisor (`a_killed_member_is_declared_dead_by_every_survivor_and_no_live_one_is`)
//! starts `NODES` copies of this test binary as member processes (`member_process`, selected by
//! `HYPER_SWIM_NODE`). Each runs the detector as the library configures it: it polls when the
//! detector asks, sends what the detector returns, and prints, every period, its view and what its
//! detector reports of each peer. The test times nothing of its own. The supervisor waits on
//! facts, in two phases:
//! - every member judges every peer by a configured verdict, the pair's own or, while the pair's
//!   estimator refuses, the pool's; then it SIGKILLs one member ([`POOLED`]), which within a few
//!   hundred milliseconds of the start is judged mostly by the pools;
//! - every survivor reports it dead; then the run goes on until every surviving pair is judged by
//!   its own estimator, and the supervisor SIGKILLs another ([`OWNED`]);
//! - every survivor reports that one dead too. Each survivor holds each victim dead within the
//!   detection bound its detector stated, with the wait it measured for evidence of its own health
//!   added where its condemnation was pending on it.
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
use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::net::UdpSocket;
use std::num::NonZeroUsize;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use hyper_datagram::{
    AdmitAll, ExporterSecret, LENGTH_BYTES, OVERHEAD_BYTES, Plane, PlaneLimits, Role, SECRET_BYTES,
};
use hyper_swim::HostId;
use hyper_swim::codec::{Coordinate, GossipBatch, SwimMessage, gossip_capacity};
use hyper_swim::detector::{Detector, PeerReport, PingReq};
use hyper_swim::membership::{Liveness, MemberState};
use hyper_timing::Exposure;

/// Members in the test cluster: two victims, a prober, and two members it can ask to relay once both
/// are dead.
const NODES: u64 = 5;
/// The member killed once every pair is judged, mostly by the pools.
const POOLED: u64 = NODES;
/// The member killed once every surviving pair is judged by its own estimator.
const OWNED: u64 = NODES - 1;
/// The path's datagram size: QUIC's minimum, which every path carries (RFC 9000 §14.1).
const DATAGRAM: usize = 1_200;
/// The 95 % score interval's lower end for a Poisson count `k` (`hyper_timing::poisson95`), with
/// which the trace analyser and `hyper-timing`'s replay refute Theorem 7 (`docs/timing.md` §2.6).
fn poisson_lower(k: u64) -> f64 {
    hyper_timing::poisson95(k).0
}

fn secret_between(a: u64, b: u64) -> ExporterSecret {
    // Stands for the QUIC exporter both ends of a connection compute: one secret per pair.
    let (low, high) = (a.min(b), a.max(b));
    let mut bytes = [0u8; SECRET_BYTES];
    bytes[..8].copy_from_slice(&low.to_le_bytes());
    bytes[8..16].copy_from_slice(&high.to_le_bytes());
    ExporterSecret::new(bytes)
}

/// A peer's liveness as a member reports it; `F` for one its view has forgotten, once dead.
fn liveness_letter(liveness: Option<Liveness>) -> char {
    match liveness {
        Some(Liveness::Alive) => 'A',
        Some(Liveness::Suspect) => 'S',
        Some(Liveness::Dead) => 'D',
        None => 'F',
    }
}

/// One member process: runs until it is killed, or until its supervisor is gone (its output
/// closes), when it ends.
#[test]
#[allow(
    clippy::disallowed_methods,
    reason = "real processes on the host's clock, threads and environment (CLAUDE.md §1a, end to end)"
)]
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
    // The cluster is the placement: every member knows the others and no more.
    let members = NonZeroUsize::new(NODES as usize).unwrap();
    let mut detector = Detector::new(HostId(me), Exposure::new(), members);
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
        detector.join(HostId(peer)).unwrap();
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
        tallies: BTreeMap::new(),
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
    /// What its detector has reported of each peer, kept across the peer's being forgotten.
    tallies: BTreeMap<u64, Tally>,
}

/// The suspicions and condemnations a member's detector reports of a peer, with their allowances.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Counts {
    suspicions: u64,
    suspicion_allowance: f64,
    condemnations: u64,
    condemnation_allowance: f64,
}

impl Counts {
    fn of(report: Option<PeerReport>) -> Self {
        report.map_or_else(Self::default, |report| Self {
            suspicions: report.suspicions,
            suspicion_allowance: report.suspicion_allowance,
            condemnations: report.condemnations,
            condemnation_allowance: report.condemnation_allowance,
        })
    }

    fn plus(self, other: Self) -> Self {
        Self {
            suspicions: self.suspicions + other.suspicions,
            suspicion_allowance: self.suspicion_allowance + other.suspicion_allowance,
            condemnations: self.condemnations + other.condemnations,
            condemnation_allowance: self.condemnation_allowance + other.condemnation_allowance,
        }
    }

    /// Whether any count is below `other`'s: counts only grow while a report lives.
    fn below(self, other: Self) -> bool {
        self.suspicions < other.suspicions
            || self.suspicion_allowance < other.suspicion_allowance
            || self.condemnations < other.condemnations
            || self.condemnation_allowance < other.condemnation_allowance
    }
}

/// A peer's counts across its reports: a report that goes with its member, forgotten once dead
/// (a live member falsely condemned, then refuting late), or starts over, carries what it reached.
#[derive(Clone, Copy, Debug, Default)]
struct Tally {
    carried: Counts,
    last: Counts,
}

impl Tally {
    fn update(&mut self, report: Option<PeerReport>) -> Counts {
        let now = Counts::of(report);
        if now.below(self.last) {
            self.carried = self.carried.plus(self.last);
        }
        self.last = now;
        self.carried.plus(now)
    }
}

impl Member {
    /// Nanoseconds on this member's monotonic clock.
    fn now(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_nanos()).unwrap()
    }

    /// The gossip entries that fit the datagram beside the largest message, an acknowledgement
    /// with this member's coordinate.
    fn gossip_room(&mut self) -> usize {
        let coordinate = *self.detector.coordinate();
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
    #[allow(
        clippy::disallowed_methods,
        reason = "the socket waits by a peek with the timeout, never a timed receive"
    )]
    fn receive(&mut self) {
        let timeout = match self.detector.wake() {
            Some(at) => match at.checked_sub(self.now()).filter(|left| *left > 0) {
                Some(left) => Some(Duration::from_nanos(left)),
                None => return,
            },
            None => None,
        };
        self.socket.set_read_timeout(timeout).unwrap();
        // Waited for by a peek and taken once there: on Windows a receive that times out can lose
        // the datagram that arrives as it times out (`setsockopt`, `SO_RCVTIMEO`; `docs/raft.md`,
        // "The harness's receive"), which this detector would count as a heartbeat lost.
        // A peek leaves a reset in place, so whatever it found is taken by a receive that does not
        // wait, which clears it.
        if let Err(error) = self.socket.peek_from(&mut self.buffer)
            && matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut)
        {
            return;
        }
        self.socket.set_nonblocking(true).unwrap();
        let taken = self.socket.recv_from(&mut self.buffer);
        self.socket.set_nonblocking(false).unwrap();
        let Ok((length, _)) = taken else {
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
                let coordinate = *self.detector.coordinate();
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
            // A peer forgotten was held dead first, and noted then.
            let held = self.detector.membership().state(HostId(peer));
            if held.is_none_or(|state| state.liveness != Liveness::Dead)
                || self.dead_seen.contains_key(&peer)
            {
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
            let state = self.detector.membership().state(HostId(peer));
            let held = self.detector.report(HostId(peer));
            let counts = self.tallies.entry(peer).or_default().update(held);
            let report = held.unwrap_or_default();
            let (after, within) = self.dead_seen.get(&peer).copied().unwrap_or_default();
            // Judged: a configured verdict times this member's probes of the peer, the pair's own
            // or, while the pair's estimator refuses, the pool's.
            let judged = self.detector.verdict(HostId(peer)).is_some();
            line.push_str(&format!(
                " {peer}:{}:{}:{}:{}:{}:{}:{}:{}:{}",
                liveness_letter(state.map(|state| state.liveness)),
                u8::from(judged),
                u8::from(report.configured),
                counts.suspicions,
                counts.suspicion_allowance,
                counts.condemnations,
                counts.condemnation_allowance,
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
#[allow(
    clippy::disallowed_methods,
    reason = "real processes on the host's clock, threads and environment (CLAUDE.md §1a, end to end)"
)]
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

    // Every member's latest report, and the members killed, in order, each with every member's
    // latest report when it was killed.
    let mut latest: Reports = BTreeMap::new();
    let mut killed: Vec<(u64, Reports)> = Vec::new();
    loop {
        let line = receiver.recv().expect("a member stopped reporting");
        let Some((member, peers)) = parse(&line) else {
            continue;
        };
        if killed.iter().any(|(victim, _)| *victim == member) {
            continue;
        }
        latest.insert(member, peers);
        let alive: Vec<u64> = (1..=NODES)
            .filter(|member| killed.iter().all(|(victim, _)| victim != member))
            .collect();
        let settled = alive.iter().all(|member| latest.contains_key(member))
            && killed
                .iter()
                .all(|(victim, _)| held_dead(&latest, &alive, *victim));
        if !settled {
            continue;
        }
        // Each phase waits on its fact: every pair judged, then every pair judged by its own
        // estimator; then its victim is killed.
        let (victim, ready) = match killed.len() {
            0 => (POOLED, every_pair(&latest, &alive, |seen| seen.judged)),
            1 => (OWNED, every_pair(&latest, &alive, |seen| seen.own)),
            _ => break,
        };
        if ready {
            let mut child = members.0.remove(&victim).unwrap();
            child.kill().unwrap();
            child.wait().unwrap();
            killed.push((victim, latest.clone()));
        }
    }
    drop(members);

    let killed_at = |member: u64| killed.iter().position(|(victim, _)| *victim == member);
    for (index, (victim, _)) in killed.iter().enumerate() {
        // Every member alive at the kill, and its own record of the death, kept to its last line.
        for (member, peers) in &latest {
            if killed_at(*member).is_some_and(|at| at <= index) {
                continue;
            }
            let seen = peers[victim];
            assert!(
                seen.dead_after <= seen.dead_within,
                "member {member} saw {victim} dead {:?} after its last answer, past its stated \
                 bound of {:?}",
                seen.dead_after,
                seen.dead_within
            );
        }
    }
    // Of live members: each pair's counts while its peer lived, at the peer's kill where the member
    // outlived it.
    let mut live = Counts::default();
    for (member, peers) in &latest {
        for (peer, seen) in peers {
            let seen = match killed_at(*peer) {
                Some(at) if killed_at(*member).is_none_or(|own| own > at) => {
                    killed[at].1[member][peer]
                }
                _ => *seen,
            };
            live = live.plus(Counts {
                suspicions: seen.suspicions,
                suspicion_allowance: seen.suspicion_allowance,
                condemnations: seen.condemnations,
                condemnation_allowance: seen.condemnation_allowance,
            });
        }
    }
    for (index, (victim, reports)) in killed.iter().enumerate() {
        // The members alive when it was killed: it, and every member killed after it or never.
        let alive: Vec<u64> = (1..=NODES)
            .filter(|member| killed_at(*member).is_none_or(|at| at >= index))
            .collect();
        let pairs = alive.len() * (alive.len() - 1);
        let own = alive
            .iter()
            .flat_map(|member| {
                let alive = &alive;
                reports[member]
                    .iter()
                    .filter(move |(peer, _)| alive.contains(peer))
            })
            .filter(|(_, seen)| seen.own)
            .count();
        println!(
            "member {victim} killed with {own} of {pairs} pairs judged by their own estimator; \
             detection, from its last answer: {:?}",
            alive
                .iter()
                .filter(|member| *member != victim)
                .map(|member| {
                    let seen = latest[member][victim];
                    (*member, seen.dead_after, seen.dead_within)
                })
                .collect::<Vec<_>>()
        );
    }
    let Counts {
        suspicions,
        suspicion_allowance,
        condemnations,
        condemnation_allowance,
    } = live;
    println!(
        "suspicions of live members {suspicions} (Theorem 7 allows {suspicion_allowance:.3}); \
         condemnations {condemnations} (allows {condemnation_allowance:.3})"
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

/// Every member's latest report of each peer.
type Reports = BTreeMap<u64, BTreeMap<u64, Seen>>;

/// Whether every member alive reports `victim` dead, or forgotten once dead.
fn held_dead(latest: &Reports, alive: &[u64], victim: u64) -> bool {
    alive.iter().all(|member| {
        latest
            .get(member)
            .and_then(|peers| peers.get(&victim))
            .is_some_and(|seen| matches!(seen.letter, 'D' | 'F'))
    })
}

/// Whether `judged` holds of every pair of members alive.
fn every_pair(latest: &Reports, alive: &[u64], judged: impl Fn(&Seen) -> bool) -> bool {
    alive.iter().all(|member| {
        alive.iter().filter(|peer| *peer != member).all(|peer| {
            latest
                .get(member)
                .and_then(|peers| peers.get(peer))
                .is_some_and(&judged)
        })
    })
}
