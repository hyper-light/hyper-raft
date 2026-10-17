//! What a node's windows hold of its socket's room and its budget when some of its peers stop
//! sending: three endpoints over hyper-sim's network, one receiving from two.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::disallowed_macros,
    clippy::cast_possible_truncation,
    clippy::cognitive_complexity,
    missing_docs
)]

mod common;

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use common::*;
use hyper_transport::tls::Tls;
use hyper_transport::{Classes, Config, Endpoint, Event, Fixed, PeerId, Refusal};

/// The turns a run may take before it is a protocol that did not end.
const TURNS: usize = 10_000;
/// The receiver, and the peers that send to it.
const RECEIVER: PeerId = 2;
const QUIETED: PeerId = 1;
const ACTIVE: PeerId = 3;

/// What the network schedules: a datagram's arrival.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Arrival {
    Datagram(hyper_sim::net::Ticket),
}

/// A certificate for each of nodes `1..=n` from one authority: made once for every run of a
/// scenario, so two runs from one seed send datagrams of the same sizes (a signature's DER length
/// varies with its key).
struct Credentials {
    pki: Pki,
    issued: Vec<(Vec<u8>, Vec<u8>)>,
}

impl Credentials {
    fn new(count: PeerId) -> Self {
        let pki = Pki::new();
        let issued = (1..=count).map(|peer| pki.issue(&name(peer))).collect();
        Self { pki, issued }
    }
}

/// Endpoints `1..=n` on hyper-sim's network, node `i` at world node `i - 1`, every path the zero
/// one: a datagram arrives the instant it is sent, in the order sent.
struct Mesh {
    now: Instant,
    start: Instant,
    nodes: Vec<Node<Mantle>>,
    world: hyper_sim::World<Arrival>,
    wire: hyper_sim::net::Net<Vec<u8>>,
    out: Vec<u8>,
}

fn address(peer: PeerId) -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 4432 + peer as u16))
}

fn peer_at(address: SocketAddr) -> PeerId {
    PeerId::from(address.port() - 4432)
}

impl Mesh {
    /// The nodes `credentials` names, each knowing every other, node `RECEIVER` given `prepare`
    /// before any dials it, on a world drawing from `source`.
    fn new(
        credentials: &Credentials,
        budget: u64,
        source: hyper_sim::Source,
        prepare: impl FnOnce(&mut Node<Mantle>),
    ) -> Self {
        let now = hyper_sim::Anchor::new().instant(0).unwrap();
        let (pki, issued) = (&credentials.pki, &credentials.issued);
        let count = issued.len() as PeerId;
        let book = || {
            let mut book = Book::default();
            for (at, (certificate, _)) in issued.iter().enumerate() {
                book.add(certificate, at as PeerId + 1, Role::Node);
            }
            book
        };
        let mut nodes: Vec<Node<Mantle>> = issued
            .iter()
            .map(|(certificate, key)| {
                let config = Config {
                    tls: Tls::mutual(&common::credentials(&pki.root, certificate, key), true)
                        .unwrap(),
                    role: Role::Node,
                    limits: limits(),
                    congestion: Default::default(),
                };
                let mut node = Endpoint::new(config, Fixed::new(budget, 64), book(), now).unwrap();
                node.set_oracle(true);
                node
            })
            .collect();
        prepare(&mut nodes[(RECEIVER - 1) as usize]);
        let limits = hyper_sim::Limits {
            events: 1 << 12,
            nodes: count as usize,
            streams: 8,
            steps: u64::MAX,
            trace_words: 0,
        };
        let mut world =
            hyper_sim::World::new(source, hyper_sim::Discipline::Ordered, limits).unwrap();
        for _ in 0..count {
            world.node(hyper_sim::Clock::default()).unwrap();
        }
        let wire = hyper_sim::net::Net::new(hyper_sim::net::NetLimits {
            flows: (count * count) as usize,
            links: 0,
            nats: 0,
            link_messages: 0,
            messages: 1 << 12,
            bytes: 1 << 26,
        });
        Self {
            now,
            start: now,
            nodes,
            world,
            wire,
            out: Vec::with_capacity(1500),
        }
    }

    /// The run's record: the digest of every arrival the world ordered.
    fn finish(self) -> hyper_sim::Record {
        self.world.finish()
    }

    fn node(&mut self, peer: PeerId) -> &mut Node<Mantle> {
        &mut self.nodes[(peer - 1) as usize]
    }

    /// Moves every datagram until no node has any.
    fn exchange(&mut self) {
        for _ in 0..1_000_000 {
            let mut moved = false;
            for at in 0..self.nodes.len() {
                while let Some(transmit) = self.nodes[at].poll_transmit(self.now, &mut self.out) {
                    let to = peer_at(transmit.destination) - 1;
                    let bytes = self.out[..transmit.size].to_vec();
                    self.wire
                        .send(
                            &mut self.world,
                            (hyper_sim::NodeId(at as u32), hyper_sim::NodeId(to as u32)),
                            bytes,
                            transmit.size,
                            Arrival::Datagram,
                        )
                        .unwrap();
                    moved = true;
                }
            }
            while self
                .world
                .earliest()
                .is_some_and(|due| due <= self.world.now())
            {
                let hyper_sim::Step::Event {
                    event: Arrival::Datagram(ticket),
                    ..
                } = self.world.next(&mut hyper_sim::Fifo).unwrap()
                else {
                    panic!("only datagrams are scheduled");
                };
                let delivery = self
                    .wire
                    .deliver(&mut self.world, ticket, Arrival::Datagram)
                    .unwrap()
                    .unwrap();
                let from = address(delivery.from.0 as PeerId + 1);
                self.nodes[delivery.to.0 as usize].handle_datagram(
                    self.now,
                    from,
                    None,
                    &delivery.payload,
                );
                moved = true;
            }
            if !moved {
                return;
            }
        }
        panic!("the network never went quiet");
    }

    /// Moves the clock to the earliest timer of any node, or to `limit` if that comes first, and
    /// fires every node's timers due.
    fn advance_until(&mut self, limit: Option<Instant>) {
        let timer = self
            .nodes
            .iter_mut()
            .filter_map(|node| node.poll_timeout())
            .min();
        let next = match (timer, limit) {
            (Some(timer), Some(limit)) => timer.min(limit),
            (Some(timer), None) => timer,
            (None, Some(limit)) => limit,
            (None, None) => panic!("a node keeps a timer: its keep-alive"),
        };
        let at = u64::try_from(next.saturating_duration_since(self.start).as_nanos()).unwrap();
        self.world.advance(at.max(self.world.now())).unwrap();
        self.now = self.now.max(next);
        for node in &mut self.nodes {
            node.handle_timeout(self.now);
        }
    }

    /// Moves the clock to the earliest timer of any node and fires every node's timers.
    fn advance(&mut self) {
        self.advance_until(None);
    }

    /// Every frame the receiver has waiting given back as soon as it came: an owner that reads
    /// what arrives at once, whose windows therefore grow.
    fn read_frames(&mut self) {
        let receiver = self.node(RECEIVER);
        while let Some(event) = receiver.poll_event() {
            if let Event::Frame { frame, .. } = event {
                receiver.release(frame);
            }
        }
    }

    /// `frames` frames of `bytes` from `peer` to the receiver, each read as it comes.
    fn send(&mut self, peer: PeerId, frames: usize, bytes: usize) {
        let frame = vec![7u8; bytes];
        let mut sent = 0;
        for _ in 0..TURNS {
            while sent < frames {
                match self
                    .node(peer)
                    .send_frame(RECEIVER, 0, Kind::Append, &frame)
                {
                    Ok(()) => sent += 1,
                    Err(Refusal::LaneFull) => break,
                    Err(other) => panic!("{other:?}"),
                }
            }
            self.exchange();
            self.read_frames();
            self.exchange();
            if sent == frames {
                return;
            }
        }
        panic!("{frames} frames never left node {peer}");
    }

    /// [`Mesh::send`] at most `per_turn` frames at a time, the clock run on `gap` between them:
    /// a peer whose window is never consumed whole within two round trips, so it does not grow.
    fn send_slowly(
        &mut self,
        peer: PeerId,
        frames: usize,
        bytes: usize,
        per_turn: usize,
        gap: Duration,
    ) {
        let mut left = frames;
        while left > 0 {
            let now = per_turn.min(left);
            self.send(peer, now, bytes);
            left -= now;
            self.wait(gap);
        }
    }

    /// The window the receiver advertises to `peer` now, reserve excluded.
    fn window(&mut self, peer: PeerId) -> u64 {
        self.node(RECEIVER).path(peer).unwrap().receive_window
    }

    /// Runs the clock on by `quiet` exactly, firing every timer on the way and reading every frame.
    fn wait(&mut self, quiet: Duration) {
        let until = self.now + quiet;
        while self.now < until {
            self.advance_until(Some(until));
            self.exchange();
            self.read_frames();
        }
    }
}

/// A connection's first window and the reserve on top of it: what a node grants at a handshake.
fn first_window() -> u64 {
    hyper_transport::initial_window(hyper_transport::MIN_DATAGRAM)
        + hyper_transport::class_reserve(<Mantle as Classes>::RANKS - 1)
}

/// Connects `peer` to the receiver.
fn connect(mesh: &mut Mesh, peer: PeerId) {
    let now = mesh.now;
    mesh.node(peer)
        .connect(now, RECEIVER, address(RECEIVER))
        .unwrap();
    for _ in 0..TURNS {
        mesh.exchange();
        let up = std::iter::from_fn(|| mesh.node(peer).poll_event())
            .any(|event| matches!(event, Event::Connected { .. }));
        mesh.read_frames();
        if up {
            return;
        }
        mesh.advance();
    }
    panic!("node {peer} never connected");
}

/// A peer that grew its window and went quiet holds the growth's room only until another window
/// wants it: its window is set back to its first, and what the growth held comes back as the
/// peer's own reads pay off the credit QUIC already gave it, never sooner, so the windows granted
/// never pass the room. Then the active peer's window grows into it.
///
/// Before the window was set back, the quiet peer's growth stood for as long as its connection:
/// the active peer's window never grew past what the room left it, even after the quiet peer came
/// back and sent again, each read re-extending the grown window.
#[test]
fn a_quiet_peers_grown_window_is_set_back_when_another_wants_the_room() {
    set_back(&Credentials::new(3));
}

/// The run-twice check (docs/sim.md §3.9): the three nodes connect on the zero path to one digest
/// from a seed twice and from its trace, whatever keys and nonces the endpoints draw for
/// themselves. The scenarios past the handshakes are judged by their exact counts instead: their
/// datagrams' sizes carry the endpoints' own draws (a signature's DER length, QUIC's greasing),
/// which move how many datagrams a burst takes.
#[test]
fn the_nodes_connect_the_same_twice() {
    let credentials = Credentials::new(3);
    let record = hyper_sim::twice(0, |source| {
        let mut mesh = Mesh::new(&credentials, 256 << 20, source, |_| {});
        connect(&mut mesh, QUIETED);
        connect(&mut mesh, ACTIVE);
        Ok::<_, std::convert::Infallible>(mesh.finish())
    })
    .unwrap();
    assert!(record.steps > 0);
}

/// [`a_quiet_peers_grown_window_is_set_back_when_another_wants_the_room`]'s run.
fn set_back(credentials: &Credentials) {
    let first = first_window();
    // The test limits let 8 handshakes wait, their first windows kept; beside the two
    // connections' first windows the room holds four more.
    let kept = 8 * first;
    let capacity = 2 * first + kept + 4 * first;
    let mut mesh = Mesh::new(
        credentials,
        256 << 20,
        hyper_sim::Source::Seed(0),
        |receiver| receiver.set_receive_capacity(capacity, 0),
    );
    connect(&mut mesh, QUIETED);
    connect(&mut mesh, ACTIVE);
    let initial = mesh.window(QUIETED);
    assert_eq!(mesh.window(ACTIVE), initial);

    // The first peer's window grows into the room while the receiver reads it at once.
    mesh.send(QUIETED, 2_000, 1_000);
    let grown = mesh.window(QUIETED);
    assert!(grown > initial, "the window grew: {grown}");
    let granted = mesh.node(RECEIVER).stats().receive_granted;
    assert!(granted <= capacity, "{granted} granted past {capacity}");

    // It goes quiet past the receiver's serve period, its connection kept alive.
    mesh.wait(limits().serve_period * 2);
    assert_eq!(mesh.window(QUIETED), grown, "nothing wanted its room yet");

    // The other peer's window wants to grow into the room the first one's growth holds.
    mesh.send(ACTIVE, 2_000, 1_000);
    let stats = mesh.node(RECEIVER).stats();
    assert_eq!(
        stats.windows_set_back, 1,
        "the quiet peer's window was set back"
    );
    assert_eq!(mesh.window(QUIETED), initial);
    assert!(
        stats.receive_granted <= capacity,
        "{} granted past {capacity}",
        stats.receive_granted
    );
    let held = mesh.window(ACTIVE);

    // Nothing comes back until the quiet peer's reads pay what QUIC already gave it.
    assert_eq!(
        mesh.node(RECEIVER).stats().receive_granted,
        stats.receive_granted,
        "the credit given stands"
    );

    // It sends again, slowly enough that its window does not grow: its reads pay off what QUIC
    // gave it past its first window, and the growth goes back.
    let debt = grown - initial;
    let frames = (debt / 1_000 + 1) as usize;
    mesh.send_slowly(QUIETED, frames, 1_000, 10, Duration::from_millis(10));
    assert_eq!(mesh.window(QUIETED), initial, "it did not grow again");
    let returned = mesh.node(RECEIVER).stats().receive_granted;
    assert_eq!(
        returned,
        stats.receive_granted - debt,
        "the growth went back, and nothing more"
    );

    // The active peer's window grows into the room.
    mesh.send(ACTIVE, 2_000, 1_000);
    let grew = mesh.window(ACTIVE);
    assert!(
        grew > held,
        "the active peer's window grew into the room: {held} to {grew}"
    );
    let granted = mesh.node(RECEIVER).stats().receive_granted;
    assert!(granted <= capacity, "{granted} granted past {capacity}");
}

/// A peer whose window was set back and that sends nothing more pays nothing back: QUIC keeps the
/// credit it was given. Once it has stayed idle a quiet period since, its window owing what is
/// wanted again, its connection is closed, typed, and the room it held comes back as it goes, for
/// the active peer's window to grow into.
///
/// Before the close, the active peer's window stood where the room left it for as long as the
/// quiet peer's connection was kept alive.
#[test]
fn a_set_back_peer_that_stays_quiet_is_closed_for_the_room() {
    let first = first_window();
    let kept = 8 * first;
    let capacity = 2 * first + kept + 4 * first;
    let mut mesh = Mesh::new(
        &Credentials::new(3),
        256 << 20,
        hyper_sim::Source::Seed(0),
        |receiver| receiver.set_receive_capacity(capacity, 0),
    );
    connect(&mut mesh, QUIETED);
    connect(&mut mesh, ACTIVE);
    mesh.send(QUIETED, 2_000, 1_000);
    let grown = mesh.window(QUIETED);
    mesh.wait(limits().serve_period * 2);
    mesh.send(ACTIVE, 2_000, 1_000);
    assert_eq!(mesh.node(RECEIVER).stats().windows_set_back, 1);
    let held = mesh.window(ACTIVE);
    let granted = mesh.node(RECEIVER).stats().receive_granted;

    // The quiet peer sends nothing; the active one keeps wanting the room. Within the quiet
    // period after the set back nothing closes.
    mesh.wait(limits().serve_period / 2);
    mesh.send(ACTIVE, 2_000, 1_000);
    let stats = mesh.node(RECEIVER).stats();
    assert_eq!(stats.windows_closed, 0, "not yet a quiet period since");
    assert_eq!(stats.receive_granted, granted, "the credit given stands");

    // Past it, the next want closes the quiet peer's connection.
    mesh.wait(limits().serve_period);
    mesh.send(ACTIVE, 2_000, 1_000);
    assert_eq!(mesh.node(RECEIVER).stats().windows_closed, 1);
    let mut closed = None;
    for _ in 0..TURNS {
        if let Some(why) =
            std::iter::from_fn(|| mesh.node(QUIETED).poll_event()).find_map(|event| match event {
                Event::Closed { peer, refusal, .. } if peer == RECEIVER => Some(refusal),
                _ => None,
            })
        {
            closed = Some(why);
            break;
        }
        mesh.advance();
        mesh.exchange();
        mesh.read_frames();
    }
    assert_eq!(
        closed,
        Some(Some(Refusal::ReceiveBuffer)),
        "the quiet peer is told why"
    );

    // Once it is gone, what it held is back, and the active peer's window grows into it.
    for _ in 0..TURNS {
        if mesh.node(RECEIVER).stats().connections == 1 {
            break;
        }
        mesh.advance();
        mesh.exchange();
        mesh.read_frames();
    }
    assert_eq!(mesh.node(RECEIVER).stats().connections, 1);
    let after = mesh.node(RECEIVER).stats().receive_granted;
    let initial = hyper_transport::initial_window(hyper_transport::MIN_DATAGRAM);
    assert_eq!(
        after,
        granted - (first + grown - initial),
        "the quiet peer's first window and growth came back"
    );
    mesh.send(ACTIVE, 2_000, 1_000);
    let grew = mesh.window(ACTIVE);
    assert!(
        grew > held,
        "the active peer's window grew into the room: {held} to {grew}"
    );
    let granted = mesh.node(RECEIVER).stats().receive_granted;
    assert!(granted <= capacity, "{granted} granted past {capacity}");
}
