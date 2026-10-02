//! Elections started by suspicion (timing step L-2, `docs/timing.md` §2.3), each rule on a
//! group in time: a network that delivers every message a fixed one-way latency after it was
//! sent, members that persist what they take at once, and one clock. The owner's part is the
//! harness's: it tells the members what its detectors say, wakes each member after each call and
//! at its deadline, and does nothing else. No member ever ticks.
#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::disallowed_macros,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cognitive_complexity,
    unreachable_pub
)]
mod support;

use std::collections::BTreeMap;
use std::time::Duration;

use hyper_raft::Timing;
use hyper_raft::proto::{
    ConfChangeSingle, ConfChangeTransition, ConfChangeType, ConfChangeV2, ConfState, Message,
    MessageType,
};
use support::{New, Replica, Settings, Store};

/// The one-way latency of the harness's network, nanoseconds: a millisecond.
const LATENCY: u64 = 1_000_000;
/// The timer granularity the span is searched to: a microsecond, finer than any span here.
const GRANULARITY: Duration = Duration::from_micros(1);

/// The timing hyper-timing's law gives a group of `voters` on this network: the split-vote span
/// for all but a crashed leader's voters, on the one-way latency and a vote round of one round
/// trip (nothing is flushed), and the round's tail, one round trip.
fn timing_for(voters: u32) -> (Timing, hyper_timing::Span) {
    let available = if voters - 1 > voters / 2 {
        voters - 1
    } else {
        voters
    };
    let round = Duration::from_nanos(2 * LATENCY);
    let span = hyper_timing::election_span(
        voters,
        available,
        Duration::from_nanos(LATENCY),
        round,
        GRANULARITY,
    )
    .expect("a span");
    (
        Timing {
            span: span.span,
            round,
        },
        span,
    )
}

/// A group in time (the module's documentation).
struct Sim {
    nodes: Vec<Option<New>>,
    /// Messages in flight, by when they arrive and in the order sent.
    flight: BTreeMap<(u64, u64), Message>,
    sent_count: u64,
    now: u64,
    /// Every message sent, in order.
    sent: Vec<Message>,
    /// Members cut off: what is sent to them is lost.
    cut: Vec<u64>,
    /// Who led each term.
    leaders: BTreeMap<u64, u64>,
    /// The members' timing: how long a run may go with nothing moving.
    timing: Timing,
}

impl Sim {
    /// `count` members of a group whose configuration is `boot`, electing by suspicion with
    /// `timing`, each member's draws seeded from `seed`.
    fn new(count: u64, boot: &ConfState, timing: Timing, seed: u64) -> Self {
        let settings = Settings::focal().by_suspicion();
        let nodes = (1..=count)
            .map(|id| {
                let mut node = New::open(
                    id,
                    Store::new(boot.clone()),
                    &settings,
                    seed.wrapping_mul(1_000_003).wrapping_add(id),
                );
                node.raw.set_timing(timing).unwrap();
                Some(node)
            })
            .collect();
        Self {
            nodes,
            flight: BTreeMap::new(),
            sent_count: 0,
            now: 0,
            sent: Vec::new(),
            cut: Vec::new(),
            leaders: BTreeMap::new(),
            timing,
        }
    }
    fn voters(voters: &[u64]) -> ConfState {
        ConfState {
            voters: voters.to_vec(),
            ..ConfState::default()
        }
    }
    fn node(&mut self, id: u64) -> &mut New {
        self.nodes[(id - 1) as usize]
            .as_mut()
            .expect("a member that runs")
    }
    fn peek(&self, id: u64) -> &New {
        self.nodes[(id - 1) as usize]
            .as_ref()
            .expect("a member that runs")
    }
    fn up(&self) -> Vec<u64> {
        (1..=self.nodes.len() as u64)
            .filter(|id| self.nodes[(*id - 1) as usize].is_some())
            .collect()
    }
    /// After a call on `id`, the owner wakes it at its clock and sends what it gives.
    fn settle(&mut self, id: u64) {
        let now = self.now;
        let node = self.node(id);
        node.wake(now);
        let output = node.drain();
        let view = node.view();
        if view.role == 2 {
            let leader = *self.leaders.entry(view.term).or_insert(id);
            assert_eq!(leader, id, "two leaders of term {}", view.term);
        }
        for message in output.messages {
            self.sent.push(message.clone());
            self.sent_count += 1;
            self.flight
                .insert((self.now + LATENCY, self.sent_count), message);
        }
    }
    /// The next event: a message arriving, or a member's deadline. False when there is none.
    fn next(&mut self) -> bool {
        let arrival = self.flight.keys().next().map(|(at, _)| *at);
        let deadline = self
            .up()
            .into_iter()
            .filter_map(|id| self.peek(id).deadline().map(|at| (at, id)))
            .min();
        match (arrival, deadline) {
            (None, None) => false,
            (Some(at), deadline) if deadline.is_none_or(|(due, _)| at <= due) => {
                let key = *self.flight.keys().next().unwrap();
                let message = self.flight.remove(&key).unwrap();
                self.now = self.now.max(at);
                let to = message.to;
                if self.nodes[(to - 1) as usize].is_some() && !self.cut.contains(&to) {
                    self.node(to).step(message);
                    self.settle(to);
                }
                true
            }
            (_, Some((due, id))) => {
                self.now = self.now.max(due);
                self.settle(id);
                true
            }
            (Some(_), None) => unreachable!("matched above"),
        }
    }
    /// How long a run may go with no member's term, role, commit or last index moving before it
    /// is stuck (`docs/sim.md` §4.2): the longest draw of the span, the election's three rounds and
    /// a replication round, from the members' own timing. The test suspects explicitly, so no
    /// detection time is added.
    fn quiet(&self) -> u64 {
        (self.timing.span + self.timing.round * 4).as_nanos() as u64
    }
    fn progress(&self) -> Vec<(u64, u64, u8, u64, u64)> {
        self.up()
            .into_iter()
            .map(|id| {
                let view = self.peek(id).view();
                (id, view.term, view.role, view.commit, view.last_index)
            })
            .collect()
    }
    /// Runs until nothing is in flight and nothing is due; false when it is stuck instead: events
    /// go on and nothing moves for a quiet period.
    fn run(&mut self) -> bool {
        self.run_until(|_| false)
    }
    /// Runs until `done` holds, or until nothing is in flight and nothing is due (then whether
    /// `done` holds); false when it is stuck.
    fn run_until(&mut self, mut done: impl FnMut(&Self) -> bool) -> bool {
        let mut seen = self.progress();
        let mut moved = self.now;
        loop {
            if done(self) {
                return true;
            }
            if !self.next() {
                return done(self) || self.flight.is_empty();
            }
            let now = self.progress();
            if now != seen {
                seen = now;
                moved = self.now;
            } else if self.now > moved + self.quiet() {
                return false;
            }
        }
    }
    /// The clock moves on by `by` with nothing delivered: a member woken now finds what fell due.
    fn pass(&mut self, by: u64) {
        let end = self.now + by;
        while let Some(due) = self
            .up()
            .into_iter()
            .filter_map(|id| self.peek(id).deadline().map(|at| (at, id)))
            .min()
            .filter(|(at, _)| *at <= end)
        {
            self.now = self.now.max(due.0);
            self.settle(due.1);
        }
        self.now = end;
    }
    fn leader(&self) -> Option<u64> {
        self.up()
            .into_iter()
            .filter(|id| self.peek(*id).view().role == 2)
            .max_by_key(|id| self.peek(*id).view().term)
    }
    /// The member stops: what it sent is still in flight, what is sent to it is lost.
    fn stop(&mut self, id: u64) {
        self.nodes[(id - 1) as usize] = None;
    }
    fn suspect(&mut self, at: u64, of: u64) {
        self.node(at).suspect(of);
        self.settle(at);
    }
    fn trust(&mut self, at: u64, of: u64) {
        self.node(at).trust(of);
        self.settle(at);
    }
    fn term(&self, id: u64) -> u64 {
        self.peek(id).view().term
    }
    /// Elects `id`, as an owner that founds a group tells one member to campaign.
    fn found(&mut self, id: u64) {
        self.node(id).campaign();
        self.settle(id);
        assert!(self.run_until(|sim| sim.leader() == Some(id)));
        // Everything in flight lands, and the group goes quiet.
        assert!(self.run(), "the group never went quiet");
    }
    fn count_sent(&self, from: usize, kind: MessageType) -> usize {
        self.sent[from..]
            .iter()
            .filter(|m| m.msg_type == kind)
            .count()
    }
}

/// A group opened together, with no history and no word from any detector, elects: every
/// member knows no leader and draws its delay over the span from when it opened, as followers
/// that suspected their leader together do. Then, its work done, it goes quiet: no member is due
/// to wake, and the clock passing an hour sends nothing (no ticks, no heartbeats).
#[test]
fn a_group_opened_on_no_history_elects_and_then_sleeps() {
    let (timing, _) = timing_for(3);
    for seed in 0..64 {
        let mut sim = Sim::new(3, &Sim::voters(&[1, 2, 3]), timing, seed);
        for id in 1..=3 {
            sim.settle(id);
        }
        assert!(
            sim.run_until(|sim| sim.leader().is_some()),
            "seed {seed}: no leader"
        );
        let leader = sim.leader().unwrap();
        sim.node(leader).propose(b"x".to_vec());
        sim.settle(leader);
        assert!(sim.run(), "seed {seed}: never quiet");
        for id in 1..=3 {
            assert_eq!(sim.peek(id).deadline(), None, "seed {seed}: member {id}");
            assert_eq!(sim.peek(id).app().index, 2, "seed {seed}: member {id}");
        }
        let sent = sim.sent.len();
        let term = sim.term(leader);
        sim.pass(3_600 * 1_000_000_000);
        assert_eq!(sim.sent.len(), sent, "seed {seed}: an idle group sent");
        assert_eq!(sim.term(leader), term);
        assert_eq!(sim.leader(), Some(leader));
    }
}

/// An election starts only on suspicion: with the leader alive and trusted, its followers never
/// campaign however long the clock runs; once their detectors suspect it, they do, and a new
/// leader is elected in the next term.
#[test]
fn elections_start_only_on_suspicion() {
    let (timing, _) = timing_for(3);
    let mut sim = Sim::new(3, &Sim::voters(&[1, 2, 3]), timing, 1);
    sim.found(1);
    let sent = sim.sent.len();
    sim.pass(86_400 * 1_000_000_000);
    assert_eq!(sim.sent.len(), sent, "a day passed and something was sent");
    assert_eq!(sim.leader(), Some(1));
    sim.stop(1);
    sim.pass(86_400 * 1_000_000_000);
    assert_eq!(
        sim.sent.len(),
        sent,
        "nothing suspected the leader, nothing moved"
    );
    for id in [2, 3] {
        sim.suspect(id, 1);
    }
    assert!(sim.run_until(|sim| sim.leader().is_some()));
    let leader = sim.leader().unwrap();
    assert!(leader == 2 || leader == 3);
    assert!(sim.term(leader) > 1);
}

/// The delay from a suspicion to a campaign is the election law's draw, `election_delay` over
/// the span, at the member's seed and its campaign count, exactly; over many members it is
/// uniform on `[0, W)`.
#[test]
fn the_delay_is_the_laws_draw_and_uniform_over_the_span() {
    let (timing, _) = timing_for(3);
    let span = timing.span.as_nanos() as u64;
    let mut bins = [0u32; 10];
    let members = 20_000u64;
    for seed in 0..members {
        let settings = Settings::focal().by_suspicion();
        let local = seed.wrapping_mul(0x2545_f491_4f6c_dd1d);
        let mut node = New::open(2, Store::new(Sim::voters(&[1, 2, 3])), &settings, local);
        node.raw.set_timing(timing).unwrap();
        let opened = 7_000_000_000;
        node.wake(opened);
        let due = node
            .deadline()
            .expect("a member that knows no leader is armed");
        let drawn = hyper_timing::election_delay(timing.span, local, 0);
        assert_eq!(due - opened, drawn.as_nanos() as u64);
        bins[((due - opened) * 10 / span) as usize] += 1;
    }
    // 2,000 a bin; a bin past 2,000 ± 5σ (σ ≈ 42) is not uniform.
    for bin in bins {
        assert!((1_790..=2_210).contains(&bin), "{bins:?}");
    }
}

/// Split votes resolve, and as often as the law says they happen. Five voters; the leader
/// crashes and the four others suspect it at the same instant; each campaigns after its own draw
/// over the span `election_span` chose for this network. A first round fails exactly when three
/// of the four start within the one-way latency of the first (Ongaro, dissertation §9.2), which
/// the span's `split` is the probability of. Over a thousand crashes every group elects, and
/// the share whose first round split is that probability, within a 99.9 % interval.
#[test]
fn split_votes_resolve_as_often_as_the_law_says() {
    let (timing, span) = timing_for(5);
    let trials = 1_000u64;
    let mut split = 0u64;
    let mut terms = 0u64;
    for seed in 0..trials {
        let mut sim = Sim::new(5, &Sim::voters(&[1, 2, 3, 4, 5]), timing, seed);
        sim.found(1);
        let term = sim.term(1);
        sim.stop(1);
        for id in 2..=5 {
            sim.node(id).suspect(1);
        }
        for id in 2..=5 {
            sim.settle(id);
        }
        assert!(
            sim.run_until(|sim| sim.leader().is_some_and(|l| l != 1)),
            "seed {seed}: no leader"
        );
        let elected = sim.term(sim.leader().unwrap());
        terms += elected - term;
        if elected > term + 1 {
            split += 1;
        }
    }
    let expected = span.split * trials as f64;
    let sd = (trials as f64 * span.split * (1.0 - span.split)).sqrt();
    println!(
        "{trials} crashes: {split} first rounds split, {expected:.1} expected (split {:.4}, span {:?}); {terms} terms",
        span.split, span.span
    );
    // 3.29 standard deviations: a two-sided 99.9 % interval.
    assert!(
        (split as f64 - expected).abs() <= 3.29 * sd + 1.0,
        "{split} splits against {expected:.1} ± {:.1}",
        3.29 * sd
    );
}

/// A suspicion withdrawn before the delay ends cancels the campaign: the follower trusts its
/// leader again, nothing is due, and nothing is sent.
#[test]
fn a_suspicion_withdrawn_before_the_delay_cancels_the_campaign() {
    let (timing, _) = timing_for(3);
    let mut sim = Sim::new(3, &Sim::voters(&[1, 2, 3]), timing, 3);
    sim.found(1);
    let sent = sim.sent.len();
    sim.suspect(2, 1);
    let due = sim.peek(2).deadline().expect("armed");
    assert!(due > sim.now, "a delay of zero draws nothing to cancel");
    sim.now += (due - sim.now) / 2;
    sim.trust(2, 1);
    assert_eq!(sim.peek(2).deadline(), None);
    sim.pass(3_600 * 1_000_000_000);
    assert_eq!(sim.sent.len(), sent);
    assert_eq!(sim.term(2), sim.term(1));
}

/// Pre-vote keeps its role: a follower whose detector wrongly suspects the leader asks for
/// pre-votes, the other follower, which trusts the leader, grants none, and the leader tells the
/// asker who leads. No term moves.
#[test]
fn a_member_that_trusts_its_leader_refuses_a_pre_vote() {
    let (timing, _) = timing_for(3);
    let mut sim = Sim::new(3, &Sim::voters(&[1, 2, 3]), timing, 4);
    sim.found(1);
    let from = sim.sent.len();
    let term = sim.term(1);
    sim.suspect(3, 1);
    // While its detector is wrong it asks again, a round and a draw after each ask.
    let asked = |sim: &Sim| sim.count_sent(from, MessageType::MsgRequestPreVote) >= 6;
    assert!(sim.run_until(asked));
    sim.trust(3, 1);
    assert!(
        sim.run(),
        "it trusts its leader again, and the group goes quiet"
    );
    let granted = sim.sent[from..]
        .iter()
        .filter(|m| m.msg_type == MessageType::MsgRequestPreVoteResponse && !m.reject)
        .count();
    assert_eq!(
        granted, 0,
        "a member that trusts its leader granted a pre-vote"
    );
    assert_eq!(sim.leader(), Some(1));
    for id in 1..=3 {
        assert_eq!(sim.term(id), term, "member {id}");
    }
    // The leader told it who leads.
    assert_eq!(sim.peek(3).view().leader, 1);
}

/// A leader steps down once its detectors suspect so many voters that it and those it trusts
/// are no quorum of either half of its configuration: in a simple configuration, a majority;
/// in a joint one, a majority of either half while the other is whole.
#[test]
fn a_leader_steps_down_when_its_detectors_suspect_a_majority_of_either_half() {
    let (timing, _) = timing_for(3);
    let mut sim = Sim::new(3, &Sim::voters(&[1, 2, 3]), timing, 5);
    sim.found(1);
    sim.suspect(1, 2);
    assert_eq!(
        sim.leader(),
        Some(1),
        "one of three suspected leaves a quorum"
    );
    sim.suspect(1, 3);
    assert_eq!(
        sim.peek(1).view().role,
        0,
        "two of three suspected leave none"
    );

    // Joint: {1, 2, 3} and {1, 4, 5}.
    let joint = ConfState {
        voters: vec![1, 4, 5],
        voters_outgoing: vec![1, 2, 3],
        ..ConfState::default()
    };
    for (suspected, stays) in [
        (vec![2], true),
        (vec![4], true),
        (vec![2, 4], true),
        (vec![2, 3], false),
        (vec![4, 5], false),
    ] {
        let mut sim = Sim::new(5, &joint, timing, 6);
        sim.found(1);
        for &member in &suspected {
            sim.suspect(1, member);
        }
        assert_eq!(
            sim.peek(1).view().role == 2,
            stays,
            "suspecting {suspected:?} of a joint configuration"
        );
    }
}

/// A leader that steps down in its term hands over: its followers trust its node, which lives,
/// and would never campaign; the voter that holds the most of its log is told to, and the group
/// elects.
#[test]
fn a_leader_that_steps_down_hands_over() {
    let (timing, _) = timing_for(5);
    let mut sim = Sim::new(5, &Sim::voters(&[1, 2, 3, 4, 5]), timing, 7);
    sim.found(1);
    let from = sim.sent.len();
    // The leader's detectors are wrong about three of its four followers, which trust it.
    for member in [3, 4, 5] {
        sim.suspect(1, member);
    }
    assert_eq!(sim.peek(1).view().role, 0);
    assert_eq!(sim.count_sent(from, MessageType::MsgTimeoutNow), 1);
    assert!(sim.run_until(|sim| sim.leader().is_some()));
    assert_eq!(sim.leader(), Some(2), "the one voter it still trusted");
}

/// A leader that removes itself, with no voter holding its whole log, hands over all the same.
#[test]
fn a_leader_that_removes_itself_hands_over_to_the_voter_that_holds_the_most() {
    let (timing, _) = timing_for(3);
    let mut sim = Sim::new(3, &Sim::voters(&[1, 2, 3]), timing, 8);
    sim.found(1);
    // Member 3 is cut off from what follows, and comes back behind.
    sim.cut.push(3);
    let change = ConfChangeV2 {
        transition: ConfChangeTransition::Auto,
        changes: vec![ConfChangeSingle {
            change_type: ConfChangeType::RemoveNode,
            node_id: 1,
        }],
        context: vec![],
    };
    sim.node(1).propose_change(&change);
    sim.settle(1);
    assert!(sim.run_until(|sim| sim.peek(1).view().role != 2));
    // No voter holds its whole log: the one that holds the most is told to campaign.
    assert!(
        sim.sent
            .iter()
            .any(|m| m.msg_type == MessageType::MsgTimeoutNow && m.from == 1 && m.to == 2)
    );
    sim.cut.clear();
    assert!(sim.run_until(|sim| sim.leader().is_some_and(|l| l != 1)));
    assert_eq!(sim.leader(), Some(2));
}

/// A member whose campaigns its owner holds (its log may lack what it acknowledged, or it is
/// stalled for room) does not campaign whatever its detectors say, and is due for nothing; let
/// go, it campaigns.
#[test]
fn no_campaign_while_held() {
    let (timing, _) = timing_for(3);
    let mut sim = Sim::new(3, &Sim::voters(&[1, 2, 3]), timing, 9);
    sim.found(1);
    sim.stop(1);
    sim.node(2).raw.hold_campaigns(true).unwrap();
    sim.node(3).raw.hold_campaigns(true).unwrap();
    let sent = sim.sent.len();
    sim.suspect(2, 1);
    sim.suspect(3, 1);
    assert_eq!(sim.peek(2).deadline(), None);
    sim.pass(3_600 * 1_000_000_000);
    assert_eq!(sim.sent.len(), sent);
    sim.node(2).raw.hold_campaigns(false).unwrap();
    sim.settle(2);
    assert!(sim.run_until(|sim| sim.leader() == Some(2)));
}

/// A restarted leader its owner holds from campaigning (its log was marked) leads nothing, and
/// its followers trust its node, which lives. It voted for itself in its term, so it may have led
/// it: it hands over until its term moves, and the group elects without it.
#[test]
fn a_restarted_leader_held_from_campaigning_hands_over() {
    let (timing, _) = timing_for(3);
    let mut sim = Sim::new(3, &Sim::voters(&[1, 2, 3]), timing, 10);
    sim.found(1);
    let store = std::mem::take(sim.node(1).store_mut());
    let mut node = New::open(1, store, &Settings::focal().by_suspicion(), 99);
    node.raw.set_timing(timing).unwrap();
    node.raw.hold_campaigns(true).unwrap();
    sim.nodes[0] = Some(node);
    sim.settle(1);
    for id in [2, 3] {
        assert_eq!(sim.peek(id).view().leader, 1, "they trust its node");
    }
    assert!(sim.run_until(|sim| sim.leader().is_some()));
    assert!(sim.leader() != Some(1));
    // Its order is answered once the term has moved, and it stops.
    assert!(sim.run(), "it handed over for ever");
}

/// A follower whose detectors see its leader's node start again forgets it, and campaigns.
#[test]
fn a_follower_forgets_a_leader_that_started_again() {
    let (timing, _) = timing_for(3);
    let mut sim = Sim::new(3, &Sim::voters(&[1, 2, 3]), timing, 12);
    sim.found(1);
    sim.stop(1);
    for id in [2, 3] {
        sim.node(id).raw.restarted(1).unwrap();
        assert_eq!(sim.peek(id).view().leader, 0);
        sim.settle(id);
        assert!(sim.peek(id).deadline().is_some(), "member {id} campaigns");
    }
    assert!(sim.run_until(|sim| sim.leader().is_some()));
}

/// A leader beats only while its group has work in flight: an append lost on its way leaves a
/// follower behind, the leader wakes a round tail later and sends a heartbeat, the answer shows
/// the gap, the entry is sent again, and once every member holds it and its commit the leader is
/// due for nothing.
#[test]
fn a_leader_beats_while_work_is_in_flight_and_then_sleeps() {
    let (timing, _) = timing_for(3);
    let mut sim = Sim::new(3, &Sim::voters(&[1, 2, 3]), timing, 11);
    sim.found(1);
    assert_eq!(sim.peek(1).deadline(), None);
    sim.node(1).propose(b"lost".to_vec());
    sim.settle(1);
    // The append to member 3 is lost.
    let lost: Vec<_> = sim
        .flight
        .iter()
        .filter(|(_, m)| m.to == 3 && m.msg_type == MessageType::MsgAppend)
        .map(|(key, _)| *key)
        .collect();
    assert_eq!(lost.len(), 1);
    sim.flight.remove(&lost[0]);
    let beat = sim
        .peek(1)
        .deadline()
        .expect("work in flight: a beat is due");
    assert_eq!(beat, sim.now + timing.round.as_nanos() as u64);
    let from = sim.sent.len();
    assert!(sim.run());
    assert!(sim.count_sent(from, MessageType::MsgHeartbeat) > 0);
    for id in 1..=3 {
        assert_eq!(sim.peek(id).app().index, 2, "member {id}");
        assert_eq!(sim.peek(id).deadline(), None, "member {id}");
    }
}

/// A sole voter campaigns at once, with no timing at all: it has no one to split a vote with and
/// no leader to suspect.
#[test]
fn a_sole_voter_elects_itself_without_timing() {
    let settings = Settings::focal().by_suspicion();
    let mut node = New::open(1, Store::new(Sim::voters(&[1])), &settings, 1);
    node.wake(0);
    node.drain();
    assert_eq!(node.view().role, 2);
}

/// A member that elects by suspicion takes no ticks, and one on ticks takes no word of its
/// detectors: each is refused, and nothing changes.
#[test]
fn ticks_and_suspicion_do_not_mix() {
    let settings = Settings::focal().by_suspicion();
    let mut node = New::open(1, Store::new(Sim::voters(&[1, 2, 3])), &settings, 1);
    assert!(matches!(
        node.raw.tick(),
        Err(hyper_raft::Error::Settings(_))
    ));
    let mut ticking = New::open(
        1,
        Store::new(Sim::voters(&[1, 2, 3])),
        &Settings::focal(),
        1,
    );
    assert!(matches!(
        ticking.raw.suspect(2),
        Err(hyper_raft::Error::Settings(_))
    ));
    assert_eq!(ticking.raw.deadline(), None);
    assert_eq!(ticking.raw.wake(1), Ok(false));
    // Without pre-vote and check-quorum, elections by suspicion are refused at open.
    let config = hyper_raft::Config {
        elections: hyper_raft::Elections::Suspicion,
        ..hyper_raft::Config::new(1)
    };
    assert!(matches!(
        config.validate(),
        Err(hyper_raft::Error::Settings(_))
    ));
}
