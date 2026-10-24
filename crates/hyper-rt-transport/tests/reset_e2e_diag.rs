//! DIAG (hunt branch only): a server's driver serving clients that come one after another, each
//! dropped (its socket closed, no close sent) while the server still sends to it, as mantle's test
//! clients go. Prints each client's port, its handshake and exchange times, and the server's listener
//! counts at each server event. Nothing here decides a pass; every wait is bounded.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::disallowed_macros,
    clippy::disallowed_methods,
    clippy::cognitive_complexity,
    clippy::too_many_lines,
    clippy::cast_possible_truncation,
    dead_code,
    missing_docs
)]

#[path = "../../hyper-transport/tests/common/mod.rs"]
mod common;

use std::future::{Future, poll_fn};
use std::net::SocketAddr;
use std::pin::pin;
use std::task::Poll;
use std::time::{Duration, Instant};

use common::*;
use hyper_rt::runtime::{LocalRuntime, RuntimeConfig};
use hyper_rt_transport::{Driver, Io};
use hyper_transport::{Config, Endpoint, Event, Fixed};

type Drv = Driver<Mantle, Fixed, Book>;

const IO: Io = Io { batch: 10 };
const PERIOD: Duration = Duration::from_secs(2);
const TICK_NS: u64 = 1_000_000;
/// A client that takes this long for one step is abandoned and the next begins.
const STEP_BOUND: Duration = Duration::from_secs(8);
const CLIENTS: usize = 24;

fn config() -> RuntimeConfig {
    RuntimeConfig {
        shards: 1,
        tasks_per_shard: 64,
        timers_per_shard: 64,
        interests_per_shard: 64,
        ring_entries: 64,
        step_budget_ns: 1_000_000_000,
        timer_tick_ns: 100_000,
        batch: 64,
        pin: false,
        cores: Vec::new(),
        page_bytes: 4096,
        spin_ns: 0,
        wake_tracking: None,
        latency_objective_ns: None,
    }
}

fn endpoint(
    pki: &Pki,
    own: &(Vec<u8>, Vec<u8>),
    other: &[u8],
    peer: u64,
    listen: bool,
) -> Endpoint<Mantle, Fixed, Book> {
    let mut book = Book::default();
    book.add(other, peer, Role::Node);
    let config = Config {
        tls: hyper_transport::tls::Tls::mutual(&credentials(&pki.root, &own.0, &own.1), listen)
            .unwrap(),
        role: Role::Node,
        limits: limits(),
        congestion: Default::default(),
    };
    Endpoint::new(config, Fixed::new(512 << 20, 64), book, Instant::now()).unwrap()
}

async fn next(driver: &mut Drv) -> Option<Event<Mantle>> {
    let mut event = pin!(driver.event());
    let mut tick = pin!(hyper_rt::futures::sleep(TICK_NS));
    poll_fn(|context| {
        if let Poll::Ready(event) = event.as_mut().poll(context) {
            return Poll::Ready(Some(event.unwrap()));
        }
        if tick.as_mut().poll(context).is_ready() {
            return Poll::Ready(None);
        }
        Poll::Pending
    })
    .await
}

/// Drives `driver` until `done`, or the bound; whether done.
async fn until(driver: &mut Drv, asker: &mut Asker, done: &dyn Fn(&Asker) -> bool) -> bool {
    let began = Instant::now();
    while !done(asker) {
        if let Some(event) = next(driver).await {
            asker.clock = driver.clock().unwrap();
            asker.on_event(driver.endpoint(), event);
            while let Some(event) = driver.endpoint().poll_event() {
                asker.on_event(driver.endpoint(), event);
            }
        }
        asker.advance_all(driver.endpoint());
        if began.elapsed() > STEP_BOUND {
            return false;
        }
    }
    true
}

fn run(variant: &str, body: u64, drop_at_reply: bool) {
    let pki = Pki::new();
    let one = pki.issue(&name(1));
    let two = pki.issue(&name(2));
    let mut rt = LocalRuntime::new(&config()).unwrap();
    let start = Instant::now();
    let variant = variant.to_string();
    rt.block_on(async move {
        let loopback: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let mut server_driver: Drv =
            Driver::bind(endpoint(&pki, &two, &one.0, 1, true), loopback, IO).unwrap();
        let server_addr = server_driver.local_addr().unwrap();
        let tag = variant.clone();
        hyper_rt::futures::spawn_detached(async move {
            let mut server = Server::new();
            let mut last = server_driver.stats();
            loop {
                let Ok(event) = server_driver.event().await else {
                    return;
                };
                let line = match &event {
                    Event::Connected { .. } | Event::Closed { .. } | Event::Unreachable { .. } => {
                        Some(format!("{event:?}"))
                    }
                    _ => None,
                };
                server.on_event(server_driver.endpoint(), event);
                while let Some(event) = server_driver.endpoint().poll_event() {
                    server.on_event(server_driver.endpoint(), event);
                }
                server.advance_all(server_driver.endpoint());
                let stats = server_driver.stats();
                if stats.astray != last.astray || line.is_some() {
                    println!(
                        "DIAG {tag} server at {:?}: {} astray {} empty {} received {} sent {}",
                        start.elapsed(),
                        line.unwrap_or_default(),
                        stats.astray,
                        stats.receives_empty,
                        stats.received,
                        stats.sent
                    );
                }
                last = stats;
            }
        })
        .unwrap();
        for client in 0..CLIENTS {
            let mut driver: Drv =
                Driver::bind(endpoint(&pki, &one, &two.0, 2, false), loopback, IO).unwrap();
            let port = driver.local_addr().unwrap().port();
            let mut asker = Asker::new();
            let began = Instant::now();
            let now = driver.clock().unwrap();
            driver.endpoint().connect(now, 2, server_addr).unwrap();
            let connected = until(&mut driver, &mut asker, &|asker| !asker.connected.is_empty()).await;
            let handshake = began.elapsed();
            let now = driver.clock().unwrap();
            asker
                .ask(driver.endpoint(), now, 2, (Kind::Get, Class::Request), 7, Some(body), PERIOD)
                .unwrap();
            asker.advance_all(driver.endpoint());
            let asked = Instant::now();
            let replied = if drop_at_reply {
                until(&mut driver, &mut asker, &|asker| asker.asked[0].reply.is_some()).await
            } else {
                until(&mut driver, &mut asker, &|asker| asker.finished()).await
            };
            let exchange = asked.elapsed();
            let stats = driver.stats();
            if !replied {
                let a = &asker.asked[0];
                println!(
                    "DIAG {variant} client {client} STALLED: written {} of {} blocked {} reply {:?} read {} refused {:?}; events {:?}; quic {:?}",
                    a.written,
                    a.body,
                    a.blocked,
                    a.reply,
                    a.read,
                    a.refused,
                    asker.events.iter().rev().take(6).collect::<Vec<_>>(),
                    driver.endpoint().connection_stats(2)
                );
            }
            println!(
                "DIAG {variant} client {client} port {port} at {:?}: connected {connected} in {handshake:?}, \
                 replied {replied} in {exchange:?}; client astray {} received {} sent {}",
                start.elapsed(),
                stats.astray,
                stats.received,
                stats.sent
            );
            drop(driver);
        }
        // Let the server's last sends meet the closed port and be counted.
        let _ = hyper_rt::futures::sleep(500_000_000).await;
    })
    .unwrap();
}

#[test]
fn e1_clients_dropped_after_a_whole_exchange() {
    run("e1", 64 << 10, false);
}

#[test]
fn e2_clients_dropped_at_the_reply_head_of_a_large_body() {
    run("e2", 4 << 20, true);
}

fn endpoint_of(
    root: &[u8],
    own: &(Vec<u8>, Vec<u8>),
    other: &[u8],
    peer: u64,
    listen: bool,
) -> Endpoint<Mantle, Fixed, Book> {
    let mut book = Book::default();
    book.add(other, peer, Role::Node);
    let config = Config {
        tls: hyper_transport::tls::Tls::mutual(&credentials(root, &own.0, &own.1), listen).unwrap(),
        role: Role::Node,
        limits: limits(),
        congestion: Default::default(),
    };
    Endpoint::new(config, Fixed::new(512 << 20, 64), book, Instant::now()).unwrap()
}

/// As mantle's cell tests go: the server on a thread and runtime of its own, each client on a thread
/// and runtime of its own that ends (its socket closed, no close sent) once its exchange is whole,
/// the next client made only then.
#[test]
fn e3_clients_each_on_a_thread_of_its_own() {
    let pki = Pki::new();
    let one = pki.issue(&name(1));
    let two = pki.issue(&name(2));
    let root = pki.root.clone();
    let start = Instant::now();
    let (addr_tx, addr_rx) = std::sync::mpsc::channel();
    let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
    let server_root = root.clone();
    let (server_own, server_other) = (two.clone(), one.0.clone());
    let server = std::thread::spawn(move || {
        let mut rt = LocalRuntime::new(&config()).unwrap();
        rt.block_on(async move {
            let loopback: SocketAddr = "127.0.0.1:0".parse().unwrap();
            let mut driver: Drv = Driver::bind(
                endpoint_of(&server_root, &server_own, &server_other, 1, true),
                loopback,
                IO,
            )
            .unwrap();
            addr_tx.send(driver.local_addr().unwrap()).unwrap();
            let mut server = Server::new();
            let mut last = driver.stats();
            loop {
                if let Some(event) = next(&mut driver).await {
                    let line = format!("{event:?}");
                    let lifecycle = line.starts_with("Connected") || line.starts_with("Closed");
                    server.on_event(driver.endpoint(), event);
                    if lifecycle {
                        println!("DIAG e3 server at {:?}: {line}", start.elapsed());
                    }
                }
                while let Some(event) = driver.endpoint().poll_event() {
                    let line = format!("{event:?}");
                    if line.starts_with("Connected") || line.starts_with("Closed") {
                        println!("DIAG e3 server at {:?}: {line}", start.elapsed());
                    }
                    server.on_event(driver.endpoint(), event);
                }
                server.advance_all(driver.endpoint());
                let stats = driver.stats();
                if stats.astray != last.astray || stats.send_errors != last.send_errors {
                    println!(
                        "DIAG e3 server at {:?}: astray {} -> {} send_errors {} sent {} send_calls {} received {} receive_calls {} empty {}",
                        start.elapsed(),
                        last.astray,
                        stats.astray,
                        stats.send_errors,
                        stats.sent,
                        stats.send_calls,
                        stats.received,
                        stats.receive_calls,
                        stats.receives_empty
                    );
                }
                last = stats;
                if stop_rx.try_recv().is_ok() {
                    return;
                }
            }
        })
        .unwrap();
    });
    let server_addr: SocketAddr = addr_rx.recv().unwrap();
    for client in 0..CLIENTS {
        let (root, own, other) = (root.clone(), one.clone(), two.0.clone());
        #[cfg(windows)]
        let _ = hyper_rt::udp::diag_take();
        let line = std::thread::spawn(move || {
            let mut rt = LocalRuntime::new(&config()).unwrap();
            rt.block_on(async move {
                let loopback: SocketAddr = "127.0.0.1:0".parse().unwrap();
                let mut driver: Drv =
                    Driver::bind(endpoint_of(&root, &own, &other, 2, false), loopback, IO).unwrap();
                let port = driver.local_addr().unwrap().port();
                let mut asker = Asker::new();
                let began = Instant::now();
                let now = driver.clock().unwrap();
                driver.endpoint().connect(now, 2, server_addr).unwrap();
                let connected =
                    until(&mut driver, &mut asker, &|asker| !asker.connected.is_empty()).await;
                let handshake = began.elapsed();
                let now = driver.clock().unwrap();
                asker
                    .ask(driver.endpoint(), now, 2, (Kind::Put, Class::Request), 9, Some(16 << 10), PERIOD)
                    .unwrap();
                asker.advance_all(driver.endpoint());
                let asked = Instant::now();
                let replied = until(&mut driver, &mut asker, &|asker| asker.finished()).await;
                let exchange = asked.elapsed();
                let stats = driver.stats();
                let path = driver.endpoint().connection_stats(2).map(|quic| quic.path);
                let refused = asker.asked[0].refused;
                let events: Vec<String> = asker.events.iter().map(|e| e.chars().take(48).collect()).collect();
                format!(
                    "client {client} port {port} at {:?}: connected {connected} in {handshake:?}, replied {replied} in {exchange:?} refused {refused:?} events {events:?}; sent {} received {} astray {} send_errors {}; path {:?}",
                    start.elapsed(),
                    stats.sent,
                    stats.received,
                    stats.astray,
                    stats.send_errors,
                    path.map(|p| (p.rtt, p.min_rtt, p.lost_packets, p.sent_packets, p.sent_plpmtud_probes, p.lost_plpmtud_probes, p.current_mtu))
                )
            })
            .unwrap()
        })
        .join()
        .unwrap();
        println!("DIAG e3 {line}");
        #[cfg(windows)]
        {
            let log = hyper_rt::udp::diag_take();
            let slow = line.contains("replied true in") && {
                let at = line.find("replied true in ").map_or(0, |at| at + 16);
                let rest = &line[at..];
                let end = rest.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(0);
                let unit = &rest[end..];
                let value: f64 = rest[..end].parse().unwrap_or(0.0);
                !(unit.starts_with("µs") || (unit.starts_with("ms") && value < 20.0))
            };
            if slow {
                for entry in log {
                    println!("DIAG e3 log {entry}");
                }
            }
        }
    }
    let _ = std::thread::sleep(Duration::from_millis(300));
    stop_tx.send(()).unwrap();
    server.join().unwrap();
}
