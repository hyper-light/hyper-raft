//! hyper-quic at a geographic distance: 500 ms one way, the owner's test condition (hyper-sim's
//! `Path::GEOGRAPHIC` mean), on hyper-sim's network in virtual time.
//!
//! A client and a server endpoint, real TLS 1.3 with the hybrid post-quantum key exchange the
//! client offers first (X25519MLKEM768, a 1,184-byte key share that puts the ClientHello in two
//! Initial datagrams), exchange over one path each way. Every datagram either side sends is
//! recorded with its time and its first packet's type, so a test reads the handshake's schedule
//! off the record and checks it exactly against the RFCs' arithmetic.
//!
//! A run is its seed's: the world orders every arrival and timer, and the endpoints' keys and
//! nonces change no size and no time (Ed25519 signatures are always 64 bytes, RFC 8032 §5.1.6;
//! ML-KEM-768's encapsulation key and ciphertext are fixed sizes, FIPS 203 Table 3), so each check
//! is exact for the seeds it names.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::disallowed_macros,
    clippy::cast_possible_truncation,
    clippy::cognitive_complexity,
    clippy::too_many_lines,
    missing_docs
)]

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use bytes::BytesMut;
use hyper_quic::rustls::RootCertStore;
use hyper_quic::rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use hyper_quic::{
    ClientConfig, ClientConfigHandle, Connection, ConnectionHandle, DatagramEvent, Dir, Endpoint,
    EndpointConfig, Event, ServerConfig, StreamEvent, StreamId, TimeSource,
};
use hyper_sim::net::{Loss, Net, NetLimits, NetStats, Path, Ticket};
use hyper_sim::{Clock, Discipline, Fifo, Limits, NodeId, Record, Source, Step, World, twice};

const MS: u64 = 1_000_000;
/// The owner's condition: 500 ms one way (hyper-sim's `Path::GEOGRAPHIC` mean).
const ONE_WAY: u64 = 500 * MS;
const RTT: u64 = 2 * ONE_WAY;
/// RFC 9002 §6.2.2's kInitialRtt.
const K_INITIAL_RTT: u64 = 333 * MS;
/// The first PTO with no RTT sample: smoothed_rtt = kInitialRtt and rttvar = kInitialRtt / 2
/// (RFC 9002 §5.3), so PTO = kInitialRtt + max(4 × rttvar, kGranularity) (§6.2.1), with no
/// max_ack_delay in the Initial space.
const FIRST_PTO: u64 = K_INITIAL_RTT + 4 * (K_INITIAL_RTT / 2);
/// RFC 9000 §8.1's anti-amplification factor.
const AMPLIFICATION: u64 = 3;
/// 5% loss each way, independent: the second condition's loss.
const LOSS_PPM: u32 = 50_000;
/// The second condition's jitter, which lets datagrams overtake one another: hyper-sim's
/// `Path::GEOGRAPHIC` spread, ±100 ms about the 500 ms.
const JITTER: u64 = 100 * MS;
/// The seeds the lossy condition is checked over, each exactly.
const SEEDS: std::ops::Range<u64> = 1..33;
/// What a request and its reply carry.
const REQUEST: &[u8] = &[0x51; 100];
const REPLY: &[u8] = &[0x52; 100];
/// RFC 9000 §18.2's default max_ack_delay, which hyper-quic's endpoints advertise.
const MAX_ACK_DELAY: u64 = 25 * MS;
/// The longest a run goes on in virtual time: the default idle timeout (RFC 9308 §3.2's 30 s,
/// hyper-quic's `max_idle_timeout`) twice, past which a connection that has not finished is
/// stuck, not slow.
const HORIZON: u64 = 60_000 * MS;

/// The certificate names that make the server's first flight larger than three times the
/// client's two-datagram first flight, so the server runs into its anti-amplification limit
/// (each name adds its DNS SAN entry to the certificate).
const MANY_NAMES: usize = 1000;

/// A fixed wall clock for the tokens' lifetimes: the sim reads no host clock.
struct Fixed;

impl TimeSource for Fixed {
    fn now(&self) -> std::time::SystemTime {
        std::time::UNIX_EPOCH + Duration::from_secs(1_800_000_000)
    }
}

struct Pki {
    certificate: CertificateDer<'static>,
    key: PrivateKeyDer<'static>,
}

impl Pki {
    fn new(names: usize) -> Self {
        let mut subject = vec!["localhost".to_string()];
        // Labels from SplitMix64 (Steele, Lea and Flood, OOPSLA 2014), so the certificate does not
        // compress away (rustls compresses certificates, RFC 8879): hex carries 4 bits a byte.
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        subject.extend((0..names).map(|_| {
            state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            format!("{:016x}.geo.example", z ^ (z >> 31))
        }));
        let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ED25519).unwrap();
        let cert = rcgen::CertificateParams::new(subject)
            .unwrap()
            .self_signed(&key)
            .unwrap();
        Self {
            certificate: cert.der().clone(),
            key: PrivatePkcs8KeyDer::from(key.serialize_der()).into(),
        }
    }

    fn client_config(&self) -> ClientConfig {
        let mut roots = RootCertStore::empty();
        roots.add(self.certificate.clone()).unwrap();
        ClientConfig::with_root_certificates(roots).unwrap()
    }

    fn server_config(&self) -> ServerConfig {
        let mut config =
            ServerConfig::with_single_cert(vec![self.certificate.clone()], self.key.clone_key())
                .unwrap();
        config.time_source(Box::new(Fixed));
        config
    }
}

/// The first packet's type of a datagram (RFC 9000 §17.2: the long header's type bits).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Initial,
    ZeroRtt,
    Handshake,
    Retry,
    Short,
}

impl Kind {
    fn of(first: u8) -> Self {
        if first & 0x80 == 0 {
            return Self::Short;
        }
        match (first >> 4) & 0x03 {
            0 => Self::Initial,
            1 => Self::ZeroRtt,
            2 => Self::Handshake,
            _ => Self::Retry,
        }
    }
}

/// The types of the packets a datagram coalesces (RFC 9000 §12.2), read off their long
/// headers' Length fields (§17.2); a short-header packet runs to the datagram's end.
fn kinds(datagram: &[u8]) -> Vec<Kind> {
    fn varint(bytes: &[u8]) -> (usize, usize) {
        let first = bytes[0];
        let len = 1usize << (first >> 6);
        let mut value = u64::from(first & 0x3f);
        for byte in &bytes[1..len] {
            value = (value << 8) | u64::from(*byte);
        }
        (value as usize, len)
    }
    let mut out = Vec::new();
    let mut at = 0;
    while at < datagram.len() {
        let kind = Kind::of(datagram[at]);
        out.push(kind);
        if matches!(kind, Kind::Short | Kind::Retry) {
            break;
        }
        at += 1 + 4;
        at += 1 + usize::from(datagram[at]);
        at += 1 + usize::from(datagram[at]);
        if kind == Kind::Initial {
            let (token, n) = varint(&datagram[at..]);
            at += n + token;
        }
        let (length, n) = varint(&datagram[at..]);
        at += n + length;
    }
    out
}

/// One datagram as the network saw it.
#[derive(Clone, Debug)]
struct Sent {
    at: u64,
    from_client: bool,
    size: usize,
    kind: Kind,
    kinds: Vec<Kind>,
    /// When it arrives, or `None` if the network dropped it.
    arrives: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ev {
    Arrive(Ticket),
    /// The client dials (again).
    Dial,
}

const CLIENT: NodeId = NodeId(0);
const SERVER: NodeId = NodeId(1);

struct Side {
    address: SocketAddr,
    endpoint: Endpoint,
    connection: Option<(ConnectionHandle, Connection)>,
}

/// What one connection of the run saw, from the client's side unless named.
#[derive(Clone, Debug, Default)]
struct Dialed {
    started: u64,
    /// The client reported `Connected`.
    connected: Option<u64>,
    /// The reply to the first request was read to its end.
    replied: Option<u64>,
    lost: Option<(u64, String)>,
    server_lost: Option<(u64, String)>,
    accepted_0rtt: bool,
    /// Attempts the server's endpoint surfaced for this dial.
    surfaced: u32,
    /// Each later request's latency on the kept connection, sent as the last reply arrived.
    kept: Vec<u64>,
}

#[derive(Clone, Copy, Debug)]
struct Scenario {
    path: Path,
    seed: u64,
    /// How many times the client dials, one after another, each once the last has its reply.
    dials: u32,
    /// Whether the client sends its request before the handshake ends (0-RTT where it can).
    early: bool,
    /// Requests sent one after another on the connection after the first is answered.
    kept_requests: u32,
    /// The client's datagrams, by their order of sending from zero, the network loses.
    drop_client: &'static [usize],
    /// The server's datagrams, likewise.
    drop_server: &'static [usize],
    /// Whether the server never hears the client.
    silent: bool,
    /// The server's bound on attempts pending at once (`ServerConfig::max_incoming`).
    max_incoming: Option<usize>,
    /// The bytes of each reply.
    reply_bytes: usize,
    /// Whether the endpoints run Careful Resume (RFC 9959), their default.
    careful_resume: bool,
    /// Whether the first dial runs on a clean path and the later ones on `path`.
    clean_first: bool,
    /// The virtual time past which the run stops.
    horizon: u64,
}

impl Scenario {
    fn clean() -> Self {
        Self {
            path: Path::in_order(ONE_WAY, 0),
            seed: 1,
            dials: 1,
            early: false,
            kept_requests: 0,
            drop_client: &[],
            drop_server: &[],
            silent: false,
            max_incoming: None,
            reply_bytes: REPLY.len(),
            careful_resume: true,
            clean_first: false,
            horizon: HORIZON,
        }
    }

    fn lossy(seed: u64) -> Self {
        Self {
            path: Path::reordering(ONE_WAY, JITTER).with_loss(Loss::random(LOSS_PPM)),
            seed,
            ..Self::clean()
        }
    }
}

/// The server's side of RFC 9000 §8.1 for the current connection: the bytes received from the
/// client's address, the bytes sent to it, and whether the address is validated, which receiving a
/// Handshake packet from the client does (§8.1).
#[derive(Clone, Copy, Debug, Default)]
struct Amplification {
    received: u64,
    sent: u64,
    validated: bool,
}

/// Every datagram sent, and the server's amplification account.
#[derive(Default)]
struct Wire {
    drop_client: &'static [usize],
    drop_server: &'static [usize],
    client_sent: usize,
    server_sent: usize,
    sent: Vec<Sent>,
    server: Amplification,
    violations: Vec<(u64, usize, u64, u64)>,
}

impl Wire {
    fn emit(
        &mut self,
        world: &mut World<Ev>,
        net: &mut Net<Vec<u8>>,
        node: NodeId,
        bytes: Vec<u8>,
    ) {
        let now = world.now();
        let from_client = node == CLIENT;
        let peer = if from_client { SERVER } else { CLIENT };
        let kinds = kinds(&bytes);
        let size = bytes.len();
        if !from_client && !self.server.validated {
            let limit = AMPLIFICATION * self.server.received;
            if self.server.sent + size as u64 > limit {
                self.violations
                    .push((now, size, self.server.sent, self.server.received));
            }
        }
        if !from_client {
            self.server.sent += size as u64;
        }
        let dropped = if from_client {
            self.client_sent += 1;
            self.drop_client.contains(&(self.client_sent - 1))
        } else {
            self.server_sent += 1;
            self.drop_server.contains(&(self.server_sent - 1))
        };
        let fate = if dropped {
            hyper_sim::net::Fate::Dropped(hyper_sim::net::Dropped::Loss)
        } else {
            net.send(world, (node, peer), bytes, size, Ev::Arrive)
                .unwrap()
        };
        self.sent.push(Sent {
            at: now,
            from_client,
            size,
            kind: kinds[0],
            kinds,
            arrives: match fate {
                hyper_sim::net::Fate::Arrives { at } => Some(at),
                hyper_sim::net::Fate::Dropped(_) => None,
            },
        });
    }

    /// A datagram from the client reaches the server.
    fn received(&mut self, datagram: &[u8]) {
        self.server.received += datagram.len() as u64;
        if kinds(datagram).contains(&Kind::Handshake) {
            self.server.validated = true;
        }
    }
}

struct Run {
    scenario: Scenario,
    epoch: Instant,
    world: World<Ev>,
    net: Net<Vec<u8>>,
    client: Side,
    server: Side,
    client_config: ClientConfigHandle,
    wire: Wire,
    dialed: Vec<Dialed>,
    request: Option<StreamId>,
    reply: Vec<u8>,
    asked_at: u64,
    inbound: Option<StreamId>,
    asked: Vec<u8>,
    /// The bytes of the current reply the server has written
    answered: usize,
    scratch: Vec<u8>,
}

impl Run {
    fn new(scenario: Scenario, pki: &Pki) -> Self {
        Self::from_source(scenario, pki, Source::Seed(scenario.seed))
    }

    fn from_source(scenario: Scenario, pki: &Pki, source: Source) -> Self {
        let limits = Limits {
            events: 4_096,
            nodes: 2,
            streams: 8,
            steps: 10_000_000,
            // A lossy path draws a loss and a delay for each datagram: room for half a million.
            trace_words: 1 << 20,
        };
        let mut world = World::new(source, Discipline::Ordered, limits).unwrap();
        world.node(Clock::default()).unwrap();
        world.node(Clock::default()).unwrap();
        let epoch = world.instant(CLIENT).unwrap();
        let mut net = Net::new(NetLimits {
            flows: 2,
            links: 0,
            nats: 0,
            link_messages: 0,
            messages: 4_096,
            bytes: 4_096 * 1_500,
        });
        let first_path = match scenario.clean_first {
            true => Path::in_order(ONE_WAY, 0),
            false => scenario.path,
        };
        net.set_pair_path(CLIENT, SERVER, first_path).unwrap();
        net.set_pair_path(SERVER, CLIENT, first_path).unwrap();
        let rng = |node: NodeId| {
            let mut bytes = [0u8; 32];
            bytes[..8].copy_from_slice(&scenario.seed.to_le_bytes());
            bytes[8..12].copy_from_slice(&node.0.to_le_bytes());
            Some(bytes)
        };
        let mut endpoint_config = EndpointConfig::default();
        if !scenario.careful_resume {
            endpoint_config.careful_resume(None);
        }
        let mut client = Endpoint::new(endpoint_config.clone(), None, false, rng(CLIENT)).unwrap();
        let client_config = client.insert_client_config(pki.client_config()).unwrap();
        if scenario.silent {
            net.partition(CLIENT, SERVER, true);
        }
        let mut server_config = pki.server_config();
        if let Some(max) = scenario.max_incoming {
            server_config.max_incoming(max);
        }
        let server =
            Endpoint::new(endpoint_config, Some(server_config), false, rng(SERVER)).unwrap();
        Self {
            scenario,
            epoch,
            world,
            net,
            client: Side {
                address: "10.0.0.1:4433".parse().unwrap(),
                endpoint: client,
                connection: None,
            },
            server: Side {
                address: "10.0.0.2:4433".parse().unwrap(),
                endpoint: server,
                connection: None,
            },
            client_config,
            wire: Wire {
                drop_client: scenario.drop_client,
                drop_server: scenario.drop_server,
                ..Wire::default()
            },
            dialed: Vec::new(),
            request: None,
            reply: Vec::new(),
            asked_at: 0,
            inbound: None,
            asked: Vec::new(),
            answered: 0,
            scratch: Vec::with_capacity(1_500),
        }
    }

    fn at(&self, now: u64) -> Instant {
        self.epoch + Duration::from_nanos(now)
    }

    fn dial(&mut self) {
        let now = self.world.now();
        if self.scenario.clean_first && self.dialed.len() == 1 {
            let path = self.scenario.path;
            self.net.set_pair_path(CLIENT, SERVER, path).unwrap();
            self.net.set_pair_path(SERVER, CLIENT, path).unwrap();
        }
        let at = self.at(now);
        let connection = self
            .client
            .endpoint
            .connect(
                at,
                self.client_config,
                self.server.address,
                "localhost",
                None,
            )
            .unwrap();
        self.client.connection = Some(connection);
        self.server.connection = None;
        self.request = None;
        self.reply.clear();
        self.inbound = None;
        self.asked.clear();
        self.answered = 0;
        self.wire.server = Amplification::default();
        self.dialed.push(Dialed {
            started: now,
            ..Dialed::default()
        });
        self.serve(CLIENT);
    }

    fn serve(&mut self, node: NodeId) {
        let now = self.world.now();
        let at = self.at(now);
        let is_client = node == CLIENT;
        let early = self.scenario.early;
        let side = if is_client {
            &mut self.client
        } else {
            &mut self.server
        };
        let Some((handle, connection)) = &mut side.connection else {
            return;
        };
        while let Some(event) = connection.poll_endpoint_events() {
            if let Some(event) = side.endpoint.handle_event(*handle, event) {
                connection.handle_event(event, side.endpoint.configs_mut());
            }
        }
        let dialed = self.dialed.last_mut().unwrap();
        while let Some(event) = connection.poll() {
            match (is_client, event) {
                (true, Event::Connected) => {
                    dialed.connected.get_or_insert(now);
                    dialed.accepted_0rtt = connection.accepted_0rtt();
                }
                (true, Event::ConnectionLost { reason }) => {
                    dialed.lost.get_or_insert((now, reason.to_string()));
                }
                (false, Event::ConnectionLost { reason }) => {
                    dialed.server_lost.get_or_insert((now, reason.to_string()));
                }
                (false, Event::Stream(StreamEvent::Opened { dir: Dir::Bi })) => {
                    if let Some(id) = connection.streams().accept(Dir::Bi) {
                        self.inbound = Some(id);
                        self.asked.clear();
                        self.answered = 0;
                    }
                }
                _ => {}
            }
        }
        if is_client {
            let ready = dialed.connected.is_some() || (early && connection.has_0rtt());
            if ready && self.request.is_none() && dialed.lost.is_none() {
                let id = connection.streams().open(Dir::Bi).unwrap();
                let mut stream = connection.send_stream(id);
                assert_eq!(stream.write(REQUEST).unwrap(), REQUEST.len());
                stream.finish().unwrap();
                self.request = Some(id);
            }
            if let Some(id) = self.request {
                read_to_end(connection, id, &mut self.reply);
                if self.reply.len() == self.scenario.reply_bytes {
                    match dialed.replied {
                        None => dialed.replied = Some(now),
                        Some(_) => dialed.kept.push(now - self.asked_at),
                    }
                    // The next request on the kept connection, at once
                    if (dialed.kept.len() as u32) < self.scenario.kept_requests {
                        let id = connection.streams().open(Dir::Bi).unwrap();
                        let mut stream = connection.send_stream(id);
                        assert_eq!(stream.write(REQUEST).unwrap(), REQUEST.len());
                        stream.finish().unwrap();
                        self.request = Some(id);
                        self.asked_at = now;
                    }
                    self.reply.clear();
                }
            }
        } else if let Some(id) = self.inbound {
            read_to_end(connection, id, &mut self.asked);
            let total = self.scenario.reply_bytes;
            if self.asked.len() == REQUEST.len() && self.answered < total {
                // Written as the stream's buffer takes it, a large reply over several turns
                let mut stream = connection.send_stream(id);
                while self.answered < total {
                    let chunk = &[0x52; 1_200][..(total - self.answered).min(1_200)];
                    match stream.write(chunk) {
                        Ok(n) if n > 0 => self.answered += n,
                        _ => break,
                    }
                }
                if self.answered == total {
                    stream.finish().unwrap();
                }
            }
        }
        loop {
            self.scratch.clear();
            let Some(transmit) =
                connection.poll_transmit(at, 1, &mut self.scratch, side.endpoint.configs())
            else {
                break;
            };
            let bytes = self.scratch[..transmit.size].to_vec();
            self.wire.emit(&mut self.world, &mut self.net, node, bytes);
        }
        let deadline = connection.poll_timeout().map(|due| {
            u64::try_from(due.saturating_duration_since(self.epoch).as_nanos()).unwrap()
        });
        self.world.wake(node, deadline).unwrap();
    }

    fn arrive(&mut self, node: NodeId, ticket: Ticket) {
        let at = self.at(self.world.now());
        let Some(delivery) = self
            .net
            .deliver(&mut self.world, ticket, Ev::Arrive)
            .unwrap()
        else {
            return;
        };
        let is_client = node == CLIENT;
        let from = if is_client {
            self.server.address
        } else {
            self.client.address
        };
        let side = if is_client {
            &mut self.client
        } else {
            &mut self.server
        };
        if !is_client {
            self.wire.received(&delivery.payload);
        }
        self.scratch.clear();
        let bytes = BytesMut::from(delivery.payload.as_slice());
        match side
            .endpoint
            .handle(at, from, None, None, bytes, &mut self.scratch)
        {
            Some(DatagramEvent::ConnectionEvent(handle, event)) => {
                if let Some((own, connection)) = &mut side.connection
                    && *own == handle
                {
                    connection.handle_event(event, side.endpoint.configs_mut());
                }
            }
            Some(DatagramEvent::NewConnection(incoming)) => {
                self.scratch.clear();
                let accepted = side
                    .endpoint
                    .accept(incoming, at, &mut self.scratch, None, None)
                    .unwrap_or_else(|error| panic!("the server refused: {:?}", error.cause));
                side.connection = Some(accepted);
                self.dialed.last_mut().unwrap().surfaced += 1;
            }
            Some(DatagramEvent::Response(transmit)) => {
                let bytes = self.scratch[..transmit.size].to_vec();
                self.wire.emit(&mut self.world, &mut self.net, node, bytes);
            }
            None => {}
        }
        self.serve(node);
    }

    fn fire(&mut self, node: NodeId) {
        let at = self.at(self.world.now());
        let side = if node == CLIENT {
            &mut self.client
        } else {
            &mut self.server
        };
        if let Some((_, connection)) = &mut side.connection {
            connection.handle_timeout(at);
        }
        self.serve(node);
    }

    /// Runs every dial to its reply, or to the horizon.
    fn run(mut self) -> Outcome {
        self.world.schedule(0, CLIENT, Ev::Dial).unwrap();
        // Whether the next dial is scheduled, so the last one's end schedules it once
        let mut redialing = true;
        loop {
            let step = self.world.next(&mut Fifo).unwrap();
            if self.world.now() > self.scenario.horizon {
                break;
            }
            match step {
                Step::Event {
                    node,
                    event: Ev::Arrive(ticket),
                } => self.arrive(node, ticket),
                Step::Event {
                    event: Ev::Dial, ..
                } => {
                    redialing = false;
                    self.dial();
                }
                Step::Wake { node } => self.fire(node),
                Step::Idle | Step::Spent => break,
            }
            let done = self.dialed.last().is_some_and(|d| {
                (d.replied.is_some() && d.kept.len() as u32 >= self.scenario.kept_requests)
                    || d.lost.is_some()
            });
            if done && !redialing {
                if self.dialed.len() as u32 >= self.scenario.dials {
                    break;
                }
                redialing = true;
                // The next dial once the last has its reply: the kept state (session ticket,
                // address validation token) is what a re-dial uses.
                let now = self.world.now();
                if let Some((_, connection)) = &mut self.client.connection {
                    let at = self.epoch + Duration::from_nanos(now);
                    connection.close(at, 0u32.into(), bytes::Bytes::new());
                }
                self.serve(CLIENT);
                self.world.schedule(now + RTT, CLIENT, Ev::Dial).unwrap();
            }
        }
        // The digest covers every datagram's time, sender and size (`docs/sim.md` §3.9)
        for sent in &self.wire.sent {
            self.world.observe(sent.at);
            self.world.observe(u64::from(sent.from_client));
            self.world.observe(sent.size as u64);
        }
        let stats = self.net.stats();
        Outcome {
            record: self.world.finish(),
            stats,
            sent: self.wire.sent,
            violations: self.wire.violations,
            dialed: self.dialed,
        }
    }
}

fn read_to_end(connection: &mut Connection, id: StreamId, into: &mut Vec<u8>) {
    let mut stream = connection.recv_stream(id);
    let Ok(mut chunks) = stream.read(true) else {
        return;
    };
    while let Ok(Some(chunk)) = chunks.next(usize::MAX) {
        into.extend_from_slice(&chunk.bytes);
    }
    let _ = chunks.finalize();
}

struct Outcome {
    record: Record,
    stats: NetStats,
    sent: Vec<Sent>,
    /// Each server datagram sent past RFC 9000 §8.1's limit before the client's address was
    /// validated: when, its size, and the bytes sent and received before it.
    violations: Vec<(u64, usize, u64, u64)>,
    dialed: Vec<Dialed>,
}

impl Outcome {
    fn first(&self) -> &Dialed {
        &self.dialed[0]
    }

    /// The client's Initial datagrams of the first dial, by send time, up to `before`.
    fn client_initials_before(&self, before: u64) -> Vec<&Sent> {
        self.sent
            .iter()
            .filter(|s| s.from_client && s.kind == Kind::Initial && s.at < before)
            .collect()
    }

    /// The client's flights of Initial datagrams sent before `before`: each send instant and how
    /// many datagrams went then.
    fn client_flights_before(&self, before: u64) -> Vec<(u64, usize)> {
        let mut flights: Vec<(u64, usize)> = Vec::new();
        for sent in self.client_initials_before(before) {
            match flights.last_mut() {
                Some((at, count)) if *at == sent.at => *count += 1,
                _ => flights.push((sent.at, 1)),
            }
        }
        flights
    }

    /// The server's bytes in the datagrams it sent before `before`.
    fn server_bytes_before(&self, before: u64) -> u64 {
        self.sent
            .iter()
            .filter(|s| !s.from_client && s.at < before)
            .map(|s| s.size as u64)
            .sum()
    }

    /// When the first datagram from the server reached the client.
    fn first_reply(&self) -> Option<u64> {
        self.sent
            .iter()
            .filter(|s| !s.from_client)
            .filter_map(|s| s.arrives)
            .min()
    }

    fn timeline(&self) -> String {
        let mut out = String::new();
        for s in &self.sent {
            out.push_str(&format!(
                "{:>9.3} ms {} {:?} {} B -> {}\n",
                s.at as f64 / MS as f64,
                if s.from_client { "C" } else { "S" },
                s.kinds,
                s.size,
                s.arrives.map_or("lost".to_string(), |a| format!(
                    "{:.3}",
                    a as f64 / MS as f64
                ))
            ));
        }
        out
    }
}

#[test]
#[ignore = "prints the schedule; run by hand"]
fn print_the_clean_schedule() {
    for names in [0, MANY_NAMES] {
        let pki = Pki::new(names);
        let out = Run::new(Scenario::clean(), &pki).run();
        println!("names {names}:\n{}{:?}\n", out.timeline(), out.dialed);
    }
}

#[test]
#[ignore = "prints the lossy summary; run by hand"]
fn print_the_lossy_summary() {
    let pki = Pki::new(MANY_NAMES);
    for seed in SEEDS {
        let out = Run::new(Scenario::lossy(seed), &pki).run();
        let reply = out.first_reply();
        let initials: Vec<u64> = out
            .client_initials_before(reply.unwrap_or(u64::MAX))
            .iter()
            .map(|s| s.at / MS)
            .collect();
        let d = out.first();
        println!(
            "seed {seed}: first reply {:?} ms, client initials {initials:?}, connected {:?}, replied {:?}, lost {:?}/{:?}, surfaced {}, violations {:?}, dropped {}",
            reply.map(|r| r / MS),
            d.connected.map(|t| (t - d.started) / MS),
            d.replied.map(|t| (t - d.started) / MS),
            d.lost,
            d.server_lost,
            d.surfaced,
            out.violations,
            out.stats.dropped_loss
        );
    }
}

/// RFC 9002 §6.2.1 and §6.2.2: with no RTT sample the first PTO is kInitialRtt + 4 × kInitialRtt/2,
/// 999 ms, and each expiry doubles the next (§6.2.1, "exponential backoff"). The first flight goes at
/// zero; the server's reply reaches the client at one round trip, 1,000 ms, so exactly one
/// retransmission, at 999 ms, precedes it, within the guarantee's two.
fn the_client_retransmits_its_first_flight_on_the_pto_from_333_ms(names: usize) {
    let pki = Pki::new(names);
    let out = Run::new(Scenario::clean(), &pki).run();
    let reply = out.first_reply().unwrap();
    assert_eq!(reply, RTT, "{}", out.timeline());
    assert_eq!(
        out.client_flights_before(reply),
        vec![(0, 2), (FIRST_PTO, 2)],
        "{}",
        out.timeline()
    );
}

#[test]
fn a_client_retransmits_its_first_flight_on_the_rfc_9002_pto_from_333_ms() {
    the_client_retransmits_its_first_flight_on_the_pto_from_333_ms(0);
    the_client_retransmits_its_first_flight_on_the_pto_from_333_ms(MANY_NAMES);
}

/// With no reply at all, the flight goes at 0, then after 999 ms, then after twice that, and so on:
/// each flight of two datagrams is one PTO expiry, so the backoff doubles once a flight (guarantee b
/// for the client's own timer); were each datagram counted, it would quadruple.
#[test]
fn an_unanswered_client_backs_off_by_doubling_one_flight_at_a_time() {
    let pki = Pki::new(0);
    let out = Run::new(
        Scenario {
            silent: true,
            ..Scenario::clean()
        },
        &pki,
    )
    .run();
    let flights = out.client_flights_before(u64::MAX);
    let mut expected = Vec::new();
    let (mut at, mut pto) = (0, FIRST_PTO);
    while at < 30_000 * MS {
        expected.push((at, 2));
        at += pto;
        pto *= 2;
    }
    assert_eq!(flights, expected);
    assert_eq!(out.first().lost.as_ref().unwrap().1, "timed out");
}

/// The lossy condition, each seed exactly: every retransmission of the first flight is a whole
/// flight of two datagrams on the doubling schedule, at most two precede the first reply, and the
/// client is never refused for its own retransmissions.
#[test]
fn under_loss_and_reordering_the_first_flight_follows_the_pto_schedule() {
    let pki = Pki::new(MANY_NAMES);
    for seed in SEEDS {
        let out = Run::new(Scenario::lossy(seed), &pki).run();
        let reply = out.first_reply().unwrap();
        let flights = out.client_flights_before(reply);
        let mut pto = FIRST_PTO;
        for pair in flights.windows(2) {
            assert_eq!(pair[1].0 - pair[0].0, pto, "seed {seed}: {flights:?}");
            pto *= 2;
        }
        assert!(
            flights.iter().all(|&(_, count)| count == 2),
            "seed {seed}: {flights:?}"
        );
        assert!(flights.len() <= 3, "seed {seed}: {flights:?}");
        assert_eq!(out.first().surfaced, 1, "seed {seed}");
    }
}

/// Guarantee b at the server: the first datagram of the ClientHello is lost, so the second is held
/// (it does not begin the ClientHello); the retransmitted flight then begins the attempt. With room
/// for one pending attempt, the held datagram and the retransmission are the one attempt and count
/// once against `max_incoming`.
#[test]
fn a_retransmitted_two_datagram_flight_counts_once_against_the_attempt_bound() {
    let pki = Pki::new(MANY_NAMES);
    let out = Run::new(
        Scenario {
            drop_client: &[0],
            max_incoming: Some(1),
            ..Scenario::clean()
        },
        &pki,
    )
    .run();
    let first = out.first();
    assert_eq!(first.surfaced, 1, "{}", out.timeline());
    assert!(first.lost.is_none(), "{first:?}\n{}", out.timeline());
    // The retransmission at 999 ms reaches the server at 1,499 ms and its answer the client a round
    // trip after that; the handshake then ends as it does with no loss, one round trip later than
    // the clean run (`a_post_quantum_hello_and_a_large_server_flight_complete_in_two_round_trips`).
    assert_eq!(
        first.connected,
        Some(FIRST_PTO + 2 * RTT),
        "{}",
        out.timeline()
    );
}

/// Guarantee c: the server acknowledges every duplicate Initial datagram at once (RFC 9000
/// §13.2.1: Initial packets are acknowledged immediately), surfaces no second attempt and feeds no
/// ClientHello twice (a second one would fail the handshake), and its allowance grows with the
/// duplicates' bytes (RFC 9000 §8.1 counts every byte received): it sends more than three times
/// the first flight before the client's address is validated, and never more than three times
/// what it has received, in the clean run and every lossy seed.
#[test]
fn duplicate_initials_are_acknowledged_once_and_grow_the_allowance() {
    let pki = Pki::new(MANY_NAMES);
    let out = Run::new(Scenario::clean(), &pki).run();
    assert!(
        out.violations.is_empty(),
        "{:?}\n{}",
        out.violations,
        out.timeline()
    );
    // The duplicate flight (sent at 999 ms) arrives at 1,499 ms; each of its datagrams is answered
    // by an Initial datagram at that instant.
    let duplicates_at = FIRST_PTO + ONE_WAY;
    let answers = out
        .sent
        .iter()
        .filter(|s| !s.from_client && s.at == duplicates_at && s.kinds[0] == Kind::Initial)
        .count();
    assert!(answers >= 2, "{}", out.timeline());
    // Before the client's Handshake packets (sent at 1,000 ms) arrive at 1,500 ms, the server has
    // sent more than three times the first flight: the duplicates' bytes grew its allowance.
    let first_flight = 2 * 1_200;
    assert!(
        out.server_bytes_before(RTT + ONE_WAY) > AMPLIFICATION * first_flight,
        "{}",
        out.timeline()
    );
    assert_eq!(out.first().surfaced, 1);
    assert!(out.first().server_lost.is_none());
    for seed in SEEDS {
        let out = Run::new(Scenario::lossy(seed), &pki).run();
        assert!(
            out.violations.is_empty(),
            "seed {seed}: {:?}",
            out.violations
        );
        assert_eq!(out.first().surfaced, 1, "seed {seed}");
    }
}

/// Guarantee d: the X25519MLKEM768 ClientHello in two datagrams, and a server flight larger than
/// three times them, so the server must wait for the client's next bytes (RFC 9000 §8.1): the
/// handshake completes at two round trips, the floor that limit sets.
#[test]
fn a_post_quantum_hello_and_a_large_server_flight_complete_in_two_round_trips() {
    let pki = Pki::new(MANY_NAMES);
    let out = Run::new(Scenario::clean(), &pki).run();
    let first_flight: u64 = out
        .client_initials_before(1)
        .iter()
        .map(|s| s.size as u64)
        .sum();
    assert_eq!(first_flight, 2 * 1_200);
    let server_flight: u64 = out
        .sent
        .iter()
        .filter(|s| !s.from_client && s.kinds.contains(&Kind::Handshake))
        .map(|s| s.size as u64)
        .sum();
    assert!(
        server_flight > AMPLIFICATION * first_flight,
        "{server_flight}"
    );
    let connected = out.first().connected.unwrap();
    assert!(connected <= 2 * RTT, "{connected}\n{}", out.timeline());
}

/// Guarantee d under 5% loss and reordering, each seed exactly: every handshake completes and its
/// first request is answered; none runs out of a budget (the idle timeout, the held Initials'
/// expiry, the attempt bound).
#[test]
fn under_loss_and_reordering_every_post_quantum_handshake_completes() {
    let pki = Pki::new(MANY_NAMES);
    for seed in SEEDS {
        let out = Run::new(Scenario::lossy(seed), &pki).run();
        let first = out.first();
        assert!(
            first.lost.is_none() && first.server_lost.is_none(),
            "seed {seed}: {first:?}\n{}",
            out.timeline()
        );
        assert!(
            first.connected.is_some() && first.replied.is_some(),
            "seed {seed}: {first:?}"
        );
    }
}

/// Guarantee d with reordering and no loss, each seed whose network lost nothing: the handshake
/// completes within two round trips of the path's slowest one-way delay (500 ms + 100 ms of jitter
/// each way). A Handshake packet overtaking the Initial that carries the ServerHello is held until
/// its keys arrive (RFC 9001 §4.1.4), not dropped and waited for again.
#[test]
fn reordering_alone_costs_no_round_trip() {
    let pki = Pki::new(MANY_NAMES);
    let slowest_round_trip = 2 * (ONE_WAY + JITTER);
    let mut lossless = 0;
    for seed in SEEDS {
        let out = Run::new(Scenario::lossy(seed), &pki).run();
        if out.stats.dropped_loss != 0 {
            continue;
        }
        lossless += 1;
        let connected = out.first().connected.unwrap_or(u64::MAX);
        assert!(
            connected <= 2 * slowest_round_trip,
            "seed {seed}: {connected}\n{}",
            out.timeline()
        );
    }
    assert!(lossless > 0);
}

/// The run-twice check (`docs/sim.md` §3.9): the first lossy seed gives one digest from its seed
/// twice and from its trace.
#[test]
fn a_lossy_run_replays_from_its_seed_and_from_its_trace() {
    let pki = Pki::new(MANY_NAMES);
    let scenario = Scenario::lossy(SEEDS.start);
    twice(scenario.seed, |source| {
        Ok::<_, std::convert::Infallible>(Run::from_source(scenario, &pki, source).run().record)
    })
    .unwrap();
}

/// The overhead table of `docs/benchmarks.md` ("At 500 ms one way"): for each case, each dial's
/// handshake and first reply from its start, and the kept connection's later requests.
#[test]
#[ignore = "prints the overhead table; run by hand"]
fn print_the_overhead_table() {
    for (label, names, early) in [
        ("small certificate", 0, false),
        ("small certificate, early request", 0, true),
        ("large certificate", MANY_NAMES, false),
        ("large certificate, early request", MANY_NAMES, true),
    ] {
        let pki = Pki::new(names);
        let out = Run::new(
            Scenario {
                dials: 2,
                early,
                kept_requests: 3,
                ..Scenario::clean()
            },
            &pki,
        )
        .run();
        for (i, d) in out.dialed.iter().enumerate() {
            println!(
                "{label}, dial {}: connected {:?} ms, first reply {:?} ms, 0-RTT {}, kept {:?} ms",
                i + 1,
                d.connected.map(|t| (t - d.started) as f64 / MS as f64),
                d.replied.map(|t| (t - d.started) as f64 / MS as f64),
                d.accepted_0rtt,
                d.kept
                    .iter()
                    .map(|&t| t as f64 / MS as f64)
                    .collect::<Vec<_>>()
            );
        }
    }
}

/// The lossy overhead table of `docs/benchmarks.md`: over the 32 seeds, each dial's handshake and
/// first reply above its floor of round trips (handshake 2 and reply 3 for a fresh dial, whose
/// large certificate meets the amplification limit; 1 and 1 for the resumed dial with its request
/// in 0-RTT data), as median, 90th percentile and maximum, and the dials that never completed.
#[test]
#[ignore = "prints the lossy overhead table; run by hand"]
fn print_the_lossy_overhead_table() {
    let pki = Pki::new(MANY_NAMES);
    let mut rows: [[Vec<u64>; 2]; 2] = Default::default();
    let mut stuck = [0u32; 2];
    // What the network carried over every seed: the cost of the probes beside their latency
    let (mut datagrams, mut bytes) = (0usize, 0usize);
    for seed in SEEDS {
        let out = Run::new(
            Scenario {
                dials: 2,
                early: true,
                ..Scenario::lossy(seed)
            },
            &pki,
        )
        .run();
        datagrams += out.sent.len();
        bytes += out.sent.iter().map(|s| s.size).sum::<usize>();
        for (i, floors) in [(0usize, (2, 3)), (1, (1, 1))] {
            match out.dialed.get(i) {
                Some(Dialed {
                    started,
                    connected: Some(c),
                    replied: Some(r),
                    ..
                }) => {
                    rows[i][0].push((c - started).saturating_sub(floors.0 * RTT));
                    rows[i][1].push((r - started).saturating_sub(floors.1 * RTT));
                }
                _ => stuck[i] += 1,
            }
        }
    }
    for (i, label) in ["fresh dial", "resumed dial, 0-RTT request"]
        .iter()
        .enumerate()
    {
        for (j, what) in ["handshake", "first reply"].iter().enumerate() {
            let v = &mut rows[i][j];
            v.sort_unstable();
            let at = |q: usize| {
                v.get((v.len() * q).div_ceil(100).max(1) - 1)
                    .map(|&x| x / MS)
            };
            println!(
                "{label}, {what} over floor: median {:?} ms, p90 {:?} ms, max {:?} ms, completed {}, stuck {}",
                at(50),
                at(90),
                v.last().map(|&x| x / MS),
                v.len(),
                stuck[i]
            );
        }
    }
    println!(
        "sent over {} seeds: {datagrams} datagrams, {bytes} bytes",
        SEEDS.end - SEEDS.start
    );
}

/// The datagrams a scenario drops, chosen from a clean run's record: the harness's scenarios hold
/// `'static` slices, and a test lives as long as the process.
fn chosen(indices: Vec<usize>) -> &'static [usize] {
    Box::leak(indices.into_boxed_slice())
}

/// The position among one side's datagrams of the first that `pick` selects.
fn position(out: &Outcome, from_client: bool, pick: impl Fn(&Sent) -> bool) -> (usize, u64) {
    out.sent
        .iter()
        .filter(|s| s.from_client == from_client)
        .enumerate()
        .find(|(_, s)| pick(s))
        .map(|(i, s)| (i, s.at))
        .unwrap()
}

/// A reply lost on the wire goes again in the server's first probe, which waits twice the
/// smoothed RTT and the peer's ACK delay at most (`PROBE_VARIANCE_MULTIPLIER`, RFC 8985 §7.2).
/// Upstream's probe carried no STREAM frames, so the reply went only once the probe's
/// acknowledgement declared it lost, a round trip after a probe RFC 9002's variation weight of 4
/// put at three smoothed RTTs: about 4 s where 2 s suffice.
#[test]
fn a_lost_reply_goes_again_in_the_first_probe_within_two_round_trips() {
    let pki = Pki::new(0);
    let clean = Run::new(Scenario::clean(), &pki).run();
    let replied = clean.first().replied.unwrap();
    let (reply, lost_at) = position(&clean, false, |s| s.arrives == Some(replied));
    let out = Run::new(
        Scenario {
            drop_server: chosen(vec![reply]),
            ..Scenario::clean()
        },
        &pki,
    )
    .run();
    let replied = out.first().replied.unwrap();
    let resent = out
        .sent
        .iter()
        .find(|s| !s.from_client && s.arrives == Some(replied))
        .unwrap();
    assert!(
        resent.at - lost_at <= 2 * RTT + MAX_ACK_DELAY,
        "{}",
        out.timeline()
    );
}

/// The client's Finished and its first request share a datagram; when it is lost, the client's
/// Handshake probe carries both (RFC 9002 §6.2.4: probe "other packet number spaces with
/// in-flight data"), two smoothed RTTs after it, and the reply comes one round trip later.
/// Upstream probed the Handshake space alone, and the request waited for its own loss to be
/// found once the handshake ended.
#[test]
fn a_lost_finished_and_request_go_again_in_one_probe() {
    let pki = Pki::new(0);
    let clean = Run::new(Scenario::clean(), &pki).run();
    let (request, lost_at) = position(&clean, true, |s| {
        s.kinds.contains(&Kind::Handshake) && s.kinds.contains(&Kind::Short)
    });
    let out = Run::new(
        Scenario {
            drop_client: chosen(vec![request]),
            ..Scenario::clean()
        },
        &pki,
    )
    .run();
    let asked = out.first().replied.unwrap() - RTT;
    assert!(
        out.sent.iter().any(|s| s.from_client
            && s.at == asked
            && s.kinds.contains(&Kind::Handshake)
            && s.kinds.contains(&Kind::Short)),
        "{}",
        out.timeline()
    );
    assert!(asked - lost_at <= 2 * RTT, "{}", out.timeline());
}

/// Every resumed dial that closes at its first reply, one round trip in, still leaves the client a
/// ticket for the next: the server sends its tickets with its Finished on a handshake that does not
/// authenticate the client (RFC 8446 §4.6.1), so they arrive with the reply. Upstream sent them
/// once the client's Finished arrived, half a round trip after the reply, so dials closed at their
/// reply received none, and with the two tickets of the first dial spent the fourth dial fell back
/// to 1-RTT (`docs/benchmarks.md`, "Why the fourth dial lost 0-RTT").
#[test]
fn every_resumed_dial_closed_at_its_reply_leaves_a_ticket_for_the_next() {
    let pki = Pki::new(0);
    let out = Run::new(
        Scenario {
            dials: 6,
            early: true,
            ..Scenario::clean()
        },
        &pki,
    )
    .run();
    assert_eq!(out.dialed.len(), 6);
    for (i, d) in out.dialed.iter().enumerate().skip(1) {
        assert!(d.accepted_0rtt, "dial {}: {d:?}", i + 1);
        assert_eq!(d.replied.unwrap() - d.started, RTT, "dial {}", i + 1);
    }
}

/// A reply large enough that the server's slow start meets the round trip many times: a mebibyte
/// takes the first connection's window from 12,000 bytes past 400 kB, and its server measures more
/// than four initial windows a round trip (RFC 9959 §3.1).
const BULK: usize = 1 << 20;

/// Careful Resume (RFC 9959): the server measured the first connection's delivery a round trip,
/// and the resumed connection, once its initial data is acknowledged, jumps to half of it where
/// slow start would still be doubling from the initial window. The fresh dial is the same with it
/// or without it, and the resumed dial's reply comes at least a round trip sooner with it.
#[test]
fn a_resumed_connection_starts_from_half_what_the_last_delivered() {
    let pki = Pki::new(0);
    let run = |careful_resume| {
        Run::new(
            Scenario {
                dials: 2,
                early: true,
                reply_bytes: BULK,
                careful_resume,
                ..Scenario::clean()
            },
            &pki,
        )
        .run()
    };
    let (with, without) = (run(true), run(false));
    let reply = |out: &Outcome, i: usize| {
        let d = &out.dialed[i];
        d.replied.unwrap() - d.started
    };
    assert_eq!(reply(&with, 0), reply(&without, 0));
    assert!(with.dialed[1].accepted_0rtt);
    assert!(
        reply(&with, 1) + RTT <= reply(&without, 1),
        "with {} ms, without {} ms",
        reply(&with, 1) / MS,
        reply(&without, 1) / MS
    );
}

/// Careful Resume meeting the lossy condition, each seed exactly: the first dial measures a clean
/// path, the resumed dial jumps on the lossy one and retreats at its first loss (RFC 9959 §3.5),
/// and every transfer of both dials completes, neither connection lost but by the client's close. A mebibyte at 5%
/// loss and a 1 s round trip takes minutes (loss-based congestion control's rate falls as the
/// square root of the loss rate, Mathis et al., CCR 1997), so the run's horizon is ten minutes.
#[test]
fn a_jump_onto_a_lossy_path_retreats_and_every_transfer_completes() {
    let pki = Pki::new(0);
    for seed in SEEDS {
        let out = Run::new(
            Scenario {
                dials: 2,
                early: true,
                reply_bytes: BULK,
                clean_first: true,
                horizon: 600_000 * MS,
                ..Scenario::lossy(seed)
            },
            &pki,
        )
        .run();
        assert_eq!(out.dialed.len(), 2, "seed {seed}: {:?}", out.dialed);
        for d in &out.dialed {
            // The client closes each dial at its reply, which the server sees as closed by peer
            let closed_by_client = d
                .server_lost
                .as_ref()
                .is_none_or(|(_, reason)| reason == "closed by peer: 0");
            assert!(
                d.replied.is_some() && d.lost.is_none() && closed_by_client,
                "seed {seed}: {d:?}"
            );
        }
    }
}

/// A server whose window is full of 0.5-RTT data when the client's Finished is lost: the client's
/// acknowledgements of that data come in 1-RTT packets the server may not process before its
/// handshake completes (RFC 9001 §5.7), so it drops them, and when the retransmitted Finished
/// completes the handshake it must arm its probe timer for the Data space (RFC 9002 §6.2.1: the
/// server's handshake is confirmed when it completes). Upstream armed the timer while discarding
/// the Handshake keys, a step before the connection counted as established, so the Data space was
/// still skipped; with the window full nothing was sent again and both sides idled out.
#[test]
fn a_server_whose_handshake_completes_late_probes_its_full_window() {
    let pki = Pki::new(0);
    let scenario = Scenario {
        dials: 2,
        early: true,
        reply_bytes: BULK,
        ..Scenario::clean()
    };
    let clean = Run::new(scenario, &pki).run();
    let resumed = clean.dialed[1].started;
    let (finished, _) = position(&clean, true, |s| {
        s.at >= resumed && s.kinds.contains(&Kind::Handshake)
    });
    let out = Run::new(
        Scenario {
            drop_client: chosen(vec![finished]),
            ..scenario
        },
        &pki,
    )
    .run();
    assert_eq!(out.dialed.len(), 2);
    let d = &out.dialed[1];
    assert!(d.replied.is_some() && d.lost.is_none(), "{d:?}");
}

/// The Careful Resume table of `docs/benchmarks.md`: a fresh dial and a resumed one, each asking
/// for a reply of each size, on the clean path, with Careful Resume and without; each dial's first
/// reply from its start, against its floor of round trips (2 fresh, 1 resumed with 0-RTT).
#[test]
#[ignore = "prints the Careful Resume table; run by hand"]
fn print_the_resume_table() {
    let pki = Pki::new(0);
    for bytes in [BULK / 2, BULK, 2 * BULK] {
        for careful_resume in [false, true] {
            let out = Run::new(
                Scenario {
                    dials: 2,
                    early: true,
                    reply_bytes: bytes,
                    careful_resume,
                    ..Scenario::clean()
                },
                &pki,
            )
            .run();
            let replies: Vec<String> = out
                .dialed
                .iter()
                .map(|d| format!("{} ms", (d.replied.unwrap() - d.started) / MS))
                .collect();
            println!(
                "{bytes} bytes, careful resume {careful_resume}: fresh then resumed first reply {replies:?}"
            );
        }
    }
}
