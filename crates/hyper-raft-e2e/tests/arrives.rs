//! A wait for a datagram never loses one (`wire::arrives`): datagrams sent at every phase of the
//! receiver's waits, many of which time out as one arrives, are all taken.
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::disallowed_macros,
    clippy::cast_possible_truncation
)]

use std::net::UdpSocket;
use std::sync::mpsc::sync_channel;
use std::time::{Duration, Instant};

use hyper_raft_e2e::wire;

/// Each wait: one millisecond, `SO_RCVTIMEO`'s unit on Windows (`setsockopt` takes a `DWORD` of
/// milliseconds), the shortest wait that times out there, so that the waits time out as often as
/// they can.
const WAIT: Duration = Duration::from_millis(1);
/// Datagrams sent: enough that a receive losing them at the rate measured on GitHub's
/// windows-11-arm runner, 65 of 40,000 (`docs/raft.md`, "The harness's receive"), loses one with
/// probability `1 − (1 − 65/40,000)^ROUNDS ≥ 1 − 10⁻³`: `ROUNDS ≥ ln 1000 / (65/40,000) = 4,251`.
const ROUNDS: u64 = 4_251;
/// The datagram that ends the run, sent after the last until the receiver has taken one, at most
/// `ROUNDS` times.
const FENCE: u64 = u64::MAX;

/// Busy-waits `spin`: a sender that sleeps would send only at its own timer's ticks.
#[allow(
    clippy::disallowed_methods,
    reason = "a test of real sockets on the host's clock (CLAUDE.md §1a, end to end)"
)]
fn spin(spin: Duration) {
    let start = Instant::now();
    while start.elapsed() < spin {
        std::hint::spin_loop();
    }
}

#[test]
#[allow(
    clippy::disallowed_methods,
    reason = "a test of real sockets on the host's clock and a thread of its own (CLAUDE.md §1a, end to end)"
)]
fn a_wait_that_times_out_loses_no_datagram() {
    let receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
    let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
    let to = receiver.local_addr().unwrap();
    let (stopped, stop) = sync_channel::<()>(1);
    let (gave_up, given_up) = sync_channel::<()>(1);
    let sending = std::thread::spawn(move || {
        let mut state = 0x5eed_u64;
        for seq in 0..ROUNDS {
            // splitmix64: arrivals spread over two waits, so at every phase of one.
            state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            let phase = (z ^ (z >> 31)) % (2 * WAIT.as_nanos() as u64);
            spin(Duration::from_nanos(phase));
            sender.send_to(&seq.to_le_bytes(), to).unwrap();
        }
        for _ in 0..ROUNDS {
            sender.send_to(&FENCE.to_le_bytes(), to).unwrap();
            if stop.try_recv().is_ok() {
                return;
            }
            spin(WAIT);
        }
        let _ = gave_up.send(());
    });
    let mut taken = vec![false; ROUNDS as usize];
    let mut timeouts = 0u64;
    let mut buffer = [0u8; 16];
    loop {
        if !wire::arrives(&receiver, Some(WAIT), &mut buffer).unwrap() {
            timeouts += 1;
            if given_up.try_recv().is_ok() {
                break;
            }
            continue;
        }
        let (length, _) = wire::take(&receiver, &mut buffer).unwrap();
        assert_eq!(length, 8);
        let seq = u64::from_le_bytes(buffer[..8].try_into().unwrap());
        if seq == FENCE {
            let _ = stopped.send(());
            break;
        }
        taken[seq as usize] = true;
    }
    sending.join().unwrap();
    let lost = taken.iter().filter(|taken| !**taken).count();
    assert_eq!(
        lost, 0,
        "{lost} of {ROUNDS} datagrams lost over {timeouts} waits that timed out"
    );
}

/// A reset the socket reports (Windows, on the receive after a send to a closed port) does not
/// wedge the wait: a peek leaves it in place, and the take that follows clears it, so a datagram
/// behind it is taken by the next wait. Elsewhere an unconnected socket reports no reset, and the
/// datagram is the first thing taken.
#[test]
fn a_reset_is_taken_and_the_datagram_behind_it_after() {
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    let closed = UdpSocket::bind("127.0.0.1:0").unwrap();
    let gone = closed.local_addr().unwrap();
    drop(closed);
    socket.send_to(b"refused", gone).unwrap();
    socket
        .send_to(b"kept", socket.local_addr().unwrap())
        .unwrap();
    let mut buffer = [0u8; 16];
    // The reset, if one is reported, and the datagram: two takes at most.
    let mut taken = None;
    for _ in 0..2 {
        assert!(wire::arrives(&socket, None, &mut buffer).unwrap());
        match wire::take(&socket, &mut buffer) {
            Ok((length, _)) => {
                taken = Some(buffer[..length].to_vec());
                break;
            }
            Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::ConnectionReset),
        }
    }
    assert_eq!(taken.as_deref(), Some(&b"kept"[..]));
}
