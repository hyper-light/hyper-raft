//! The member's loop, turn by turn, on a real socket and a real log.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::disallowed_macros
)]

use std::{
    net::UdpSocket,
    path::PathBuf,
    time::{Duration, Instant},
};

use hyper_raft_e2e::{
    node::{Node, Settings},
    wal::Wal,
    wire::{self, Kind, Op, Outcome},
};

/// How long the test waits for a loopback datagram it knows was sent before it calls the test
/// failed. The wait ends when the datagram arrives; this bound only stops a test that would
/// otherwise hang.
const LOOPBACK_BOUND: Duration = Duration::from_secs(5);

#[expect(
    clippy::disallowed_methods,
    reason = "a test removes the files it made in its own target directory"
)]
fn remove(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
}

/// A member whose tick is already due when it turns to its socket still takes what has
/// arrived. Before the fix, a member skipped its socket whenever it was behind its ticks, as it
/// is on a loaded machine. It ticked on without reading its peers' answers, and as leader it
/// stepped down by its quorum check while those answers waited unread in its socket.
#[test]
#[allow(
    clippy::disallowed_methods,
    reason = "real processes on the host's clock, threads and environment (CLAUDE.md §1a, end to end); the member's socket is waited on by a peek, never a timed receive"
)]
fn a_member_behind_its_tick_still_reads_what_arrived() {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("node-{}-behind-its-tick.wal", std::process::id()));
    remove(&path);
    let wal = Wal::open(&path, vec![1], 16).unwrap();
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    let address = socket.local_addr().unwrap();
    let arrived = socket.try_clone().unwrap();
    let settings = Settings {
        id: 1,
        voters: vec![1],
        tick: Duration::from_millis(1),
        max_keys: 1,
        max_pending: 1,
        max_entries: 16,
    };
    let mut node = Node::open(settings, socket, wal).unwrap();

    let client = UdpSocket::bind("127.0.0.1:0").unwrap();
    let mut request = Vec::new();
    wire::put_request(&mut request, 7, &Op::Status);
    assert!(wire::seal(&mut request, wire::MAX_DATAGRAM));
    client.send_to(&request, address).unwrap();
    // The fact the turn below needs: the request is in the member's socket.
    let mut peeked = vec![0u8; wire::MAX_DATAGRAM];
    arrived.set_read_timeout(Some(LOOPBACK_BOUND)).unwrap();
    arrived.peek_from(&mut peeked).unwrap();

    // The tick was due before the member turned to its socket.
    node.receive_until(Instant::now()).unwrap();

    let mut answer = vec![0u8; wire::MAX_DATAGRAM];
    assert!(
        wire::arrives(&client, Some(LOOPBACK_BOUND), &mut answer).unwrap(),
        "the member read the request and answered it"
    );
    let (length, _) = wire::take(&client, &mut answer).unwrap();
    let (kind, body) = wire::open(&answer[..length]).unwrap();
    assert_eq!(kind, Kind::Response);
    let (id, outcome) = wire::read_response(body).unwrap();
    assert_eq!(id, 7);
    assert!(matches!(outcome, Outcome::Status(status) if status.id == 1));
    drop(node);
    remove(&path);
}
