//! The SWIM failure detector, its timing measured (`docs/timing.md` §2.7): the protocol-period
//! machine that probes members and drives the [`Membership`] view from alive to suspect to dead.
//! Sans-io: it is fed `now`, acknowledgements and pings, and returns the pings and ping-requests to
//! send and the time to be polled again ([`wake`](Detector::wake)).
//!
//! Evidence: SWIM (Das, Gupta and Motivala, DSN 2002) for the probe, the indirect probe, the
//! suspicion and infection-style dissemination; Lifeguard (Dadgar, Phillips and Currey, DSN 2018)
//! for the buddy system and for local health; Chen, Toueg and Aguilera (2002) for the detector each
//! probe stream is; `docs/research/timing.md` holds what each establishes.
//!
//! **Each pair is an NFD-E detector.** A member's probes to one peer and that peer's
//! acknowledgements are a heartbeat stream on the member's own clock: probe `k` sent at `s_k`,
//! answered at `A_k`, so NFD-E's delay `A_k − σ_k` is the probe's round trip, with no second clock
//! in it. A [`LinkEstimator`] per peer holds its mean, variance, loss, correlation and window, and
//! [`detector_at`] chooses the margin `α` that minimizes unavailability at the pair's probe
//! interval. A probe's acknowledgement is due at `s + μ + α`; if none came, the indirect probe asks
//! relays, and a probe answered by neither suspects the peer. The period is what its probe needs:
//! the direct deadline, and the indirect one when the direct passed unanswered (SWIM §3.1: the
//! protocol's properties hold for the average period).
//!
//! **Before a pair can be judged.** A pair's estimator refuses until it has its evidence
//! ([`Refusal`]). Its probes are then judged by the member's pooled estimator, every round trip the
//! member measured to anyone (the hosts' stalls, which the traces found dominate, are in it); and
//! while that too refuses, nobody is judged: a probe is measurement only. Its period ends when it
//! is answered or at its expected arrival from the latest round trip (NFD-E's estimate over a
//! window of one), whichever is first; unanswered then, it is a loss to the estimators unless its
//! answer comes later, and it judges nothing. Its wake measures the member's timer. Before any
//! round trip, the first probe waits on its answer or on another member.
//!
//! **Dead.** A suspected peer is told by the member's next probe to it (Lifeguard's buddy system);
//! it is condemned when that probe also goes unanswered, and only once the member has since had an
//! answer from someone else, so a member whose own network has failed condemns nobody (Lifeguard's
//! local health, decided by evidence). A gossiped suspicion is a hint: only a member's own probes
//! condemn. A member with nobody alive or suspected left probes the members it holds dead and
//! tells each so; a live one refutes in its answer.
//!
//! **The member's own lateness** is measured, not multiplied: every wake the member is late for is
//! folded into its granularity `G` ([`Wakes`]), which floors the margins; the member's own
//! delay in reading acknowledgements is in the round trips it measures; and a probe is resolved
//! when the member wakes, with every acknowledgement delivered by then, so a late member does not
//! blame its peers for its own lateness.
//!
//! Coordinate-aware indirect probing: the detector carries a Vivaldi coordinate engine
//! ([`crate::coordinates`]) fed by the round trips it measures and the coordinates it learns for
//! peers ([`learn_coordinate`](Detector::learn_coordinate)). Indirect-probe relays are chosen
//! nearest the target in coordinate space, with a deterministic id order where coordinates are
//! unknown.

use std::collections::BTreeMap;
use std::time::Duration;

use hyper_timing::{
    Costs, Exposure, Floors, LinkBehaviour, LinkEstimator, Refusal, Schedule, Wakes, detector_at,
    mistake_bound,
};

use crate::HostId;

use crate::codec::Coordinate;
use crate::coordinates::{CoordinateEngine, NetworkCoordinate};
use crate::extension::{ExtensionDecision, ExtensionDenial, ExtensionTracker};
use crate::gossip::Gossip;
use crate::membership::{Change, Liveness, MemberState, Membership};

/// A ping to send to `to` — the period's probe of it, or a probe relayed for another member.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ping {
    /// The member to probe.
    pub to: HostId,
    /// The nonce the acknowledgement echoes.
    pub nonce: u64,
}

/// An acknowledgement to send to `to` — the reply to a received [`Ping`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ack {
    /// The member that pinged us.
    pub to: HostId,
}

/// A ping-request: ask `relay` to ping `target` on our behalf and relay the acknowledgement back,
/// echoing `nonce`. SWIM sends these when a direct ping goes unanswered, so a lost packet on the
/// direct path is not taken for a failed member.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PingReq {
    /// The peer asked to probe on our behalf.
    pub relay: HostId,
    /// The member to probe indirectly.
    pub target: HostId,
    /// The nonce of the probe the request is for.
    pub nonce: u64,
}

/// The detector configured for a probe: NFD-E's margin at the pair's interval, and what it
/// promises (`docs/timing.md` §2.7).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Verdict {
    /// The mean round trip `μ`, the expected arrival's offset from the probe.
    pub round_trip: Duration,
    /// The margin `α` past it: the acknowledgement is due at `s + μ + α`.
    pub margin: Duration,
    /// The pair's probe interval `η` it was configured at: the round, `m` periods.
    pub interval: Duration,
    /// The loss the configurator was fed: lost, or later than anything the history has seen.
    pub loss: f64,
    /// Theorem 7's bound on the probability that a probe of a live peer goes unanswered by its
    /// deadline, `(V + p·α²)/(V + α²)`: the margin holds one probe.
    pub mistake: f64,
}

impl Verdict {
    /// `μ + α` in nanoseconds: from the probe to its deadline.
    fn span_ns(&self) -> u64 {
        nanos(self.round_trip.saturating_add(self.margin))
    }
}

/// What a member has done and promised about one peer, for its owner and its tests.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PeerReport {
    /// Whether the pair's own estimator configures its probes (rather than the pool's, or none).
    pub configured: bool,
    /// Suspicions this member's own probes started.
    pub suspicions: u64,
    /// Theorem 7's allowance for them: `Σβ` over every judged probe, the expected number of
    /// suspicions of the peer were it alive throughout.
    pub suspicion_allowance: f64,
    /// Times this member condemned the peer by its own probes.
    pub condemnations: u64,
    /// The allowance for condemnations of a live peer: over every judged probe, the bound on it
    /// and the probe before both going unanswered.
    pub condemnation_allowance: f64,
    /// From the peer's last answer to its condemnation, on this member's clock.
    pub condemned_after: Option<Duration>,
    /// The detection bound the member stated when it condemned: [`Detector::detection_bound`]
    /// plus the wait it measured for an answer from another member.
    pub condemned_within: Option<Duration>,
    /// When the peer last answered one of this member's probes, on the caller's clock.
    pub last_answer_ns: Option<u64>,
    /// When the probe that told the peer it was suspected went unanswered, on the caller's clock:
    /// from then the condemnation waits on an answer from another member, which nothing bounds in
    /// advance. Kept once the peer is dead; cleared when it is alive again.
    pub pending_since_ns: Option<u64>,
}

/// Probes of one peer that may still be answered: the one that suspected it, the one that told it,
/// and the one more an extension can grant (at most one base window of one probe,
/// [`crate::extension`]). An older probe's acknowledgement is past every deadline that could use
/// it and is not kept for.
const OUTSTANDING: usize = 3;

/// A probe sent and not yet answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Sent {
    seq: u64,
    nonce: u64,
    at_ns: u64,
}

/// A stream of round trips and the verdict configured from it: one peer's, or the pool's.
#[derive(Debug, Default)]
struct Stream {
    /// Boxed: the estimator's Allan levels are a kilobyte, and a peer's other fields are read
    /// every period, so they stay small and together (the allocation is made with the ring's).
    estimator: Option<Box<LinkEstimator>>,
    /// The sequence number the estimator was anchored at.
    anchor: u64,
    /// The granularity the estimator was last given, nanoseconds.
    granularity_ns: u64,
    verdict: Option<Verdict>,
    /// Round trips taken.
    samples: u64,
    /// Round trips taken at the last configuration, and the window then: the verdict is renewed
    /// once a window's worth more have come (Chen et al.'s adaptive detector, §6).
    configured_at: u64,
    renewal: u64,
}

impl Stream {
    /// Takes the round trip of heartbeat `seq`, building the estimator at the stream's interval
    /// on the first.
    fn take(&mut self, seq: u64, rtt: u64, granularity: Duration, interval: Duration) {
        if self.estimator.is_none() {
            self.anchor = seq;
            self.granularity_ns = nanos(granularity);
            self.estimator =
                LinkEstimator::new(interval, granularity, Some(Schedule { seq, at_ns: 0 }))
                    .ok()
                    .map(Box::new);
        }
        let Some(estimator) = self.estimator.as_mut() else {
            return;
        };
        if nanos(granularity) != self.granularity_ns {
            self.granularity_ns = nanos(granularity);
            estimator.set_granularity(granularity);
        }
        // A sample from before the anchor, or past what a window can sum, is not taken.
        if feed(estimator, seq, self.anchor, rtt).is_ok() {
            self.samples = self.samples.saturating_add(1);
        }
    }

    /// Whether the verdict is due: never configured, or a window's worth of samples since.
    fn due(&self) -> bool {
        self.verdict.is_none() || self.samples.saturating_sub(self.configured_at) >= self.renewal
    }

    /// Configures the verdict from the estimates as they stand. A refusal leaves the verdict in
    /// force, as `LinkEstimator::configure` leaves its margin: a stall can make `τ_int` unmeasured
    /// again, and a probe that went unjudged then would suspect nobody.
    fn configure(&mut self, mtbf: Option<Duration>, floors: &Floors, interval: Duration) {
        let Some(estimator) = self.estimator.as_ref() else {
            return;
        };
        self.configured_at = self.samples;
        self.renewal = estimator.estimates().window.length;
        if let Ok(renewed) = verdict(estimator, mtbf, floors, interval) {
            self.verdict = Some(renewed);
        }
    }
}

/// What this member holds about one peer.
#[derive(Debug)]
struct Peer {
    stream: Stream,
    /// Probes sent to the peer.
    sent: u64,
    outstanding: [Option<Sent>; OUTSTANDING],
    /// The probes sent when the current suspicion took hold: a probe numbered from it on told the
    /// peer (it carried the suspicion).
    suspected_from: Option<u64>,
    told_missed: u32,
    /// When the probe that told the peer went unanswered: its condemnation waits on an answer
    /// from another member.
    pending_since: Option<u64>,
    /// The previous judged probe's bound.
    last_mistake: Option<f64>,
    last_answer_ns: Option<u64>,
    report: PeerReport,
}

impl Peer {
    fn new() -> Self {
        Self {
            stream: Stream::default(),
            sent: 0,
            outstanding: [None; OUTSTANDING],
            suspected_from: None,
            told_missed: 0,
            pending_since: None,
            last_mistake: None,
            last_answer_ns: None,
            report: PeerReport::default(),
        }
    }

    /// The outstanding probe `nonce`, taken.
    fn take(&mut self, nonce: u64) -> Option<Sent> {
        self.outstanding
            .iter_mut()
            .find(|slot| slot.is_some_and(|sent| sent.nonce == nonce))
            .and_then(Option::take)
    }

    /// Records a probe sent, over the oldest still outstanding.
    fn send(&mut self, sent: Sent) {
        let slots = u64::try_from(OUTSTANDING).unwrap_or(1);
        let at = usize::try_from(sent.seq.checked_rem(slots).unwrap_or(0)).unwrap_or(0);
        if let Some(slot) = self.outstanding.get_mut(at) {
            *slot = Some(sent);
        }
    }

    fn clear_pending(&mut self) {
        self.told_missed = 0;
        self.pending_since = None;
        self.report.pending_since_ns = None;
    }

    fn clear_suspicion(&mut self) {
        self.suspected_from = None;
        self.clear_pending();
    }
}

/// The period's probe.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Probe {
    target: HostId,
    nonce: u64,
    seq: u64,
    sent_ns: u64,
    answered: bool,
    verdict: Option<Verdict>,
    /// When the indirect probe's answers are due, once it was asked.
    indirect_until: Option<u64>,
    /// A measurement probe's expected arrival from the latest round trip: where its period ends,
    /// answered or not, judging nothing; its wake measures the member's timer.
    expected: Option<u64>,
}

impl Probe {
    fn due_ns(&self) -> Option<u64> {
        self.verdict
            .map(|verdict| self.sent_ns.saturating_add(verdict.span_ns()))
    }
}

/// The member's periods: count, total and longest, nanoseconds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Periods {
    count: u64,
    total: u128,
    longest: u64,
}

impl Periods {
    fn add(&mut self, length: u64) {
        self.count = self.count.saturating_add(1);
        self.total = self.total.saturating_add(u128::from(length));
        self.longest = self.longest.max(length);
    }

    fn mean_ns(&self) -> Option<u64> {
        self.total
            .checked_div(u128::from(self.count))
            .and_then(|mean| u64::try_from(mean).ok())
            .filter(|mean| *mean > 0)
    }
}

/// `duration` in nanoseconds, saturating at `u64::MAX` (584 years).
fn nanos(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

/// SWIM §4.1's dissemination budget for `members`: an update piggybacked for `λ·ln n` periods
/// leaves at most `n^{−((2−4/n)λ−2)}` members uninfected in expectation, which is below one member
/// once `λ > n/(n−2)`. The budget is the least whole count past `n·ln n/(n−2)`. With two members
/// or fewer every message reaches the only other one, and one transmission is all there is to
/// send. The logarithm is the fixed-point one ([`crate::fixed`]), so every host computes the same
/// budget.
pub fn gossip_transmits(members: usize) -> u32 {
    let n = u64::try_from(members).unwrap_or(u64::MAX);
    if n <= 2 {
        return 1;
    }
    let fraction = f64::from(1u32 << crate::fixed::FRACTION_BITS);
    // u64 → f64 rounds only past 2⁵³ members.
    let ln = crate::fixed::log2_fixed(n) as f64 / fraction * std::f64::consts::LN_2;
    let rounds = n as f64 * ln / (n.saturating_sub(2)) as f64;
    // `rounds` is at most about 45 (ln 2⁶⁴ with n/(n−2) near one), so the count ends quickly.
    let mut budget = 1u32;
    while f64::from(budget) <= rounds && budget < u32::MAX {
        budget = budget.saturating_add(1);
    }
    budget
}

/// The relays an indirect probe asks: the fewest whose paths together fail no more often than the
/// direct probe did. A relayed probe is two round trips, so with per-probe loss `p` one relay fails
/// with `1 − (1 − p)²`; `k` relays all fail with that to the `k`th, and the retry is at least as
/// reliable as the try it backs up once that is at most `p`. Bounded by the relays there are.
fn relay_count(loss: f64, available: usize) -> usize {
    let through = 1.0 - (1.0 - loss) * (1.0 - loss);
    let mut all_fail = through;
    let mut count = 1usize;
    while all_fail > loss && count < available {
        all_fail *= through;
        count = count.saturating_add(1);
    }
    count.min(available)
}

/// The failure detector for one node: its [`Membership`] view, its probe rotation, the period's
/// probe, and per peer the estimator and verdict that time its probes.
pub struct Detector {
    membership: Membership,
    local: HostId,
    order: Vec<HostId>,
    cursor: usize,
    probe: Option<Probe>,
    /// Whether another member was heard from during a measurement probe.
    heard_other: bool,
    peers: BTreeMap<HostId, Peer>,
    /// Every round trip the member measured, to anyone: the judge of a pair that cannot configure
    /// yet. Its sequence numbers are the member's probe nonces, so a probe never answered is a
    /// loss.
    pool: Stream,
    gossip: Gossip,
    transmits: u32,
    shuffler: RandomizedOrder,
    coordinates: CoordinateEngine,
    peer_coordinates: BTreeMap<HostId, NetworkCoordinate>,
    /// Probes this member has sent: the next probe's nonce.
    nonce: u64,
    /// Probes relayed for others, counted down from the top of the nonce space so they never meet
    /// the member's own.
    relayed: u64,
    /// The wakes asked of the caller and how late each came: `G` and the latest lateness.
    wakes: Wakes,
    /// The latest round trip measured, to anyone.
    last_rtt_ns: Option<u64>,
    /// The longest span `μ + α` any verdict of this member has had, nanoseconds.
    longest_span: u64,
    periods: Periods,
    exposure: Exposure,
    /// Protocol periods run: the clock an extension's rate limit counts in.
    period: u64,
    /// The extensions granted to each suspected member (mantle note 32 S13).
    extensions: BTreeMap<HostId, ExtensionTracker>,
    /// The suspects a condemnation visits, held across periods so it allocates nothing once grown.
    aging: Vec<(HostId, u64)>,
    /// The relays an indirect probe ranks, held for the same reason.
    relays: Vec<HostId>,
}

/// A deterministic pseudo-random order over the members to probe (SWIM §4.3): each round probes a
/// fresh shuffled permutation, so every member is probed once a round and successive probes of one
/// member are at most `2m − 1` periods apart. Seeded from the node id, so a simulation replays.
struct RandomizedOrder {
    state: u64,
}

/// Format: the golden-ratio odd constant (2^64 / φ), the standard seed mixer, so distinct node ids seed
/// visibly different sequences.
const SEED_MIXER: u64 = 0x9E37_79B9_7F4A_7C15;
/// Format: Marsaglia's xorshift64 shift triple (`Xorshift RNGs`, 2003) — the three shifts of the
/// full-period 64-bit generator.
const XORSHIFT_TRIPLE: [u32; 3] = [13, 7, 17];

impl RandomizedOrder {
    /// A generator seeded from `local`, never zero (xorshift stays at zero forever from a zero seed).
    fn seeded(local: HostId) -> RandomizedOrder {
        RandomizedOrder {
            state: (local.0 ^ SEED_MIXER) | 1,
        }
    }

    /// The next pseudo-random word (xorshift64).
    fn next(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << XORSHIFT_TRIPLE[0];
        x ^= x >> XORSHIFT_TRIPLE[1];
        x ^= x << XORSHIFT_TRIPLE[2];
        self.state = x;
        x
    }

    /// Shuffles `items` in place with a Fisher–Yates pass driven by the generator.
    fn shuffle(&mut self, items: &mut [HostId]) {
        let len = items.len();
        for index in (1..len).rev() {
            let span = u64::try_from(index).unwrap_or(0).saturating_add(1);
            let pick = usize::try_from(self.next().checked_rem(span).unwrap_or(0)).unwrap_or(0);
            items.swap(index, pick);
        }
    }
}

/// Where the period's probe stands.
enum Stage {
    /// Waiting for an answer or a deadline.
    Wait,
    /// The direct deadline passed unanswered: ask relays.
    Indirect,
    /// The period is over.
    Over,
}

impl Detector {
    /// A detector for `local`, with the fleet's failure history so far (`history`, the node time it
    /// has run and the failures it has had; [`Exposure::new`] for a fleet with none).
    pub fn new(local: HostId, history: Exposure) -> Detector {
        Detector {
            membership: Membership::new(local),
            local,
            order: Vec::new(),
            cursor: 0,
            probe: None,
            heard_other: false,
            peers: BTreeMap::new(),
            pool: Stream::default(),
            gossip: Gossip::default(),
            transmits: 1,
            shuffler: RandomizedOrder::seeded(local),
            coordinates: CoordinateEngine::new(),
            peer_coordinates: BTreeMap::new(),
            nonce: 0,
            relayed: u64::MAX,
            wakes: Wakes::new(),
            last_rtt_ns: None,
            longest_span: 0,
            periods: Periods::default(),
            exposure: history,
            period: 0,
            extensions: BTreeMap::new(),
            aging: Vec::new(),
            relays: Vec::new(),
        }
    }

    /// Advances the detector to `now_ns` (the caller's monotonic clock): ends the period when its
    /// probe is answered past its deadline, or unanswered past the indirect probe's; asks relays
    /// when the direct deadline passes unanswered (`requests`, replaced); and starts the next
    /// period, returning its [`Ping`] for the caller to send now. Called at every
    /// [`wake`](Detector::wake) and after every message the caller feeds in.
    pub fn poll(&mut self, now_ns: u64, requests: &mut Vec<PingReq>) -> Option<Ping> {
        requests.clear();
        self.wakes.woke(now_ns);
        let ping = match self.stage(now_ns) {
            Stage::Wait => None,
            Stage::Indirect => {
                self.request_indirect(now_ns, requests);
                if requests.is_empty() {
                    self.next_period(now_ns)
                } else {
                    None
                }
            }
            Stage::Over => self.next_period(now_ns),
        };
        self.wakes.ask(self.wake());
        ping
    }

    /// When to [`poll`](Detector::poll) next, on the caller's clock: the probe's deadline, or the
    /// indirect probe's once asked. `None` while a probe is measurement only (poll on the next
    /// message), or before the first poll.
    pub fn wake(&self) -> Option<u64> {
        let probe = self.probe?;
        if probe.verdict.is_none() {
            return probe.expected.filter(|_| !probe.answered);
        }
        if probe.answered {
            return probe.due_ns();
        }
        probe.indirect_until.or_else(|| probe.due_ns())
    }

    fn stage(&self, now_ns: u64) -> Stage {
        let Some(probe) = self.probe else {
            return Stage::Over;
        };
        let due = probe.due_ns();
        match (probe.answered, due, probe.indirect_until) {
            // Measurement: over when answered or at its expected arrival; before any round trip,
            // when another member is heard from.
            (true, None, _) => Stage::Over,
            (false, None, _) => match probe.expected {
                Some(at) if now_ns < at => Stage::Wait,
                Some(_) => Stage::Over,
                None if self.heard_other => Stage::Over,
                None => Stage::Wait,
            },
            (true, Some(due), _) | (false, Some(due), None) if now_ns < due => Stage::Wait,
            (true, Some(_), _) => Stage::Over,
            (false, Some(_), None) => Stage::Indirect,
            (false, Some(_), Some(until)) if now_ns < until => Stage::Wait,
            (false, Some(_), Some(_)) => Stage::Over,
        }
    }

    /// Resolves the period's probe and starts the next.
    fn next_period(&mut self, now_ns: u64) -> Option<Ping> {
        if let Some(probe) = self.probe.take() {
            self.resolve(probe, now_ns);
        }
        self.period = self.period.saturating_add(1);
        let membership = &self.membership;
        self.extensions.retain(|host, _| {
            membership.state(*host).map(|state| state.liveness) == Some(Liveness::Suspect)
        });
        self.start(now_ns)
    }

    /// Starts a probe of the next member in the rotation at `now_ns`.
    fn start(&mut self, now_ns: u64) -> Option<Ping> {
        let target = self.next_target()?;
        let nonce = self.nonce;
        self.nonce = self.nonce.saturating_add(1);
        let own = self.peers.get(&target).and_then(|peer| peer.stream.verdict);
        let verdict = match own {
            Some(own) => Some(own),
            None => self.pooled(),
        };
        let peer = self.peers.entry(target).or_insert_with(Peer::new);
        let seq = peer.sent;
        peer.sent = peer.sent.saturating_add(1);
        peer.send(Sent {
            seq,
            nonce,
            at_ns: now_ns,
        });
        self.probe = Some(Probe {
            target,
            nonce,
            seq,
            sent_ns: now_ns,
            answered: false,
            verdict,
            indirect_until: None,
            expected: self.last_rtt_ns.map(|rtt| now_ns.saturating_add(rtt)),
        });
        self.heard_other = false;
        Some(Ping { to: target, nonce })
    }

    /// Fills `requests` with the relays to ask for the period's probe, and sets when their answers
    /// are due: the slowest relay's own deadline span (the leg to it and back) plus the target's
    /// (the relay's leg to the target, whose stalls are the target's own).
    fn request_indirect(&mut self, now_ns: u64, requests: &mut Vec<PingReq>) {
        let Some(mut probe) = self.probe else {
            return;
        };
        let Some(verdict) = probe.verdict else {
            return;
        };
        self.rank_relays(probe.target);
        let count = relay_count(verdict.loss, self.relays.len());
        let mut slowest = 0u64;
        for &relay in self.relays.iter().take(count) {
            requests.push(PingReq {
                relay,
                target: probe.target,
                nonce: probe.nonce,
            });
            let span = self
                .peers
                .get(&relay)
                .and_then(|peer| peer.stream.verdict)
                .or(self.pool.verdict)
                .map_or(verdict.span_ns(), |relayed| relayed.span_ns());
            slowest = slowest.max(span);
        }
        let until = now_ns
            .saturating_add(slowest)
            .saturating_add(verdict.span_ns());
        probe.indirect_until = Some(until);
        self.probe = Some(probe);
    }

    /// Ranks the alive peers other than `target` as relays, nearest the target first.
    fn rank_relays(&mut self, target: HostId) {
        let mut relays = std::mem::take(&mut self.relays);
        relays.clear();
        relays.extend(
            self.membership
                .alive()
                .filter(|host| *host != self.local && *host != target),
        );
        relays.sort_by(|a, b| {
            match (
                self.predicted_between(*a, target),
                self.predicted_between(*b, target),
            ) {
                (Some(x), Some(y)) => x.total_cmp(&y).then(a.0.cmp(&b.0)),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => a.0.cmp(&b.0),
            }
        });
        self.relays = relays;
    }

    /// The period ends: its length is folded, the exposure grows, and an unanswered judged probe
    /// suspects its target or, when it had told the target, condemns it.
    fn resolve(&mut self, probe: Probe, now_ns: u64) {
        let length = now_ns.saturating_sub(probe.sent_ns);
        self.periods.add(length);
        let watched = u32::try_from(self.order.len()).unwrap_or(u32::MAX);
        self.exposure
            .on_exposure(Duration::from_nanos(length).saturating_mul(watched));
        let Some(peer) = self.peers.get_mut(&probe.target) else {
            return;
        };
        Self::account(peer, probe.verdict);
        if probe.answered {
            peer.clear_pending();
            self.condemn_pending(probe.target, now_ns);
        } else if probe.verdict.is_some() {
            self.missed(probe, now_ns);
        }
    }

    /// Theorem 7's allowance for a judged probe: its bound for a suspicion, and for a
    /// condemnation the bound on it and the previous probe both missing, the lesser of the two
    /// (Fréchet's bound). Not their product: a pair's probes are a few periods apart, inside the
    /// stalls' correlation time the traces measured (20 to 250 ms, `docs/timing.md` §2.6), and a
    /// short history's `τ_int` of one has not yet seen a stall.
    fn account(peer: &mut Peer, verdict: Option<Verdict>) {
        let Some(verdict) = verdict else {
            peer.last_mistake = None;
            return;
        };
        let report = &mut peer.report;
        report.suspicion_allowance += verdict.mistake;
        if let Some(previous) = peer.last_mistake {
            report.condemnation_allowance += previous.min(verdict.mistake);
        }
        peer.last_mistake = Some(verdict.mistake);
    }

    /// A judged probe went unanswered.
    fn missed(&mut self, probe: Probe, now_ns: u64) {
        let Some(state) = self.membership.state(probe.target) else {
            return;
        };
        match state.liveness {
            Liveness::Alive => {
                if let Some(peer) = self.peers.get_mut(&probe.target) {
                    peer.report.suspicions = peer.report.suspicions.saturating_add(1);
                }
                self.record(
                    probe.target,
                    MemberState {
                        liveness: Liveness::Suspect,
                        incarnation: state.incarnation,
                    },
                );
            }
            Liveness::Suspect => {
                let granted = self
                    .extensions
                    .get(&probe.target)
                    .map_or(0, ExtensionTracker::total);
                if let Some(peer) = self.peers.get_mut(&probe.target)
                    && peer.suspected_from.is_some_and(|from| probe.seq >= from)
                {
                    peer.told_missed = peer.told_missed.saturating_add(1);
                    if peer.told_missed > granted && peer.pending_since.is_none() {
                        peer.pending_since = Some(now_ns);
                        peer.report.pending_since_ns = Some(now_ns);
                    }
                }
            }
            Liveness::Dead => {}
        }
    }

    /// An answer from `answered` proves this member's own network works: every suspect whose told
    /// probe went unanswered is condemned now.
    fn condemn_pending(&mut self, answered: HostId, now_ns: u64) {
        let mut suspects = std::mem::take(&mut self.aging);
        suspects.clear();
        suspects.extend(self.membership.suspects());
        let mut bound = None;
        for &(host, incarnation) in &suspects {
            let Some(peer) = self.peers.get_mut(&host) else {
                continue;
            };
            let Some(pending) = peer.pending_since else {
                continue;
            };
            if host == answered {
                continue;
            }
            let after = peer
                .last_answer_ns
                .map(|at| Duration::from_nanos(now_ns.saturating_sub(at)));
            // The bound to the pending condemnation, and the wait for an answer from another
            // member, measured as it happened.
            let within = bound
                .get_or_insert_with(|| self.detection_bound(now_ns))
                .map(|bound| {
                    bound.saturating_add(Duration::from_nanos(now_ns.saturating_sub(pending)))
                });
            if let Some(peer) = self.peers.get_mut(&host) {
                peer.report.condemnations = peer.report.condemnations.saturating_add(1);
                peer.report.condemned_after = after;
                peer.report.condemned_within = within;
            }
            self.record(
                host,
                MemberState {
                    liveness: Liveness::Dead,
                    incarnation,
                },
            );
        }
        self.aging = suspects;
    }

    /// The bound on the time from a peer's last answer to this member's condemnation of it pending,
    /// were it to crash then, `m` the members the view holds besides this one: its next probe is
    /// at most `2m − 1` periods away (SWIM §4.3),
    /// unanswered it suspects; the probe that tells it starts at most as far again, and when its
    /// own period resolves it unanswered the condemnation is pending. It then waits on an answer from another member, the evidence that
    /// this member's own network works, which nothing bounds in advance: the member measures that
    /// wait ([`PeerReport::pending_since_ns`]) and adds it. A period lasts at most the longest this
    /// member has run, or, where longer, what an unanswered probe's deadlines allow: its target's
    /// span, then the slowest relay's and the target's again, at most three times the longest
    /// span any of its verdicts has had, plus the latest this member has woken past a wake it
    /// asked, or is late for now; the period in progress counts as run. `None` before a period.
    pub fn detection_bound(&self, now_ns: u64) -> Option<Duration> {
        if self.periods.count == 0 {
            return None;
        }
        // Every member the view holds but this one: no round is larger, whatever the rounds in
        // the window were (a member that condemned another, even falsely, runs smaller ones).
        let watched = u64::try_from(self.membership.len().saturating_sub(1)).unwrap_or(u64::MAX);
        let spacing = watched.saturating_mul(2).saturating_sub(1);
        // The told probe's own period resolves it: one more.
        let periods = spacing.saturating_mul(2).saturating_add(1);
        // The period in progress and the wake it is late for are measured too: a stall the
        // member is in when it states the bound is in the bound.
        let running = self
            .probe
            .map_or(0, |probe| now_ns.saturating_sub(probe.sent_ns));
        let late = self.wakes.latest_ns(now_ns);
        let unanswered = self.longest_span.saturating_mul(3).saturating_add(late);
        let period = self.periods.longest.max(running).max(unanswered);
        Some(Duration::from_nanos(period.saturating_mul(periods)))
    }

    /// Records an acknowledgement from `from` of the probe `nonce`, received at `at_ns` (the
    /// kernel's receive stamp where the caller has one, else when it was read). It answers the
    /// period's probe if it is that probe's, and its round trip is measured whatever probe it
    /// answers, however late: a late answer is the tail the margin must cover.
    pub fn on_ack(&mut self, from: HostId, nonce: u64, at_ns: u64) {
        match self.probe.as_mut() {
            Some(probe) if probe.target == from && probe.nonce == nonce => probe.answered = true,
            Some(probe) if probe.target != from => self.heard_other = true,
            _ => {}
        }
        let measure = self.granularity().zip(self.periods.mean_ns());
        let interval = measure.map(|(_, period)| self.pair_interval(period));
        let mtbf = self.exposure.mtbf();
        let Some(peer) = self.peers.get_mut(&from) else {
            return;
        };
        // Any answer is evidence of life when it arrives, even one too late to be measured.
        peer.last_answer_ns = Some(peer.last_answer_ns.map_or(at_ns, |last| last.max(at_ns)));
        let Some(sent) = peer.take(nonce) else {
            return;
        };
        let rtt = at_ns.saturating_sub(sent.at_ns);
        self.last_rtt_ns = Some(rtt);
        if let Some(((granularity, period), interval)) = measure.zip(interval) {
            peer.stream.take(sent.seq, rtt, granularity, interval);
            if peer.stream.due() {
                peer.stream.configure(mtbf, &floors(granularity), interval);
                peer.report.configured = peer.stream.verdict.is_some();
                if let Some(verdict) = peer.stream.verdict {
                    self.longest_span = self.longest_span.max(verdict.span_ns());
                }
            }
            // The pool is fed while it judges: by pairs with no verdict of their own, until it has
            // one.
            if !peer.report.configured || self.pool.verdict.is_none() {
                self.pool
                    .take(sent.nonce, rtt, granularity, Duration::from_nanos(period));
            }
        }
        let seconds = Duration::from_nanos(rtt).as_secs_f64();
        if let Some(coordinate) = self.peer_coordinates.get(&from) {
            self.coordinates.update_with_rtt(coordinate, seconds);
        }
    }

    /// The pool's verdict for a probe of a pair that has none of its own, renewed when due.
    fn pooled(&mut self) -> Option<Verdict> {
        if self.pool.due()
            && let Some((granularity, period)) = self.granularity().zip(self.periods.mean_ns())
        {
            let interval = self.pair_interval(period);
            self.pool
                .configure(self.exposure.mtbf(), &floors(granularity), interval);
            if let Some(verdict) = self.pool.verdict {
                self.longest_span = self.longest_span.max(verdict.span_ns());
            }
        }
        self.pool.verdict
    }

    /// The pair's probe interval: the round, one period for each member watched.
    fn pair_interval(&self, period_ns: u64) -> Duration {
        let watched = u64::try_from(self.order.len()).unwrap_or(1).max(1);
        Duration::from_nanos(period_ns.saturating_mul(watched))
    }

    /// `G`, the mean lateness of this member's wakes, once measured and not zero.
    pub fn granularity(&self) -> Option<Duration> {
        self.wakes.granularity()
    }

    /// Records an indirect acknowledgement that `target` answered the probe `nonce` through a
    /// relay, at `at_ns`: the period's probe is answered. A relayed round trip is two paths' and
    /// is not the pair's sample.
    pub fn on_indirect_ack(&mut self, target: HostId, nonce: u64, at_ns: u64) {
        if let Some(probe) = self.probe.as_mut()
            && probe.target == target
            && probe.nonce == nonce
        {
            probe.answered = true;
            if let Some(peer) = self.peers.get_mut(&target) {
                peer.last_answer_ns = Some(at_ns);
            }
        }
    }

    /// Responds to a ping from `from` with the acknowledgement to send back. A ping from a member
    /// other than the one being measured shows the network carries this member's traffic.
    pub fn on_ping(&mut self, from: HostId) -> Ack {
        if self.probe.is_some_and(|probe| probe.target != from) {
            self.heard_other = true;
        }
        Ack { to: from }
    }

    /// As a relay, the ping to send `target` for a ping-request; its nonce is the relay's own,
    /// from a range its own probes never use, and the caller maps the answer back to the asker.
    pub fn on_ping_req(&mut self, target: HostId) -> Ping {
        let nonce = self.relayed;
        self.relayed = self.relayed.saturating_sub(1);
        Ping { to: target, nonce }
    }

    /// What this member has done and promised about `peer`.
    pub fn report(&self, peer: HostId) -> Option<PeerReport> {
        self.peers.get(&peer).map(|held| PeerReport {
            last_answer_ns: held.last_answer_ns,
            ..held.report
        })
    }

    /// The verdict that times this member's probes of `peer` now: the pair's, else the pool's.
    pub fn verdict(&self, peer: HostId) -> Option<Verdict> {
        self.peers
            .get(&peer)
            .and_then(|held| held.stream.verdict)
            .or(self.pool.verdict)
    }

    /// The mean of this member's periods, once one has ended.
    pub fn mean_period(&self) -> Option<Duration> {
        self.periods.mean_ns().map(Duration::from_nanos)
    }

    /// A suspected `subject` asks for more time with a progress `witness` it cannot fake while
    /// stuck, saying whether it is `overloaded` (mantle note 32 S13; focal's witnessed extensions).
    /// The base window is the one probe that tells a suspect, so a grant is one more probe.
    pub fn request_extension(
        &mut self,
        subject: HostId,
        witness: u64,
        overloaded: bool,
    ) -> ExtensionDecision {
        let suspected = self
            .membership
            .state(subject)
            .is_some_and(|state| state.liveness == Liveness::Suspect);
        if !suspected {
            return ExtensionDecision::Denied(ExtensionDenial::NotSuspected);
        }
        self.extensions
            .entry(subject)
            .or_default()
            .request(self.period, 1, witness, overloaded)
    }

    /// This node's own network coordinate, to gossip so peers can predict the round-trip time to it.
    pub fn coordinate(&self) -> &NetworkCoordinate {
        self.coordinates.coordinate()
    }

    /// Learns `peer`'s network coordinate (from a probe reply or gossip). Only a member this node
    /// probes is learned, and a member's coordinate is forgotten when it is declared dead, so the
    /// coordinates held are bounded by the membership.
    pub fn learn_coordinate(&mut self, peer: HostId, coordinate: Coordinate<'_>) {
        if peer == self.local || !self.is_probed(peer) {
            return;
        }
        match self.peer_coordinates.get_mut(&peer) {
            Some(held) => coordinate.write_into(held),
            None => {
                self.peer_coordinates
                    .insert(peer, coordinate.to_coordinate());
            }
        }
    }

    /// The predicted round-trip time from this node to `peer`, in seconds, when `peer`'s coordinate
    /// is known.
    pub fn predicted_rtt(&self, peer: HostId) -> Option<f64> {
        self.peer_coordinates
            .get(&peer)
            .map(|coordinate| self.coordinates.predict(coordinate))
    }

    /// The predicted round-trip time between two peers whose coordinates this node has learned.
    fn predicted_between(&self, from: HostId, to: HostId) -> Option<f64> {
        let from_coordinate = self.peer_coordinates.get(&from)?;
        let to_coordinate = self.peer_coordinates.get(&to)?;
        Some(CoordinateEngine::estimate_rtt(
            from_coordinate,
            to_coordinate,
        ))
    }

    /// Applies a membership update, enqueues the change for gossip, and keeps the peer's suspicion
    /// state in step with the view.
    fn record(&mut self, subject: HostId, update: MemberState) -> Option<Change> {
        let change = self.membership.apply(subject, update);
        match change {
            Some(Change::Adopted { member, state }) => {
                self.gossip.record(member, state);
                self.adopted(member, state.liveness);
            }
            Some(Change::Refuted { incarnation }) => {
                let state = MemberState {
                    liveness: Liveness::Alive,
                    incarnation,
                };
                self.gossip.record(self.local, state);
            }
            None => {}
        }
        change
    }

    fn adopted(&mut self, member: HostId, liveness: Liveness) {
        let peer = self.peers.entry(member).or_insert_with(Peer::new);
        match liveness {
            Liveness::Alive => peer.clear_suspicion(),
            // A suspicion already held, adopted again at a newer incarnation, keeps the probes
            // that told the peer: they carried a suspicion and went unanswered all the same.
            Liveness::Suspect if peer.suspected_from.is_none() => {
                peer.suspected_from = Some(peer.sent);
                peer.clear_pending();
            }
            Liveness::Suspect => {}
            Liveness::Dead => {
                // The report keeps when the condemnation was pending, for the bound it is held to.
                peer.suspected_from = None;
                peer.told_missed = 0;
                peer.pending_since = None;
                self.exposure.on_failure();
                self.extensions.remove(&member);
                self.peer_coordinates.remove(&member);
            }
        }
    }

    /// The batch of membership updates to piggyback on an outgoing message: up to `max`, the
    /// least-disseminated first, each sent its budget ([`gossip_transmits`]) and then dropped.
    /// The batch replaces what `batch` held; `batch` keeps its capacity.
    pub fn gossip_into(&mut self, max: usize, batch: &mut Vec<(HostId, MemberState)>) {
        self.gossip.drain(max, self.transmits, batch);
    }

    /// The gossip batch to piggyback on a direct ping to `target`: the ordinary batch plus, while
    /// this node suspects `target` or holds it dead, that belief — even after its transmit budget
    /// is spent (Lifeguard's buddy system), so the target hears it from the probe it answers and
    /// refutes at once. Within `max`: the least-fresh entry makes room.
    pub fn ping_gossip_into(
        &mut self,
        target: HostId,
        max: usize,
        batch: &mut Vec<(HostId, MemberState)>,
    ) {
        self.gossip_into(max, batch);
        if let Some(state) = self.membership.state(target)
            && state.liveness != Liveness::Alive
            && !batch.iter().any(|(host, _)| *host == target)
        {
            if batch.len() >= max {
                batch.pop();
            }
            batch.insert(0, (target, state));
        }
    }

    /// Applies a received gossip batch, folding each update into the view (and re-enqueueing
    /// anything it adopts so the change spreads onward).
    pub fn apply_gossip(&mut self, updates: impl IntoIterator<Item = (HostId, MemberState)>) {
        for (subject, state) in updates {
            self.apply(subject, state);
        }
    }

    /// The membership view this detector maintains.
    pub fn membership(&self) -> &Membership {
        &self.membership
    }

    /// Learns a peer (alive at incarnation zero) — a join. A later round will probe it.
    pub fn join(&mut self, peer: HostId) {
        if peer != self.local {
            self.record(
                peer,
                MemberState {
                    liveness: Liveness::Alive,
                    incarnation: 0,
                },
            );
        }
    }

    /// Applies a gossiped membership update, returning the change and enqueuing it for onward
    /// gossip.
    pub fn apply(&mut self, subject: HostId, update: MemberState) -> Option<Change> {
        self.record(subject, update)
    }

    /// Whether this node holds no other member alive or suspected.
    fn isolated(&self) -> bool {
        let local = self.local;
        self.membership.alive().all(|host| host == local)
            && self.membership.suspects().next().is_none()
    }

    /// Whether `member` is one this node probes: alive or suspected — a member until it is dead.
    fn is_probed(&self, member: HostId) -> bool {
        matches!(
            self.membership.state(member).map(|state| state.liveness),
            Some(Liveness::Alive | Liveness::Suspect)
        )
    }

    /// The next peer to probe — alive or suspected — in randomized order (SWIM §4.3). A suspect is
    /// still a member and is probed until it is dead, so the probe that tells it is sent and its
    /// answer counts. Each round is a fresh permutation; members dead mid-round are skipped. A
    /// member with nobody alive or suspected left probes those it holds dead. The dissemination
    /// budget follows the membership at each round.
    fn next_target(&mut self) -> Option<HostId> {
        loop {
            while let Some(&candidate) = self.order.get(self.cursor) {
                self.cursor = self.cursor.saturating_add(1);
                if self.is_probed(candidate) || self.isolated() {
                    return Some(candidate);
                }
            }
            let local = self.local;
            let suspected = self.membership.suspects().map(|(host, _)| host);
            self.order.clear();
            self.order.extend(
                self.membership
                    .alive()
                    .chain(suspected)
                    .filter(|host| *host != local),
            );
            if self.order.is_empty() {
                // Nobody left alive in this member's view: it is likelier the one cut off than
                // every other member dead (Lifeguard §IV), so it probes the members it holds dead;
                // each ping tells its target so, and a live one refutes in its answer.
                self.order.extend(self.membership.dead());
            }
            if self.order.is_empty() {
                return None;
            }
            self.transmits = gossip_transmits(self.order.len().saturating_add(1));
            self.shuffler.shuffle(&mut self.order);
            self.cursor = 0;
        }
    }
}

/// Feeds a round trip as heartbeat `seq` on the estimator's schedule: arrival
/// `(seq − anchor)·η + rtt`, so the offset it measures is the round trip itself. The schedule is
/// the estimator's own interval, which tracks the pair's real spacing on average; a probe's
/// spacing does not enter NFD-E's offset.
fn feed(
    estimator: &mut LinkEstimator,
    seq: u64,
    anchor: u64,
    rtt: u64,
) -> Result<(), hyper_timing::EstimateError> {
    let interval = nanos(estimator.interval());
    let arrival = seq
        .checked_sub(anchor)
        .and_then(|steps| steps.checked_mul(interval))
        .and_then(|scheduled| scheduled.checked_add(rtt))
        .ok_or(hyper_timing::EstimateError::OutOfRange)?;
    estimator.on_heartbeat(seq, arrival).map(|_| ())
}

/// The floors under a verdict: the member's granularity `G`, and the sender's `E[flush] + G = G`
/// (an acknowledgement is not flushed). The suspicion's margin holds one probe: SWIM judges each
/// probe on its own, and the condemnation that follows is the multi-probe rule
/// (`docs/timing.md` §2.7), so no correlation time enters.
fn floors(granularity: Duration) -> Floors {
    Floors {
        granularity,
        sender: granularity,
        correlation: Duration::MAX,
    }
}

/// The verdict for a probe stream: the margin minimizing unavailability at the pair's interval,
/// where a false suspicion costs the time until it is refuted — the pair's next probe, which
/// carries it, answered: `η + μ` — and a crash costs its detection.
fn verdict(
    estimator: &LinkEstimator,
    mtbf: Option<Duration>,
    floors: &Floors,
    interval: Duration,
) -> Result<Verdict, Refusal> {
    let link: LinkBehaviour = estimator.behaviour()?;
    let mtbf = mtbf.ok_or(Refusal::Unconfigurable)?;
    let costs = Costs {
        election: interval.saturating_add(link.mean_delay),
        mtbf,
    };
    let configured = detector_at(&link, &costs, floors, interval).ok_or(Refusal::Unconfigurable)?;
    let variance = link.delay_deviation.as_secs_f64().powi(2);
    let mistake = mistake_bound(
        link.loss,
        variance,
        interval.as_secs_f64(),
        configured.margin.as_secs_f64(),
    );
    Ok(Verdict {
        round_trip: link.mean_delay,
        margin: configured.margin,
        interval,
        loss: link.loss,
        mistake,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOCAL: HostId = HostId(1);
    const A: HostId = HostId(2);
    const B: HostId = HostId(3);
    const C: HostId = HostId(4);
    const MS: u64 = 1_000_000;

    /// A xorshift stream (Marsaglia 2003): deterministic test noise.
    fn noise(state: &mut u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state
    }

    /// A detector for `LOCAL` that knows `peers`.
    fn detector(peers: &[HostId]) -> Detector {
        let mut detector = Detector::new(LOCAL, Exposure::new());
        for &peer in peers {
            detector.join(peer);
        }
        detector
    }

    /// The driver of one detector over simulated time: answers each probe after the round trip
    /// `answer` gives (none for silence), wakes `late` after each wake asked, and, when nothing
    /// is due, hears a ping from `heard`.
    struct World {
        now: u64,
        late: u64,
        heard: HostId,
        pings: Vec<Ping>,
        requests: Vec<PingReq>,
        asked: Vec<PingReq>,
    }

    impl World {
        fn new() -> Self {
            Self {
                now: MS,
                late: MS / 10,
                heard: C,
                pings: Vec::new(),
                requests: Vec::new(),
                asked: Vec::new(),
            }
        }

        /// Runs `periods` periods.
        fn run(
            &mut self,
            detector: &mut Detector,
            periods: usize,
            mut answer: impl FnMut(HostId) -> Option<u64>,
        ) {
            let mut pending: Option<(HostId, u64, u64)> = None;
            let mut started = 0;
            while started <= periods {
                if let Some(ping) = detector.poll(self.now, &mut self.requests) {
                    self.pings.push(ping);
                    started += 1;
                    pending = answer(ping.to).map(|rtt| (ping.to, ping.nonce, self.now + rtt));
                }
                self.asked.extend(self.requests.iter().copied());
                let ack = pending.map(|(_, _, at)| at);
                match (detector.wake().map(|w| w + self.late), ack) {
                    (Some(wake), Some(at)) => self.now = wake.min(at).max(self.now),
                    (Some(wake), None) => self.now = wake.max(self.now),
                    (None, Some(at)) => self.now = at.max(self.now),
                    (None, None) => {
                        self.now += MS;
                        detector.on_ping(self.heard);
                    }
                }
                if let Some((from, nonce, at)) = pending
                    && at <= self.now
                {
                    detector.on_ack(from, nonce, at);
                    pending = None;
                }
            }
        }
    }

    /// A round trip of 1 ms and up to 0.5 ms of jitter.
    fn jitter(state: &mut u64) -> u64 {
        MS + noise(state) % (MS / 2)
    }

    /// Runs until every pair is configured by its own estimator, everyone answering.
    fn configured(detector: &mut Detector, world: &mut World, peers: &[HostId]) {
        let mut state = 0x2545_F491_4F6C_DD1D;
        for _ in 0..100 {
            world.run(detector, 100, |_| Some(jitter(&mut state)));
            if peers
                .iter()
                .all(|peer| detector.report(*peer).is_some_and(|r| r.configured))
            {
                return;
            }
        }
        panic!("the pairs never configured");
    }

    /// The direct deadline of the period's probe.
    fn ping_sent(detector: &Detector) -> u64 {
        detector.probe.unwrap().due_ns().unwrap()
    }

    fn liveness(detector: &Detector, peer: HostId) -> Liveness {
        detector.membership().state(peer).unwrap().liveness
    }

    /// A measurement probe whose ping or answer was lost ends at its expected arrival: with every
    /// member's probe lost at once (a throttled container dropping a burst), waiting for another
    /// member left all of them waiting for ever.
    #[test]
    fn a_lost_measurement_probe_ends_at_its_expected_arrival() {
        let mut detector = detector(&[A, B]);
        let mut requests = Vec::new();
        let first = detector.poll(0, &mut requests).unwrap();
        detector.on_ack(first.to, first.nonce, MS);
        let lost = detector.poll(MS, &mut requests).unwrap();
        assert_eq!(detector.verdict(lost.to), None, "measurement only");
        let expected = detector.wake().unwrap();
        assert_eq!(expected, 2 * MS, "the latest round trip on");
        assert_eq!(detector.poll(expected - 1, &mut requests), None);
        let next = detector.poll(expected, &mut requests);
        assert!(next.is_some(), "the period ended at the expected arrival");
        assert_eq!(
            liveness(&detector, lost.to),
            Liveness::Alive,
            "and judged nothing"
        );
    }

    /// A reconfiguration the estimator refuses leaves the verdict in force: under a CPU throttle a
    /// stall made `τ_int` unmeasured again, the verdict went, and the probe that would have
    /// suspected a crashed member was not judged (one Linux run in three hundred at one CPU).
    #[test]
    fn a_refused_reconfiguration_leaves_the_verdict_in_force() {
        let in_force = Verdict {
            round_trip: Duration::from_millis(1),
            margin: Duration::from_millis(2),
            interval: Duration::from_millis(30),
            loss: 0.01,
            mistake: 0.01,
        };
        let mut stream = Stream {
            verdict: Some(in_force),
            ..Stream::default()
        };
        let g = Duration::from_micros(50);
        stream.take(0, MS, g, Duration::from_millis(30));
        let refused = stream.estimator.as_ref().unwrap().behaviour();
        assert_eq!(refused.err(), Some(Refusal::TooFewHeartbeats));
        stream.configure(
            Some(Duration::from_secs(60)),
            &floors(g),
            Duration::from_millis(30),
        );
        assert_eq!(stream.verdict, Some(in_force));
    }

    /// A suspicion re-adopted at a newer incarnation keeps the probes that already told the peer,
    /// and its pending condemnation: resetting them made a crashed member be told again from the
    /// start, past the detection bound (Linux at one CPU, where live members were suspected and
    /// refuted often).
    #[test]
    fn a_suspicion_adopted_again_keeps_its_told_probes() {
        let peers = [A, B, C];
        let mut detector = detector(&peers);
        let mut world = World::new();
        configured(&mut detector, &mut world, &peers);
        let mut state = 21;
        while detector.report(A).unwrap().pending_since_ns.is_none() {
            world.run(&mut detector, 0, |peer| {
                (peer != A).then(|| jitter(&mut state))
            });
            assert_ne!(liveness(&detector, A), Liveness::Dead, "pending first");
        }
        let incarnation = detector.membership().state(A).unwrap().incarnation;
        detector.apply(
            A,
            MemberState {
                liveness: Liveness::Suspect,
                incarnation: incarnation + 1,
            },
        );
        assert!(detector.report(A).unwrap().pending_since_ns.is_some());
    }

    /// The detection bound counts every member the view holds, not the current round's: a member
    /// that condemned two others (falsely, under a CPU throttle) ran rounds of one while the
    /// victim's last probe had been in a round of three, and stated a bound a third as long.
    #[test]
    fn the_detection_bound_does_not_shrink_with_the_round() {
        let peers = [A, B, C];
        let mut detector = detector(&peers);
        let mut world = World::new();
        configured(&mut detector, &mut world, &peers);
        let before = detector.detection_bound(world.now).unwrap();
        for peer in [B, C] {
            detector.apply(
                peer,
                MemberState {
                    liveness: Liveness::Dead,
                    incarnation: 0,
                },
            );
        }
        let mut state = 17;
        world.run(&mut detector, 4, |_| Some(jitter(&mut state)));
        assert_eq!(detector.order.len(), 1, "a round of one");
        assert!(detector.detection_bound(world.now).unwrap() >= before);
    }

    #[test]
    fn nothing_is_judged_before_the_estimates_exist() {
        let mut detector = detector(&[A, B]);
        let mut world = World::new();
        world.heard = B;
        let mut state = 7;
        world.run(&mut detector, 40, |peer| {
            (peer == B).then(|| jitter(&mut state))
        });
        assert_eq!(detector.verdict(A), None, "no verdict from no evidence");
        assert_eq!(liveness(&detector, A), Liveness::Alive);
        assert_eq!(detector.report(A).unwrap().suspicions, 0);
    }

    #[test]
    fn the_deadline_is_the_mean_round_trip_plus_the_margin() {
        let peers = [A, B, C];
        let mut detector = detector(&peers);
        let mut world = World::new();
        configured(&mut detector, &mut world, &peers);
        let ping = detector.poll(world.now, &mut world.requests);
        let ping = ping.or_else(|| {
            world.now = detector.wake().unwrap();
            detector.on_ack(
                world.pings.last().unwrap().to,
                world.pings.last().unwrap().nonce,
                world.now,
            );
            detector.poll(world.now, &mut world.requests)
        });
        let ping = ping.unwrap();
        let verdict = detector.verdict(ping.to).unwrap();
        assert_eq!(detector.wake(), Some(world.now + verdict.span_ns()));
        assert!(verdict.round_trip >= Duration::from_millis(1));
        assert!(verdict.round_trip <= Duration::from_micros(1_500));
        assert!(verdict.mistake > 0.0 && verdict.mistake < 1.0);
        assert!(verdict.margin < verdict.interval, "one probe in the margin");
        // G is the lateness of the wakes: the world's, or less where an answer woke it first.
        let g = detector.granularity().unwrap();
        assert!(g > Duration::ZERO && g <= Duration::from_nanos(world.late));
    }

    #[test]
    fn a_silent_member_is_suspected_told_and_condemned() {
        let peers = [A, B, C];
        let mut detector = detector(&peers);
        let mut world = World::new();
        configured(&mut detector, &mut world, &peers);
        let mut state = 11;
        let mut suspected_first = false;
        for _ in 0..200 {
            world.run(&mut detector, 1, |peer| {
                (peer != A).then(|| jitter(&mut state))
            });
            match liveness(&detector, A) {
                Liveness::Suspect => suspected_first = true,
                Liveness::Dead => break,
                Liveness::Alive => {}
            }
        }
        assert!(suspected_first, "suspected before condemned");
        assert_eq!(liveness(&detector, A), Liveness::Dead);
        let report = detector.report(A).unwrap();
        assert_eq!((report.suspicions, report.condemnations), (1, 1));
        assert!(report.condemned_after.unwrap() <= report.condemned_within.unwrap());
        assert!(
            world.asked.iter().any(|r| r.target == A && r.relay != A),
            "relays were asked before the suspicion"
        );
        for peer in [B, C] {
            assert_eq!(liveness(&detector, peer), Liveness::Alive);
        }
    }

    #[test]
    fn an_isolated_member_condemns_nobody() {
        let peers = [A, B, C];
        let mut detector = detector(&peers);
        let mut world = World::new();
        configured(&mut detector, &mut world, &peers);
        world.run(&mut detector, 60, |_| None);
        for peer in peers {
            assert_eq!(liveness(&detector, peer), Liveness::Suspect, "{peer:?}");
            assert_eq!(detector.report(peer).unwrap().condemnations, 0);
        }
    }

    #[test]
    fn an_indirect_answer_spares_the_target() {
        let peers = [A, B, C];
        let mut detector = detector(&peers);
        let mut world = World::new();
        configured(&mut detector, &mut world, &peers);
        // Run until a probe of A is out, unanswered directly.
        let mut state = 3;
        while world.pings.last().map(|p| p.to) != Some(A) {
            world.run(&mut detector, 0, |peer| {
                (peer != A).then(|| jitter(&mut state))
            });
        }
        let ping = *world.pings.last().unwrap();
        world.now = ping_sent(&detector);
        assert_eq!(detector.poll(world.now, &mut world.requests), None);
        assert!(!world.requests.is_empty(), "relays asked at the deadline");
        assert!(
            world
                .requests
                .iter()
                .all(|r| r.target == A && r.nonce == ping.nonce)
        );
        let until = detector.wake().unwrap();
        assert!(until > world.now);
        detector.on_indirect_ack(A, ping.nonce, until - 1);
        world.now = until;
        assert!(detector.poll(world.now, &mut world.requests).is_some());
        assert_eq!(liveness(&detector, A), Liveness::Alive);
    }

    #[test]
    fn a_refutation_clears_the_suspicion_and_its_pending_condemnation() {
        let peers = [A, B, C];
        let mut detector = detector(&peers);
        let mut world = World::new();
        configured(&mut detector, &mut world, &peers);
        let mut state = 5;
        while liveness(&detector, A) != Liveness::Suspect {
            world.run(&mut detector, 1, |peer| {
                (peer != A).then(|| jitter(&mut state))
            });
        }
        detector.apply(
            A,
            MemberState {
                liveness: Liveness::Alive,
                incarnation: 1,
            },
        );
        world.run(&mut detector, 30, |_| Some(jitter(&mut state)));
        assert_eq!(liveness(&detector, A), Liveness::Alive);
        assert_eq!(detector.report(A).unwrap().condemnations, 0);
    }

    #[test]
    fn an_extension_buys_one_more_told_probe() {
        let peers = [A, B, C];
        let silent_after = |extend: bool| {
            let mut detector = detector(&peers);
            let mut world = World::new();
            configured(&mut detector, &mut world, &peers);
            let mut state = 9;
            while liveness(&detector, A) != Liveness::Suspect {
                world.run(&mut detector, 1, |peer| {
                    (peer != A).then(|| jitter(&mut state))
                });
            }
            if extend {
                assert_eq!(
                    detector.request_extension(A, 1, false),
                    ExtensionDecision::Granted { periods: 1 }
                );
                assert_eq!(
                    detector.request_extension(A, 2, false),
                    ExtensionDecision::Denied(ExtensionDenial::RateLimited)
                );
            }
            let mut probes = 0;
            while liveness(&detector, A) != Liveness::Dead {
                let before = world.pings.len();
                world.run(&mut detector, 1, |peer| {
                    (peer != A).then(|| jitter(&mut state))
                });
                probes += world.pings[before..].iter().filter(|p| p.to == A).count();
                assert!(probes < 10, "never condemned");
            }
            probes
        };
        assert!(silent_after(true) > silent_after(false));
        let mut detector = detector(&peers);
        assert_eq!(
            detector.request_extension(A, 1, false),
            ExtensionDecision::Denied(ExtensionDenial::NotSuspected)
        );
    }

    #[test]
    fn the_allowance_is_the_sum_of_the_judged_probes_bounds() {
        let peers = [A, B, C];
        let mut detector = detector(&peers);
        let mut world = World::new();
        configured(&mut detector, &mut world, &peers);
        let before = detector.report(A).unwrap();
        let in_force = |detector: &Detector| {
            detector
                .probe
                .filter(|probe| probe.target == A)
                .and_then(|probe| probe.verdict)
                .map(|verdict| verdict.mistake)
        };
        let mut pending = in_force(&detector);
        let (mut sum, mut state) = (0.0, 13);
        for _ in 0..60 {
            // One new probe a call: the one before it is resolved by then.
            world.run(&mut detector, 0, |_| Some(jitter(&mut state)));
            sum += pending.take().unwrap_or(0.0);
            pending = in_force(&detector);
        }
        let after = detector.report(A).unwrap();
        assert!(sum > 0.0);
        assert!(
            (after.suspicion_allowance - before.suspicion_allowance - sum).abs() < 1e-12,
            "{} against {sum}",
            after.suspicion_allowance - before.suspicion_allowance
        );
        assert_eq!(after.suspicions, 0);
    }

    #[test]
    fn the_dissemination_budget_is_swims_bound() {
        assert_eq!(gossip_transmits(1), 1);
        assert_eq!(gossip_transmits(2), 1);
        for n in 3..2_000usize {
            let t = gossip_transmits(n);
            let nf = n as f64;
            let lambda = f64::from(t) / nf.ln();
            // SWIM §4.1: at most n^{−((2−4/n)λ−2)} members uninfected in expectation, below one.
            let uninfected = nf.powf(-((2.0 - 4.0 / nf) * lambda - 2.0));
            assert!(uninfected < 1.0, "{n}: {t} transmits leave {uninfected}");
            // And one fewer would not (to within the fixed-point logarithm).
            let fewer = f64::from(t - 1) / nf.ln();
            assert!(
                nf.powf(-((2.0 - 4.0 / nf) * fewer - 2.0)) >= 0.99,
                "{n}: {t} is not the least"
            );
        }
        assert_eq!(gossip_transmits(4), 3);
        assert_eq!(gossip_transmits(256), 6);
    }

    #[test]
    fn the_relays_are_the_fewest_at_least_as_reliable_as_the_direct_probe() {
        for loss in [1e-6, 1e-3, 0.05, 0.3] {
            let k = relay_count(loss, 100);
            let through = 1.0 - (1.0 - loss) * (1.0 - loss);
            assert!(through.powi(k as i32) <= loss, "{loss}: {k}");
            assert!(k == 1 || through.powi(k as i32 - 1) > loss, "{loss}: {k}");
        }
        assert_eq!(relay_count(0.3, 1), 1);
        assert_eq!(relay_count(0.3, 0), 0);
    }

    /// A membership change is disseminated its budget of times, then dropped.
    #[test]
    fn a_change_is_gossiped_a_bounded_number_of_times() {
        let mut detector = detector(&[]);
        detector.join(A);
        let mut batch = Vec::new();
        detector.gossip_into(10, &mut batch);
        assert!(batch.iter().any(|(host, _)| *host == A), "sent once");
        detector.gossip_into(10, &mut batch);
        assert!(
            batch.iter().all(|(host, _)| *host != A),
            "one member and us: one transmit"
        );
    }

    /// Lifeguard's buddy system: a ping to a suspected member carries the suspicion even after its
    /// transmit budget is spent; a ping to another member does not.
    #[test]
    fn a_ping_to_a_suspected_member_always_carries_the_suspicion() {
        let mut detector = detector(&[A, B]);
        let suspicion = MemberState {
            liveness: Liveness::Suspect,
            incarnation: 0,
        };
        detector.apply(A, suspicion);
        let mut batch = Vec::new();
        for _ in 0..5 {
            detector.gossip_into(10, &mut batch);
        }
        detector.ping_gossip_into(A, 10, &mut batch);
        assert!(batch.contains(&(A, suspicion)), "the buddy system");
        detector.ping_gossip_into(B, 10, &mut batch);
        assert!(batch.iter().all(|(host, _)| *host != A));
        detector.apply(
            A,
            MemberState {
                liveness: Liveness::Alive,
                incarnation: 1,
            },
        );
        for _ in 0..5 {
            detector.gossip_into(10, &mut batch);
        }
        detector.ping_gossip_into(A, 10, &mut batch);
        assert!(
            batch.iter().all(|(host, _)| *host != A),
            "refuted: not injected"
        );
    }

    #[test]
    fn gossip_carries_a_change_to_another_node() {
        let mut source = detector(&[A]);
        source.apply(
            A,
            MemberState {
                liveness: Liveness::Dead,
                incarnation: 0,
            },
        );
        let mut batch = Vec::new();
        source.gossip_into(10, &mut batch);
        let mut other = Detector::new(B, Exposure::new());
        other.apply_gossip(batch);
        assert_eq!(liveness(&other, A), Liveness::Dead);
    }

    #[test]
    fn a_dead_member_is_not_probed_while_another_lives() {
        let mut detector = detector(&[A, B]);
        detector.apply(
            A,
            MemberState {
                liveness: Liveness::Dead,
                incarnation: 0,
            },
        );
        let mut requests = Vec::new();
        for at in 0..6 {
            let ping = detector.poll(at, &mut requests).unwrap();
            assert_eq!(ping.to, B);
            detector.on_ack(B, ping.nonce, at);
        }
    }

    /// Two members that hold each other dead: neither has anyone else to probe, so each probes the
    /// other and tells it, and each refutes in its answer. Without it neither ever sent again
    /// (the cluster test's deadlock under a one-CPU throttle).
    #[test]
    fn members_that_hold_each_other_dead_heal() {
        let (mut x, mut y) = (detector(&[A]), Detector::new(A, Exposure::new()));
        y.join(LOCAL);
        let dead = |host| {
            (
                host,
                MemberState {
                    liveness: Liveness::Dead,
                    incarnation: 0,
                },
            )
        };
        x.apply_gossip([dead(A)]);
        y.apply_gossip([dead(LOCAL)]);
        let mut detectors = [x, y];
        let mut requests = Vec::new();
        let mut batch = Vec::new();
        for at in 1..4u64 {
            for prober in 0..2 {
                let [first, second] = &mut detectors;
                let (prober, answering) = if prober == 0 {
                    (first, second)
                } else {
                    (second, first)
                };
                let Some(ping) = prober.poll(at * MS, &mut requests) else {
                    continue;
                };
                prober.ping_gossip_into(ping.to, 10, &mut batch);
                answering.apply_gossip(batch.iter().copied());
                answering.gossip_into(10, &mut batch);
                prober.apply_gossip(batch.iter().copied());
                prober.on_ack(ping.to, ping.nonce, at * MS + 1);
            }
        }
        let [x, y] = detectors;
        assert_eq!(liveness(&x, A), Liveness::Alive);
        assert_eq!(liveness(&y, LOCAL), Liveness::Alive);
    }

    #[test]
    fn every_peer_is_probed_once_per_round() {
        let peers = [A, B, C];
        let mut detector = detector(&peers);
        let mut world = World::new();
        let mut state = 1;
        for _ in 0..3 {
            let before = world.pings.len();
            world.run(&mut detector, peers.len() - 1, |_| Some(jitter(&mut state)));
            let mut round: Vec<u64> = world.pings[before..].iter().map(|p| p.to.0).collect();
            round.sort_unstable();
            assert_eq!(round, vec![2, 3, 4]);
        }
    }

    #[test]
    fn measured_round_trips_teach_the_coordinate() {
        let mut detector = detector(&[A]);
        let mut peer = NetworkCoordinate::origin(8);
        peer.vec[0] = 0.030;
        peer.error = 0.05;
        detector.learn_coordinate(A, Coordinate::Held(&peer));
        assert!(detector.predicted_rtt(A).is_some());
        assert_eq!(detector.predicted_rtt(B), None);
        let mut world = World::new();
        world.run(&mut detector, 400, |_| Some(35 * MS));
        let predicted = detector.predicted_rtt(A).unwrap();
        assert!((predicted - 0.035).abs() < 0.035 * 0.2, "{predicted}");
    }

    #[test]
    fn relays_are_ranked_nearest_the_target_first() {
        let positions = [(A, 0.0), (B, 1.0), (C, 2.0), (HostId(5), 10.0)];
        let mut detector = detector(&[]);
        for &(host, x) in &positions {
            detector.join(host);
            let mut coordinate = NetworkCoordinate::origin(8);
            coordinate.vec[0] = x;
            coordinate.error = 0.05;
            detector.learn_coordinate(host, Coordinate::Held(&coordinate));
        }
        detector.rank_relays(A);
        let ranked: Vec<HostId> = detector.relays.clone();
        assert_eq!(ranked, vec![B, C, HostId(5)]);
    }
}
