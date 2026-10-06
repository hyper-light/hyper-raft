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
use hyper_transport::{Event, ExchangeId, LEAST_PROGRESS, Limits, Refusal};

const PERIOD: Duration = Duration::from_secs(2);
const TURNS: usize = 10_000;
/// The simulated clock's timed waits end exactly when asked: its measured granularity is zero.
const GRANULARITY: Duration = Duration::ZERO;

/// Node 1 and node 2, both nodes, connected.
fn connected<A, B>(pair: &Pair, limits: Limits, budget: u64) -> Net<Node<A>, Node<B>>
where
    A: hyper_transport::Classes<Kind = Kind, Class = Class, Role = Role>,
    B: hyper_transport::Classes<Kind = Kind, Class = Class, Role = Role>,
{
    connected_on(pair, limits, budget, hyper_sim::Source::Seed(0))
}

/// Node 1 and node 2 connected on a network whose world draws from `source`.
fn connected_on<A, B>(
    pair: &Pair,
    limits: Limits,
    budget: u64,
    source: hyper_sim::Source,
) -> Net<Node<A>, Node<B>>
where
    A: hyper_transport::Classes<Kind = Kind, Class = Class, Role = Role>,
    B: hyper_transport::Classes<Kind = Kind, Class = Class, Role = Role>,
{
    let now = hyper_sim::Anchor::new().instant(0).unwrap();
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
    // Every running sum the scheduling core reads is checked against the fold it replaced
    // (src/tally.rs); `run` asserts none differed.
    let (mut a, mut b) = (a, b);
    a.set_oracle(true);
    b.set_oracle(true);
    let mut net = Net::of(now, a, b, source);
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
    sums_agree_with_their_folds(net);
}

/// The scheduling core's running sums (src/tally.rs) equal, at every read, the folds over the
/// connection's exchanges they replaced; the settle counter moving is the non-vacuity check.
fn sums_agree_with_their_folds(net: &Net<Node<Mantle>, Node<Mantle>>) {
    for (side, node) in [("asker", &net.a), ("server", &net.b)] {
        assert_eq!(
            node.oracle_mismatches(),
            0,
            "{side}: a running sum differed from its fold"
        );
        assert!(
            node.oracle_settles() > 0,
            "{side}: the sums were never read"
        );
    }
}

/// The run-twice check (docs/sim.md §3.9): two nodes connect on the zero path to one digest from
/// a seed twice and from its trace, whatever keys and nonces the endpoints draw for themselves.
#[test]
fn a_connection_runs_the_same_twice_and_from_its_trace() {
    let pair = Pair::new();
    let record = twice_on(1, |source| {
        connected_on::<Mantle, Mantle>(&pair, limits(), 256 << 20, source)
    });
    assert!(record.steps > 0);
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
        net.a.exchange_tail(2, GRANULARITY).is_some(),
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
    let now = hyper_sim::Anchor::new().instant(0).unwrap();
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
    let now = hyper_sim::Anchor::new().instant(0).unwrap();
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
    let tail = net.a.exchange_tail(2, GRANULARITY).unwrap();
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
    assert_eq!(net.a.exchange_tail(2, GRANULARITY), Some(tail * 2));
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
    let now = hyper_sim::Anchor::new().instant(0).unwrap();
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
    let now = hyper_sim::Anchor::new().instant(0).unwrap();
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

/// One exchange the slow owner below asked: what of its body it wrote, what of its reply it read.
struct Slow {
    exchange: hyper_transport::ExchangeId,
    seed: u8,
    body: u64,
    written: u64,
    replied: bool,
    read: u64,
    done: bool,
    refused: Option<(Refusal, bool)>,
}

impl Slow {
    fn open(net: &mut Net<Node<Mantle>, Node<Mantle>>, kind: Kind, seed: u8, body: u64) -> Self {
        let progress = hyper_transport::Progress::new(PERIOD).unwrap();
        let exchange = net
            .a
            .open(net.now, 2, kind, &[seed, 1, 2, 3], Some(body), progress)
            .unwrap();
        Self {
            exchange,
            seed,
            body,
            written: 0,
            replied: false,
            read: 0,
            done: false,
            refused: None,
        }
    }
    /// Writes as much of the body as the endpoint takes now.
    fn write(&mut self, node: &mut Node<Mantle>, piece: &mut [u8]) {
        while !self.done && self.written < self.body {
            let length = (self.body - self.written).min(piece.len() as u64) as usize;
            fill(u64::from(self.seed), self.written, &mut piece[..length]);
            match node.write_body(self.exchange, &piece[..length]) {
                Ok(0) | Err(_) => return,
                Ok(took) => self.written += took as u64,
            }
        }
    }
    /// Reads what of the reply has arrived; ends the exchange once it is whole.
    fn read(&mut self, node: &mut Node<Mantle>) {
        if self.done || !self.replied {
            return;
        }
        while self.read < self.body {
            let mut into = node.reserve(Class::Request, PIECE as u64).unwrap();
            let got = node.read_body(self.exchange, &mut into);
            node.release(into);
            match got {
                Ok(0) | Err(_) => break,
                Ok(got) => self.read += got as u64,
            }
        }
        if self.read == self.body && node.body_complete(self.exchange) && self.written == self.body
        {
            node.end(self.exchange);
            self.done = true;
        }
    }
}

/// Strict priority where credit is taken (T15), with an owner that is slow to write: requests
/// opened behind a bulk body in flight, on a connection whose window cannot grow past what they
/// need, take the credit before the bulk body takes any, although the owner writes the bulk body
/// first at every turn and reaches the requests only every eighth turn or once 100 ms have passed;
/// and none of the exchanges is refused as stalled while the peer lives. Seen first in hyper-tokio's
/// first harness, where seven requests sat at no byte written for a period while an 8 MiB bulk
/// body completed. The earlier rule counted a request as waiting for credit only once its owner had
/// been refused a write, so the bulk body took the window's credit first.
#[test]
fn requests_behind_a_bulk_body_take_credit_first_from_a_slow_owner() {
    const REQUESTS: u8 = 7;
    /// How late the owner may be in reaching the requests, well inside the 2 s period.
    const LATE: Duration = Duration::from_millis(100);
    let pair = Pair::new();
    let mut narrow = limits();
    narrow.window_ceiling = 32 << 10;
    let mut net = connected::<Mantle, Mantle>(&pair, narrow, 1 << 30);
    let mut server = Server::new();
    let mut piece = vec![0u8; PIECE];
    let mut bulk = Slow::open(&mut net, Kind::Snapshot, 100, 8 << 20);
    // The bulk body is in flight before the requests are asked.
    while bulk.written < 256 << 10 {
        bulk.write(&mut net.a, &mut piece);
        net.exchange();
        server.serve(&mut net.b);
        if !net.exchange() {
            net.advance();
        }
    }
    let mut requests: Vec<Slow> = (0..REQUESTS)
        .map(|seed| Slow::open(&mut net, Kind::Put, seed, 64 << 10))
        .collect();
    let opened_at = bulk.written;
    // What the bulk body took before any request's body took a byte.
    let mut bulk_first = None;
    let mut visited = net.now;
    for turn in 0..1_000_000u64 {
        // The owner writes bulk first at every turn.
        bulk.write(&mut net.a, &mut piece);
        if turn % 8 == 0 || net.now - visited >= LATE {
            visited = net.now;
            for request in &mut requests {
                request.write(&mut net.a, &mut piece);
            }
        }
        if bulk_first.is_none() && requests.iter().any(|request| request.written > 0) {
            bulk_first = Some(bulk.written - opened_at);
        }
        net.exchange();
        server.serve(&mut net.b);
        if !net.exchange() {
            net.advance_within(LATE);
        }
        while let Some(event) = net.a.poll_event() {
            match event {
                Event::Reply { exchange, .. } => {
                    for slow in requests.iter_mut().chain([&mut bulk]) {
                        slow.replied |= slow.exchange == exchange;
                    }
                }
                Event::Refused {
                    exchange,
                    refusal,
                    by_peer,
                } => {
                    for slow in requests.iter_mut().chain([&mut bulk]) {
                        if slow.exchange == exchange {
                            slow.refused = Some((refusal, by_peer));
                            slow.done = true;
                        }
                    }
                }
                _ => {}
            }
        }
        for request in &mut requests {
            request.read(&mut net.a);
        }
        bulk.read(&mut net.a);
        if bulk.done && requests.iter().all(|request| request.done) {
            break;
        }
    }
    for request in requests.iter().chain([&bulk]) {
        assert_eq!(
            request.refused, None,
            "refused; the server saw {:?}",
            server.refused
        );
        assert_eq!(request.read, request.body);
    }
    assert!(server.refused.is_empty(), "{:?}", server.refused);
    assert_eq!(
        bulk_first,
        Some(0),
        "the bulk body took credit before the requests"
    );
}

/// A period in which an exchange's own owner offered nothing, while the peer would take it, is
/// this side's doing and no evidence against the peer: the exchange is not refused as stalled by
/// its own side. Here the owner writes a bulk body only three periods after opening it, and the
/// peer, holding bulk bodies unread meanwhile, does not judge it either.
#[test]
fn an_owner_late_to_write_is_not_refused_by_its_own_side() {
    let pair = Pair::new();
    let mut net = connected::<Mantle, Mantle>(&pair, limits(), 1 << 30);
    let mut server = Server::new();
    server.hold_bulk = true;
    let mut piece = vec![0u8; PIECE];
    let mut bulk = Slow::open(&mut net, Kind::Snapshot, 9, 256 << 10);
    let opened = net.now;
    let mut refused = None;
    let mut turns = 0u64;
    while !bulk.done {
        turns += 1;
        assert!(turns < 1_000_000, "the exchange never ended");
        if net.now - opened >= PERIOD * 3 {
            server.hold_bulk = false;
            bulk.write(&mut net.a, &mut piece);
        }
        net.exchange();
        server.serve(&mut net.b);
        if !net.exchange() {
            net.advance_within(Duration::from_millis(100));
        }
        while let Some(event) = net.a.poll_event() {
            match event {
                Event::Reply { .. } => bulk.replied = true,
                Event::Refused {
                    refusal, by_peer, ..
                } => {
                    refused = Some((refusal, by_peer));
                    bulk.done = true;
                }
                _ => {}
            }
        }
        bulk.read(&mut net.a);
    }
    assert_eq!(refused, None, "the server saw {:?}", server.refused);
    assert_eq!(bulk.read, bulk.body);
}

/// A peer that leaves single small datagrams unread across many receive chunks: each of sixteen
/// bulk requests, whose bodies the receiver holds unread, is followed by enough answered requests
/// of 1,300-byte bodies to fill the rest of a chunk, so each chunk is pinned by one small datagram. The receiver holds
/// no more chunks than its bound, each charged to its budget, and copies the datagrams past it into
/// buffers of their own instead of growing; everything completes once the bodies are read.
#[test]
fn unread_datagrams_pin_no_more_receive_chunks_than_the_bound() {
    const CHUNKS: usize = 3;
    let pair = Pair::new();
    let mut tight = limits();
    tight.receive_chunks = CHUNKS;
    tight.streams_per_connection = 32;
    let mut net = connected::<Mantle, Mantle>(&pair, tight, 1 << 30);
    let (mut asker, mut server) = (Asker::new(), Server::new());
    server.hold_bulk = true;
    for held in 0..16u8 {
        asker
            .ask(
                &mut net.a,
                net.now,
                2,
                (Kind::Snapshot, Class::Bulk),
                held,
                Some(600),
                Duration::from_secs(60),
            )
            .unwrap();
        for filler in 0..48u8 {
            let at = asker
                .ask(
                    &mut net.a,
                    net.now,
                    2,
                    (Kind::Put, Class::Request),
                    100 + filler,
                    Some(1_300),
                    PERIOD,
                )
                .unwrap();
            net.until(TURNS, |net| {
                asker.drive(&mut net.a);
                server.serve(&mut net.b);
                asker.drive(&mut net.a);
                asker.asked[at].done
            });
            assert!(
                net.b.stats().receive_chunks <= CHUNKS,
                "the pool grew past its bound"
            );
        }
    }
    let stats = net.b.stats();
    assert_eq!(stats.receive_chunks, CHUNKS);
    assert!(
        stats.receive_copied > 0,
        "past the bound, datagrams were copied: {stats:?}"
    );
    // Released, the held bodies are read and everything completes.
    asker
        .ask(
            &mut net.a,
            net.now,
            2,
            (Kind::Vote, Class::Control),
            RELEASE,
            None,
            PERIOD,
        )
        .unwrap();
    run(&mut net, &mut asker, &mut server);
    assert!(
        asker
            .asked
            .iter()
            .all(|asked| asked.refused.is_none() && asked.read == asked.body)
    );
    assert!(net.b.stats().receive_chunks <= CHUNKS);
}

/// A body blocked by its stream's own window, not by the connection's credit, hears no
/// `Writable` until QUIC says the stream can take more. Before, the endpoint judged writability by
/// the connection's credit alone, so every write the stream refused queued a `Writable` at once: an
/// owner that answers events before reading its socket (hyper-tokio's driver hands out a queued
/// event without a system call) spun on them, never read the peer's window update, and an 8 MiB
/// bulk body stalled for good (seen once in a gate run of hyper-tokio's end-to-end exchanges).
#[test]
fn a_body_blocked_by_its_stream_window_hears_no_writable_until_quic_says_so() {
    let pair = Pair::new();
    let mut net = connected::<Mantle, Mantle>(&pair, limits(), 1 << 30);
    let mut server = Server::new();
    let mut piece = vec![0u8; PIECE];
    // The peer reads as the body arrives until the connection's window has grown past one
    // stream's (`stream_window_ceiling`, about 2.3 MB), then stops reading: the body is held back
    // by its stream's window while the connection still has credit.
    let mut bulk = Slow::open(&mut net, Kind::Snapshot, 7, 512 << 20);
    let mut blocked = false;
    for _ in 0..100_000 {
        server.hold_bulk = bulk.written > 32 << 20;
        let before = bulk.written;
        bulk.write(&mut net.a, &mut piece);
        let credit = net
            .a
            .credit(2, Class::Bulk)
            .is_some_and(|credit| credit > 0);
        if server.hold_bulk && bulk.written == before && credit {
            blocked = true;
            break;
        }
        net.exchange();
        server.serve(&mut net.b);
        while net.a.poll_event().is_some() {}
        if !net.exchange() {
            net.advance();
        }
    }
    assert!(blocked, "the stream's window never held the body back");
    // Nothing has moved on the network since the refused write: no Writable may be queued for it.
    while let Some(event) = net.a.poll_event() {
        assert!(
            !matches!(event, Event::Writable { exchange } if exchange == bulk.exchange),
            "a Writable for a body its stream still refuses"
        );
    }
}

/// A reply of `reply` bytes that node 1 asked for and does not read: node 2 writes the body until
/// no credit is left or all of it is written. Says the net, both sides' exchange, and the bytes
/// node 2's owner wrote.
fn reply_unread(
    reply: u64,
) -> (
    Net<Node<Mantle>, Node<Mantle>>,
    hyper_transport::ExchangeId,
    hyper_transport::ExchangeId,
    u64,
) {
    let pair = Pair::new();
    let mut net = connected::<Mantle, Mantle>(&pair, limits(), 1 << 30);
    let progress = hyper_transport::Progress::new(PERIOD).unwrap();
    let asked = net
        .a
        .open(net.now, 2, Kind::Get, &[7, 1, 2, 3], None, progress)
        .unwrap();
    let mut served = None;
    net.until(TURNS, |net| {
        while let Some(event) = net.b.poll_event() {
            if let Event::Request { exchange, .. } = event {
                served = Some(exchange);
            }
        }
        served.is_some()
    });
    let served = served.expect("the request arrived");
    net.b.reply(served, b"ok", Some(reply)).unwrap();
    let mut piece = vec![0u8; PIECE];
    let mut written = 0u64;
    for _ in 0..TURNS {
        let before = written;
        while written < reply {
            let length = (reply - written).min(PIECE as u64) as usize;
            fill(7, written, &mut piece[..length]);
            match net.b.write_body(served, &piece[..length]).unwrap() {
                0 => break,
                took => written += took as u64,
            }
        }
        let moved = net.exchange();
        while net.a.poll_event().is_some() {}
        while net.b.poll_event().is_some() {}
        if written == reply || (written == before && !moved) {
            break;
        }
    }
    (net, asked, served, written)
}

/// The owner wrote its reply's last byte with the last of the credit, so the trailer the endpoint
/// adds could not go, and ended the exchange: the peer still reads the whole reply, checksum and
/// all. An end that reset the stream refused the peer a message written whole (windows-11-arm,
/// e2e `every exchange done`: `refused: Some((Closed, true))` at 46,154 of 65,536 bytes read).
#[test]
fn a_reply_ended_before_its_trailer_left_still_reaches_the_peer_whole() {
    // How much an unread reply takes before the credit runs out.
    let (_, _, _, room) = reply_unread(REQUEST_BOUND / 2);
    let (mut net, asked, served, written) = reply_unread(room);
    assert_eq!(written, room, "the whole body was written");
    assert_eq!(
        net.b.credit(1, Class::Request),
        Some(0),
        "no credit is left for the trailer"
    );
    net.b.end(served);
    let mut read = 0u64;
    let mut whole = false;
    net.until(TURNS, |net| {
        while let Some(event) = net.a.poll_event() {
            assert!(
                !matches!(event, Event::Refused { exchange, .. } if exchange == asked),
                "the reply was refused: {event:?}"
            );
        }
        loop {
            let mut into = net.a.reserve(Class::Request, PIECE as u64).unwrap();
            let got = net.a.read_body(asked, &mut into).unwrap();
            for (at, byte) in into.bytes().iter().enumerate() {
                assert_eq!(*byte, pattern(7, read + at as u64), "a reply byte");
            }
            net.a.release(into);
            read += got as u64;
            if got == 0 {
                break;
            }
        }
        if read == room && !whole {
            let mut empty = hyper_transport::Reservation::default();
            let _ = net.a.read_body(asked, &mut empty);
            whole = net.a.body_complete(asked);
        }
        whole
    });
    assert_eq!((read, whole), (room, true), "the reply arrived whole");
    net.a.end(asked);
    net.exchange();
    assert!(
        net.b.credit(1, Class::Request).is_some(),
        "node 2 is still connected"
    );
}

/// A server owner whose replies' bodies come from a source that gives it a piece every `step`:
/// it writes what it has as credit takes it, to the exchanges in the order their requests came,
/// so its bodies arrive at the source's rate, not the path's. A reply whose seed is in `withheld`
/// declares its body and is never sent a byte of it.
struct Paced {
    step: Duration,
    next: Instant,
    /// What the source has given and what of it was written.
    given: u64,
    written: u64,
    /// The exchange, its seed, its reply body's length and what of it was written.
    served: Vec<(hyper_transport::ExchangeId, u8, u64, u64)>,
    withheld: Vec<u8>,
    piece: Vec<u8>,
}

impl Paced {
    fn new(step: Duration, now: Instant) -> Self {
        Self {
            step,
            next: now,
            given: 0,
            written: 0,
            served: Vec::new(),
            withheld: Vec::new(),
            piece: vec![0; PIECE],
        }
    }
    /// Answers every request with a body of `reply(seed)` bytes, takes a piece from the source if
    /// its step has come, and writes what it has.
    fn serve(&mut self, node: &mut Node<Mantle>, now: Instant, reply: impl Fn(u8) -> u64) {
        while let Some(event) = node.poll_event() {
            if let Event::Request { exchange, .. } = event {
                let head = node.head(exchange).unwrap().to_vec();
                let mut answer = b"ok:".to_vec();
                answer.extend_from_slice(&head);
                let body = reply(head[0]);
                node.reply(exchange, &answer, Some(body)).unwrap();
                self.served.push((exchange, head[0], body, 0));
            }
        }
        while now >= self.next {
            self.given += PIECE as u64;
            self.next += self.step;
        }
        for (exchange, seed, body, written) in &mut self.served {
            if self.withheld.contains(seed) || *written == *body {
                continue;
            }
            while *written < *body && self.written < self.given {
                let length = (*body - *written).min(self.given - self.written) as usize;
                let length = length.min(PIECE);
                fill(u64::from(*seed), *written, &mut self.piece[..length]);
                let took = node.write_body(*exchange, &self.piece[..length]).unwrap();
                *written += took as u64;
                self.written += took as u64;
                if took < length {
                    return;
                }
            }
            if *written == *body {
                node.end(*exchange);
            } else {
                return;
            }
        }
    }
}

/// Runs node 1's asker against node 2's paced server until every exchange is done.
fn run_paced(
    net: &mut Net<Node<Mantle>, Node<Mantle>>,
    asker: &mut Asker,
    paced: &mut Paced,
    reply: impl Fn(u8) -> u64,
) {
    for _ in 0..1_000_000 {
        asker.drive(&mut net.a);
        paced.serve(&mut net.b, net.now, &reply);
        net.exchange();
        asker.drive(&mut net.a);
        if asker.finished() {
            return;
        }
        if !net.exchange() {
            net.advance_within(paced.step);
        }
    }
    panic!("the exchanges never ended");
}

/// A reply body its owner writes slower than the path would carry it is not refused while it
/// arrives: here 2 MiB at 64 KiB every 250 ms, four periods, each bringing half a megabyte. The
/// answering wait gave a body only its residency at two datagrams a round trip of the path, and at
/// least a period: on a path whose round trip is microseconds that is one period, so a body that
/// took longer was refused at the first judgement that found it unfinished, however much arrived
/// (ubuntu-24.04 at ac10f6f, windows-11-arm at 35d35d8: an 8 MiB bulk reply refused as stalled at
/// 5.6 and 5.5 MB read, its sender CPU-bound on a loaded runner).
#[test]
fn a_reply_written_slower_than_the_path_carries_is_not_refused_while_it_arrives() {
    const BODY: u64 = 2 << 20;
    let pair = Pair::new();
    let mut net = connected::<Mantle, Mantle>(&pair, limits(), 1 << 30);
    let mut asker = Asker::new();
    let mut paced = Paced::new(Duration::from_millis(250), net.now);
    asker
        .ask(
            &mut net.a,
            net.now,
            2,
            (Kind::Snapshot, Class::Bulk),
            3,
            None,
            PERIOD,
        )
        .unwrap();
    let began = net.now;
    run_paced(&mut net, &mut asker, &mut paced, |_| BODY);
    let asked = &asker.asked[0];
    assert_eq!(asked.refused, None, "{asked:?}");
    assert_eq!(asked.read, BODY);
    assert!(net.now - began > PERIOD * 3, "the body took four periods");
}

/// A reply queued behind the peer's other replies is not refused while the connection carries
/// them: sixteen 128 KiB replies, their heads at once and their bodies one after another at 64 KiB
/// every 250 ms, so the last body begins 7.5 s after its head. Reproduced on macOS with three
/// loaded runs at once under background QoS: a reply refused with none of its 65,536 bytes read
/// while the connection received 464 KB in its period.
#[test]
fn a_reply_queued_behind_the_peers_others_is_not_refused_while_they_arrive() {
    const REQUESTS: u8 = 16;
    let pair = Pair::new();
    let mut net = connected::<Mantle, Mantle>(&pair, limits(), 1 << 30);
    let mut asker = Asker::new();
    let mut paced = Paced::new(Duration::from_millis(250), net.now);
    for seed in 0..REQUESTS {
        asker
            .ask(
                &mut net.a,
                net.now,
                2,
                (Kind::Get, Class::Request),
                seed,
                None,
                PERIOD,
            )
            .unwrap();
    }
    run_paced(&mut net, &mut asker, &mut paced, |_| 2 * PIECE as u64);
    for asked in &asker.asked {
        assert_eq!(asked.refused, None, "{asked:?}");
        assert_eq!(asked.read, 2 * PIECE as u64);
    }
}

/// A peer that answers with a head, declares a body and never sends it, while it keeps the
/// connection busy with the replies to the requests that follow, is still given up: once the
/// connection has delivered everything the peer owed, the withheld body has a period more, and no
/// longer, though every period brings a quarter of a megabyte.
#[test]
fn a_body_the_peer_withholds_while_it_sends_others_is_given_up() {
    const OTHERS: usize = 12;
    let pair = Pair::new();
    let mut net = connected::<Mantle, Mantle>(&pair, limits(), 1 << 30);
    let mut asker = Asker::new();
    let mut paced = Paced::new(Duration::from_millis(250), net.now);
    paced.withheld.push(0);
    let reply = |seed: u8| {
        if seed == 0 {
            PIECE as u64
        } else {
            4 * PIECE as u64
        }
    };
    let ask = |asker: &mut Asker, net: &mut Net<Node<Mantle>, Node<Mantle>>, seed: u8| {
        asker
            .ask(
                &mut net.a,
                net.now,
                2,
                (Kind::Get, Class::Request),
                seed,
                None,
                PERIOD,
            )
            .unwrap();
    };
    ask(&mut asker, &mut net, 0);
    ask(&mut asker, &mut net, 1);
    let began = net.now;
    let mut given_up = None;
    for _ in 0..1_000_000 {
        asker.drive(&mut net.a);
        paced.serve(&mut net.b, net.now, reply);
        net.exchange();
        asker.drive(&mut net.a);
        if given_up.is_none() && asker.asked[0].done {
            given_up = Some(net.now - began);
        }
        // A request follows each reply, so the connection is never idle.
        let outstanding = asker.asked[1..].iter().filter(|asked| !asked.done).count();
        if outstanding == 0 && asker.asked.len() <= OTHERS {
            let seed = asker.asked.len() as u8;
            ask(&mut asker, &mut net, seed);
        }
        if asker.finished() {
            break;
        }
        if !net.exchange() {
            net.advance_within(paced.step);
        }
    }
    assert_eq!(asker.asked[0].refused, Some((Refusal::Stalled, false)));
    assert_eq!(asker.asked[0].read, 0);
    for asked in &asker.asked[1..] {
        assert_eq!(asked.refused, None, "{asked:?}");
        assert_eq!(asked.read, 4 * PIECE as u64);
    }
    // Owed at first: the withheld 64 KiB and the first reply's 256 KiB, which with the second
    // reply's are delivered within the first period; the second period, which began with them
    // delivered, ends the exchange, while replies arrive until 12 s.
    let given_up = given_up.unwrap();
    assert!(
        given_up > PERIOD * 2 && given_up < PERIOD * 3,
        "given up after {given_up:?}"
    );
    assert!(
        net.now - began > PERIOD * 5,
        "the others kept the connection busy"
    );
}

/// The frames on the queue of `node`.
fn frames_waiting(net: &Net<Node<Mantle>, Node<Mantle>>) -> usize {
    net.b.stats().events
}

/// An owner that polls nothing is held a lane's window of its frames and no more: the rest wait
/// in QUIC, whose flow control holds the sender, and each frame polled lets one more be read.
#[test]
fn a_lane_holds_an_owner_that_polls_nothing_to_its_window() {
    let pair = Pair::new();
    let mut narrow = limits();
    narrow.lane_window = 8;
    let mut net = connected::<Mantle, Mantle>(&pair, narrow, 256 << 20);
    let mut sent = 0u32;
    while sent < 100 {
        match net.a.send_frame(2, 0, Kind::Append, &sent.to_be_bytes()) {
            Ok(()) => sent += 1,
            Err(Refusal::LaneFull) => {
                net.exchange();
            }
            Err(other) => panic!("{other:?}"),
        }
        assert!(frames_waiting(&net) <= narrow.lane_window);
    }
    net.exchange();
    assert_eq!(
        frames_waiting(&net),
        narrow.lane_window,
        "the lane's window and no more"
    );
    let mut order = Vec::new();
    while let Some(event) = net.b.poll_event() {
        let Event::Frame { frame, .. } = event else {
            panic!("{event:?}")
        };
        order.push(u32::from_be_bytes(frame.bytes().try_into().unwrap()));
        net.b.release(frame);
        let left = 100 - order.len();
        assert_eq!(
            frames_waiting(&net),
            narrow.lane_window.min(left),
            "the poll gave its seat to the next frame"
        );
        net.exchange();
    }
    assert_eq!(order, (0..100).collect::<Vec<_>>(), "in order");
}

/// A peer's frames wait against the peer whatever connection they came on: back on a new one, it
/// has its lanes read only as the owner polls what the old one left.
#[test]
fn a_peer_is_held_to_its_lanes_windows_across_its_connections() {
    let pair = Pair::new();
    let mut narrow = limits();
    narrow.lane_window = 4;
    narrow.lanes_per_peer = 2;
    let mut net = connected::<Mantle, Mantle>(&pair, narrow, 256 << 20);
    let address = net.b_address;
    for index in 0..8u32 {
        net.a
            .send_frame(2, index % 2, Kind::Append, &index.to_be_bytes())
            .unwrap();
    }
    net.exchange();
    let seats = 2 * narrow.lane_window;
    assert_eq!(frames_waiting(&net), seats, "both lanes at their windows");
    net.a.disconnect(net.now, 2);
    net.exchange();
    net.a.connect(net.now, 2, address).unwrap();
    net.until(TURNS, |net| {
        events(&mut net.a)
            .iter()
            .any(|event| matches!(event, Event::Connected { epoch: 2, .. }))
    });
    for index in 8..12u32 {
        net.a
            .send_frame(2, 0, Kind::Append, &index.to_be_bytes())
            .unwrap();
    }
    net.exchange();
    // The old connection's frames and the peer's one lifecycle event, its new connection.
    assert_eq!(frames_waiting(&net), seats + 1, "the new lane is not read");
    let Event::Frame { lane: 0, frame, .. } = net.b.poll_event().unwrap() else {
        panic!("the first to wait is the old connection's first frame")
    };
    net.b.release(frame);
    assert_eq!(frames_waiting(&net), seats + 1, "one seat, one frame read");
    let mut order = Vec::new();
    while let Some(event) = net.b.poll_event() {
        match event {
            Event::Frame { frame, .. } => {
                order.push(u32::from_be_bytes(frame.bytes().try_into().unwrap()));
                net.b.release(frame);
            }
            Event::Connected { peer: 1, epoch, .. } => assert_eq!(epoch, 2),
            other => panic!("{other:?}"),
        }
        assert!(frames_waiting(&net) <= seats + 1);
        net.exchange();
    }
    let new: Vec<u32> = order.iter().copied().filter(|index| *index >= 8).collect();
    assert_eq!(
        new,
        vec![8, 9, 10, 11],
        "the new connection's frames, in order"
    );
    assert_eq!(order.len(), 11);
}

/// An owner that polls nothing holds each exchange's slot with its events: the peer's exchanges
/// past the table are refused, typed, and no more events wait than its slots hold.
#[test]
fn exchanges_wait_in_their_table_for_an_owner_that_polls_nothing() {
    let pair = Pair::new();
    let mut tight = limits();
    tight.exchanges = 4;
    let mut net = connected::<Mantle, Mantle>(&pair, tight, 256 << 20);
    let mut asker = Asker::new();
    for round in 0..10u8 {
        while asker
            .ask(
                &mut net.a,
                net.now,
                2,
                (Kind::Get, Class::Request),
                round,
                None,
                PERIOD,
            )
            .is_ok()
        {}
        net.until(TURNS, |net| {
            asker.drive(&mut net.a);
            assert!(net.b.stats().events <= tight.event_bound());
            asker.asked.iter().all(|asked| asked.done)
        });
    }
    // Each slot holds the request it served and the refusal it ended in.
    assert_eq!(net.b.stats().events, 2 * tight.exchanges);
    assert_eq!(net.b.stats().exchanges, 0, "none is open");
    assert!(
        asker
            .asked
            .iter()
            .any(|asked| asked.refused == Some((Refusal::Exchanges, true))),
        "past the table the peer refused"
    );
    // The requests and refusals the owner now polls are of exchanges long gone.
    assert_eq!(events(&mut net.b).len(), 2 * tight.exchanges);
    let mut server = Server::new();
    asker
        .ask(
            &mut net.a,
            net.now,
            2,
            (Kind::Get, Class::Request),
            99,
            None,
            PERIOD,
        )
        .unwrap();
    run(&mut net, &mut asker, &mut server);
    assert_eq!(
        asker.asked.last().unwrap().refused,
        None,
        "served once polled"
    );
}

/// An owner that reads a body without polling holds one `BodyReady` of it, however often more
/// arrives: a later one says nothing the waiting one does not.
#[test]
fn an_owner_reading_without_polling_holds_one_body_ready() {
    let pair = Pair::new();
    let mut net = connected::<Mantle, Mantle>(&pair, limits(), 256 << 20);
    let body = 4 << 20;
    let mut asker = Asker::new();
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
    let mut served = None;
    net.until(TURNS, |net| {
        asker.drive(&mut net.a);
        served = events(&mut net.b)
            .into_iter()
            .find_map(|event| match event {
                Event::Request { exchange, .. } => Some(exchange),
                _ => None,
            });
        served.is_some()
    });
    let exchange = served.unwrap();
    let mut read = 0u64;
    for _ in 0..TURNS {
        // Everything that has arrived, read without polling.
        loop {
            let mut into = net.b.reserve(Class::Request, PIECE as u64).unwrap();
            let got = net.b.read_body(exchange, &mut into).unwrap() as u64;
            net.b.release(into);
            read += got;
            if got == 0 {
                break;
            }
        }
        assert!(net.b.stats().events <= 1, "one BodyReady waits");
        if net.b.body_complete(exchange) {
            break;
        }
        asker.drive(&mut net.a);
        if !net.exchange() {
            net.advance();
        }
    }
    assert_eq!(read, body);
    assert!(matches!(
        events(&mut net.b)[..],
        [Event::BodyReady { exchange: waiting }] if waiting == exchange
    ));
}

/// A peer that goes and comes back while the owner polls nothing waits as one event: its latest
/// state, with its epoch.
#[test]
fn a_peers_lifecycle_waits_as_its_latest_state() {
    let pair = Pair::new();
    let mut net = connected::<Mantle, Mantle>(&pair, limits(), 256 << 20);
    let address = net.b_address;
    for epoch in 2..=6u64 {
        net.a.disconnect(net.now, 2);
        net.exchange();
        net.a.connect(net.now, 2, address).unwrap();
        net.until(TURNS, |net| {
            events(&mut net.a)
                .iter()
                .any(|event| matches!(event, Event::Connected { epoch: e, .. } if *e == epoch))
        });
    }
    let waiting = events(&mut net.b);
    assert!(
        matches!(
            waiting[..],
            [Event::Connected {
                peer: 1,
                epoch: 6,
                ..
            }]
        ),
        "{waiting:?}"
    );
    net.a.disconnect(net.now, 2);
    net.exchange();
    assert!(
        matches!(
            events(&mut net.b)[..],
            [Event::Closed { peer: 1, epoch: 6 }]
        ),
        "a change after the poll waits again"
    );
}

/// A peer table whose every entry is held by events the owner has not polled takes no new peer:
/// its connection is refused before it is charged, and taken once the owner polls.
#[test]
fn a_peer_table_held_by_unpolled_events_refuses_a_new_peer() {
    let pair = Pair::new();
    let third = pair.pki.issue(&name(3));
    let mut small = limits();
    small.admission.identities = 1;
    small.admission.connections = 1;
    let book = || {
        let mut book = pair.book(Role::Node, Role::Node);
        book.add(&third.0, 3, Role::Node);
        book
    };
    let now = hyper_sim::Anchor::new().instant(0).unwrap();
    let a = pair.node::<Mantle>(1, Role::Node, book(), small, 256 << 20, now);
    let b = pair.node::<Mantle>(2, Role::Node, book(), small, 256 << 20, now);
    let mut net = Net::new(now, a, b);
    let address = net.b_address;
    net.a.connect(now, 2, address).unwrap();
    net.until(TURNS, |net| {
        events(&mut net.a)
            .iter()
            .any(|event| matches!(event, Event::Connected { peer: 2, .. }))
    });
    net.a.disconnect(net.now, 2);
    net.exchange();
    // Peer 3 dials in from where peer 1 was, while peer 1's lifecycle waits for b's owner.
    let config = hyper_transport::Config {
        credentials: credentials(&pair.pki.root, &third.0, &third.1),
        role: Role::Node,
        limits: small,
        listen: true,
    };
    net.a = Node::<Mantle>::new(
        config,
        hyper_transport::Fixed::new(256 << 20, 64),
        book(),
        net.now,
    )
    .unwrap();
    let admitted = net.b.stats().admission.admitted;
    net.a.connect(net.now, 2, address).unwrap();
    net.until(TURNS, |net| {
        events(&mut net.a).iter().any(|event| {
            matches!(
                event,
                Event::Closed { peer: 2, .. } | Event::Unreachable { peer: 2 }
            )
        })
    });
    assert_eq!(
        net.b.stats().admission.admitted,
        admitted,
        "refused, not charged"
    );
    assert_eq!(net.b.connection_stats(3).map(|_| ()), None);
    assert!(
        matches!(
            events(&mut net.b)[..],
            [Event::Closed { peer: 1, epoch: 1 }]
        ),
        "only peer 1's state waited"
    );
    net.a.connect(net.now, 2, address).unwrap();
    net.until(TURNS, |net| {
        events(&mut net.b)
            .iter()
            .any(|event| matches!(event, Event::Connected { peer: 3, .. }))
    });
}

/// Exchanges queued behind the peer's stream credit (rule 2's bound, policy C): a queued exchange is judged on whether
/// the connection's credit queue moves, not on its own age. Do: one stream a connection, eight exchanges asked at once,
/// and a server that answers one every half period, so the last waits about four periods for its stream. Expect: all
/// eight answered whole, none refused, though the last waited several of its own progress periods.
#[test]
fn exchanges_queued_behind_steady_credit_all_complete_past_several_periods() {
    let pair = Pair::new();
    let mut one = limits();
    one.streams_per_connection = 1;
    one.exchanges = 8;
    let mut net = connected::<Mantle, Mantle>(&pair, one, 256 << 20);
    let (mut asker, mut server) = (Asker::new(), Server::new());
    for seed in 0..8 {
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
    let started = net.now;
    let pace = PERIOD / 2;
    let mut next_serve = net.now;
    // The clock moves no faster than a tenth of the pace, so the server answers on time rather than when a transport
    // timer next fires.
    let step = pace / 10;
    for _ in 0..TURNS {
        asker.drive(&mut net.a);
        // Every request is taken as it arrives; the answers are paced, one every half period.
        while let Some(event) = net.b.poll_event() {
            server.on_event(&mut net.b, event);
        }
        if net.now >= next_serve {
            let before = server.answered;
            server.advance_all(&mut net.b);
            if server.answered > before {
                next_serve = net.now + pace;
            }
        }
        asker.drive(&mut net.a);
        if asker.finished() {
            break;
        }
        if !net.exchange() {
            net.advance_within(step);
        }
    }
    assert!(
        asker
            .asked
            .iter()
            .all(|asked| asked.refused.is_none() && asked.read == 1_000),
        "every queued exchange completed: {:?}",
        asker
            .asked
            .iter()
            .map(|asked| (asked.refused, asked.read))
            .collect::<Vec<_>>()
    );
    assert!(
        net.now.duration_since(started) > PERIOD * 3,
        "the last exchange waited past several of its own periods ({:?})",
        net.now.duration_since(started)
    );
}

/// Policy C's other half: a peer that stops granting credit stops the queue, and every exchange queued behind it is
/// refused, typed, within one progress period of the queue's last advance. Do: one stream a connection, four exchanges;
/// the server answers the first and trickles its reply (twice the least progress every half period, so the first is
/// never itself stalled and never frees its stream). Expect: the three queued refused `Stalled` within two periods of
/// the first being given its stream (one period, and the judgement that finds it), while the first is still alive.
#[test]
fn exchanges_queued_behind_withheld_credit_are_refused_within_a_period_of_the_last_advance() {
    let pair = Pair::new();
    let mut one = limits();
    one.streams_per_connection = 1;
    one.exchanges = 4;
    let mut net = connected::<Mantle, Mantle>(&pair, one, 256 << 20);
    let mut asker = Asker::new();
    for seed in 0..4 {
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
    // The first exchange is given the connection's only stream as it opens: the queue's last advance.
    let last_advance = net.now;
    let pace = PERIOD / 2;
    // Shape: twice the least a period must move, every half period.
    let trickle = usize::try_from(LEAST_PROGRESS * 2).unwrap();
    // Shape: far more than the test runs long enough to trickle, so the first exchange never completes.
    let total = LEAST_PROGRESS * 2 * 1_000;
    let step = pace / 10;
    let mut first: Option<(ExchangeId, u64, u64, bool, u64)> = None;
    let mut next_trickle = net.now;
    let mut refused_at: Vec<Option<Instant>> = vec![None; 4];
    let mut piece = vec![0u8; trickle];
    for _ in 0..TURNS {
        asker.drive(&mut net.a);
        while let Some(event) = net.b.poll_event() {
            if let Event::Request { exchange, .. } = event {
                let seed = u64::from(
                    net.b
                        .head(exchange)
                        .and_then(|head| head.first().copied())
                        .unwrap(),
                );
                first = Some((exchange, seed, 0, false, 0));
            }
        }
        if let Some((exchange, seed, read, replied, written)) = &mut first {
            while *read < 1_000 {
                let mut into = net.b.reserve(Class::Request, 1_000).unwrap();
                let got = net.b.read_body(*exchange, &mut into).unwrap();
                net.b.release(into);
                if got == 0 {
                    break;
                }
                *read += got as u64;
            }
            if *read == 1_000 && !*replied {
                let mut head = b"ok:".to_vec();
                head.extend_from_slice(net.b.head(*exchange).unwrap());
                net.b.reply(*exchange, &head, Some(total)).unwrap();
                *replied = true;
            }
            if *replied && net.now >= next_trickle {
                fill(*seed, *written, &mut piece);
                *written += net.b.write_body(*exchange, &piece).unwrap() as u64;
                next_trickle = net.now + pace;
            }
        }
        asker.drive(&mut net.a);
        for (at, asked) in asker.asked.iter().enumerate() {
            if asked.refused.is_some() && refused_at[at].is_none() {
                refused_at[at] = Some(net.now);
            }
        }
        if refused_at.iter().skip(1).all(Option::is_some) {
            break;
        }
        if !net.exchange() {
            net.advance_within(step);
        }
    }
    assert_eq!(
        asker.asked[0].refused, None,
        "the stream holder is still alive"
    );
    assert!(asker.asked[0].read > 0, "and its reply is moving");
    for (at, asked) in asker.asked.iter().enumerate().skip(1) {
        assert_eq!(
            asked.refused.map(|(refusal, _)| refusal),
            Some(Refusal::Stalled),
            "exchange {at}"
        );
        let after = refused_at[at].unwrap().duration_since(last_advance);
        assert!(
            after <= PERIOD * 2,
            "exchange {at} refused {after:?} after the last advance"
        );
    }
}

/// The table's bound, exactly: at the limit every exchange is taken, and one more is refused at `open`, typed, never
/// lost. Do: an exchange limit of six, six asked, then a seventh. Expect: six taken and answered, the seventh refused
/// `Exchanges` when asked.
#[test]
fn exchanges_at_the_limit_are_taken_and_one_past_it_is_refused_at_open() {
    let pair = Pair::new();
    let mut tight = limits();
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
            6,
            Some(1_000),
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
