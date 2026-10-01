//! hyper-tokio between real processes over real UDP sockets on loopback (CLAUDE.md §1a), driven
//! the way focal drives its transport: a tokio runtime, the owner's task awaiting the driver.
//!
//! This binary is both sides. Run as a test it is the asking side, and it spawns itself with
//! `HK_PEER` set as the answering side, a process that shares nothing with it but the kernel's
//! sockets. Each side is one current-thread runtime; one task owns its driver and its plane.
//!
//! - `exchanges`: request and reply in every class, bodies of megabytes both ways, every byte
//!   checked; the owner races every wait against its own 1 ms tick, and each time the tick
//!   wins the driver's future is dropped mid-wait and made again (cancel safety);
//! - `lanes`: replication frames on two lanes, echoed back by the peer, in order;
//! - `plane`: both sides key a datagram plane from the connection's TLS exporter, on a socket of
//!   its own, and every message crosses sealed and is echoed back;
//! - `killed`: the peer is killed with SIGKILL mid-upload; the exchange is refused as stalled
//!   within its progress deadline, and a new peer process is reached after the route is retired.
//!
//! Every wait is for a fact; the only wall-clock bound is a failure guard far past anything the
//! protocol needs. Receive errors from datagrams sent to a closed port (Windows' resets) are read
//! past by the socket and counted.

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

#[path = "../../hyper-transport/tests/common/mod.rs"]
mod common;

use std::future::{Future, poll_fn};
use std::io::{BufRead, BufReader};
use std::net::SocketAddr;
use std::pin::pin;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::Poll;
use std::time::{Duration, Instant};

use common::*;
use hyper_datagram::{
    AdmitAll, EXPORTER_LABEL, ExporterSecret, Plane, PlaneLimits, Role as PlaneRole, SECRET_BYTES,
};
use hyper_tokio::{Driver, Io, IoStats, PlaneSocket};
use hyper_transport::{Config, Endpoint, Event, Fixed, PeerId, Refusal};

/// The failure guard of a wait: far past any outcome these scenarios wait for on loopback, so
/// that a protocol that never ends fails the test instead of hanging it.
const GUARD: Duration = Duration::from_secs(120);
/// The period exchanges are judged at.
const PERIOD: Duration = Duration::from_secs(2);
/// The owner's own cadence, which every wait races: tokio's timer granularity, so the driver's
/// future is dropped and made again as often as the timer allows.
const TICK: Duration = Duration::from_millis(1);
/// Datagrams a system call carries: one QUIC initial window's worth (RFC 9002 §7.2: ten
/// datagrams), the most a fresh connection sends in one burst.
const IO: Io = Io { batch: 10 };
/// The plane's bounds: two peers' epochs, and a replay window of RFC 4303's 64 widened to 1,024.
const PLANE: PlaneLimits = PlaneLimits {
    max_peers: 4,
    epochs_per_peer: 2,
    window_limit: 1_024,
};
/// Messages the plane scenario sends, and their length.
const PLANE_MESSAGES: u32 = 2_000;
const PLANE_MESSAGE: usize = 48;

type Node<C> = common::Node<C>;

/// How many times the owner's tick won a race and the driver's future was dropped mid-wait.
static DROPPED: AtomicU64 = AtomicU64::new(0);
type Drv<C> = Driver<C, Fixed, Book>;

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

/// What a race ended on.
enum Woke {
    Event(Event<Mantle>),
    /// The messages of the datagrams the plane opened.
    Plane(Vec<Vec<u8>>),
    Tick,
}

/// The driver's next event, the plane's next batch (when there is a plane), or the owner's tick,
/// whichever comes first; the others' futures are dropped.
async fn next(driver: &mut Drv<Mantle>, plane: Option<(&mut PlaneSocket, &mut Plane)>) -> Woke {
    let mut opened = Vec::new();
    let woke = {
        let mut event = pin!(driver.event());
        let mut tick = pin!(tokio::time::sleep(TICK));
        let mut receive = pin!(async {
            match plane {
                Some((socket, plane)) => {
                    socket
                        .receive(plane, &AdmitAll, |_, result| {
                            if let Ok(datagram) = result {
                                opened.extend(datagram.messages().map(<[u8]>::to_vec));
                            }
                        })
                        .await
                        .unwrap();
                }
                None => std::future::pending::<()>().await,
            }
        });
        poll_fn(|context| {
            if let Poll::Ready(event) = event.as_mut().poll(context) {
                return Poll::Ready(Some(Woke::Event(event.unwrap())));
            }
            if receive.as_mut().poll(context).is_ready() {
                return Poll::Ready(None);
            }
            if tick.as_mut().poll(context).is_ready() {
                return Poll::Ready(Some(Woke::Tick));
            }
            Poll::Pending
        })
        .await
    };
    woke.unwrap_or(Woke::Plane(opened))
}

/// The epoch a connection gives the plane, with keys from its TLS exporter.
fn key_plane(node: &mut Node<Mantle>, plane: &mut Plane, peer: PeerId, role: PlaneRole) {
    let mut secret = [0u8; SECRET_BYTES];
    let epoch = node
        .export_keying_material(peer, EXPORTER_LABEL, b"", &mut secret)
        .unwrap();
    plane
        .install_epoch(
            peer,
            u32::try_from(epoch).unwrap(),
            &ExporterSecret::new(secret),
            role,
        )
        .unwrap();
    let path = node.path(peer).unwrap();
    plane
        .set_path(peer, usize::from(path.max_datagram))
        .unwrap();
}

// ---------------------------------------------------------------------------------------------
// The answering side.

fn peer_process() {
    runtime().block_on(serve());
}

async fn serve() {
    let root = unhex(&std::env::var("HK_ROOT").unwrap());
    let certificate = unhex(&std::env::var("HK_CERT").unwrap());
    let key = unhex(&std::env::var("HK_KEY").unwrap());
    let asker = unhex(&std::env::var("HK_ASKER").unwrap());
    let mut book = Book::default();
    book.add(&asker, 1, Role::Node);
    let config = Config {
        credentials: credentials(&root, &certificate, &key),
        role: Role::Node,
        limits: limits(),
        listen: true,
    };
    let endpoint =
        Endpoint::<Mantle, _, _>::new(config, Fixed::new(512 << 20, 64), book, Instant::now())
            .unwrap();
    let mut driver = Driver::bind(endpoint, "127.0.0.1:0".parse().unwrap(), IO).unwrap();
    let mut socket = PlaneSocket::bind("127.0.0.1:0".parse().unwrap(), IO).unwrap();
    let mut plane = Plane::new(2, PLANE).unwrap();
    println!(
        "listening {} {}",
        driver.local_addr().unwrap().port(),
        socket.local_addr().unwrap().port()
    );
    let plane_peer: SocketAddr = std::env::var("HK_PLANE")
        .ok()
        .and_then(|port| port.parse::<u16>().ok())
        .map_or(SocketAddr::from(([127, 0, 0, 1], 0)), |port| {
            SocketAddr::from(([127, 0, 0, 1], port))
        });
    let began = Instant::now();
    let mut server = Server::new();
    let mut ever = false;
    loop {
        match next(&mut driver, Some((&mut socket, &mut plane))).await {
            Woke::Event(event) => {
                if let Event::Connected { peer, .. } = event {
                    key_plane(driver.endpoint(), &mut plane, peer, PlaneRole::Acceptor);
                }
                server.on_event(driver.endpoint(), event);
                while let Some(event) = driver.endpoint().poll_event() {
                    if let Event::Connected { peer, .. } = event {
                        key_plane(driver.endpoint(), &mut plane, peer, PlaneRole::Acceptor);
                    }
                    server.on_event(driver.endpoint(), event);
                }
                server.advance_all(driver.endpoint());
            }
            Woke::Plane(messages) => {
                for message in messages {
                    if plane.queue(1, &message).is_err() {
                        socket.flush(&mut plane, |_| Some(plane_peer), |_, _| {});
                        plane.queue(1, &message).unwrap();
                    }
                }
                socket.flush(&mut plane, |_| Some(plane_peer), |_, _| {});
            }
            Woke::Tick => server.advance_all(driver.endpoint()),
        }
        for (peer, lane, frame) in server.frames.drain(..) {
            driver
                .endpoint()
                .send_frame(peer, lane, Kind::Append, &frame)
                .unwrap();
        }
        ever |= !server.connected.is_empty();
        let connections = driver.endpoint().stats().connections;
        if (ever && connections == 0) || (!ever && began.elapsed() > limits().idle_timeout * 3) {
            driver.flush();
            return;
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The asking side.

struct Peer {
    child: Child,
    address: SocketAddr,
    plane: SocketAddr,
}

impl Drop for Peer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Setup {
    pki: Pki,
    one: (Vec<u8>, Vec<u8>),
    two: (Vec<u8>, Vec<u8>),
}

impl Setup {
    fn new() -> Self {
        let pki = Pki::new();
        let one = pki.issue(&name(1));
        let two = pki.issue(&name(2));
        Self { pki, one, two }
    }

    fn spawn(&self, plane: SocketAddr) -> Peer {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .env("HK_PEER", "1")
            .env("HK_ROOT", hex(&self.pki.root))
            .env("HK_CERT", hex(&self.two.0))
            .env("HK_KEY", hex(&self.two.1))
            .env("HK_ASKER", hex(&self.one.0))
            .env("HK_PLANE", plane.port().to_string())
            .stdout(Stdio::piped());
        let mut child = command.spawn().unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let mut ports = line
            .trim()
            .strip_prefix("listening ")
            .unwrap()
            .split(' ')
            .map(|port| port.parse::<u16>().unwrap());
        let (port, plane_port) = (ports.next().unwrap(), ports.next().unwrap());
        Peer {
            child,
            address: SocketAddr::from(([127, 0, 0, 1], port)),
            plane: SocketAddr::from(([127, 0, 0, 1], plane_port)),
        }
    }

    fn driver(&self) -> Drv<Mantle> {
        let mut book = Book::default();
        book.add(&self.two.0, 2, Role::Node);
        let config = Config {
            credentials: credentials(&self.pki.root, &self.one.0, &self.one.1),
            role: Role::Node,
            limits: limits(),
            listen: false,
        };
        let endpoint =
            Endpoint::new(config, Fixed::new(512 << 20, 64), book, Instant::now()).unwrap();
        Driver::bind(endpoint, "127.0.0.1:0".parse().unwrap(), IO).unwrap()
    }
}

/// Runs the driver until `fact` holds; panics past the failure guard.
async fn wait(
    driver: &mut Drv<Mantle>,
    asker: &mut Asker,
    what: &str,
    mut fact: impl FnMut(&mut Node<Mantle>, &mut Asker) -> bool,
) -> Duration {
    let began = Instant::now();
    loop {
        if fact(driver.endpoint(), asker) {
            return began.elapsed();
        }
        match next(driver, None).await {
            Woke::Event(event) => {
                // The event the driver woke for, then every one queued behind it, before the
                // exchanges move: moving them costs a pass over every exchange.
                asker.clock = Instant::now();
                asker.on_event(driver.endpoint(), event);
                while let Some(event) = driver.endpoint().poll_event() {
                    asker.on_event(driver.endpoint(), event);
                }
            }
            Woke::Tick => {
                DROPPED.fetch_add(1, Ordering::Relaxed);
            }
            Woke::Plane(_) => {}
        }
        asker.advance_all(driver.endpoint());
        assert!(
            began.elapsed() < GUARD,
            "{what}: never held; events {:?}",
            asker.events.iter().rev().take(8).collect::<Vec<_>>()
        );
    }
}

async fn connect(driver: &mut Drv<Mantle>, asker: &mut Asker, peer: &Peer) -> Duration {
    driver
        .endpoint()
        .connect(Instant::now(), 2, peer.address)
        .unwrap();
    let before = asker.connected.len();
    wait(driver, asker, "connected", |_, asker| {
        asker.connected.len() > before
    })
    .await
}

fn batching(stats: IoStats) -> String {
    let per = |datagrams: u64, calls: u64| datagrams as f64 / calls.max(1) as f64;
    format!(
        "{} datagrams sent, {} received; {:.2} a send call, {:.2} a receive call (segmentation {}, coalescing {})",
        stats.sent,
        stats.received,
        per(stats.sent, stats.send_calls),
        per(stats.received, stats.receive_calls),
        stats.gso,
        stats.gro
    )
}

async fn exchanges() -> String {
    let setup = Setup::new();
    let peer = setup.spawn(SocketAddr::from(([127, 0, 0, 1], 0)));
    let mut driver = setup.driver();
    let mut asker = Asker::new();
    let handshake = connect(&mut driver, &mut asker, &peer).await;
    let now = Instant::now();
    let node = driver.endpoint();
    asker
        .ask(node, now, 2, (Kind::Vote, Class::Control), 1, None, PERIOD)
        .unwrap();
    asker
        .ask(
            node,
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
            node,
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
                node,
                now,
                2,
                (Kind::Get, Class::Request),
                seed,
                Some(64 << 10),
                PERIOD,
            )
            .unwrap();
    }
    asker.advance_all(driver.endpoint());
    let took = wait(
        &mut driver,
        &mut asker,
        "every exchange done",
        |_, asker| asker.finished(),
    )
    .await;
    for asked in &asker.asked {
        assert_eq!(asked.refused, None, "{asked:?}");
        assert_eq!(
            (asked.reply, asked.read),
            (Some(asked.body), asked.body),
            "{asked:?}"
        );
    }
    let vote = &asker.asked[0];
    let latency = vote.answered.unwrap() - vote.opened.unwrap();
    driver.endpoint().disconnect(Instant::now(), 2);
    driver.flush();
    format!(
        "handshake {handshake:?}; 19 exchanges, 9.0 MiB each way, in {took:?}; a vote answered in {latency:?}; the driver's future dropped mid-wait {} times; {}",
        DROPPED.load(Ordering::Relaxed),
        batching(driver.stats())
    )
}

async fn lanes() -> String {
    const FRAMES: u32 = 4_000;
    let setup = Setup::new();
    let peer = setup.spawn(SocketAddr::from(([127, 0, 0, 1], 0)));
    let mut driver = setup.driver();
    let mut asker = Asker::new();
    connect(&mut driver, &mut asker, &peer).await;
    let window = limits().lane_window as u32;
    let mut sent = [0u32; 2];
    let began = Instant::now();
    wait(
        &mut driver,
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
    )
    .await;
    let took = began.elapsed();
    for lane in 0..2u32 {
        let order: Vec<u32> = asker
            .frames
            .iter()
            .filter(|(which, _)| *which == lane)
            .map(|(_, frame)| u32::from_be_bytes(frame[..4].try_into().unwrap()))
            .collect();
        assert_eq!(order, (0..FRAMES).collect::<Vec<_>>(), "lane {lane}");
    }
    driver.endpoint().disconnect(Instant::now(), 2);
    driver.flush();
    format!(
        "{} frames of 512 B on two lanes, echoed in order, in {took:?}; {}",
        FRAMES * 2,
        batching(driver.stats())
    )
}

async fn plane() -> String {
    let setup = Setup::new();
    let mut socket = PlaneSocket::bind("127.0.0.1:0".parse().unwrap(), IO).unwrap();
    let peer = setup.spawn(socket.local_addr().unwrap());
    let mut driver = setup.driver();
    let mut asker = Asker::new();
    connect(&mut driver, &mut asker, &peer).await;
    let mut plane = Plane::new(1, PLANE).unwrap();
    key_plane(driver.endpoint(), &mut plane, 2, PlaneRole::Initiator);
    // The peer keys its plane on its own `Connected`, which may follow ours: a datagram it cannot
    // open yet is dropped and sent again, as Raft and SWIM send again.
    let mut echoed = vec![false; PLANE_MESSAGES as usize];
    let mut next_message = 0u32;
    let (mut datagrams, mut resent) = (0u64, 0u64);
    let began = Instant::now();
    while echoed.iter().any(|seen| !seen) {
        // One datagram of the messages not yet echoed, from the oldest.
        let mut queued = 0;
        let start = echoed.iter().position(|seen| !seen).unwrap() as u32;
        if start < next_message {
            resent += 1;
        }
        for index in start..PLANE_MESSAGES {
            if echoed[index as usize] {
                continue;
            }
            let mut message = [0u8; PLANE_MESSAGE];
            message[..4].copy_from_slice(&index.to_be_bytes());
            if plane.queue(2, &message).is_err() {
                break;
            }
            next_message = next_message.max(index + 1);
            queued += 1;
        }
        assert!(queued > 0);
        socket.flush(
            &mut plane,
            |_| Some(peer.plane),
            |_, refusal| panic!("refused {refusal:?}"),
        );
        datagrams += 1;
        // Its echo, or the owner's tick after a round trip's worth of ticks: then send again.
        for _ in 0..50 {
            match next(&mut driver, Some((&mut socket, &mut plane))).await {
                Woke::Plane(messages) => {
                    for message in &messages {
                        let index = u32::from_be_bytes(message[..4].try_into().unwrap());
                        echoed[index as usize] = true;
                    }
                    break;
                }
                Woke::Event(event) => asker.on_event(driver.endpoint(), event),
                Woke::Tick => {}
            }
        }
        assert!(began.elapsed() < GUARD, "the plane's echoes never came");
    }
    let took = began.elapsed();
    driver.endpoint().disconnect(Instant::now(), 2);
    driver.flush();
    format!(
        "{PLANE_MESSAGES} messages of {PLANE_MESSAGE} B sealed under exporter keys, echoed, in {datagrams} datagrams ({resent} sent again) in {took:?}"
    )
}

async fn killed() -> String {
    let setup = Setup::new();
    let peer = setup.spawn(SocketAddr::from(([127, 0, 0, 1], 0)));
    let mut driver = setup.driver();
    let mut asker = Asker::new();
    connect(&mut driver, &mut asker, &peer).await;
    let upload = asker
        .ask(
            driver.endpoint(),
            Instant::now(),
            2,
            (Kind::Snapshot, Class::Bulk),
            1,
            Some(512 << 20),
            PERIOD,
        )
        .unwrap();
    wait(&mut driver, &mut asker, "a megabyte sent", |_, asker| {
        asker.asked[upload].written > 1 << 20
    })
    .await;
    let mut peer = peer;
    peer.child.kill().unwrap();
    peer.child.wait().unwrap();
    let ended = wait(&mut driver, &mut asker, "the upload refused", |_, asker| {
        asker.asked[upload].done
    })
    .await;
    assert_eq!(asker.asked[upload].refused, Some((Refusal::Stalled, false)));
    assert!(ended <= PERIOD * 3, "ended {ended:?} after the kill");
    driver.endpoint().disconnect(Instant::now(), 2);
    let again = setup.spawn(SocketAddr::from(([127, 0, 0, 1], 0)));
    connect(&mut driver, &mut asker, &again).await;
    let next_exchange = asker
        .ask(
            driver.endpoint(),
            Instant::now(),
            2,
            (Kind::Get, Class::Request),
            2,
            Some(4096),
            PERIOD,
        )
        .unwrap();
    wait(&mut driver, &mut asker, "the next exchange", |_, asker| {
        asker.asked[next_exchange].done
    })
    .await;
    assert_eq!(asker.asked[next_exchange].refused, None);
    driver.endpoint().disconnect(Instant::now(), 2);
    driver.flush();
    let stats = driver.stats();
    format!(
        "an upload to a killed peer refused as stalled {ended:?} after the kill; the next peer answered; {} receive errors read past",
        stats.receive_errors
    )
}

fn main() {
    if std::env::var("HK_PEER").is_ok() {
        peer_process();
        return;
    }
    let only = std::env::args()
        .skip(1)
        .find(|argument| !argument.starts_with('-'));
    let runtime = runtime();
    type Scenario = fn() -> std::pin::Pin<Box<dyn Future<Output = String>>>;
    let scenarios: [(&str, Scenario); 4] = [
        ("exchanges", || Box::pin(exchanges())),
        ("lanes", || Box::pin(lanes())),
        ("plane", || Box::pin(plane())),
        ("killed", || Box::pin(killed())),
    ];
    for (name, scenario) in scenarios {
        if only.as_deref().is_some_and(|only| !name.contains(only)) {
            continue;
        }
        let began = Instant::now();
        let report = runtime.block_on(scenario());
        println!("e2e {name}: ok in {:?}: {report}", began.elapsed());
    }
}
