//! Two endpoints over an in-memory network on the caller's clock: every behaviour of the protocol,
//! deterministic and fast. The same behaviours between real processes over real sockets are in
//! `tests/e2e.rs`.

#![allow(
    clippy::unwrap_in_result,
    clippy::type_complexity,
    clippy::too_many_arguments,
    clippy::string_slice,
    clippy::cast_possible_truncation,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::disallowed_macros,
    clippy::cognitive_complexity,
    missing_docs
)]

mod common;

use std::time::{Duration, Instant};

use common::*;
use hyper_transport::{Event, Limits, Refusal};

const PERIOD: Duration = Duration::from_secs(2);
const TURNS: usize = 10_000;

/// Node 1 and node 2, both nodes, connected.
fn connected<A, B>(pair: &Pair, limits: Limits, budget: u64) -> Net<Node<A>, Node<B>>
where
    A: hyper_transport::Classes<Kind = Kind, Class = Class, Role = Role>,
    B: hyper_transport::Classes<Kind = Kind, Class = Class, Role = Role>,
{
    let now = Instant::now();
    let a = pair.node::<A>(
        1,
        Role::Node,
        pair.book(Role::Node, Role::Node),
        limits,
        budget,
        now,
    );
    let b = pair.node::<B>(
        2,
        Role::Node,
        pair.book(Role::Node, Role::Node),
        limits,
        budget,
        now,
    );
    let mut net = Net::new(now, a, b);
    let address = net.b_address;
    net.a.connect(now, 2, address).unwrap();
    let (mut one, mut two) = (false, false);
    net.until(TURNS, |net| {
        for event in events(&mut net.a) {
            one |= matches!(
                event,
                Event::Connected {
                    peer: 2,
                    epoch: 1,
                    role: Role::Node
                }
            );
        }
        for event in events(&mut net.b) {
            two |= matches!(
                event,
                Event::Connected {
                    peer: 1,
                    epoch: 1,
                    role: Role::Node
                }
            );
        }
        one && two
    });
    net
}

fn run(net: &mut Net<Node<Mantle>, Node<Mantle>>, asker: &mut Asker, server: &mut Server) {
    net.until(TURNS, |net| {
        asker.drive(&mut net.a);
        server.serve(&mut net.b);
        asker.drive(&mut net.a);
        asker.finished()
    });
}

#[test]
fn a_request_and_its_reply_cross_in_every_class_with_streaming_bodies() {
    let pair = Pair::new();
    let mut net = connected::<Mantle, Mantle>(&pair, limits(), 256 << 20);
    let (mut asker, mut server) = (Asker::new(), Server::new());
    let now = net.now;
    asker
        .ask(
            &mut net.a,
            now,
            2,
            (Kind::Vote, Class::Control),
            1,
            None,
            PERIOD,
        )
        .unwrap();
    asker
        .ask(
            &mut net.a,
            now,
            2,
            (Kind::Get, Class::Request),
            2,
            Some(10_000),
            PERIOD,
        )
        .unwrap();
    asker
        .ask(
            &mut net.a,
            now,
            2,
            (Kind::Snapshot, Class::Bulk),
            3,
            Some(8 << 20),
            PERIOD,
        )
        .unwrap();
    asker
        .ask(
            &mut net.a,
            now,
            2,
            (Kind::Put, Class::Request),
            4,
            Some(0),
            PERIOD,
        )
        .unwrap();
    run(&mut net, &mut asker, &mut server);
    for asked in &asker.asked {
        assert_eq!(asked.refused, None, "{asked:?}");
        assert_eq!(asked.reply, Some(asked.body), "{asked:?}");
        assert_eq!(asked.read, asked.body);
    }
    assert_eq!(server.answered, 4);
    // Everything was given back: the budget holds only the two connections' windows.
    net.until(TURNS, |net| {
        net.a.stats().exchanges == 0 && net.b.stats().exchanges == 0
    });
    assert!(
        net.a.exchange_tail(2).is_some(),
        "answered exchanges were timed"
    );
    let path = net.a.path(2).unwrap();
    assert!(path.cwnd > 0 && path.max_datagram >= 1_200, "{path:?}");
    let mut one = [0u8; 32];
    let mut two = [0u8; 32];
    assert_eq!(
        net.a
            .export_keying_material(2, b"hyper-datagram", b"", &mut one),
        Ok(1)
    );
    assert_eq!(
        net.b
            .export_keying_material(1, b"hyper-datagram", b"", &mut two),
        Ok(1)
    );
    assert_eq!(one, two, "both ends export the same secret");
}

/// The class reserve (T16; slates' 68 ms bug): bulk exchanges whose bodies the peer does not read
/// fill the connection's credit, and a control exchange still crosses at once.
#[test]
fn a_class_reserve_keeps_control_moving_under_bulk_load() {
    let pair = Pair::new();
    let mut net = connected::<Mantle, Mantle>(&pair, limits(), 256 << 20);
    let (mut asker, mut server) = (Asker::new(), Server::new());
    server.hold_bulk = true;
    let now = net.now;
    for seed in 0..4 {
        asker
            .ask(
                &mut net.a,
                now,
                2,
                (Kind::Snapshot, Class::Bulk),
                seed,
                Some(4 << 20),
                Duration::from_secs(60),
            )
            .unwrap();
    }
    // Bulk writes until the credit is spent.
    for _ in 0..50 {
        net.exchange();
        asker.drive(&mut net.a);
        server.serve(&mut net.b);
    }
    let written: u64 = asker.asked.iter().map(|asked| asked.written).sum();
    assert!(
        written < 16 << 20,
        "the bulk bodies were held back by credit: {written}"
    );
    let vote = asker
        .ask(
            &mut net.a,
            net.now,
            2,
            (Kind::Vote, Class::Control),
            9,
            None,
            PERIOD,
        )
        .unwrap();
    let before = net.now;
    net.until(TURNS, |net| {
        asker.drive(&mut net.a);
        server.serve(&mut net.b);
        asker.drive(&mut net.a);
        asker.asked[vote].done
    });
    assert_eq!(asker.asked[vote].refused, None);
    assert_eq!(
        net.now, before,
        "the vote crossed without waiting for a timer"
    );
    // Released, the bulk completes.
    server.hold_bulk = false;
    run(&mut net, &mut asker, &mut server);
    assert!(
        asker
            .asked
            .iter()
            .all(|asked| asked.refused.is_none() && asked.read == asked.body)
    );
}

/// The frame bound is checked from the prefix, before any byte of the body: a receiver whose
/// bound is below the sender's refuses, and the sender hears which bound.
#[test]
fn a_message_past_its_class_bound_is_refused_from_its_prefix() {
    let pair = Pair::new();
    let mut net = connected::<Mantle, Strict>(&pair, limits(), 256 << 20);
    let mut asker = Asker::new();
    let body = STRICT_REQUEST_BOUND * 4;
    asker
        .ask(
            &mut net.a,
            net.now,
            2,
            (Kind::Put, Class::Request),
            1,
            Some(body),
            PERIOD,
        )
        .unwrap();
    let used_before = net.b.budget().used();
    net.until(TURNS, |net| {
        asker.drive(&mut net.a);
        let _ = events(&mut net.b);
        asker.finished()
    });
    assert_eq!(asker.asked[0].refused, Some((Refusal::FrameBound, true)));
    assert_eq!(
        net.b.budget().used(),
        used_before,
        "nothing was reserved for the body"
    );
    // The sender's own bound is checked before anything is sent.
    let head = vec![0u8; (limits().max_head + 1) as usize];
    let progress = hyper_transport::Progress::new(PERIOD).unwrap();
    assert_eq!(
        net.a.open(net.now, 2, Kind::Get, &head, None, progress),
        Err(Refusal::FrameBound)
    );
    assert_eq!(
        net.a.open(
            net.now,
            2,
            Kind::Vote,
            b"",
            Some(CONTROL_BOUND + 1),
            progress
        ),
        Err(Refusal::FrameBound)
    );
}

/// A period that hears nothing ends an exchange, so a period no longer than the peer's
/// acknowledgement delay could end one whose peer lives: it is refused before anything is sent,
/// and one a millisecond longer is taken. The peer here states RFC 9000 §18.2's default of 25 ms.
#[test]
fn a_period_no_longer_than_the_peers_acknowledgement_delay_is_refused() {
    let pair = Pair::new();
    let mut net = connected::<Mantle, Mantle>(&pair, limits(), 256 << 20);
    let delay = Duration::from_millis(25);
    let short = hyper_transport::Progress::new(delay).unwrap();
    assert_eq!(
        net.a.open(net.now, 2, Kind::Get, b"", None, short),
        Err(Refusal::Configuration)
    );
    let long = hyper_transport::Progress::new(delay + Duration::from_millis(1)).unwrap();
    assert!(net.a.open(net.now, 2, Kind::Get, b"", None, long).is_ok());
}

/// The class comes from the message's kind and the sender's role (audit §13.3): a client may not
/// send a snapshot, whatever it believes.
#[test]
fn a_role_that_may_not_send_a_kind_is_refused_on_both_sides() {
    let pair = Pair::new();
    let now = Instant::now();
    let book = || pair.book(Role::Client, Role::Node);
    let a = pair.node::<Lax>(1, Role::Client, book(), limits(), 64 << 20, now);
    let b = pair.node::<Mantle>(2, Role::Node, book(), limits(), 64 << 20, now);
    let mut net = Net::new(now, a, b);
    let address = net.b_address;
    net.a.connect(now, 2, address).unwrap();
    net.until(TURNS, |net| {
        events(&mut net.a)
            .iter()
            .any(|event| matches!(event, Event::Connected { .. }))
    });
    let mut asker = Asker::new();
    asker
        .ask(
            &mut net.a,
            net.now,
            2,
            (Kind::Snapshot, Class::Bulk),
            1,
            None,
            PERIOD,
        )
        .unwrap();
    net.until(TURNS, |net| {
        asker.drive(&mut net.a);
        let _ = events(&mut net.b);
        asker.finished()
    });
    assert_eq!(asker.asked[0].refused, Some((Refusal::Kind, true)));
    // An honest client is refused before it sends anything.
    let mut honest = pair.node::<Mantle>(1, Role::Client, book(), limits(), 64 << 20, now);
    let progress = hyper_transport::Progress::new(PERIOD).unwrap();
    assert_eq!(
        honest.open(now, 2, Kind::Snapshot, b"", None, progress),
        Err(Refusal::Kind)
    );
    assert_eq!(
        honest.open(now, 2, Kind::Get, b"", None, progress),
        Err(Refusal::NotConnected)
    );
}

/// The budget funds a body's head before it is read; a peer whose budget cannot is refused.
#[test]
fn a_budget_that_cannot_fund_a_head_refuses_it() {
    let pair = Pair::new();
    let now = Instant::now();
    let a = pair.node::<Mantle>(
        1,
        Role::Node,
        pair.book(Role::Node, Role::Node),
        limits(),
        64 << 20,
        now,
    );
    // Enough for the connection's window and little else.
    let b = pair.node::<Mantle>(
        2,
        Role::Node,
        pair.book(Role::Node, Role::Node),
        limits(),
        16_000,
        now,
    );
    let mut net = Net::new(now, a, b);
    let address = net.b_address;
    net.a.connect(now, 2, address).unwrap();
    net.until(TURNS, |net| {
        events(&mut net.a)
            .iter()
            .any(|event| matches!(event, Event::Connected { .. }))
    });
    let head = vec![7u8; 12_000];
    let progress = hyper_transport::Progress::new(PERIOD).unwrap();
    let exchange = net
        .a
        .open(net.now, 2, Kind::Get, &head, None, progress)
        .unwrap();
    let mut refused = None;
    net.until(TURNS, |net| {
        let _ = events(&mut net.b);
        for event in events(&mut net.a) {
            if let Event::Refused {
                exchange: which,
                refusal,
                by_peer,
            } = event
            {
                assert_eq!(which, exchange);
                refused = Some((refusal, by_peer));
            }
        }
        refused.is_some()
    });
    assert_eq!(refused, Some((Refusal::Budget, true)));
}

/// The exchange table and the lanes are bounded; past the peer's stream limit an exchange waits
/// its turn instead of being refused (T37).
#[test]
fn exchanges_past_the_stream_limit_wait_their_turn_and_the_table_is_bounded() {
    let pair = Pair::new();
    let mut tight = limits();
    tight.streams_per_connection = 2;
    tight.exchanges = 6;
    let mut net = connected::<Mantle, Mantle>(&pair, tight, 256 << 20);
    let (mut asker, mut server) = (Asker::new(), Server::new());
    for seed in 0..6 {
        asker
            .ask(
                &mut net.a,
                net.now,
                2,
                (Kind::Get, Class::Request),
                seed,
                Some(1_000),
                PERIOD,
            )
            .unwrap();
    }
    assert_eq!(
        asker.ask(
            &mut net.a,
            net.now,
            2,
            (Kind::Get, Class::Request),
            9,
            None,
            PERIOD
        ),
        Err(Refusal::Exchanges)
    );
    run(&mut net, &mut asker, &mut server);
    assert!(
        asker
            .asked
            .iter()
            .all(|asked| asked.refused.is_none() && asked.read == 1_000)
    );
    assert_eq!(server.answered, 6);
}

/// A peer that stops answering ends its exchanges within their progress deadline (T39), and the
/// estimate of what it takes doubles (T40).
#[test]
fn a_peer_that_stops_answering_is_given_up_within_its_period() {
    let pair = Pair::new();
    let mut net = connected::<Mantle, Mantle>(&pair, limits(), 256 << 20);
    let (mut asker, mut server) = (Asker::new(), Server::new());
    asker
        .ask(
            &mut net.a,
            net.now,
            2,
            (Kind::Get, Class::Request),
            1,
            None,
            PERIOD,
        )
        .unwrap();
    run(&mut net, &mut asker, &mut server);
    let tail = net.a.exchange_tail(2).unwrap();
    net.b_dead = true;
    let began = net.now;
    asker
        .ask(
            &mut net.a,
            net.now,
            2,
            (Kind::Get, Class::Request),
            2,
            Some(100_000),
            PERIOD,
        )
        .unwrap();
    net.until(TURNS, |net| {
        asker.drive(&mut net.a);
        asker.finished()
    });
    assert_eq!(asker.asked[1].refused, Some((Refusal::Stalled, false)));
    let took = net.now - began;
    // The first judgement finds a period in which the peer said nothing: what was sent into its
    // silence (the flight in the air and the probe timeout's probes) is no progress.
    assert!(took <= PERIOD, "given up after {took:?}");
    assert_eq!(net.a.exchange_tail(2), Some(tail * 2));
}

/// Replication frames on lanes arrive in order, each lane on its own stream; a lane is as wide as
/// the core's window, and the lanes to a peer are bounded.
#[test]
fn frames_on_a_lane_arrive_in_order() {
    let pair = Pair::new();
    let mut narrow = limits();
    narrow.lane_window = 64;
    narrow.lanes_per_peer = 2;
    let mut net = connected::<Mantle, Mantle>(&pair, narrow, 256 << 20);
    let mut server = Server::new();
    let mut sent = 0u32;
    let mut full = 0;
    for round in 0..20u32 {
        for index in 0..32u32 {
            let lane = index % 2;
            let frame = [round.to_be_bytes(), index.to_be_bytes(), [0u8; 4]].concat();
            match net.a.send_frame(2, lane, Kind::Append, &frame) {
                Ok(()) => sent += 1,
                Err(Refusal::LaneFull) => full += 1,
                Err(other) => panic!("{other:?}"),
            }
        }
        net.exchange();
        server.serve(&mut net.b);
    }
    assert_eq!(
        net.a.send_frame(2, 7, Kind::Append, b"x"),
        Err(Refusal::Lanes)
    );
    assert_eq!(
        net.a.send_frame(2, 0, Kind::Get, b"x"),
        Ok(()),
        "a request kind may ride a lane"
    );
    sent += 1;
    net.until(TURNS, |net| {
        server.serve(&mut net.b);
        server.frames.len() as u32 == sent
    });
    assert_eq!(
        full, 0,
        "a core within its window never finds its lane full"
    );
    for lane in 0..2u32 {
        let order: Vec<(u32, u32)> = server
            .frames
            .iter()
            .filter(|(peer, which, frame)| *peer == 1 && *which == lane && frame.len() == 12)
            .map(|(_, _, frame)| {
                (
                    u32::from_be_bytes(frame[..4].try_into().unwrap()),
                    u32::from_be_bytes(frame[4..8].try_into().unwrap()),
                )
            })
            .collect();
        let mut sorted = order.clone();
        sorted.sort_unstable();
        assert_eq!(order, sorted, "lane {lane} kept its order");
        assert_eq!(order.len(), 320);
    }
}

/// A lane's queue holds the core's window and no more.
#[test]
fn a_lane_refuses_a_frame_past_the_core_s_window() {
    let pair = Pair::new();
    let mut narrow = limits();
    narrow.lane_window = 4;
    let mut net = connected::<Mantle, Mantle>(&pair, narrow, 256 << 20);
    let frame = vec![1u8; 200_000];
    let mut queued = 0;
    let refused = loop {
        match net.a.send_frame(2, 0, Kind::Append, &frame) {
            Ok(()) => queued += 1,
            Err(refusal) => break refusal,
        }
        assert!(queued < 100, "the lane never filled");
    };
    assert_eq!(refused, Refusal::LaneFull);
}

/// Admission (T9–T11) over real handshakes: a certificate that names no peer is charged to no
/// identity, and an identity past its bound replaces its own connection used longest ago.
#[test]
fn a_certificate_the_directory_does_not_know_is_refused_and_charged_to_no_one() {
    let pair = Pair::new();
    let now = Instant::now();
    let a = pair.node::<Mantle>(
        1,
        Role::Node,
        pair.book(Role::Node, Role::Node),
        limits(),
        64 << 20,
        now,
    );
    // Node 2 knows nobody.
    let b = pair.node::<Mantle>(2, Role::Node, Book::default(), limits(), 64 << 20, now);
    let mut net = Net::new(now, a, b);
    let address = net.b_address;
    net.a.connect(now, 2, address).unwrap();
    let mut asker = Asker::new();
    net.until(TURNS, |net| {
        asker.drive(&mut net.a);
        let _ = events(&mut net.b);
        !asker.unreachable.is_empty() || !asker.closed.is_empty()
    });
    let stats = net.b.stats().admission;
    assert_eq!(
        (stats.identities, stats.admitted, stats.refused_identity),
        (0, 0, 1)
    );
}

#[test]
fn an_identity_past_its_bound_replaces_its_connection_used_least() {
    let pair = Pair::new();
    let now = Instant::now();
    let mut one = limits();
    one.admission.per_identity = 1;
    let book = || pair.book(Role::Node, Role::Node);
    let first = pair.node::<Mantle>(1, Role::Node, book(), one, 64 << 20, now);
    let second = pair.node::<Mantle>(1, Role::Node, book(), one, 64 << 20, now);
    let server = pair.node::<Mantle>(2, Role::Node, book(), one, 64 << 20, now);
    let mut net = Net::new(now, first, server);
    let address = net.b_address;
    net.a.connect(now, 2, address).unwrap();
    net.until(TURNS, |net| {
        let _ = events(&mut net.b);
        events(&mut net.a)
            .iter()
            .any(|event| matches!(event, Event::Connected { .. }))
    });
    // The same identity again, from another endpoint at another address: it replaces the first.
    let Net {
        now,
        a: first,
        b: server,
        ..
    } = net;
    let mut net = Net::new(now, second, server);
    net.a_address = "127.0.0.1:5555".parse().unwrap();
    let address = net.b_address;
    net.a.connect(now, 2, address).unwrap();
    net.until(TURNS, |net| {
        events(&mut net.a)
            .iter()
            .any(|event| matches!(event, Event::Connected { .. }))
    });
    let stats = net.b.stats().admission;
    assert_eq!(
        (stats.connections, stats.replaced, stats.admitted),
        (1, 1, 2)
    );
    drop(first);
}
