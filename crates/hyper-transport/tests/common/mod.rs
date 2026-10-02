//! What the tests, the end-to-end scenarios and the benchmarks share: a certificate authority, a
//! project's classes (mantle's: control, request, bulk), a directory, and an in-memory network
//! that drives two endpoints on the caller's clock.

#![allow(
    clippy::unwrap_in_result,
    clippy::type_complexity,
    clippy::too_many_arguments,
    clippy::string_slice,
    clippy::cast_possible_truncation,
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::disallowed_macros,
    missing_docs,
    unreachable_pub
)]

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use hyper_transport::tls::{CertificateDer, Credentials, PrivateKeyDer, PrivatePkcs8KeyDer};
use hyper_transport::{
    AdmissionLimits, Budget, Classes, Config, Directory, Endpoint, Event, Fixed, Limits, PeerId,
    Transmit,
};

/// The deployment's authority, issuing one certificate per node.
pub struct Pki {
    issuer: rcgen::Issuer<'static, rcgen::KeyPair>,
    pub root: Vec<u8>,
}

impl Pki {
    pub fn new() -> Self {
        let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let key = rcgen::KeyPair::generate().unwrap();
        let root = params.self_signed(&key).unwrap().der().to_vec();
        Self {
            issuer: rcgen::Issuer::new(params, key),
            root,
        }
    }
    /// A certificate for `name` and its PKCS #8 key, both DER.
    pub fn issue(&self, name: &str) -> (Vec<u8>, Vec<u8>) {
        let params = rcgen::CertificateParams::new(vec![name.to_owned()]).unwrap();
        let key = rcgen::KeyPair::generate().unwrap();
        let certificate = params.signed_by(&key, &self.issuer).unwrap();
        (certificate.der().to_vec(), key.serialize_der())
    }
}

pub fn credentials(root: &[u8], certificate: &[u8], key: &[u8]) -> Credentials {
    Credentials {
        chain: vec![CertificateDer::from(certificate.to_vec())],
        key: PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.to_vec())),
        roots: vec![CertificateDer::from(root.to_vec())],
    }
}

/// The name a node's certificate carries.
pub fn name(peer: PeerId) -> String {
    format!("node-{peer}")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Class {
    Control,
    Request,
    Bulk,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Vote,
    Get,
    Put,
    Snapshot,
    Append,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Node,
    Client,
}

const KINDS: [Kind; 5] = [
    Kind::Vote,
    Kind::Get,
    Kind::Put,
    Kind::Snapshot,
    Kind::Append,
];

fn kind_code(kind: Kind) -> u16 {
    KINDS.iter().position(|known| *known == kind).unwrap() as u16 + 1
}

fn kind_of(code: u16) -> Option<Kind> {
    KINDS.get(usize::from(code).checked_sub(1)?).copied()
}

fn rank(class: Class) -> u8 {
    match class {
        Class::Control => 0,
        Class::Request => 1,
        Class::Bulk => 2,
    }
}

/// mantle's classes: a vote and an append are control and only a node sends them; a get and a put
/// are requests from anyone; a snapshot is bulk and only a node sends it.
pub struct Mantle;

/// The frame bounds of [`Mantle`].
pub const CONTROL_BOUND: u64 = 1 << 20;
pub const REQUEST_BOUND: u64 = 64 << 20;
pub const BULK_BOUND: u64 = 1 << 30;

fn mantle_class(kind: Kind, role: Role) -> Option<Class> {
    match (kind, role) {
        (Kind::Vote | Kind::Append, Role::Node) => Some(Class::Control),
        (Kind::Get | Kind::Put, _) => Some(Class::Request),
        (Kind::Snapshot, Role::Node) => Some(Class::Bulk),
        _ => None,
    }
}

impl Classes for Mantle {
    type Class = Class;
    type Kind = Kind;
    type Role = Role;
    const RANKS: u8 = 3;
    fn rank(class: Class) -> u8 {
        rank(class)
    }
    fn class_of(kind: Kind, role: Role) -> Option<Class> {
        mantle_class(kind, role)
    }
    fn frame_bound(class: Class) -> u64 {
        match class {
            Class::Control => CONTROL_BOUND,
            Class::Request => REQUEST_BOUND,
            Class::Bulk => BULK_BOUND,
        }
    }
    fn kind_code(kind: Kind) -> u16 {
        kind_code(kind)
    }
    fn kind_of(code: u16) -> Option<Kind> {
        kind_of(code)
    }
}

/// [`Mantle`] with a request bound of [`STRICT_REQUEST_BOUND`]: a receiver whose bound is below its
/// sender's.
pub struct Strict;
pub const STRICT_REQUEST_BOUND: u64 = 64 << 10;

impl Classes for Strict {
    type Class = Class;
    type Kind = Kind;
    type Role = Role;
    const RANKS: u8 = 3;
    fn rank(class: Class) -> u8 {
        rank(class)
    }
    fn class_of(kind: Kind, role: Role) -> Option<Class> {
        mantle_class(kind, role)
    }
    fn frame_bound(class: Class) -> u64 {
        match class {
            Class::Request => STRICT_REQUEST_BOUND,
            other => Mantle::frame_bound(other),
        }
    }
    fn kind_code(kind: Kind) -> u16 {
        kind_code(kind)
    }
    fn kind_of(code: u16) -> Option<Kind> {
        kind_of(code)
    }
}

/// [`Mantle`] that lets a client send a snapshot: a sender that believes a role may send what its
/// receiver says it may not.
pub struct Lax;

impl Classes for Lax {
    type Class = Class;
    type Kind = Kind;
    type Role = Role;
    const RANKS: u8 = 3;
    fn rank(class: Class) -> u8 {
        rank(class)
    }
    fn class_of(kind: Kind, role: Role) -> Option<Class> {
        match (kind, role) {
            (Kind::Snapshot, Role::Client) => Some(Class::Bulk),
            _ => mantle_class(kind, role),
        }
    }
    fn frame_bound(class: Class) -> u64 {
        Mantle::frame_bound(class)
    }
    fn kind_code(kind: Kind) -> u16 {
        kind_code(kind)
    }
    fn kind_of(code: u16) -> Option<Kind> {
        kind_of(code)
    }
}

/// Which certificate is which peer.
#[derive(Default)]
pub struct Book {
    entries: Vec<(Vec<u8>, PeerId, Role)>,
    names: Vec<(PeerId, String)>,
}

impl Book {
    pub fn add(&mut self, certificate: &[u8], peer: PeerId, role: Role) {
        self.entries.push((certificate.to_vec(), peer, role));
        self.names.push((peer, name(peer)));
    }
}

impl Directory for Book {
    type Role = Role;
    fn identify(&mut self, certificate: &[u8]) -> Option<(PeerId, Role)> {
        self.entries
            .iter()
            .find(|(known, _, _)| known == certificate)
            .map(|(_, peer, role)| (*peer, *role))
    }
    fn server_name(&self, peer: PeerId) -> Option<&str> {
        self.names
            .iter()
            .find(|(known, _)| *known == peer)
            .map(|(_, name)| name.as_str())
    }
}

/// Bounds for the tests: small enough to reach, large enough to work.
pub fn limits() -> Limits {
    Limits {
        admission: AdmissionLimits {
            pending: 8,
            identities: 8,
            connections: 16,
            per_identity: 2,
        },
        streams_per_connection: 16,
        exchanges: 64,
        lanes_per_peer: 4,
        lane_window: 128,
        max_head: 16 << 10,
        window_ceiling: 16 << 20,
        serve_period: Duration::from_secs(2),
        idle_timeout: Duration::from_secs(10),
        keep_alive: Some(Duration::from_secs(2)),
        max_responses: 16,
        // A full 16 MiB window of unread data, packed: 16 MiB / 65,527 B, and one.
        receive_chunks: (16 << 20) / hyper_transport::RECEIVE_CHUNK + 1,
    }
}

/// What a driver needs of an endpoint, whatever its classes.
pub trait Drive {
    fn datagram(&mut self, now: Instant, from: SocketAddr, bytes: &[u8]);
    fn transmit(&mut self, now: Instant, out: &mut Vec<u8>) -> Option<Transmit>;
    fn timeout(&mut self) -> Option<Instant>;
    fn fire(&mut self, now: Instant);
    /// The driver measured its timer granularity: what has a use for it takes it.
    fn granularity(&mut self, _granularity: Duration) {}
}

impl<C: Classes, B: Budget<C::Class>, D: Directory<Role = C::Role>> Drive for Endpoint<C, B, D> {
    fn datagram(&mut self, now: Instant, from: SocketAddr, bytes: &[u8]) {
        self.handle_datagram(now, from, None, bytes);
    }
    fn transmit(&mut self, now: Instant, out: &mut Vec<u8>) -> Option<Transmit> {
        self.poll_transmit(now, out)
    }
    fn timeout(&mut self) -> Option<Instant> {
        self.poll_timeout()
    }
    fn fire(&mut self, now: Instant) {
        self.handle_timeout(now);
    }
    fn granularity(&mut self, granularity: Duration) {
        self.set_granularity(granularity);
    }
}

/// A node of the in-memory network.
pub type Node<C> = Endpoint<C, Fixed, Book>;

/// Two endpoints, `a` and `b`, joined by an in-memory network on the caller's clock. Datagrams
/// arrive the instant they are sent; time moves only to the next timer when nothing is in flight.
/// A datagram's buffer is reused, so the network allocates nothing once warm.
pub struct Net<A: Drive, B: Drive> {
    pub now: Instant,
    pub a: A,
    pub b: B,
    pub a_address: SocketAddr,
    pub b_address: SocketAddr,
    /// Datagrams in flight: towards `b` when the flag is set.
    queue: VecDeque<(bool, Vec<u8>)>,
    spare: Vec<Vec<u8>>,
    out: Vec<u8>,
    /// Whether datagrams to `b` are lost: `b` is gone.
    pub b_dead: bool,
}

impl<A: Drive, B: Drive> Net<A, B> {
    pub fn new(now: Instant, a: A, b: B) -> Self {
        Self {
            now,
            a,
            b,
            a_address: "127.0.0.1:4433".parse().unwrap(),
            b_address: "127.0.0.1:4434".parse().unwrap(),
            queue: VecDeque::with_capacity(64),
            spare: Vec::with_capacity(64),
            out: Vec::with_capacity(1500),
            b_dead: false,
        }
    }

    /// Moves every datagram until neither side has any; returns whether any moved.
    pub fn exchange(&mut self) -> bool {
        let mut moved = false;
        for _ in 0..1_000_000 {
            let mut any = false;
            while let Some(transmit) = self.a.transmit(self.now, &mut self.out) {
                let mut bytes = self.spare.pop().unwrap_or_default();
                bytes.clear();
                bytes.extend_from_slice(&self.out[..transmit.size]);
                self.queue.push_back((true, bytes));
                any = true;
            }
            while let Some(transmit) = self.b.transmit(self.now, &mut self.out) {
                let mut bytes = self.spare.pop().unwrap_or_default();
                bytes.clear();
                bytes.extend_from_slice(&self.out[..transmit.size]);
                self.queue.push_back((false, bytes));
                any = true;
            }
            while let Some((to_b, bytes)) = self.queue.pop_front() {
                if to_b {
                    if !self.b_dead {
                        self.b.datagram(self.now, self.a_address, &bytes);
                    }
                } else {
                    self.a.datagram(self.now, self.b_address, &bytes);
                }
                self.spare.push(bytes);
                any = true;
            }
            if !any {
                return moved;
            }
            moved = true;
        }
        panic!("the network never went quiet");
    }

    /// Fires the earliest timer of either side, moving the clock to it.
    pub fn advance(&mut self) -> bool {
        let next = [
            self.a.timeout(),
            if self.b_dead { None } else { self.b.timeout() },
        ]
        .into_iter()
        .flatten()
        .min();
        let Some(next) = next else {
            return false;
        };
        self.now = self.now.max(next);
        self.a.fire(self.now);
        if !self.b_dead {
            self.b.fire(self.now);
        }
        true
    }

    /// Moves the clock to the earliest timer of either side, or by `most` if that comes first: an
    /// owner that acts at least every `most` sees time pass no faster than that.
    pub fn advance_within(&mut self, most: Duration) {
        let next = [
            self.a.timeout(),
            if self.b_dead { None } else { self.b.timeout() },
        ]
        .into_iter()
        .flatten()
        .min()
        .map_or(self.now + most, |next| next.min(self.now + most));
        self.now = self.now.max(next);
        self.a.fire(self.now);
        if !self.b_dead {
            self.b.fire(self.now);
        }
    }

    /// Runs the network until `done` holds, exchanging datagrams and firing timers; `done` is
    /// asked after every exchange, so it can poll events and act on them. Panics after `turns`
    /// turns of the clock: a test's bound on a protocol that did not end.
    pub fn until(&mut self, turns: usize, mut done: impl FnMut(&mut Self) -> bool) {
        for _ in 0..turns {
            self.exchange();
            if done(self) {
                return;
            }
            if !self.exchange() && !self.advance() {
                if done(self) {
                    return;
                }
                panic!("nothing is left to happen");
            }
        }
        panic!("the condition never held in {turns} turns");
    }
}

/// The parts of two nodes that know each other: node 1 a client of node 2, or both nodes.
pub struct Pair {
    pub pki: Pki,
    pub one: (Vec<u8>, Vec<u8>),
    pub two: (Vec<u8>, Vec<u8>),
}

impl Pair {
    pub fn new() -> Self {
        let pki = Pki::new();
        let one = pki.issue(&name(1));
        let two = pki.issue(&name(2));
        Self { pki, one, two }
    }
    pub fn book(&self, one: Role, two: Role) -> Book {
        let mut book = Book::default();
        book.add(&self.one.0, 1, one);
        book.add(&self.two.0, 2, two);
        book
    }
    pub fn node<C: Classes<Role = Role>>(
        &self,
        which: PeerId,
        role: Role,
        book: Book,
        limits: Limits,
        budget: u64,
        now: Instant,
    ) -> Node<C> {
        let (certificate, key) = if which == 1 { &self.one } else { &self.two };
        let config = Config {
            credentials: credentials(&self.pki.root, certificate, key),
            role,
            limits,
            listen: true,
        };
        Endpoint::new(config, Fixed::new(budget, 64), book, now).unwrap()
    }
}

/// Every event of `endpoint` so far.
pub fn events<C: Classes, B: Budget<C::Class>, D: Directory<Role = C::Role>>(
    endpoint: &mut Endpoint<C, B, D>,
) -> Vec<Event<C>> {
    std::iter::from_fn(|| endpoint.poll_event()).collect()
}

/// The byte at `offset` of a body seeded `seed`: what every body in the tests is made of, so that
/// a receiver checks every byte without holding the body.
pub fn pattern(seed: u64, offset: u64) -> u8 {
    (offset.wrapping_mul(31).wrapping_add(seed) % 251) as u8
}

/// Fills `into` with the pattern from `offset`.
pub fn fill(seed: u64, offset: u64, into: &mut [u8]) {
    for (at, byte) in into.iter_mut().enumerate() {
        *byte = pattern(seed, offset + at as u64);
    }
}

/// The piece the tests read a body in, and write one in.
pub const PIECE: usize = 64 << 10;

/// One exchange as the side that answers it sees it.
#[derive(Debug)]
struct Served {
    exchange: hyper_transport::ExchangeId,
    class: Class,
    seed: u64,
    /// The request body read so far, and its length.
    read: u64,
    body: u64,
    replied: bool,
    /// The reply body written so far, and its length.
    written: u64,
    reply: u64,
}

/// A node that answers every request: it reads the request's body, checking every byte, and
/// replies with a head of `ok:` and the request's head, and a body as long as the request's.
/// Bulk bodies are left unread while `hold_bulk` is set.
pub struct Server {
    served: Vec<Served>,
    pub hold_bulk: bool,
    pub answered: u64,
    pub frames: Vec<(PeerId, u32, Vec<u8>)>,
    pub connected: Vec<PeerId>,
    pub closed: Vec<PeerId>,
    pub refused: Vec<(hyper_transport::Refusal, bool)>,
    piece: Vec<u8>,
}

impl Server {
    pub fn new() -> Self {
        Self {
            served: Vec::new(),
            hold_bulk: false,
            answered: 0,
            frames: Vec::new(),
            connected: Vec::new(),
            closed: Vec::new(),
            refused: Vec::new(),
            piece: vec![0; PIECE],
        }
    }

    /// Handles every event of `node` and moves every exchange as far as it goes.
    pub fn serve<C: Classes<Kind = Kind, Class = Class, Role = Role>>(
        &mut self,
        node: &mut Node<C>,
    ) {
        while let Some(event) = node.poll_event() {
            self.on_event(node, event);
        }
        self.advance_all(node);
    }

    /// Handles one event of `node`; [`Server::advance_all`] then moves the exchanges on.
    pub fn on_event<C: Classes<Kind = Kind, Class = Class, Role = Role>>(
        &mut self,
        node: &mut Node<C>,
        event: Event<C>,
    ) {
        match event {
            Event::Request {
                exchange,
                class,
                body,
                ..
            } => {
                let head = node.head(exchange).unwrap();
                let seed = u64::from(head.first().copied().unwrap_or(0));
                if seed == u64::from(RELEASE) {
                    self.hold_bulk = false;
                }
                self.served.push(Served {
                    exchange,
                    class,
                    seed,
                    read: 0,
                    body: body.unwrap_or(0),
                    replied: false,
                    written: 0,
                    reply: body.unwrap_or(0),
                });
            }
            Event::Frame {
                peer, lane, frame, ..
            } => {
                self.frames.push((peer, lane, frame.bytes().to_vec()));
                node.release(frame);
            }
            Event::Connected { peer, .. } => self.connected.push(peer),
            Event::Closed { peer, .. } => self.closed.push(peer),
            Event::Refused {
                exchange,
                refusal,
                by_peer,
            } => {
                self.refused.push((refusal, by_peer));
                self.served.retain(|served| served.exchange != exchange);
            }
            _ => {}
        }
    }

    /// Moves every exchange as far as it goes.
    pub fn advance_all<C: Classes<Kind = Kind, Class = Class, Role = Role>>(
        &mut self,
        node: &mut Node<C>,
    ) {
        let mut served = std::mem::take(&mut self.served);
        served.retain_mut(|exchange| !self.advance(node, exchange));
        self.served = served;
    }

    /// Moves one exchange on; whether it is done.
    fn advance<C: Classes<Kind = Kind, Class = Class, Role = Role>>(
        &mut self,
        node: &mut Node<C>,
        served: &mut Served,
    ) -> bool {
        if served.class == Class::Bulk && self.hold_bulk {
            return false;
        }
        while served.read < served.body {
            let Ok(mut into) = node.reserve(served.class, PIECE as u64) else {
                return false;
            };
            let got = node.read_body(served.exchange, &mut into);
            for (at, byte) in into.bytes().iter().enumerate() {
                assert_eq!(
                    *byte,
                    pattern(served.seed, served.read + at as u64),
                    "a request byte"
                );
            }
            node.release(into);
            match got {
                Ok(0) => return false,
                Ok(got) => served.read += got as u64,
                Err(_) => return true,
            }
        }
        if !node.body_complete(served.exchange) && served.body > 0 {
            let mut empty = hyper_transport::Reservation::default();
            let _ = node.read_body(served.exchange, &mut empty);
            if !node.body_complete(served.exchange) {
                return false;
            }
        }
        if !served.replied {
            let mut head = b"ok:".to_vec();
            head.extend_from_slice(node.head(served.exchange).unwrap_or(&[]));
            let body = (served.reply > 0 || served.body > 0).then_some(served.reply);
            if node.reply(served.exchange, &head, body).is_err() {
                return true;
            }
            served.replied = true;
        }
        while served.written < served.reply {
            let length = (served.reply - served.written).min(PIECE as u64) as usize;
            fill(served.seed, served.written, &mut self.piece[..length]);
            match node.write_body(served.exchange, &self.piece[..length]) {
                Ok(0) => return false,
                Ok(took) => served.written += took as u64,
                Err(_) => return true,
            }
        }
        node.end(served.exchange);
        self.answered += 1;
        true
    }
}

/// One exchange as the side that asked sees it.
#[derive(Debug, Clone)]
pub struct Asked {
    pub exchange: hyper_transport::ExchangeId,
    pub class: Class,
    pub seed: u64,
    pub head: Vec<u8>,
    pub written: u64,
    pub body: u64,
    pub reply: Option<u64>,
    pub read: u64,
    pub done: bool,
    pub refused: Option<(hyper_transport::Refusal, bool)>,
    /// A body write was refused for want of credit.
    pub blocked: bool,
    /// When the reply's head arrived, and when the exchange was opened, by the driver's clock.
    pub opened: Option<Instant>,
    pub answered: Option<Instant>,
}

/// A node that asks: it writes request bodies as credit allows, reads replies, checking every
/// byte, and ends each exchange once its reply is whole.
pub struct Asker {
    pub asked: Vec<Asked>,
    pub frames: Vec<(u32, Vec<u8>)>,
    /// The driver's clock, as it last said.
    pub clock: Instant,
    pub events: Vec<String>,
    pub connected: Vec<PeerId>,
    pub unreachable: Vec<PeerId>,
    pub closed: Vec<PeerId>,
    piece: Vec<u8>,
}

impl Asker {
    pub fn new() -> Self {
        Self {
            asked: Vec::new(),
            frames: Vec::new(),
            clock: Instant::now(),
            events: Vec::new(),
            connected: Vec::new(),
            unreachable: Vec::new(),
            closed: Vec::new(),
            piece: vec![0; PIECE],
        }
    }

    /// Asks `peer` a `kind` with a head starting with `seed` and a body of `body` bytes.
    pub fn ask<C: Classes<Kind = Kind, Class = Class, Role = Role>>(
        &mut self,
        node: &mut Node<C>,
        now: Instant,
        peer: PeerId,
        (kind, class): (Kind, Class),
        seed: u8,
        body: Option<u64>,
        period: Duration,
    ) -> Result<usize, hyper_transport::Refusal> {
        let head = vec![seed, 1, 2, 3];
        let progress = hyper_transport::Progress::new(period).unwrap();
        let exchange = node.open(now, peer, kind, &head, body, progress)?;
        self.asked.push(Asked {
            exchange,
            class,
            seed: u64::from(seed),
            head,
            written: 0,
            body: body.unwrap_or(0),
            reply: None,
            read: 0,
            done: false,
            refused: None,
            blocked: false,
            opened: Some(now),
            answered: None,
        });
        Ok(self.asked.len() - 1)
    }

    pub fn drive<C: Classes<Kind = Kind, Class = Class, Role = Role>>(
        &mut self,
        node: &mut Node<C>,
    ) {
        while let Some(event) = node.poll_event() {
            self.on_event(node, event);
        }
        self.advance_all(node);
    }

    /// Handles one event of `node`; [`Asker::advance_all`] then moves the exchanges on.
    pub fn on_event<C: Classes<Kind = Kind, Class = Class, Role = Role>>(
        &mut self,
        node: &mut Node<C>,
        event: Event<C>,
    ) {
        self.events.push(format!("{event:?}"));
        match event {
            Event::Reply { exchange, body } => {
                if let Some(asked) = self
                    .asked
                    .iter_mut()
                    .find(|asked| asked.exchange == exchange)
                {
                    asked.reply = Some(body.unwrap_or(0));
                    asked.answered = Some(self.clock);
                    let mut head = b"ok:".to_vec();
                    head.extend_from_slice(&asked.head);
                    assert_eq!(node.head(exchange), Some(&head[..]), "the reply's head");
                }
            }
            Event::Refused {
                exchange,
                refusal,
                by_peer,
            } => {
                if let Some(asked) = self
                    .asked
                    .iter_mut()
                    .find(|asked| asked.exchange == exchange)
                {
                    asked.refused = Some((refusal, by_peer));
                    asked.done = true;
                }
            }
            Event::Connected { peer, .. } => self.connected.push(peer),
            Event::Unreachable { peer } => self.unreachable.push(peer),
            Event::Frame { lane, frame, .. } => {
                self.frames.push((lane, frame.bytes().to_vec()));
                node.release(frame);
            }
            Event::Closed { peer, .. } => self.closed.push(peer),
            _ => {}
        }
    }

    /// Moves every exchange as far as it goes.
    pub fn advance_all<C: Classes<Kind = Kind, Class = Class, Role = Role>>(
        &mut self,
        node: &mut Node<C>,
    ) {
        for at in 0..self.asked.len() {
            self.advance(node, at);
        }
    }

    fn advance<C: Classes<Kind = Kind, Class = Class, Role = Role>>(
        &mut self,
        node: &mut Node<C>,
        at: usize,
    ) {
        let asked = &mut self.asked[at];
        if asked.done {
            return;
        }
        while asked.written < asked.body {
            let length = (asked.body - asked.written).min(PIECE as u64) as usize;
            fill(asked.seed, asked.written, &mut self.piece[..length]);
            match node.write_body(asked.exchange, &self.piece[..length]) {
                Ok(0) => {
                    asked.blocked = true;
                    break;
                }
                Err(_) => break,
                Ok(took) => asked.written += took as u64,
            }
        }
        let Some(reply) = asked.reply else {
            return;
        };
        while asked.read < reply {
            let Ok(mut into) = node.reserve(asked.class, PIECE as u64) else {
                return;
            };
            let got = node.read_body(asked.exchange, &mut into);
            for (offset, byte) in into.bytes().iter().enumerate() {
                assert_eq!(
                    *byte,
                    pattern(asked.seed, asked.read + offset as u64),
                    "a reply byte"
                );
            }
            node.release(into);
            match got {
                Ok(0) | Err(_) => return,
                Ok(got) => asked.read += got as u64,
            }
        }
        if reply > 0 && !node.body_complete(asked.exchange) {
            let mut empty = hyper_transport::Reservation::default();
            let _ = node.read_body(asked.exchange, &mut empty);
            if !node.body_complete(asked.exchange) {
                return;
            }
        }
        if asked.written == asked.body {
            node.end(asked.exchange);
            asked.done = true;
        }
    }

    pub fn finished(&self) -> bool {
        self.asked.iter().all(|asked| asked.done)
    }
}

/// The seed of a request that releases a [`Server`]'s held bulk bodies.
pub const RELEASE: u8 = 255;

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn unhex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&text[at..at + 2], 16).unwrap())
        .collect()
}
