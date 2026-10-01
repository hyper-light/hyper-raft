//! hyper-transport against focal-wire's core on one workload, both on tokio (docs/benchmarks.md,
//! "hyper-transport").
//!
//! Two endpoints in one process over two UDP sockets on loopback, on one current-thread tokio
//! runtime: the answering side in a spawned task, the asking side in the runtime's main task, both
//! parked on tokio's reactor and timer between datagrams. A round is one exchange: a request of
//! `size` bytes and a reply of `size` bytes, the next round once the reply is whole. A row reports
//! the median round's wall time over its rounds, and the allocations, reallocations and bytes the
//! whole process made per round.
//!
//! - **focal-wire**: its own transport configuration (`quic_transport` through `server_tls` and
//!   `client_tls`), its frame codec (`write_frame`, `read_frame_header`, `read_payload_arriving`,
//!   `require_end`) and its exchange shape (one bidirectional stream, priority set, the request
//!   written and finished, the reply's header then its payload), over quinn. The domain envelope
//!   and the registry are left out: the payload is the bytes.
//! - **hyper-transport**: one exchange through `open`, `write_body`, `reply`, `read_body`, with a
//!   16-byte head each way and the body of `size` bytes, each endpoint driven by hyper-tokio's
//!   `Driver`.
//!
//! One row a process: `hyper-transport-compare <focal|hyper> <size> [rounds]`. The rows of the
//! tables are medians over fresh processes in rotated order (`compare.sh` in this directory).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    missing_docs
)]

#[path = "../../hyper-transport/tests/common/mod.rs"]
mod common;

use std::time::{Duration, Instant};

use common::*;
use hyper_measure::alloc;
use hyper_tokio::{Driver, Io};
use hyper_transport::{Event, ExchangeId, Fixed, Progress};

#[global_allocator]
static ALLOCATOR: alloc::Counting = alloc::Counting;

/// The head each side sends with its body: a request's or a reply's header, as in the
/// allocation bench.
const HEAD: usize = 16;
/// Datagrams a system call carries: one QUIC initial window (RFC 9002 §7.2), as the end-to-end
/// scenarios use.
const IO: Io = Io { batch: 10 };

type Drv = Driver<Mantle, Fixed, Book>;

struct Row {
    median: Duration,
    /// Datagrams the asking side sent and received in the measured rounds.
    datagrams: u64,
    counts: alloc::Counts,
    rounds: usize,
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

// ---------------------------------------------------------------------------------------------
// hyper-transport, driven by hyper-tokio.

/// The answering side: reads each request's body into reservations, replies with a body as long,
/// and ends the exchange once it is written.
async fn answer(mut driver: Drv, size: u64, piece: Vec<u8>) {
    let head = [7u8; HEAD];
    let mut served: Option<(ExchangeId, u64, u64, bool)> = None;
    loop {
        let Ok(event) = driver.event().await else {
            return;
        };
        if let Event::Request { exchange, .. } = event {
            served = Some((exchange, 0, 0, false));
        }
        let node = driver.endpoint();
        let Some((exchange, got, sent, replied)) = &mut served else {
            continue;
        };
        while *got < size {
            let mut into = node.reserve(Class::Request, size - *got).unwrap();
            let n = node.read_body(*exchange, &mut into).unwrap();
            node.release(into);
            if n == 0 {
                break;
            }
            *got += n as u64;
        }
        if *got == size && node.body_complete(*exchange) && !*replied {
            node.reply(*exchange, &head, Some(size)).unwrap();
            *replied = true;
        }
        while *replied && *sent < size {
            match node
                .write_body(*exchange, &piece[..(size - *sent) as usize])
                .unwrap()
            {
                0 => break,
                took => *sent += took as u64,
            }
        }
        if *replied && *sent == size {
            node.end(*exchange);
            served = None;
        }
    }
}

/// One round from the asking side.
async fn ask(driver: &mut Drv, size: u64, piece: &[u8]) {
    let head = [7u8; HEAD];
    let progress = Progress::new(Duration::from_secs(30)).unwrap();
    let node = driver.endpoint();
    let id = node
        .open(Instant::now(), 2, Kind::Get, &head, Some(size), progress)
        .unwrap();
    let (mut written, mut read, mut answered) = (0u64, 0u64, false);
    loop {
        let node = driver.endpoint();
        while written < size {
            match node
                .write_body(id, &piece[..(size - written) as usize])
                .unwrap()
            {
                0 => break,
                took => written += took as u64,
            }
        }
        if answered {
            while read < size {
                let mut into = node.reserve(Class::Request, size - read).unwrap();
                let n = node.read_body(id, &mut into).unwrap();
                node.release(into);
                if n == 0 {
                    break;
                }
                read += n as u64;
            }
            if read == size && node.body_complete(id) {
                node.end(id);
                driver.flush();
                return;
            }
        }
        answered |= matches!(driver.event().await.unwrap(), Event::Reply { .. });
    }
}

fn hyper(rounds: usize, size: usize) -> Row {
    runtime().block_on(async move {
        let pair = Pair::new();
        let now = Instant::now();
        let book = || pair.book(Role::Node, Role::Node);
        let a = pair.node::<Mantle>(1, Role::Node, book(), limits(), 1 << 30, now);
        let b = pair.node::<Mantle>(2, Role::Node, book(), limits(), 1 << 30, now);
        let mut asking = Driver::bind(a, "127.0.0.1:0".parse().unwrap(), IO).unwrap();
        let answering = Driver::bind(b, "127.0.0.1:0".parse().unwrap(), IO).unwrap();
        let address = answering.local_addr().unwrap();
        let piece = vec![5u8; size.max(1)];
        let serving = tokio::spawn(answer(answering, size as u64, piece.clone()));
        asking.endpoint().connect(now, 2, address).unwrap();
        while !matches!(asking.event().await.unwrap(), Event::Connected { .. }) {}
        let mut times = Vec::with_capacity(rounds);
        let warm = rounds / 10;
        for _ in 0..warm {
            ask(&mut asking, size as u64, &piece).await;
        }
        let before = asking.stats();
        alloc::begin_process();
        for _ in 0..rounds {
            let began = Instant::now();
            ask(&mut asking, size as u64, &piece).await;
            times.push(began.elapsed());
        }
        let counts = alloc::end_process();
        let after = asking.stats();
        serving.abort();
        times.sort_unstable();
        Row {
            median: times[times.len() / 2],
            datagrams: (after.sent + after.received) - (before.sent + before.received),
            counts,
            rounds,
        }
    })
}

// ---------------------------------------------------------------------------------------------
// focal-wire's core over quinn.

fn focal(rounds: usize, size: usize) -> Row {
    use focal_wire::{
        FrameKind, TlsIdentity, WireLimits, client_tls, read_frame_header, read_payload_arriving,
        require_end, server_tls, write_frame,
    };
    runtime().block_on(async move {
        let pki = Pki::new();
        let (client_certificate, client_key) = pki.issue("node-1");
        let (server_certificate, server_key) = pki.issue("node-2");
        let limits = WireLimits::default();
        let server = server_tls(
            TlsIdentity::from_pkcs8(vec![server_certificate], server_key),
            vec![pki.root.clone()],
            &limits,
        )
        .unwrap();
        let client = client_tls(
            TlsIdentity::from_pkcs8(vec![client_certificate], client_key),
            vec![pki.root.clone()],
            &limits,
        )
        .unwrap();
        let server = quinn::Endpoint::server(server, "127.0.0.1:0".parse().unwrap()).unwrap();
        let address = server.local_addr().unwrap();
        let mut endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        endpoint.set_default_client_config(client);
        let max = limits.max_frame_bytes;
        let wait = limits.request_timeout;
        let serving = tokio::spawn(async move {
            let connection = server.accept().await.unwrap().await.unwrap();
            while let Ok((mut send, mut recv)) = connection.accept_bi().await {
                let rtt = connection.clone();
                tokio::spawn(async move {
                    let header = read_frame_header(&mut recv, FrameKind::Request, max)
                        .await
                        .unwrap();
                    let payload: Vec<u8> =
                        read_payload_arriving(&mut recv, header, wait, || rtt.rtt())
                            .await
                            .unwrap();
                    require_end(&mut recv).await.unwrap();
                    write_frame(&mut send, FrameKind::Response, &payload, max)
                        .await
                        .unwrap();
                    send.finish().unwrap();
                    let _ = send.stopped().await;
                });
            }
        });
        let connection = endpoint.connect(address, "node-2").unwrap().await.unwrap();
        let payload = vec![5u8; size];
        let mut times = Vec::with_capacity(rounds);
        let warm = rounds / 10;
        let round = async || {
            let (mut send, mut recv) = connection.open_bi().await.unwrap();
            send.set_priority(0).unwrap();
            write_frame(&mut send, FrameKind::Request, &payload, max)
                .await
                .unwrap();
            send.finish().unwrap();
            let header = read_frame_header(&mut recv, FrameKind::Response, max)
                .await
                .unwrap();
            let reply: Vec<u8> =
                read_payload_arriving(&mut recv, header, wait, || connection.rtt())
                    .await
                    .unwrap();
            require_end(&mut recv).await.unwrap();
            assert_eq!(reply.len(), size);
        };
        for _ in 0..warm {
            round().await;
        }
        let datagrams =
            |stats: quinn::ConnectionStats| stats.udp_tx.datagrams + stats.udp_rx.datagrams;
        let before = datagrams(connection.stats());
        alloc::begin_process();
        for _ in 0..rounds {
            let began = Instant::now();
            round().await;
            times.push(began.elapsed());
        }
        let counts = alloc::end_process();
        let after = datagrams(connection.stats());
        connection.close(0u8.into(), b"done");
        serving.abort();
        times.sort_unstable();
        Row {
            median: times[times.len() / 2],
            datagrams: after - before,
            counts,
            rounds,
        }
    })
}

fn main() {
    let mut arguments = std::env::args().skip(1);
    let which = arguments.next().unwrap_or_default();
    let size: usize = arguments
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or(4_096);
    let rounds: usize = arguments
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or(if size > 100_000 { 250 } else { 2_000 });
    let row = match which.as_str() {
        "focal" => focal(rounds, size),
        "hyper" => hyper(rounds, size),
        _ => panic!("usage: hyper-transport-compare <focal|hyper> <size> [rounds]"),
    };
    let per = |value: u64| value as f64 / row.rounds as f64;
    // One line a row: who, size, median µs, then per round: allocations, reallocations, bytes,
    // and the datagrams the asking side sent and received.
    println!(
        "{which} {size} {:.1} {:.2} {:.2} {:.0} {:.1}",
        row.median.as_secs_f64() * 1e6,
        per(row.counts.allocations),
        per(row.counts.reallocations),
        per(row.counts.bytes),
        per(row.datagrams),
    );
}
