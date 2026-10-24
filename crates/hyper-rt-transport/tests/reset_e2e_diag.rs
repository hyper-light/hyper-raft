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
const CLIENTS: usize = 12;

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
