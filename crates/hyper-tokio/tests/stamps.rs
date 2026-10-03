//! A plane socket's arrivals: each on the host's monotonic clock, the kernel's receive stamp on
//! Linux and macOS, so a datagram read late is still stamped when it arrived.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::disallowed_macros,
    clippy::cast_possible_truncation,
    missing_docs
)]

use std::net::{SocketAddr, UdpSocket};

use hyper_datagram::{AdmitAll, ExporterSecret, Plane, PlaneLimits, Role, SECRET_BYTES};
use hyper_tokio::{Arrival, Clock, Io, PlaneSocket, Stamped, Taken};

/// The datagrams the test sends.
const DATAGRAMS: u64 = 16;
/// How long a datagram waits in the socket before it is read: 20 ms, two hundred times the
/// 100 µs loopback delay the traces measured (`docs/benchmarks.md`, "Heartbeat traces"), so a stamp
/// taken at the read could not pass for the kernel's.
const HELD_NS: u64 = 20_000_000;

const LIMITS: PlaneLimits = PlaneLimits {
    max_peers: 2,
    epochs_per_peer: 2,
    window_limit: 256,
};

fn planes() -> (Plane, Plane) {
    let secret = ExporterSecret::new([7; SECRET_BYTES]);
    let mut sender = Plane::new(1, LIMITS).unwrap();
    let mut receiver = Plane::new(2, LIMITS).unwrap();
    sender
        .install_epoch(2, 1, &secret, Role::Initiator)
        .unwrap();
    receiver
        .install_epoch(1, 1, &secret, Role::Acceptor)
        .unwrap();
    (sender, receiver)
}

fn send(plane: &mut Plane, socket: &UdpSocket, to: SocketAddr, body: u64) {
    plane.queue(2, &body.to_le_bytes()).unwrap();
    plane.flush(|_, sealed| {
        socket.send_to(sealed.unwrap(), to).unwrap();
    });
}

#[test]
fn a_datagram_is_stamped_when_it_arrived_not_when_it_was_read() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let (mut sender, mut receiver) = planes();
        let mut socket =
            PlaneSocket::bind("127.0.0.1:0".parse().unwrap(), Io { batch: 8 }).unwrap();
        let kernel = cfg!(any(target_os = "linux", target_os = "macos"));
        assert_eq!(socket.stats().kernel_stamps, kernel);
        let to = socket.local_addr().unwrap();
        let out = UdpSocket::bind("127.0.0.1:0").unwrap();
        let clock = Clock::new().unwrap();
        // Linux turns receive stamping on through a static key it flips from a work queue
        // (`net_enable_timestamp`, net/core/dev.c), so a datagram that arrives before the work ran
        // is stamped when it is read (`__sock_recv_timestamp`): late, never early. One datagram
        // through first, as a socket in use has had.
        send(&mut sender, &out, to, u64::MAX);
        let mut warmed = false;
        while !warmed {
            socket
                .receive(&mut receiver, &AdmitAll, |_, _| warmed = true)
                .await
                .unwrap();
        }
        let mut previous = 0;
        for body in 0..DATAGRAMS {
            let before = clock.now_ns();
            send(&mut sender, &out, to, body);
            // Held in the socket: the reader is busy for `HELD_NS`.
            while clock.now_ns() < before + HELD_NS {
                std::hint::spin_loop();
            }
            let read_from = clock.now_ns();
            let mut got: Option<(Arrival, Vec<u8>)> = None;
            while got.is_none() {
                socket
                    .receive(&mut receiver, &AdmitAll, |arrival, opened| {
                        let opened = opened.unwrap();
                        got = Some((arrival, opened.messages().next().unwrap().to_vec()));
                    })
                    .await
                    .unwrap();
            }
            let (arrival, message) = got.unwrap();
            assert_eq!(message, body.to_le_bytes());
            assert_eq!(arrival.from, out.local_addr().unwrap());
            assert_eq!(arrival.kernel, kernel);
            assert!(arrival.at_ns >= before, "stamped before it was sent");
            assert!(arrival.at_ns <= clock.now_ns(), "stamped after it was read");
            assert!(arrival.at_ns >= previous, "stamps go back");
            if kernel {
                assert!(
                    arrival.at_ns < read_from,
                    "the kernel's stamp, {} ns after the send, is the read's ({} ns)",
                    arrival.at_ns - before,
                    read_from - before
                );
            } else {
                assert!(arrival.at_ns >= read_from, "stamped when read");
            }
            previous = arrival.at_ns;
        }
    });
}

/// Blocks until a datagram is queued on `socket`: a peek, which takes nothing.
fn until_queued(socket: &UdpSocket) {
    socket.set_nonblocking(false).unwrap();
    socket.peek_from(&mut [0u8; 1]).unwrap();
    socket.set_nonblocking(true).unwrap();
}

/// What a held datagram's read found: its arrival, and the clock before the send, once it was
/// queued and when the read began.
struct Held {
    arrival: Arrival,
    before: u64,
    in_socket: u64,
    read_from: u64,
}

/// Sends `body` to `to`, holds the reader `HELD_NS` once it is queued, and reads it.
fn held_then_read(stamped: &mut Stamped, socket: &UdpSocket, out: &UdpSocket, body: u8) -> Held {
    let mut buffer = [0u8; 64];
    let before = stamped.clock().now_ns();
    out.send_to(&[body], socket.local_addr().unwrap()).unwrap();
    until_queued(socket);
    let in_socket = stamped.clock().now_ns();
    #[allow(
        clippy::disallowed_methods,
        reason = "the reader stopped while the datagram waits in its socket"
    )]
    std::thread::sleep(std::time::Duration::from_nanos(HELD_NS));
    let read_from = stamped.clock().now_ns();
    let Some(Taken::Datagram(length, arrival)) = stamped.receive(socket, &mut buffer).unwrap()
    else {
        panic!("the datagram is queued");
    };
    assert_eq!(&buffer[..length], &[body]);
    Held {
        arrival,
        before,
        in_socket,
        read_from,
    }
}

/// A standard socket its owner drives itself, without tokio (the E2E harnesses' members): a
/// datagram held in the socket while its reader is stopped is stamped when it arrived where the
/// kernel stamps, so an echo of it states a hold that covers the stop; elsewhere when it was read.
/// The stop here is the reader asleep for `HELD_NS` once the datagram is queued, which a blocking
/// peek says.
#[test]
fn a_standard_sockets_datagram_is_stamped_when_it_arrived_not_when_it_was_read() {
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    let mut stamped = Stamped::new(&socket).unwrap();
    let kernel = cfg!(any(target_os = "linux", target_os = "macos"));
    assert_eq!(stamped.kernel(), kernel);
    let out = UdpSocket::bind("127.0.0.1:0").unwrap();
    // One datagram through first, as the plane socket's test sends (Linux's stamping key).
    held_then_read(&mut stamped, &socket, &out, u8::MAX);
    let mut previous = 0;
    for body in 0..8u8 {
        let held = held_then_read(&mut stamped, &socket, &out, body);
        let at = held.arrival.at_ns;
        assert_eq!(held.arrival.from, out.local_addr().unwrap());
        assert_eq!(held.arrival.kernel, kernel);
        assert!(at >= previous, "stamps go back");
        let (low, high) = if kernel {
            (held.before, held.in_socket)
        } else {
            (held.read_from, u64::MAX)
        };
        assert!(
            at >= low && at <= high,
            "stamped at {at}, not within {low} to {high}"
        );
        previous = at;
    }
    assert_eq!(stamped.receive(&socket, &mut [0u8; 64]).unwrap(), None);
}

/// Two clocks made apart read one clock: the host's.
#[test]
fn clocks_agree() {
    let (a, b) = (Clock::new().unwrap(), Clock::new().unwrap());
    let first = a.now_ns();
    let second = b.now_ns();
    let third = a.now_ns();
    assert!(first <= second && second <= third);
}

/// What the reactor has seen queued is taken without waiting: after one batch awaited, the rest of
/// what arrived with it, past the batch, comes from `receive_ready`.
#[test]
fn ready_datagrams_are_taken_without_a_wait() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let (mut sender, mut receiver) = planes();
        let mut socket =
            PlaneSocket::bind("127.0.0.1:0".parse().unwrap(), Io { batch: 4 }).unwrap();
        let to = socket.local_addr().unwrap();
        let out = UdpSocket::bind("127.0.0.1:0").unwrap();
        let clock = Clock::new().unwrap();
        let began = clock.now_ns();
        for body in 0..DATAGRAMS {
            send(&mut sender, &out, to, body);
        }
        // Every datagram is on the socket by now, on loopback.
        while clock.now_ns() < began + HELD_NS {
            std::hint::spin_loop();
        }
        let mut taken = Vec::new();
        let mut first = socket
            .receive(&mut receiver, &AdmitAll, |_, opened| {
                taken.push(opened.unwrap().messages().next().unwrap().to_vec());
            })
            .await
            .unwrap();
        first += socket
            .receive_ready(&mut receiver, &AdmitAll, |_, opened| {
                taken.push(opened.unwrap().messages().next().unwrap().to_vec());
            })
            .unwrap();
        assert_eq!(first, DATAGRAMS as usize);
        let expected: Vec<Vec<u8>> = (0..DATAGRAMS).map(|b| b.to_le_bytes().to_vec()).collect();
        assert_eq!(taken, expected);
        assert_eq!(
            socket
                .receive_ready(&mut receiver, &AdmitAll, |_, _| panic!(
                    "nothing more was sent"
                ))
                .unwrap(),
            0
        );
    });
}
