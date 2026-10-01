//! hyper-transport against focal-wire's core on one workload (docs/benchmarks.md,
//! "hyper-transport").
//!
//! Two endpoints in one process over two UDP sockets on loopback, one thread. A round is one
//! exchange: a request of `size` bytes and a reply of `size` bytes, the next round once the reply
//! is whole. Each row reports the median round's wall time over its rounds, and the allocations,
//! reallocations and bytes the whole process made per round.
//!
//! - **focal-wire**: its own transport configuration (`quic_transport` through `server_tls` and
//!   `client_tls`), its frame codec (`write_frame`, `read_frame_header`, `read_payload_arriving`,
//!   `require_end`) and its exchange shape (one bidirectional stream, priority set, the request
//!   written and finished, the reply's header then its payload), over quinn on a current-thread
//!   tokio runtime. The domain envelope and the registry are left out: the payload is the bytes.
//! - **hyper-transport**: one exchange through `open`, `write_body`, `reply`, `read_body`, with a
//!   16-byte head each way and the body of `size` bytes, both endpoints driven by one loop that
//!   polls the two non-blocking sockets.
//!
//! `cargo run --release` from this directory; the arguments are the rounds per row (default
//! 2,000) and the sizes, in bytes (default 64, 4096, 65536, 524288).

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

use std::io::ErrorKind;
use std::net::UdpSocket;
use std::time::{Duration, Instant};

use common::*;
use hyper_measure::alloc;
use hyper_transport::{Event, Progress};

#[global_allocator]
static ALLOCATOR: alloc::Counting = alloc::Counting;

/// The head each side sends with its body: a request's or a reply's header, as in the
/// allocation bench.
const HEAD: usize = 16;

struct Row {
    who: &'static str,
    size: usize,
    median: Duration,
    counts: alloc::Counts,
    rounds: usize,
}

// ---------------------------------------------------------------------------------------------
// hyper-transport over two sockets.

struct Sockets {
    a: UdpSocket,
    b: UdpSocket,
    buffer: Vec<u8>,
    out: Vec<u8>,
}

impl Sockets {
    fn turn<A: Drive, B: Drive>(&mut self, a: &mut A, b: &mut B) {
        let now = Instant::now();
        while let Some(transmit) = a.transmit(now, &mut self.out) {
            let _ = self.a.send_to(&self.out[..transmit.size], transmit.destination);
        }
        while let Some(transmit) = b.transmit(now, &mut self.out) {
            let _ = self.b.send_to(&self.out[..transmit.size], transmit.destination);
        }
        for _ in 0..1_024 {
            match self.a.recv_from(&mut self.buffer) {
                Ok((length, from)) => a.datagram(Instant::now(), from, &self.buffer[..length]),
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(_) => {}
            }
        }
        for _ in 0..1_024 {
            match self.b.recv_from(&mut self.buffer) {
                Ok((length, from)) => b.datagram(Instant::now(), from, &self.buffer[..length]),
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(_) => {}
            }
        }
        let now = Instant::now();
        if a.timeout().is_some_and(|due| due <= now) {
            a.fire(now);
        }
        if b.timeout().is_some_and(|due| due <= now) {
            b.fire(now);
        }
    }
}

fn ours(rounds: usize, size: usize) -> Row {
    let pair = Pair::new();
    let now = Instant::now();
    let book = || pair.book(Role::Node, Role::Node);
    let mut a = pair.node::<Mantle>(1, Role::Node, book(), limits(), 1 << 30, now);
    let mut b = pair.node::<Mantle>(2, Role::Node, book(), limits(), 1 << 30, now);
    let sockets = Sockets {
        a: UdpSocket::bind("127.0.0.1:0").unwrap(),
        b: UdpSocket::bind("127.0.0.1:0").unwrap(),
        buffer: vec![0; 65_536],
        out: Vec::with_capacity(65_536),
    };
    sockets.a.set_nonblocking(true).unwrap();
    sockets.b.set_nonblocking(true).unwrap();
    let mut sockets = sockets;
    a.connect(now, 2, sockets.b.local_addr().unwrap()).unwrap();
    let mut connected = false;
    while !connected {
        sockets.turn(&mut a, &mut b);
        while let Some(event) = a.poll_event() {
            connected |= matches!(event, Event::Connected { .. });
        }
        while b.poll_event().is_some() {}
    }
    let piece = vec![5u8; size.max(1)];
    let mut one = || exchange(&mut sockets, &mut a, &mut b, size as u64, &piece);
    measure("hyper-transport", rounds, size, &mut one)
}

fn exchange(sockets: &mut Sockets, a: &mut Node<Mantle>, b: &mut Node<Mantle>, size: u64, piece: &[u8]) {
    let head = [7u8; HEAD];
    let progress = Progress::new(Duration::from_secs(30)).unwrap();
    let id = a.open(Instant::now(), 2, Kind::Get, &head, Some(size), progress).unwrap();
    let (mut written, mut read, mut answered) = (0u64, 0u64, false);
    let mut served: Option<(hyper_transport::ExchangeId, u64, u64, bool)> = None;
    loop {
        while written < size {
            match a.write_body(id, &piece[..(size - written) as usize]).unwrap() {
                0 => break,
                took => written += took as u64,
            }
        }
        sockets.turn(a, b);
        while let Some(event) = b.poll_event() {
            if let Event::Request { exchange, .. } = event {
                served = Some((exchange, 0, 0, false));
            }
        }
        if let Some((exchange, got, sent, replied)) = &mut served {
            while *got < size {
                let mut into = b.reserve(Class::Request, size - *got).unwrap();
                let n = b.read_body(*exchange, &mut into).unwrap();
                b.release(into);
                if n == 0 {
                    break;
                }
                *got += n as u64;
            }
            if *got == size && b.body_complete(*exchange) && !*replied {
                b.reply(*exchange, &head, Some(size)).unwrap();
                *replied = true;
            }
            while *replied && *sent < size {
                match b.write_body(*exchange, &piece[..(size - *sent) as usize]).unwrap() {
                    0 => break,
                    took => *sent += took as u64,
                }
            }
            if *replied && *sent == size {
                b.end(*exchange);
                served = None;
            }
        }
        while let Some(event) = a.poll_event() {
            answered |= matches!(event, Event::Reply { .. });
        }
        if answered {
            while read < size {
                let mut into = a.reserve(Class::Request, size - read).unwrap();
                let n = a.read_body(id, &mut into).unwrap();
                a.release(into);
                if n == 0 {
                    break;
                }
                read += n as u64;
            }
            if read == size && a.body_complete(id) {
                a.end(id);
                if served.is_none() {
                    return;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// focal-wire's core over quinn and tokio.

fn focal(rounds: usize, size: usize) -> Row {
    use focal_wire::{
        FrameKind, TlsIdentity, WireLimits, client_tls, read_frame_header, read_payload_arriving,
        require_end, server_tls, write_frame,
    };
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async move {
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
                    let header = read_frame_header(&mut recv, FrameKind::Request, max).await.unwrap();
                    let payload: Vec<u8> = read_payload_arriving(&mut recv, header, wait, || rtt.rtt()).await.unwrap();
                    require_end(&mut recv).await.unwrap();
                    write_frame(&mut send, FrameKind::Response, &payload, max).await.unwrap();
                    send.finish().unwrap();
                    let _ = send.stopped().await;
                });
            }
        });
        let connection = endpoint.connect(address, "node-2").unwrap().await.unwrap();
        let payload = vec![5u8; size];
        let mut times = Vec::with_capacity(rounds);
        let warm = rounds / 10;
        let mut counts = None;
        for round in 0..rounds + warm {
            if round == warm {
                alloc::begin();
            }
            let began = Instant::now();
            let (mut send, mut recv) = connection.open_bi().await.unwrap();
            send.set_priority(0).unwrap();
            write_frame(&mut send, FrameKind::Request, &payload, max).await.unwrap();
            send.finish().unwrap();
            let header = read_frame_header(&mut recv, FrameKind::Response, max).await.unwrap();
            let reply: Vec<u8> = read_payload_arriving(&mut recv, header, wait, || connection.rtt()).await.unwrap();
            require_end(&mut recv).await.unwrap();
            assert_eq!(reply.len(), size);
            if round >= warm {
                times.push(began.elapsed());
            }
        }
        counts.get_or_insert(alloc::end());
        connection.close(0u8.into(), b"done");
        serving.abort();
        times.sort_unstable();
        Row {
            who: "focal-wire",
            size,
            median: times[times.len() / 2],
            counts: counts.unwrap(),
            rounds,
        }
    })
}

fn measure(who: &'static str, rounds: usize, size: usize, one: &mut dyn FnMut()) -> Row {
    for _ in 0..rounds / 10 {
        one();
    }
    let mut times = Vec::with_capacity(rounds);
    alloc::begin();
    for _ in 0..rounds {
        let began = Instant::now();
        one();
        times.push(began.elapsed());
    }
    let counts = alloc::end();
    times.sort_unstable();
    Row {
        who,
        size,
        median: times[times.len() / 2],
        counts,
        rounds,
    }
}

fn main() {
    let mut arguments = std::env::args().skip(1);
    let rounds: usize = arguments.next().and_then(|value| value.parse().ok()).unwrap_or(2_000);
    let mut sizes: Vec<usize> = arguments.filter_map(|value| value.parse().ok()).collect();
    if sizes.is_empty() {
        sizes = vec![64, 4_096, 65_536, 524_288];
    }
    println!("| Implementation | Size each way | Median round | Allocations | Reallocations | Bytes allocated |");
    println!("|---|---|---|---|---|---|");
    for size in sizes {
        let rounds = if size > 100_000 { rounds / 8 } else { rounds };
        for row in [focal(rounds, size), ours(rounds, size)] {
            let per = |value: u64| value as f64 / row.rounds as f64;
            println!(
                "| {} | {} B | {:.1} µs | {:.1} | {:.1} | {:.0} |",
                row.who,
                row.size,
                row.median.as_secs_f64() * 1e6,
                per(row.counts.allocations),
                per(row.counts.reallocations),
                per(row.counts.bytes),
            );
        }
    }
}
