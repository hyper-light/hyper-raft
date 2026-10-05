//! hyper-quic over real UDP sockets on loopback through a relay that holds every datagram for the
//! one-way delay each way (500 ms by default, the owner's condition): the overhead the stack adds
//! above that physical floor, measured (`docs/benchmarks.md`, "At 500 ms one way").
//!
//! Four threads, each the single owner of its state: the client, the server, and the relay's two
//! directions. They share nothing but sockets and one stop flag.
//!
//! - **Dials.** The client dials `--dials` times in turn. The first dial is fresh. Each later one
//!   resumes the session (its ticket and the server's address validation token are kept by the
//!   client's configuration and endpoint) and sends its request in 0-RTT data. Each dial's
//!   handshake and first reply are timed from its start.
//! - **Open loop.** On the last connection, kept, `--requests` requests are scheduled ahead at
//!   `--rate` a second. Each latency runs from its scheduled time to its reply's end (Tene's
//!   coordinated omission; `docs/tails.md` §3.1), each a 100-byte request answered by 100 bytes.
//!
//! Overhead is a latency less the round trip the relay imposes. The relay's own lateness, how long
//! after its due time it sent each datagram, is reported beside it, so the relay's cost is not
//! charged to the stack (`docs/tails.md` §4.2).
//!
//! `cargo run --release -p hyper-quic --example geo_open_loop -- --rate 200 --requests 20000`

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::disallowed_macros,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::too_many_lines,
    clippy::cognitive_complexity,
    missing_docs
)]
// A benchmark on real sockets reads the host clock: the time it measures is the host's, as the
// end-to-end tests' is (`tests/e2e.rs`)
#![allow(clippy::disallowed_methods)]

use std::collections::VecDeque;
use std::io::ErrorKind;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use bytes::BytesMut;
use hyper_quic::rustls::RootCertStore;
use hyper_quic::rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use hyper_quic::{
    ClientConfig, Connection, ConnectionHandle, DatagramEvent, Dir, Endpoint, EndpointConfig,
    Event, ServerConfig, StreamEvent, StreamId, TransportConfig, VarInt,
};

const REQUEST: [u8; 100] = [0x51; 100];
const REPLY: [u8; 100] = [0x52; 100];
/// The largest UDP payload (RFC 9000 §18.2's `max_udp_payload_size` ceiling): the receive buffer.
const MAX_DATAGRAM: usize = 65_527;
/// The longest a thread blocks on its socket with nothing due: a bound on one turn of a loop, so
/// the stop flag is seen; any datagram or deadline ends the wait first.
const IDLE_TURN: Duration = Duration::from_millis(50);
/// The coverage of each quantile's interval (`docs/tails.md` §3.3).
const COVERAGE: f64 = 0.95;

struct Args {
    one_way: Duration,
    rate: u64,
    requests: usize,
    dials: usize,
}

fn args() -> Args {
    let mut parsed = Args {
        one_way: Duration::from_millis(500),
        rate: 200,
        requests: 20_000,
        dials: 5,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let value = it.next().unwrap_or_else(|| panic!("{flag} takes a value"));
        match flag.as_str() {
            "--one-way-ms" => parsed.one_way = Duration::from_millis(value.parse().unwrap()),
            "--rate" => parsed.rate = value.parse().unwrap(),
            "--requests" => parsed.requests = value.parse().unwrap(),
            "--dials" => parsed.dials = value.parse().unwrap(),
            _ => panic!("unknown flag {flag}"),
        }
    }
    parsed
}

/// Streams open at once: four times what the open loop keeps in flight, its rate times the round
/// trip. A stream's credit returns only once the server has seen it closed and its MAX_STREAMS has
/// crossed back, half a round trip more, so the limit never paces the offered load.
fn transport(args: &Args) -> TransportConfig {
    let in_flight = args.rate * 2 * args.one_way.as_millis() as u64 / 1_000;
    let mut transport = TransportConfig::default();
    transport.max_concurrent_bidi_streams(VarInt::from_u64(4 * in_flight.max(100)).unwrap());
    transport
}

fn bind() -> UdpSocket {
    UdpSocket::bind("127.0.0.1:0").unwrap()
}

/// Waits on `socket` until `deadline` or `IDLE_TURN`, whichever is first, then takes one datagram
/// if one arrived: the wait never takes it (`hyper_measure::wait::arrives`).
fn recv_until(socket: &UdpSocket, deadline: Option<Instant>, buf: &mut [u8]) -> Option<usize> {
    let wait = deadline
        .map_or(IDLE_TURN, |d| d.saturating_duration_since(Instant::now()))
        .clamp(Duration::from_micros(1), IDLE_TURN);
    if !hyper_measure::wait::arrives(socket, Some(wait), buf).unwrap() {
        return None;
    }
    match socket.recv_from(buf) {
        Ok((n, _)) => Some(n),
        Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => None,
        Err(e) => panic!("recv: {e}"),
    }
}

/// One direction of the relay: what arrives on `from` goes out on `to`, to `peer`, `one_way`
/// after it arrived. Returns the lateness of each send past its due time, in nanoseconds.
fn relay(
    from: &UdpSocket,
    to: &UdpSocket,
    peer: SocketAddr,
    one_way: Duration,
    stop: &AtomicBool,
) -> Vec<u64> {
    // Every datagram in flight one way at the bench's rate fits many times over: a refusal at the
    // bound is a dropped datagram, counted
    const HELD: usize = 1 << 16;
    let mut held: VecDeque<(Instant, Vec<u8>)> = VecDeque::new();
    let mut lateness = Vec::new();
    let mut buf = vec![0u8; MAX_DATAGRAM];
    let mut refused = 0u64;
    while !stop.load(Ordering::Relaxed) {
        if let Some(n) = recv_until(from, held.front().map(|(due, _)| *due), &mut buf) {
            if held.len() < HELD {
                held.push_back((Instant::now() + one_way, buf[..n].to_vec()));
            } else {
                refused += 1;
            }
        }
        let now = Instant::now();
        while held.front().is_some_and(|(due, _)| *due <= now) {
            let (due, bytes) = held.pop_front().unwrap();
            to.send_to(&bytes, peer).unwrap();
            lateness.push(Instant::now().saturating_duration_since(due).as_nanos() as u64);
        }
    }
    assert_eq!(refused, 0, "the relay's queue refused datagrams");
    lateness
}

fn pki() -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let key = PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der());
    (cert.cert.into(), key.into())
}

/// Sends everything `connection` has to send.
fn flush(
    connection: &mut Connection,
    endpoint: &Endpoint,
    socket: &UdpSocket,
    to: SocketAddr,
    buf: &mut Vec<u8>,
) {
    loop {
        buf.clear();
        let Some(transmit) = connection.poll_transmit(Instant::now(), 1, buf, endpoint.configs())
        else {
            break;
        };
        socket.send_to(&buf[..transmit.size], to).unwrap();
    }
}

fn read_all(connection: &mut Connection, id: StreamId) -> (usize, bool) {
    let mut stream = connection.recv_stream(id);
    let Ok(mut chunks) = stream.read(true) else {
        return (0, true);
    };
    let mut n = 0;
    let mut finished = false;
    loop {
        match chunks.next(usize::MAX) {
            Ok(Some(chunk)) => n += chunk.bytes.len(),
            Ok(None) => {
                finished = true;
                break;
            }
            Err(_) => break,
        }
    }
    let _ = chunks.finalize();
    (n, finished)
}

/// A connection the server serves, with each open request stream and the bytes it has read of it
type Served = (ConnectionHandle, Connection, Vec<(StreamId, usize)>);

/// The server: answers every request on every connection it accepts, until stopped.
fn server(socket: &UdpSocket, config: ServerConfig, relay: SocketAddr, stop: &AtomicBool) {
    let mut endpoint = Endpoint::new(EndpointConfig::default(), Some(config), false, None).unwrap();
    let mut connections: Vec<Served> = Vec::new();
    let mut buf = vec![0u8; MAX_DATAGRAM];
    let mut out = Vec::with_capacity(MAX_DATAGRAM);
    while !stop.load(Ordering::Relaxed) {
        let deadline = connections
            .iter_mut()
            .filter_map(|(_, c, _)| c.poll_timeout())
            .min();
        if let Some(n) = recv_until(socket, deadline, &mut buf) {
            out.clear();
            match endpoint.handle(
                Instant::now(),
                relay,
                None,
                None,
                BytesMut::from(&buf[..n]),
                &mut out,
            ) {
                Some(DatagramEvent::NewConnection(incoming)) => {
                    out.clear();
                    let (ch, conn) = endpoint
                        .accept(incoming, Instant::now(), &mut out, None, None)
                        .unwrap();
                    connections.push((ch, conn, Vec::new()));
                }
                Some(DatagramEvent::ConnectionEvent(ch, event)) => {
                    if let Some((_, conn, _)) = connections.iter_mut().find(|(h, _, _)| *h == ch) {
                        conn.handle_event(event, endpoint.configs_mut());
                    }
                }
                Some(DatagramEvent::Response(transmit)) => {
                    socket.send_to(&out[..transmit.size], relay).unwrap();
                }
                None => {}
            }
        }
        let now = Instant::now();
        for (ch, conn, streams) in &mut connections {
            if conn.poll_timeout().is_some_and(|t| t <= now) {
                conn.handle_timeout(now);
            }
            while let Some(event) = conn.poll_endpoint_events() {
                if let Some(event) = endpoint.handle_event(*ch, event) {
                    conn.handle_event(event, endpoint.configs_mut());
                }
            }
            while let Some(event) = conn.poll() {
                if let Event::Stream(StreamEvent::Opened { dir: Dir::Bi }) = event {
                    while let Some(id) = conn.streams().accept(Dir::Bi) {
                        streams.push((id, 0));
                    }
                }
            }
            streams.retain_mut(|(id, got)| {
                let (n, finished) = read_all(conn, *id);
                *got += n;
                if finished && *got == REQUEST.len() {
                    let mut send = conn.send_stream(*id);
                    assert_eq!(send.write(&REPLY).unwrap(), REPLY.len());
                    send.finish().unwrap();
                    return false;
                }
                true
            });
            flush(conn, &endpoint, socket, relay, &mut out);
        }
        connections.retain(|(_, conn, _)| !conn.is_drained());
    }
}

/// What the client measured.
struct Client {
    /// Each dial's handshake and first reply, from its start.
    dials: Vec<(Duration, Duration, bool)>,
    /// Each open-loop request's latency from its scheduled time, in nanoseconds.
    latencies: Vec<u64>,
}

struct Pending {
    id: StreamId,
    scheduled: Instant,
    got: usize,
}

fn client(
    socket: &UdpSocket,
    config: ClientConfig,
    relay: SocketAddr,
    args: &Args,
    stop: &AtomicBool,
) -> Client {
    let mut endpoint = Endpoint::new(EndpointConfig::default(), None, false, None).unwrap();
    let handle = endpoint.insert_client_config(config).unwrap();
    let mut buf = vec![0u8; MAX_DATAGRAM];
    let mut out = Vec::with_capacity(MAX_DATAGRAM);
    let mut measured = Client {
        dials: Vec::new(),
        latencies: Vec::with_capacity(args.requests),
    };
    let mut kept: Option<(ConnectionHandle, Connection)> = None;
    for dial in 0..args.dials {
        if let Some((_, mut old)) = kept.take() {
            old.close(Instant::now(), 0u32.into(), bytes::Bytes::new());
            flush(&mut old, &endpoint, socket, relay, &mut out);
        }
        let start = Instant::now();
        let (ch, mut conn) = endpoint
            .connect(start, handle, relay, "localhost", None)
            .unwrap();
        let mut connected = None;
        let mut request: Option<StreamId> = None;
        let mut got = 0;
        let replied = loop {
            let now = Instant::now();
            if conn.poll_timeout().is_some_and(|t| t <= now) {
                conn.handle_timeout(now);
            }
            while let Some(event) = conn.poll_endpoint_events() {
                if let Some(event) = endpoint.handle_event(ch, event) {
                    conn.handle_event(event, endpoint.configs_mut());
                }
            }
            while let Some(event) = conn.poll() {
                match event {
                    Event::Connected => connected = Some(now - start),
                    Event::ConnectionLost { reason } => panic!("dial {dial}: {reason}"),
                    _ => {}
                }
            }
            if request.is_none() && (connected.is_some() || conn.has_0rtt()) {
                let id = conn.streams().open(Dir::Bi).unwrap();
                let mut send = conn.send_stream(id);
                assert_eq!(send.write(&REQUEST).unwrap(), REQUEST.len());
                send.finish().unwrap();
                request = Some(id);
            }
            if let Some(id) = request {
                got += read_all(&mut conn, id).0;
                if got == REPLY.len() && connected.is_some() {
                    break now - start;
                }
            }
            flush(&mut conn, &endpoint, socket, relay, &mut out);
            if let Some(n) = recv_until(socket, conn.poll_timeout(), &mut buf) {
                out.clear();
                if let Some(DatagramEvent::ConnectionEvent(h, event)) = endpoint.handle(
                    Instant::now(),
                    relay,
                    None,
                    None,
                    BytesMut::from(&buf[..n]),
                    &mut out,
                ) && h == ch
                {
                    conn.handle_event(event, endpoint.configs_mut());
                }
            }
        };
        measured
            .dials
            .push((connected.unwrap(), replied, conn.accepted_0rtt()));
        kept = Some((ch, conn));
    }

    // The open loop on the kept connection
    let (ch, mut conn) = kept.take().unwrap();
    let interval = Duration::from_nanos(1_000_000_000 / args.rate);
    let start = Instant::now();
    let mut issued = 0usize;
    let mut pending: Vec<Pending> = Vec::new();
    while measured.latencies.len() < args.requests {
        let now = Instant::now();
        while issued < args.requests && start + interval * issued as u32 <= now {
            let id = conn
                .streams()
                .open(Dir::Bi)
                .expect("the stream limit paced the load");
            let mut send = conn.send_stream(id);
            assert_eq!(send.write(&REQUEST).unwrap(), REQUEST.len());
            send.finish().unwrap();
            pending.push(Pending {
                id,
                scheduled: start + interval * issued as u32,
                got: 0,
            });
            issued += 1;
        }
        if conn.poll_timeout().is_some_and(|t| t <= now) {
            conn.handle_timeout(now);
        }
        while let Some(event) = conn.poll_endpoint_events() {
            if let Some(event) = endpoint.handle_event(ch, event) {
                conn.handle_event(event, endpoint.configs_mut());
            }
        }
        while let Some(event) = conn.poll() {
            if let Event::ConnectionLost { reason } = event {
                panic!("the kept connection: {reason}");
            }
        }
        let done = Instant::now();
        pending.retain_mut(|p| {
            p.got += read_all(&mut conn, p.id).0;
            if p.got == REPLY.len() {
                measured
                    .latencies
                    .push(done.saturating_duration_since(p.scheduled).as_nanos() as u64);
                return false;
            }
            true
        });
        flush(&mut conn, &endpoint, socket, relay, &mut out);
        let next_issue = (issued < args.requests).then(|| start + interval * issued as u32);
        let deadline = [conn.poll_timeout(), next_issue]
            .into_iter()
            .flatten()
            .min();
        if let Some(n) = recv_until(socket, deadline, &mut buf) {
            out.clear();
            if let Some(DatagramEvent::ConnectionEvent(h, event)) = endpoint.handle(
                Instant::now(),
                relay,
                None,
                None,
                BytesMut::from(&buf[..n]),
                &mut out,
            ) && h == ch
            {
                conn.handle_event(event, endpoint.configs_mut());
            }
        }
    }
    conn.close(Instant::now(), 0u32.into(), bytes::Bytes::new());
    flush(&mut conn, &endpoint, socket, relay, &mut out);
    stop.store(true, Ordering::Relaxed);
    measured
}

/// `ln C(n, k)`, by the log-gamma of Stirling's series (Abramowitz and Stegun 6.1.41)
fn ln_choose(n: f64, k: f64) -> f64 {
    fn ln_gamma(x: f64) -> f64 {
        // ln Γ(x) for x ≥ 1 by shifting to x ≥ 8 then Stirling's series
        let mut x = x;
        let mut shift = 0.0;
        while x < 8.0 {
            shift -= x.ln();
            x += 1.0;
        }
        let inv = 1.0 / x;
        let inv2 = inv * inv;
        shift + (x - 0.5) * x.ln() - x
            + 0.5 * (2.0 * std::f64::consts::PI).ln()
            + inv * (1.0 / 12.0 - inv2 * (1.0 / 360.0 - inv2 / 1260.0))
    }
    ln_gamma(n + 1.0) - ln_gamma(k + 1.0) - ln_gamma(n - k + 1.0)
}

/// The order statistics (1-based ranks) that bracket the `q`-quantile of `n` samples with at least
/// `COVERAGE`: the number of samples below the quantile is Binomial(n, q) (David and Nagaraja,
/// *Order Statistics*, §7.1). `None` when the upper rank passes `n`: too few samples.
fn interval(n: usize, q: f64) -> Option<(usize, usize)> {
    let pmf = |k: usize| {
        (ln_choose(n as f64, k as f64) + k as f64 * q.ln() + (n - k) as f64 * (1.0 - q).ln()).exp()
    };
    let centre = ((n as f64) * q).floor() as usize;
    let (mut lo, mut hi) = (centre, centre);
    let mut mass = pmf(centre);
    while mass < COVERAGE {
        let below = if lo > 0 { pmf(lo - 1) } else { 0.0 };
        let above = if hi < n { pmf(hi + 1) } else { 0.0 };
        if below == 0.0 && above == 0.0 {
            break;
        }
        if below >= above {
            lo -= 1;
            mass += below;
        } else {
            hi += 1;
            mass += above;
        }
    }
    // Ranks lo+1 ..= hi+1 bracket it: P(X_(lo+1) ≤ ξ < X_(hi+1)) = P(lo+1 ≤ B ≤ hi)
    (hi + 1 < n).then_some((lo + 1, hi + 1))
}

fn ms(ns: u64) -> f64 {
    ns as f64 / 1e6
}

fn report(label: &str, samples: &mut [u64], less: u64) {
    samples.sort_unstable();
    let n = samples.len();
    let max = samples.last().copied().unwrap_or(0);
    print!("{label}: n={n}");
    for (name, q) in [
        ("p50", 0.5),
        ("p99", 0.99),
        ("p99.9", 0.999),
        ("p99.99", 0.9999),
    ] {
        let at = samples[((n as f64 * q).ceil() as usize).clamp(1, n) - 1];
        match interval(n, q) {
            Some((lo, hi)) => print!(
                " {name}={:.3} [{:.3}, {:.3}]",
                ms(at.saturating_sub(less)),
                ms(samples[lo - 1].saturating_sub(less)),
                ms(samples[hi - 1].saturating_sub(less))
            ),
            None => print!(" {name}=unresolved"),
        }
    }
    println!(" max={:.3} ms", ms(max.saturating_sub(less)));
}

fn main() {
    let args = args();
    let (cert, key) = pki();
    let mut server_config = ServerConfig::with_single_cert(vec![cert.clone()], key).unwrap();
    server_config.transport_config(transport(&args));
    let mut roots = RootCertStore::empty();
    roots.add(cert).unwrap();
    let mut client_config = ClientConfig::with_root_certificates(roots).unwrap();
    client_config.transport_config(transport(&args));

    let (client_socket, server_socket) = (bind(), bind());
    let (relay_client_side, relay_server_side) = (bind(), bind());
    let relay_to_client = relay_client_side.try_clone().unwrap();
    let relay_to_server = relay_server_side.try_clone().unwrap();
    let (client_addr, server_addr) = (
        client_socket.local_addr().unwrap(),
        server_socket.local_addr().unwrap(),
    );
    let (relay_for_client, relay_for_server) = (
        relay_client_side.local_addr().unwrap(),
        relay_server_side.local_addr().unwrap(),
    );
    let stop = AtomicBool::new(false);
    // The client sets it as it ends, a panic included, so every thread ends
    struct StopOnDrop<'a>(&'a AtomicBool);
    impl Drop for StopOnDrop<'_> {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
        }
    }
    let (measured, mut lateness_up, mut lateness_down) = std::thread::scope(|s| {
        let up = s.spawn(|| {
            relay(
                &relay_client_side,
                &relay_to_server,
                server_addr,
                args.one_way,
                &stop,
            )
        });
        let down = s.spawn(|| {
            relay(
                &relay_server_side,
                &relay_to_client,
                client_addr,
                args.one_way,
                &stop,
            )
        });
        s.spawn(|| server(&server_socket, server_config, relay_for_server, &stop));
        let _stop = StopOnDrop(&stop);
        let measured = client(
            &client_socket,
            client_config,
            relay_for_client,
            &args,
            &stop,
        );
        (measured, up.join().unwrap(), down.join().unwrap())
    });

    let rtt = 2 * args.one_way.as_nanos() as u64;
    println!(
        "one way {} ms (round trip {} ms), rate {}/s, {} requests",
        args.one_way.as_millis(),
        rtt / 1_000_000,
        args.rate,
        args.requests
    );
    for (i, (connected, replied, zero_rtt)) in measured.dials.iter().enumerate() {
        let floor_rtts: u64 = if *zero_rtt { 1 } else { 2 };
        println!(
            "dial {}: handshake {:.3} ms (over 1 RTT {:.3}), first reply {:.3} ms (floor {} RTT, over {:.3}), 0-RTT {}",
            i + 1,
            connected.as_secs_f64() * 1e3,
            ms((connected.as_nanos() as u64).saturating_sub(rtt)),
            replied.as_secs_f64() * 1e3,
            floor_rtts,
            ms((replied.as_nanos() as u64).saturating_sub(floor_rtts * rtt)),
            zero_rtt
        );
    }
    let mut latencies = measured.latencies;
    // The worst latency over one RTT in each tenth of the run, in completion order: where in the
    // run the tail sits
    let tenth = latencies.len().div_ceil(10).max(1);
    let worst: Vec<String> = latencies
        .chunks(tenth)
        .map(|c| format!("{:.1}", ms(c.iter().max().unwrap().saturating_sub(rtt))))
        .collect();
    println!(
        "worst over one RTT by tenth of the run (ms): {}",
        worst.join(" ")
    );
    report("open loop, over one RTT (ms)", &mut latencies, rtt);
    report("relay lateness up (ms)", &mut lateness_up, 0);
    report("relay lateness down (ms)", &mut lateness_down, 0);
}
