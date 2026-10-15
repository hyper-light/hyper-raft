//! A socket's receive queue holds the room the transport counts in it (hyper-transport's
//! `socket_room` of the buffer the socket reports after `socket_buffer_ask`): full QUIC packets
//! sent unread up to the room are all there to read, none dropped. What an endpoint lets its peers
//! send unread stays within that room, so a reader the machine stops loses nothing.

// Test harness code: a panic here is a failed test.
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::arithmetic_side_effects,
    clippy::disallowed_macros,
    clippy::disallowed_methods,
    clippy::cognitive_complexity,
    clippy::cast_possible_truncation
)]

use std::time::{Duration, Instant};

use hyper_rt::udp::{Ipv4Addr, SocketAddrV4, UdpSocket};

#[test]
fn a_socket_holds_the_room_the_transport_counts_in_it() {
    /// Shape: a small room, which every platform's default ceiling grants.
    const ROOM: u64 = 32 << 10;
    /// Shape: a full QUIC packet on an Ethernet path.
    const DATAGRAM: usize = 1_252;
    /// Shape: how long datagrams sent on loopback may take to land, at most.
    const LAND: Duration = Duration::from_secs(5);
    let receiver = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)).unwrap();
    let ask = hyper_transport::socket_buffer_ask(ROOM);
    let reported = receiver.set_recv_buffer_bytes(ask as usize).unwrap();
    let room = hyper_transport::socket_room(reported as u64);
    assert!(room >= ROOM, "a room of {room} for an ask of {ROOM}");
    let target = receiver.local_addr().unwrap();
    let sender = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)).unwrap();
    let count = room as usize / DATAGRAM;
    let datagram = vec![0u8; DATAGRAM];
    let began = Instant::now();
    let mut sent = 0;
    while sent < count {
        match sender.try_send_to(&datagram, target).unwrap() {
            Some(bytes) => {
                assert_eq!(bytes, DATAGRAM);
                sent += 1;
            }
            None => {
                assert!(began.elapsed() < LAND, "the sender stayed full");
                std::thread::yield_now();
            }
        }
    }
    let mut buffer = vec![0u8; 2 * DATAGRAM];
    let mut received = 0;
    while received < count {
        match receiver.try_recv_from(&mut buffer).unwrap() {
            Some((bytes, _)) => {
                assert_eq!(bytes, DATAGRAM);
                received += 1;
            }
            None => {
                assert!(
                    began.elapsed() < LAND,
                    "{received} of {count} landed: the queue dropped what its room holds"
                );
                std::thread::yield_now();
            }
        }
    }
    assert!(
        receiver.try_recv_from(&mut buffer).unwrap().is_none(),
        "no more than sent"
    );
}
