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
        assert!(wire::seal(&mut buffer, wire::MAX_DATAGRAM));
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
        assert!(wire::seal(&mut buffer, wire::MAX_DATAGRAM));
        let (_, body) = wire::open(&buffer).unwrap();
        assert_eq!(wire::read_response(body), Some((8, outcome)));
    }
    let peers = Control::Peers(vec![(1, "127.0.0.1:9".parse().unwrap())]);
    for control in [peers, Control::Isolate(true)] {
        wire::put_control(&mut buffer, 9, &control);
        assert!(wire::seal(&mut buffer, wire::MAX_DATAGRAM));
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
    assert!(wire::seal(&mut buffer, wire::MAX_DATAGRAM));
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
    assert!(wire::seal(&mut buffer, wire::MAX_DATAGRAM));
    let (_, body) = wire::open(&buffer).unwrap();
    assert_eq!(wire::read_control(body, 8), None);
}

/// The largest datagram found is one the socket sends, and one byte more is one it refuses,
/// unless it is UDP's own most; finding it leaves nothing in the socket.
#[test]
fn the_largest_datagram_is_what_the_socket_sends() {
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let to = socket.local_addr().unwrap();
    let largest = wire::largest(&socket).unwrap();
    assert!(largest > 0 && largest <= wire::MAX_DATAGRAM, "{largest}");
    socket.set_nonblocking(true).unwrap();
    let mut received = vec![0u8; wire::MAX_DATAGRAM];
    assert!(
        socket.recv_from(&mut received).is_err(),
        "a probe reached the socket"
    );
    socket.set_nonblocking(false).unwrap();
    let datagram = vec![0u8; wire::MAX_DATAGRAM];
    socket.send_to(&datagram[..largest], to).unwrap();
    if largest < wire::MAX_DATAGRAM {
        assert!(socket.send_to(&datagram[..largest + 1], to).is_err());
    }
}

/// A leader's append whose entries fill a member's page goes in one datagram, framed and sealed as
/// a member sends it (`node.rs`): the page is the datagram less `MESSAGE_ROOM` (the member's
/// `max_size_per_msg`), and beside its entries an append takes its record's fixed part, the classic
/// commit every leader's append states among it (`hyper_raft::wire::MESSAGE_RECORD_FIXED_BYTES`).
/// A room that left the classic commit out was 8 bytes short: a full page was dropped as too long
/// at every send, and a member its leader caught up from the log never caught up (`leader-killed`,
/// CI run 39366050595, once the log's pages were the core's to the byte).
#[test]
fn a_leaders_append_filled_to_the_page_fills_its_datagram() {
    use hyper_raft::proto::{Entry, Message, MessageType};
    use hyper_raft::wire::Record;
    let datagram = wire::MAX_DATAGRAM;
    let room = datagram - hyper_raft_e2e::node::MESSAGE_ROOM;
    let fixed = hyper_raft::wire::ENTRY_FIXED_BYTES;
    // Entries of a write's size, the last taking what is left of the page.
    let mut entries = Vec::new();
    let mut left = room;
    while left > 0 {
        let data = if left >= 2 * (fixed + 40) {
            40
        } else {
            left - fixed
        };
        entries.push(Entry {
            index: 10 + entries.len() as u64,
            term: 2,
            data: vec![7; data],
            ..Entry::default()
        });
        left -= fixed + data;
    }
    let bytes: u64 = entries.iter().map(hyper_raft::proto::encoded_bytes).sum();
    assert_eq!(bytes, room as u64, "the entries fill the page");
    let append = Message {
        msg_type: MessageType::MsgAppend,
        from: 1,
        to: 2,
        term: 2,
        index: 9,
        log_term: 2,
        commit: 9,
        classic: Some(9),
        entries,
        ..Message::default()
    };
    let mut buffer = Vec::new();
    wire::begin(&mut buffer, Kind::Raft);
    wire::put_u64(&mut buffer, 1);
    append.encode(&mut buffer);
    assert_eq!(
        buffer.len(),
        datagram,
        "the room is what an append takes beside its entries"
    );
    assert!(wire::seal(&mut buffer, datagram), "a full page is sent");
}

/// A member's report reads back as written, its core's suspicions and campaign among it: what a
/// failure's dump states of each member's elections.
#[test]
fn a_report_reads_back_with_its_core_state() {
    use hyper_raft::CampaignState;
    use hyper_raft_e2e::stream::{self, Report};
    let mut buffer = Vec::new();
    for campaign in [
        None,
        Some(CampaignState::default()),
        Some(CampaignState {
            due: Some(17),
            armed: true,
            led: true,
            held: false,
            trusted_quorum: true,
            may_campaign: true,
            may_lead: false,
            promotable: true,
        }),
    ] {
        let report = Report {
            status: Status {
                id: 2,
                term: 3,
                leads: false,
                leader: 1,
                commit: 9,
                applied: 9,
                last_index: 10,
                digest: 11,
            },
            span_ns: 1,
            round_ns: 2,
            sent: 11,
            unproven: 2,
            unmeasured: 3,
            refused: 4,
            granularity_ns: 5_000,
            sending_interval_ns: 6_000,
            sending_due_ns: 7,
            stream_wake_ns: u64::MAX,
            deadline_ns: 8,
            suspected: vec![1],
            heard: vec![1, 3],
            core_suspected: vec![1, 3],
            clock_ns: 99,
            campaign,
            ..Report::default()
        };
        stream::put_report(&mut buffer, 5, &report);
        assert!(wire::seal(&mut buffer, wire::MAX_DATAGRAM));
        let (kind, body) = wire::open(&buffer).unwrap();
        assert_eq!(kind, Kind::Response);
        assert_eq!(stream::read_report(body, 3), Some((5, report)));
    }
}
