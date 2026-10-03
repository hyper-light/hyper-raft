//! The harness's wait for a datagram (`wire::arrives`, `wire::take`): a peek with the timeout, then a
//! receive that does not wait. A receive that times out on Windows can be cancelled as it
//! completes and lose the datagram (Microsoft's `setsockopt` reference, `SO_RCVTIMEO`); a peek
//! removes nothing, so one cancelled loses nothing. That no wait in the workspace is a timed
//! receive is the lint's to hold, not a count of datagrams: `clippy.toml` disallows
//! `UdpSocket::set_read_timeout` but where a site states why its wait cannot lose one.
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::disallowed_macros
)]

use std::net::UdpSocket;

use hyper_raft_e2e::wire;

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
