//! hyper-transport between real processes over real UDP sockets on loopback (CLAUDE.md §1a).
//!
//! This binary is both sides. Run as a test it is the asking side, and it spawns itself with
//! `HT_PEER` set as the answering side, a process that shares nothing with it but the kernel's
//! sockets. Scenarios run one after another, one peer process at a time:
//!
//! - `exchanges`: connect, request and reply in every class, streaming bodies of megabytes both
//!   ways, every byte checked;
//! - `reserve`: bulk exchanges whose bodies the peer holds unread spend the connection's credit,
//!   and a control exchange still crosses;
//! - `refusals`: a role that may not send a kind, a message past the peer's class bound, a head
//!   the peer's budget cannot fund, the exchange table's bound, an identity past its connection
//!   bound replaced, an identity past the identity bound refused, a certificate nobody knows;
//! - `killed`: the peer is killed with SIGKILL mid-upload; the exchange ends within its progress
//!   deadline, and a new peer process is reached after the route is retired;
//! - `lanes`: replication frames on two lanes, echoed back by the peer, in order.
//!
//! The driver reads its socket every turn, even when a timer is already due (hyper-raft-e2e
//! `a71ee26`), and every wait is for a fact; the only wall-clock bound is a failure guard far past
//! anything the protocol needs. A receive error is never the end of the loop: on Windows a send
//! to a port that has closed comes back as a connection reset on the next receive.

#![allow(
    clippy::unwrap_in_result,
    clippy::type_complexity,
    clippy::too_many_arguments,
    clippy::string_slice,
    clippy::cast_possible_truncation,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::disallowed_macros,
    clippy::cognitive_complexity,
    clippy::too_many_lines,
    missing_docs
)]

mod common;

use std::io::{BufRead, BufReader, ErrorKind};
use std::net::{SocketAddr, UdpSocket};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use common::*;
use hyper_transport::{
    Classes, Config, Endpoint, Fixed, Limits, PeerId, Progress, Refusal, class_reserve,
    initial_window,
};

/// The most a turn waits on its socket when no timer is due: a liveness bound on the loop, not a
/// wait on any outcome; every outcome arrives as a datagram or a timer, which end the wait first.
const IDLE_TURN: Duration = Duration::from_millis(50);
/// The most datagrams one turn takes: every one that arrived in the turn, at the 1,200-byte floor
/// a receive window of the tests' 16 MiB ceiling would fill 14,000 of; a turn that leaves some
/// takes them in the next.
const DRAIN: usize = 16_384;
/// The failure guard of a wait: far past any outcome these scenarios wait for on loopback, so that
/// a protocol that never ends fails the test instead of hanging it.
const GUARD: Duration = Duration::from_secs(120);
/// The period exchanges are judged at.
const PERIOD: Duration = Duration::from_secs(2);

/// One endpoint's socket, driven one turn at a time.
struct Wire {
    socket: UdpSocket,
    buffer: Vec<u8>,
    out: Vec<u8>,
    errors: u64,
}

impl Wire {
    fn bind() -> Self {
        Self {
            socket: UdpSocket::bind("127.0.0.1:0").unwrap(),
            buffer: vec![0; 65_536],
            out: Vec::with_capacity(65_536),
            errors: 0,
        }
    }
    fn address(&self) -> SocketAddr {
        self.socket.local_addr().unwrap()
    }
    fn flush<E: Drive>(&mut self, endpoint: &mut E) {
        while let Some(transmit) = endpoint.transmit(Instant::now(), &mut self.out) {
            // A datagram the kernel refuses is a lost datagram: QUIC recovers it.
            let _ = self
                .socket
                .send_to(&self.out[..transmit.size], transmit.destination);
        }
    }
    fn receive<E: Drive>(&mut self, endpoint: &mut E, most: usize) {
        for _ in 0..most {
            match self.socket.recv_from(&mut self.buffer) {
                Ok((length, from)) => {
                    endpoint.datagram(Instant::now(), from, &self.buffer[..length])
                }
                Err(error)
                    if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
                {
                    return;
                }
                // A connection reset (Windows, after a send to a closed port) and the like: the
                // socket is still good, and what else arrived is still to be read.
                Err(_) => self.errors += 1,
            }
        }
    }
    /// Sends what is due, waits for a datagram until the next timer, takes everything that has
    /// arrived (also when the timer is already due), fires the timers due, and sends again.
    fn turn<E: Drive>(&mut self, endpoint: &mut E) {
        self.flush(endpoint);
        let now = Instant::now();
        let until = endpoint
            .timeout()
            .map_or(now + IDLE_TURN, |due| due.min(now + IDLE_TURN));
        let wait = until.saturating_duration_since(now);
        if !wait.is_zero() {
            self.socket.set_nonblocking(false).unwrap();
            self.socket.set_read_timeout(Some(wait)).unwrap();
            self.receive(endpoint, 1);
        }
        self.socket.set_nonblocking(true).unwrap();
        self.receive(endpoint, DRAIN);
        let now = Instant::now();
        if endpoint.timeout().is_some_and(|due| due <= now) {
            endpoint.fire(now);
        }
        self.flush(endpoint);
    }
}

fn parse<T: std::str::FromStr>(name: &str, default: T) -> T {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

// ---------------------------------------------------------------------------------------------
// The answering side.

fn peer_process() {
    let root = unhex(&std::env::var("HT_ROOT").unwrap());
    let certificate = unhex(&std::env::var("HT_CERT").unwrap());
    let key = unhex(&std::env::var("HT_KEY").unwrap());
    let mut book = Book::default();
    for entry in std::env::var("HT_KNOWS")
        .unwrap()
        .split(',')
        .filter(|entry| !entry.is_empty())
    {
        let mut parts = entry.split(':');
        let peer: PeerId = parts.next().unwrap().parse().unwrap();
        let role = if parts.next().unwrap() == "node" {
            Role::Node
        } else {
            Role::Client
        };
        book.add(&unhex(parts.next().unwrap()), peer, role);
    }
    let mut limits = limits();
    limits.admission.per_identity = parse("HT_PER_IDENTITY", limits.admission.per_identity);
    limits.admission.identities = parse("HT_IDENTITIES", limits.admission.identities);
    let config = Config {
        credentials: credentials(&root, &certificate, &key),
        role: Role::Node,
        limits,
        listen: true,
    };
    let budget = Fixed::new(parse("HT_BUDGET", 512u64 << 20), 64);
    let hold = parse("HT_HOLD", 0u8) == 1;
    match std::env::var("HT_CLASSES").as_deref() {
        Ok("strict") => serve(
            Endpoint::<Strict, _, _>::new(config, budget, book, Instant::now()).unwrap(),
            hold,
        ),
        _ => serve(
            Endpoint::<Mantle, _, _>::new(config, budget, book, Instant::now()).unwrap(),
            hold,
        ),
    }
}

fn serve<C: Classes<Kind = Kind, Class = Class, Role = Role>>(mut node: Node<C>, hold: bool) {
    let mut wire = Wire::bind();
    println!("listening {}", wire.address().port());
    let began = Instant::now();
    let mut server = Server::new();
    server.hold_bulk = hold;
    let mut ever = false;
    let mut told = 0;
    loop {
        wire.turn(&mut node);
        server.serve(&mut node);
        // A refusal is reported with what this side saw at that moment, for the asker's failure
        // to be read beside it.
        for (refusal, by_peer) in server.refused.iter().skip(told) {
            eprintln!(
                "peer: refused {refusal:?} (by the asker: {by_peer}); {:?}; {:?}",
                node.stats(),
                node.connection_stats(1)
            );
        }
        told = server.refused.len();
        for (peer, lane, frame) in server.frames.drain(..) {
            node.send_frame(peer, lane, Kind::Append, &frame).unwrap();
        }
        wire.flush(&mut node);
        ever |= !server.connected.is_empty();
        let connections = node.stats().connections;
        // Done once every connection it had has gone; an orphan whose asker never came leaves
        // after three idle timeouts.
        if (ever && connections == 0) || (!ever && began.elapsed() > limits().idle_timeout * 3) {
            return;
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The asking side.

/// A peer process and where it listens.
struct Peer {
    child: Child,
    address: SocketAddr,
}

impl Drop for Peer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Node 2's credentials, and the identities it knows.
struct Setup {
    pki: Pki,
    two: (Vec<u8>, Vec<u8>),
    knows: Vec<(PeerId, Role, Vec<u8>, Vec<u8>)>,
}

impl Setup {
    fn new(identities: &[(PeerId, Role)]) -> Self {
        let pki = Pki::new();
        let two = pki.issue(&name(2));
        let knows = identities
            .iter()
            .map(|(peer, role)| {
                let (certificate, key) = pki.issue(&name(*peer));
                (*peer, *role, certificate, key)
            })
            .collect();
        Self { pki, two, knows }
    }
    fn spawn(&self, env: &[(&str, String)]) -> Peer {
        let knows: Vec<String> = self
            .knows
            .iter()
            .map(|(peer, role, certificate, _)| {
                let role = if *role == Role::Node {
                    "node"
                } else {
                    "client"
                };
                format!("{peer}:{role}:{}", hex(certificate))
            })
            .collect();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .env("HT_PEER", "1")
            .env("HT_ROOT", hex(&self.pki.root))
            .env("HT_CERT", hex(&self.two.0))
            .env("HT_KEY", hex(&self.two.1))
            .env("HT_KNOWS", knows.join(","))
            .stdout(Stdio::piped());
        for (name, value) in env {
            command.env(name, value);
        }
        let mut child = command.spawn().unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let port: u16 = line
            .trim()
            .strip_prefix("listening ")
            .unwrap()
            .parse()
            .unwrap();
        Peer {
            child,
            address: SocketAddr::from(([127, 0, 0, 1], port)),
        }
    }
    /// An asking endpoint for identity `which`, with `role`, knowing node 2.
    fn asker<C: Classes<Kind = Kind, Class = Class, Role = Role>>(
        &self,
        which: PeerId,
        role: Role,
        limits: Limits,
    ) -> Node<C> {
        let (_, _, certificate, key) = self.knows.iter().find(|(peer, ..)| *peer == which).unwrap();
        let mut book = Book::default();
        book.add(&self.two.0, 2, Role::Node);
        let config = Config {
            credentials: credentials(&self.pki.root, certificate, key),
            role,
            limits,
            listen: false,
        };
        Endpoint::new(config, Fixed::new(512 << 20, 64), book, Instant::now()).unwrap()
    }
}

/// Turns `node` until `fact` holds; panics past the failure guard.
fn wait<C: Classes<Kind = Kind, Class = Class, Role = Role>>(
    wire: &mut Wire,
    node: &mut Node<C>,
    asker: &mut Asker,
    what: &str,
    mut fact: impl FnMut(&mut Node<C>, &mut Asker) -> bool,
) -> Duration {
    let began = Instant::now();
    loop {
        wire.turn(node);
        asker.clock = Instant::now();
        asker.drive(node);
        wire.flush(node);
        if fact(node, asker) {
            return began.elapsed();
        }
        assert!(
            began.elapsed() < GUARD,
            "{what}: never held; events {:?}",
            asker.events.iter().rev().take(8).collect::<Vec<_>>()
        );
    }
}

fn connect<C: Classes<Kind = Kind, Class = Class, Role = Role>>(
    wire: &mut Wire,
    node: &mut Node<C>,
    asker: &mut Asker,
    peer: &Peer,
) -> Duration {
    node.connect(Instant::now(), 2, peer.address).unwrap();
    let before = asker.connected.len();
    wait(wire, node, asker, "connected", |_, asker| {
        asker.connected.len() > before
    })
}

fn period() -> Progress {
    Progress::new(PERIOD).unwrap()
}

fn exchanges() -> String {
    let setup = Setup::new(&[(1, Role::Node)]);
    let peer = setup.spawn(&[]);
    let mut node = setup.asker::<Mantle>(1, Role::Node, limits());
    let (mut wire, mut asker) = (Wire::bind(), Asker::new());
    let handshake = connect(&mut wire, &mut node, &mut asker, &peer);
    let now = Instant::now();
    asker
        .ask(
            &mut node,
            now,
            2,
            (Kind::Vote, Class::Control),
            1,
            None,
            PERIOD,
        )
        .unwrap();
    asker
        .ask(
            &mut node,
            now,
            2,
            (Kind::Put, Class::Request),
            2,
            Some(0),
            PERIOD,
        )
        .unwrap();
    asker
        .ask(
            &mut node,
            now,
            2,
            (Kind::Snapshot, Class::Bulk),
            3,
            Some(8 << 20),
            PERIOD,
        )
        .unwrap();
    for seed in 10..26 {
        asker
            .ask(
                &mut node,
                now,
                2,
                (Kind::Get, Class::Request),
                seed,
                Some(64 << 10),
                PERIOD,
            )
            .unwrap();
    }
    let took = wait(
        &mut wire,
        &mut node,
        &mut asker,
        "every exchange done",
        |_, asker| asker.finished(),
    );
    for asked in &asker.asked {
        assert_eq!(
            asked.refused,
            None,
            "{asked:?}; {:?}; {:?}",
            node.stats(),
            node.connection_stats(2)
        );
        assert_eq!(
            (asked.reply, asked.read),
            (Some(asked.body), asked.body),
            "{asked:?}"
        );
    }
    let vote = &asker.asked[0];
    let latency = vote.answered.unwrap() - vote.opened.unwrap();
    node.disconnect(Instant::now(), 2);
    wire.flush(&mut node);
    format!(
        "handshake {handshake:?}; 19 exchanges, 9.0 MiB each way, in {took:?}; a vote answered in {latency:?}"
    )
}

fn reserve() -> String {
    let setup = Setup::new(&[(1, Role::Node)]);
    let peer = setup.spawn(&[("HT_HOLD", "1".into())]);
    let mut node = setup.asker::<Mantle>(1, Role::Node, limits());
    let (mut wire, mut asker) = (Wire::bind(), Asker::new());
    connect(&mut wire, &mut node, &mut asker, &peer);
    let now = Instant::now();
    for seed in 0..4 {
        asker
            .ask(
                &mut node,
                now,
                2,
                (Kind::Snapshot, Class::Bulk),
                seed,
                Some(4 << 20),
                Duration::from_secs(60),
            )
            .unwrap();
    }
    // The fact: the bulk class has spent all the credit it may.
    wait(
        &mut wire,
        &mut node,
        &mut asker,
        "bulk credit spent",
        |node, asker| {
            node.credit(2, Class::Bulk) == Some(0) && asker.asked.iter().all(|asked| asked.blocked)
        },
    );
    let written: u64 = asker.asked.iter().map(|asked| asked.written).sum();
    let held = initial_window(1_200);
    assert!(
        written < held,
        "the held bodies are bounded by the peer's window: {written}"
    );
    let control = node.credit(2, Class::Control).unwrap();
    assert!(
        control >= class_reserve(2),
        "the reserve is kept for control: {control}"
    );
    let vote = asker
        .ask(
            &mut node,
            Instant::now(),
            2,
            (Kind::Vote, Class::Control),
            9,
            None,
            PERIOD,
        )
        .unwrap();
    let crossed = wait(
        &mut wire,
        &mut node,
        &mut asker,
        "the vote answered",
        |_, asker| asker.asked[vote].done,
    );
    assert_eq!(asker.asked[vote].refused, None);
    // Released, the bulk completes.
    asker
        .ask(
            &mut node,
            Instant::now(),
            2,
            (Kind::Vote, Class::Control),
            RELEASE,
            None,
            PERIOD,
        )
        .unwrap();
    let rest = wait(
        &mut wire,
        &mut node,
        &mut asker,
        "the bulk done",
        |_, asker| asker.finished(),
    );
    assert!(
        asker
            .asked
            .iter()
            .all(|asked| asked.refused.is_none() && asked.read == asked.body)
    );
    node.disconnect(Instant::now(), 2);
    wire.flush(&mut node);
    format!(
        "bulk held at {written} bytes; a vote crossed in {crossed:?}; 16 MiB of bulk then done in {rest:?}"
    )
}

fn refusals() -> String {
    // A peer with a strict request bound and a budget of its window and a little more, that knows
    // node 1 as a client.
    let setup = Setup::new(&[
        (1, Role::Client),
        (3, Role::Node),
        (4, Role::Node),
        (5, Role::Node),
    ]);
    let window = initial_window(1_200) + class_reserve(2);
    let peer = setup.spawn(&[
        ("HT_CLASSES", "strict".into()),
        ("HT_BUDGET", (window + 4_000).to_string()),
    ]);
    let mut tight = limits();
    tight.exchanges = 3;
    let mut node = setup.asker::<Lax>(1, Role::Client, tight);
    let (mut wire, mut asker) = (Wire::bind(), Asker::new());
    connect(&mut wire, &mut node, &mut asker, &peer);
    let now = Instant::now();
    let kind = asker
        .ask(
            &mut node,
            now,
            2,
            (Kind::Snapshot, Class::Bulk),
            1,
            None,
            PERIOD,
        )
        .unwrap();
    let bound = asker
        .ask(
            &mut node,
            now,
            2,
            (Kind::Put, Class::Request),
            2,
            Some(STRICT_REQUEST_BOUND * 4),
            PERIOD,
        )
        .unwrap();
    let progress = period();
    let head = vec![3u8; 12_000];
    let budget = node.open(now, 2, Kind::Get, &head, None, progress).unwrap();
    assert_eq!(
        asker.ask(
            &mut node,
            now,
            2,
            (Kind::Get, Class::Request),
            4,
            None,
            PERIOD
        ),
        Err(Refusal::Exchanges),
        "the table's bound"
    );
    assert_eq!(
        node.open(now, 2, Kind::Vote, b"", None, progress),
        Err(Refusal::Kind)
    );
    let mut budget_refused = None;
    wait(
        &mut wire,
        &mut node,
        &mut asker,
        "three refusals",
        |_, asker| {
            for event in &asker.events {
                if event.contains(&format!("Refused {{ exchange: {budget:?}")) {
                    budget_refused = Some(event.clone());
                }
            }
            asker.asked[kind].done && asker.asked[bound].done && budget_refused.is_some()
        },
    );
    assert_eq!(asker.asked[kind].refused, Some((Refusal::Kind, true)));
    assert_eq!(
        asker.asked[bound].refused,
        Some((Refusal::FrameBound, true))
    );
    let budget_refused = budget_refused.unwrap();
    assert!(
        budget_refused.contains("refusal: Budget, by_peer: true"),
        "{budget_refused}"
    );
    node.disconnect(Instant::now(), 2);
    wire.flush(&mut node);
    drop(peer);
    let peer = setup.spawn(&[
        ("HT_IDENTITIES", "2".into()),
        ("HT_PER_IDENTITY", "1".into()),
    ]);

    // Identity 3 twice, from two sockets: one connection per identity, so the second replaces the
    // first, which hears it closed.
    let mut first = setup.asker::<Mantle>(3, Role::Node, limits());
    let (mut first_wire, mut first_asker) = (Wire::bind(), Asker::new());
    connect(&mut first_wire, &mut first, &mut first_asker, &peer);
    let mut second = setup.asker::<Mantle>(3, Role::Node, limits());
    let (mut second_wire, mut second_asker) = (Wire::bind(), Asker::new());
    connect(&mut second_wire, &mut second, &mut second_asker, &peer);
    wait(
        &mut first_wire,
        &mut first,
        &mut first_asker,
        "the first replaced",
        |_, asker| !asker.closed.is_empty(),
    );
    // Identities 3 and 4 hold the two places; identity 5 finds none.
    let mut fourth = setup.asker::<Mantle>(4, Role::Node, limits());
    let (mut fourth_wire, mut fourth_asker) = (Wire::bind(), Asker::new());
    connect(&mut fourth_wire, &mut fourth, &mut fourth_asker, &peer);
    let mut fifth = setup.asker::<Mantle>(5, Role::Node, limits());
    let (mut fifth_wire, mut fifth_asker) = (Wire::bind(), Asker::new());
    fifth.connect(Instant::now(), 2, peer.address).unwrap();
    wait(
        &mut fifth_wire,
        &mut fifth,
        &mut fifth_asker,
        "the fifth refused",
        |_, asker| !asker.closed.is_empty() || !asker.unreachable.is_empty(),
    );
    // A certificate the peer's directory does not name.
    let stranger = Setup::new(&[(6, Role::Node)]);
    let mut unknown = {
        let (_, _, certificate, key) = &stranger.knows[0];
        let mut book = Book::default();
        book.add(&setup.two.0, 2, Role::Node);
        let config = Config {
            credentials: credentials(&setup.pki.root, certificate, key),
            role: Role::Node,
            limits: limits(),
            listen: false,
        };
        Endpoint::<Mantle, _, _>::new(config, Fixed::new(64 << 20, 8), book, Instant::now())
            .unwrap()
    };
    let (mut unknown_wire, mut unknown_asker) = (Wire::bind(), Asker::new());
    unknown.connect(Instant::now(), 2, peer.address).unwrap();
    wait(
        &mut unknown_wire,
        &mut unknown,
        &mut unknown_asker,
        "the stranger refused",
        |_, asker| !asker.closed.is_empty() || !asker.unreachable.is_empty(),
    );
    for (node, wire) in [
        (&mut second, &mut second_wire),
        (&mut fourth, &mut fourth_wire),
    ] {
        node.disconnect(Instant::now(), 2);
        wire.flush(node);
    }
    "kind, frame bound and budget refused by the peer; the table's bound and the role refused locally; \
     an identity past its bound replaced; one past the identity bound and an unknown certificate refused"
        .to_owned()
}

fn killed() -> String {
    let setup = Setup::new(&[(1, Role::Node)]);
    let peer = setup.spawn(&[]);
    let mut node = setup.asker::<Mantle>(1, Role::Node, limits());
    let (mut wire, mut asker) = (Wire::bind(), Asker::new());
    connect(&mut wire, &mut node, &mut asker, &peer);
    let upload = asker
        .ask(
            &mut node,
            Instant::now(),
            2,
            (Kind::Snapshot, Class::Bulk),
            1,
            Some(512 << 20),
            PERIOD,
        )
        .unwrap();
    wait(
        &mut wire,
        &mut node,
        &mut asker,
        "a megabyte sent",
        |_, asker| asker.asked[upload].written > 1 << 20,
    );
    let mut peer = peer;
    peer.child.kill().unwrap();
    peer.child.wait().unwrap();
    let ended = wait(
        &mut wire,
        &mut node,
        &mut asker,
        "the upload refused",
        |_, asker| asker.asked[upload].done,
    );
    assert_eq!(asker.asked[upload].refused, Some((Refusal::Stalled, false)));
    // The judgement after the kill still hears what the peer sent before it; the one after that
    // hears silence and ends the exchange: at most two periods after the kill, the third the
    // driver's lateness on a loaded machine. Before what was sent into silence stopped counting
    // as progress, every period holding a probe-timeout probe carried the exchange on (7.96 s,
    // four periods, on one loaded run).
    assert!(ended <= PERIOD * 3, "ended {ended:?} after the kill");
    // The route is retired and a new process reached.
    node.disconnect(Instant::now(), 2);
    let again = setup.spawn(&[]);
    connect(&mut wire, &mut node, &mut asker, &again);
    let next = asker
        .ask(
            &mut node,
            Instant::now(),
            2,
            (Kind::Get, Class::Request),
            2,
            Some(4096),
            PERIOD,
        )
        .unwrap();
    wait(
        &mut wire,
        &mut node,
        &mut asker,
        "the next exchange",
        |_, asker| asker.asked[next].done,
    );
    assert_eq!(asker.asked[next].refused, None);
    node.disconnect(Instant::now(), 2);
    wire.flush(&mut node);
    format!(
        "an upload to a killed peer refused as stalled {ended:?} after the kill; the next peer answered"
    )
}

fn lanes() -> String {
    const FRAMES: u32 = 4_000;
    let setup = Setup::new(&[(1, Role::Node)]);
    let peer = setup.spawn(&[]);
    let mut node = setup.asker::<Mantle>(1, Role::Node, limits());
    let (mut wire, mut asker) = (Wire::bind(), Asker::new());
    connect(&mut wire, &mut node, &mut asker, &peer);
    let window = limits().lane_window as u32;
    let mut sent = [0u32; 2];
    let began = Instant::now();
    wait(
        &mut wire,
        &mut node,
        &mut asker,
        "every frame echoed",
        |node, asker| {
            for lane in 0..2u32 {
                let echoed = asker
                    .frames
                    .iter()
                    .filter(|(which, _)| *which == lane)
                    .count() as u32;
                while sent[lane as usize] < FRAMES && sent[lane as usize] - echoed < window {
                    let index = sent[lane as usize];
                    let mut frame = vec![0u8; 512];
                    frame[..4].copy_from_slice(&index.to_be_bytes());
                    node.send_frame(2, lane, Kind::Append, &frame).unwrap();
                    sent[lane as usize] += 1;
                }
            }
            asker.frames.len() as u32 == FRAMES * 2
        },
    );
    let took = began.elapsed();
    for lane in 0..2u32 {
        let order: Vec<u32> = asker
            .frames
            .iter()
            .filter(|(which, _)| *which == lane)
            .map(|(_, frame)| u32::from_be_bytes(frame[..4].try_into().unwrap()))
            .collect();
        assert_eq!(
            order,
            (0..FRAMES).collect::<Vec<_>>(),
            "lane {lane} kept its order"
        );
    }
    node.disconnect(Instant::now(), 2);
    wire.flush(&mut node);
    format!(
        "{} frames of 512 B on two lanes, echoed in order, in {took:?}",
        FRAMES * 2
    )
}

fn main() {
    if std::env::var("HT_PEER").is_ok() {
        peer_process();
        return;
    }
    let only = std::env::args()
        .skip(1)
        .find(|argument| !argument.starts_with('-'));
    let scenarios: [(&str, fn() -> String); 5] = [
        ("exchanges", exchanges),
        ("reserve", reserve),
        ("refusals", refusals),
        ("killed", killed),
        ("lanes", lanes),
    ];
    for (name, scenario) in scenarios {
        if only.as_deref().is_some_and(|only| !name.contains(only)) {
            continue;
        }
        let began = Instant::now();
        let report = scenario();
        println!("e2e {name}: ok in {:?}: {report}", began.elapsed());
    }
}
