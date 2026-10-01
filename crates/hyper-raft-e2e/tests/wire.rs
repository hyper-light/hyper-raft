//! The datagrams read back as written, and a damaged or cut datagram reads as nothing, never as
//! something else and never by unwinding.
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::disallowed_macros,
    clippy::cognitive_complexity
)]

use hyper_raft_e2e::wire::{self, Command, Control, Kind, Op, Outcome, Status};

#[test]
fn every_kind_reads_back_as_written() {
    let mut buffer = Vec::new();
    for op in [
        Op::Put {
            key: b"k",
            value: b"v",
        },
        Op::Get { key: b"key" },
        Op::Status,
    ] {
        wire::put_request(&mut buffer, 7, &op);
        assert!(wire::seal(&mut buffer));
        let (kind, body) = wire::open(&buffer).unwrap();
        assert_eq!(kind, Kind::Request);
        assert_eq!(wire::read_request(body), Some((7, op)));
    }
    let status = Status {
        id: 1,
        term: 2,
        leads: true,
        leader: 1,
        commit: 3,
        applied: 3,
        last_index: 4,
        digest: 5,
    };
    for outcome in [
        Outcome::Put(9),
        Outcome::Value(Some(b"v".to_vec())),
        Outcome::Value(None),
        Outcome::NotLeader(2),
        Outcome::Busy,
        Outcome::Status(status),
        Outcome::Done,
    ] {
        wire::put_response(&mut buffer, 8, &outcome);
        assert!(wire::seal(&mut buffer));
        let (_, body) = wire::open(&buffer).unwrap();
        assert_eq!(wire::read_response(body), Some((8, outcome)));
    }
    let peers = Control::Peers(vec![(1, "127.0.0.1:9".parse().unwrap())]);
    for control in [peers, Control::Isolate(true)] {
        wire::put_control(&mut buffer, 9, &control);
        assert!(wire::seal(&mut buffer));
        let (_, body) = wire::open(&buffer).unwrap();
        assert_eq!(wire::read_control(body, 8), Some((9, control)));
    }
    let command = Command {
        origin: 1,
        sequence: 2,
        key: b"k",
        value: b"v",
    };
    wire::put_command(&mut buffer, &command);
    assert_eq!(wire::read_command(&buffer), Some(command));
}

#[test]
fn a_damaged_datagram_is_dropped_and_a_cut_body_reads_as_nothing() {
    let mut buffer = Vec::new();
    wire::put_request(
        &mut buffer,
        7,
        &Op::Put {
            key: b"key",
            value: b"value",
        },
    );
    assert!(wire::seal(&mut buffer));
    for at in 0..buffer.len() {
        let mut damaged = buffer.clone();
        damaged[at] ^= 0x01;
        assert_eq!(wire::open(&damaged), None, "a flip at {at} went unseen");
    }
    let (_, body) = wire::open(&buffer).unwrap();
    for end in 0..body.len() {
        assert_eq!(
            wire::read_request(&body[..end]),
            None,
            "a body cut at {end} read"
        );
    }
    // A peer list longer than the bound is refused before it is read.
    let many = Control::Peers(
        (1..=9)
            .map(|id| (id, "127.0.0.1:9".parse().unwrap()))
            .collect(),
    );
    wire::put_control(&mut buffer, 1, &many);
    assert!(wire::seal(&mut buffer));
    let (_, body) = wire::open(&buffer).unwrap();
    assert_eq!(wire::read_control(body, 8), None);
}
