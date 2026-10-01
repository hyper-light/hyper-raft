//! Election timing counted in the owner's periods, from slates' timing law (slates
//! `crates/cluster/src/timing.rs` at `c4e2c52`, §4.8 "Derived constants").
//!
//! Raft needs `broadcastTime ≪ electionTimeout ≪ MTBF` (Ongaro and Ousterhout, ATC 2014, §5.6).
//! [`ElectionTiming::derive`] sets the base election timeout to `ELECTION_MARGIN × max(tail,
//! heartbeat)` and the randomization span (Raft §5.2, §9.3) to `ELECTION_MARGIN × max(spread,
//! heartbeat)`, both in whole periods of the owner's heartbeat, from the measured paths to the
//! group's other voters.
//! - On a loopback or a LAN every tail sits inside one period, so the timing is the floor of
//!   [`ELECTION_MARGIN`] periods by construction.
//! - A far group's base grows with its slowest path's tail, and its span with that path's
//!   variation, so the worst-case leader-loss detection is `base + span`, not `2 × base`.
//!
//! The broadcast time includes the time a follower takes to make an entry durable before it
//! answers (mantle audit §11.7). A path measured by a probe that touches no disk does not hold it,
//! so the caller passes the durable-acknowledgement tail the samples lack.
//!
//! The timer ([`ElectionTimer`]) counts periods, not wall time. A node whose own periods are
//! starved waits longer instead of campaigning on its own slowness, which is the Lifeguard direction
//! (slates `docs/bugs/2026-09-13-swim-fixed-probe-deadline-kills-a-starved-live-peer.md`).
//!
//! This is the period-counting form, for a core whose election timer is driven from outside.
//! [`TickPace`](crate::TickPace) is the period-stretching form, for a core whose tick counts are
//! fixed when it opens. The derivations take any [`PathEstimate`], so the estimator that feeds them
//! is chosen by measurement (mantle note 32 §3.7), not by this module.

use crate::{ELECTION_MARGIN, ExchangeRtt, PathRtt};

/// Derived: two, the round trips a lost batch takes to repair when the leader sends batches ahead:
/// the follower's refusal of the batch after the lost one reaches the leader, and the resend
/// reaches the follower (slates `docs/wip/research/consensus-enhancements.md` §3.5).
pub const REPAIR_ROUND_TRIPS: u64 = 2;

/// What a timing law reads from a measured path. A path with no sample contributes nothing to a
/// derivation: never an initial guess such as RFC 9002's 333 ms.
pub trait PathEstimate {
    /// How many round trips have fed the estimate.
    fn samples(&self) -> u64;
    /// The central round trip, in nanoseconds; zero before any sample.
    fn smoothed_ns(&self) -> u64;
    /// The bound on the path's round-trip tail, in nanoseconds, or `None` before any sample.
    fn tail_ns(&self) -> Option<u64>;
    /// The tail's width above the central round trip, or `None` before any sample: the design's
    /// "RTT variance", in the tail's own units.
    fn spread_ns(&self) -> Option<u64> {
        self.tail_ns()
            .map(|tail| tail.saturating_sub(self.smoothed_ns()))
    }
}

impl PathEstimate for PathRtt {
    fn samples(&self) -> u64 {
        PathRtt::samples(self)
    }
    fn smoothed_ns(&self) -> u64 {
        PathRtt::smoothed_ns(self)
    }
    fn tail_ns(&self) -> Option<u64> {
        PathRtt::tail_ns(self)
    }
}

impl PathEstimate for ExchangeRtt {
    fn samples(&self) -> u64 {
        ExchangeRtt::samples(self)
    }
    fn smoothed_ns(&self) -> u64 {
        ExchangeRtt::smoothed_ns(self)
    }
    fn tail_ns(&self) -> Option<u64> {
        ExchangeRtt::tail_ns(self)
    }
}

/// A voter's election priority (slates `consensus-enhancements.md` §3.4).
///
/// It is the round trip the voter would commit in as leader, its quorum round trip (the
/// `⌊n/2⌋`-th smallest measured round trip to the other voters, since a leader commits once a
/// majority including itself holds an entry), and the spread of the path that sets it, both in
/// nanoseconds. A zero round trip is unknown: it never outranks and is never outranked, so an
/// unmeasured group behaves as one without priorities.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ElectionPriority {
    /// The quorum round trip, nanoseconds; zero when unknown.
    pub quorum_ns: u64,
    /// The spread of the path that sets it, nanoseconds.
    pub spread_ns: u64,
}

impl ElectionPriority {
    /// Whether this priority is known and commits distinguishably faster than `other`: its
    /// interval, the round trip plus its spread, lies wholly below `other`'s round trip less its
    /// spread. Overlapping intervals tie, so measurement noise never ranks one voter above another.
    pub fn outranks(&self, other: &Self) -> bool {
        self.quorum_ns > 0
            && other.quorum_ns > 0
            && self.quorum_ns.saturating_add(self.spread_ns)
                < other.quorum_ns.saturating_sub(other.spread_ns)
    }
}

/// A voter's election priority from its measured paths to the other voters of a group of
/// `voters`: the `⌊voters/2⌋`-th smallest central round trip among `paths`, with that path's
/// spread. Unknown while a sole voter, or while fewer measured paths than that exist.
pub fn quorum_priority<'a, P: PathEstimate + 'a>(
    paths: impl IntoIterator<Item = Option<&'a P>>,
    voters: usize,
) -> ElectionPriority {
    let quorum = voters / 2;
    let mut measured: Vec<(u64, u64)> = paths
        .into_iter()
        .flatten()
        .filter_map(|path| path.spread_ns().map(|spread| (path.smoothed_ns(), spread)))
        .collect();
    measured.sort_unstable();
    quorum
        .checked_sub(1)
        .and_then(|position| measured.get(position))
        .map_or_else(ElectionPriority::default, |&(quorum_ns, spread_ns)| {
            ElectionPriority {
                quorum_ns,
                spread_ns,
            }
        })
}

/// A group's election timing for one period, derived from the measured paths to its other voters.
/// Periods are the owner's, one heartbeat or longer each.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ElectionTiming {
    /// The base election timeout in periods: a follower campaigns after this many periods without
    /// leader contact, plus its jitter, and a leader judges its quorum every this many.
    pub base_periods: u32,
    /// The randomization span in periods: a node's timeout is `base + jitter`, `jitter ∈ [0,
    /// span)`.
    pub span_periods: u32,
    /// The broadcast round-trip tail the base was derived from, nanoseconds: the slowest measured
    /// voter path's tail plus the durable-acknowledgement tail; zero when no voter path has a
    /// sample.
    pub broadcast_rtt_tail_ns: u64,
    /// The variation the span was derived from, nanoseconds: the widest measured voter path's
    /// spread; zero when no voter path has a sample.
    pub broadcast_rtt_spread_ns: u64,
    /// The round trips measured across the voter paths that fed this derivation: the witness that
    /// tells a measured timing from the floor it would default to.
    pub samples: u64,
}

impl ElectionTiming {
    /// The timing with no voter path measured, as on a sole voter or before a group's first probe
    /// is answered: [`ELECTION_MARGIN`] periods for both the base and the span, since the
    /// heartbeat is the smallest broadcast time the owner can observe.
    pub fn floor() -> Self {
        Self::derive::<PathRtt>(1, 0, std::iter::empty())
    }

    /// The timing from the measured `paths` to the group's other voters:
    /// `base = ELECTION_MARGIN × max(tail + durable, heartbeat)` and
    /// `span = ELECTION_MARGIN × max(spread, heartbeat)`, each rounded up to whole periods of
    /// `heartbeat_ns`.
    ///
    /// The slowest path bounds the broadcast, since a round completes when its last voter answers.
    /// `durable_tail_ns` is the tail of making an entry durable that the path samples do not
    /// already include (mantle audit §11.7); it is added only once some path is measured. A path
    /// with no sample contributes nothing, and with none measured the result is the floor.
    pub fn derive<'a, P: PathEstimate + 'a>(
        heartbeat_ns: u64,
        durable_tail_ns: u64,
        paths: impl IntoIterator<Item = &'a P>,
    ) -> Self {
        let heartbeat = heartbeat_ns.max(1);
        let mut tail = 0u64;
        let mut spread = 0u64;
        let mut samples = 0u64;
        for path in paths {
            if let (Some(path_tail), Some(path_spread)) = (path.tail_ns(), path.spread_ns()) {
                tail = tail.max(path_tail);
                spread = spread.max(path_spread);
                samples = samples.saturating_add(path.samples());
            }
        }
        if samples > 0 {
            tail = tail.saturating_add(durable_tail_ns);
        }
        Self {
            base_periods: periods_of(
                ELECTION_MARGIN.saturating_mul(tail.max(heartbeat)),
                heartbeat,
            ),
            span_periods: periods_of(
                ELECTION_MARGIN.saturating_mul(spread.max(heartbeat)),
                heartbeat,
            ),
            broadcast_rtt_tail_ns: tail,
            broadcast_rtt_spread_ns: spread,
            samples,
        }
    }

    /// The window a leader keeps ahead, in bytes: one `batch_bytes` for each period a lost batch
    /// takes to repair on the slowest measured voter path ([`REPAIR_ROUND_TRIPS`] round trips),
    /// and at least one.
    ///
    /// It is one batch on a LAN, where an acknowledgement is back within the period. slates
    /// measured four across five Azure regions, where four cut the commit tail under 1 % loss from
    /// 458 to 321 ms at 2,000 proposals a second (slates `crates/cluster/tests/pipelining.rs`,
    /// 2026-09-29).
    pub fn window_budget(&self, heartbeat_ns: u64, batch_bytes: usize) -> usize {
        let repair = self
            .broadcast_rtt_tail_ns
            .saturating_mul(REPAIR_ROUND_TRIPS);
        let batches = usize::try_from(periods_of(repair, heartbeat_ns)).unwrap_or(usize::MAX);
        batch_bytes.saturating_mul(batches)
    }

    /// This node's own timeout in periods for its `attempt`-th campaign: `base + (draw mod span)`.
    ///
    /// The draw is a splitmix64 mix of the id and the attempt. It is deterministic, so a
    /// simulation reproduces from its seed, yet independent across nodes and attempts, as Raft's
    /// randomized timeout is (§5.2, §9.3). A draw of `(local + attempt) mod span` kept two
    /// congruent nodes congruent forever, splitting the vote round after round (slates
    /// `docs/bugs/2026-09-28-correlated-election-jitter-livelocked-a-split-vote.md`).
    pub fn timeout_periods(&self, local: u64, attempt: u32) -> u32 {
        let span = u64::from(self.span_periods.max(1));
        let draw = splitmix64(local ^ u64::from(attempt).wrapping_mul(GOLDEN_GAMMA));
        let jitter = u32::try_from(draw.checked_rem(span).unwrap_or(0)).unwrap_or(0);
        self.base_periods.saturating_add(jitter)
    }
}

/// Format: splitmix64's increment, the odd integer nearest 2^64/φ (Steele, Lea and Flood, "Fast
/// splittable pseudorandom number generators", OOPSLA 2014).
const GOLDEN_GAMMA: u64 = 0x9e37_79b9_7f4a_7c15;
/// Format: splitmix64's first finalizer multiplier (Steele, Lea and Flood 2014).
const MIX_ONE: u64 = 0xbf58_476d_1ce4_e5b9;
/// Format: splitmix64's second finalizer multiplier (Steele, Lea and Flood 2014).
const MIX_TWO: u64 = 0x94d0_49bb_1331_11eb;

/// splitmix64's finalizer: a bijective mix whose outputs are statistically independent across
/// nearby inputs.
fn splitmix64(word: u64) -> u64 {
    let mut z = word.wrapping_add(GOLDEN_GAMMA);
    z = (z ^ (z >> 30)).wrapping_mul(MIX_ONE);
    z = (z ^ (z >> 27)).wrapping_mul(MIX_TWO);
    z ^ (z >> 31)
}

/// `span_ns` in whole periods of `heartbeat_ns`, rounded up, at least one; saturating.
fn periods_of(span_ns: u64, heartbeat_ns: u64) -> u32 {
    let heartbeat = heartbeat_ns.max(1);
    let periods = span_ns.div_ceil(heartbeat).max(1);
    u32::try_from(periods).unwrap_or(u32::MAX)
}

/// What a follower's period asks of its node ([`ElectionTimer::follower_period`]).
#[must_use = "a lapsed leader must be forgotten and a campaign run, or the group elects late"]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FollowerStep {
    /// A leader made contact within the minimum election timeout: keep following.
    Follow,
    /// No leader contact for the minimum election timeout, the base: the node forgets its leader
    /// (thesis §4.2.3), so it grants a candidate's pre-vote, but does not campaign yet.
    LeaderLapsed,
    /// Campaign now.
    Campaign,
}

/// A group's election timer as its owner counts it, one tick per period.
///
/// It holds the follower's age since leader contact (reset at each of its own timeouts) and its
/// silence (not reset there), the last contact value it saw, its jitter rotation, and the timeouts
/// it has yielded to more central voters (slates `consensus-enhancements.md` §3.4).
#[derive(Debug, Default)]
pub struct ElectionTimer {
    idle_periods: u32,
    silent_periods: u32,
    seen_contact: u64,
    attempt: u32,
    yielded: u32,
}

impl ElectionTimer {
    /// A timer at zero age, no contact seen, first attempt.
    pub fn new() -> Self {
        Self::default()
    }

    /// A follower's period.
    ///
    /// `contact` is the group's leader-contact count (Raft Figure 2's two follower timer resets: a
    /// leader's append answered, a vote granted). While it advances, the age and the silence reset,
    /// the yielded timeouts clear, and the node follows.
    ///
    /// Otherwise the timer ages one period. At this node's jittered timeout it rotates the attempt,
    /// resets the age, and campaigns when the node's election `rank` (how many live voters outrank
    /// it) is within the timeouts it has already yielded. Otherwise it yields this timeout and
    /// admits the next rank, so an election waits at most one timeout per live voter that outranks
    /// this one. Short of a campaign, a node silent for the base has lost its leader's lease,
    /// whatever its jitter and rank, so the voter it yields to can win its pre-vote (slates
    /// `docs/bugs/2026-09-29-a-yielding-voter-refused-the-voter-it-yielded-to.md`).
    pub fn follower_period(
        &mut self,
        contact: u64,
        timing: &ElectionTiming,
        local: u64,
        rank: usize,
    ) -> FollowerStep {
        if contact != self.seen_contact {
            self.seen_contact = contact;
            self.idle_periods = 0;
            self.silent_periods = 0;
            self.yielded = 0;
            return FollowerStep::Follow;
        }
        self.idle_periods = self.idle_periods.saturating_add(1);
        self.silent_periods = self.silent_periods.saturating_add(1);
        if self.idle_periods >= timing.timeout_periods(local, self.attempt) {
            self.attempt = self.attempt.saturating_add(1);
            self.idle_periods = 0;
            if u32::try_from(rank).unwrap_or(u32::MAX) <= self.yielded {
                return FollowerStep::Campaign;
            }
            self.yielded = self.yielded.saturating_add(1);
        }
        if self.silent_periods >= timing.base_periods {
            FollowerStep::LeaderLapsed
        } else {
            FollowerStep::Follow
        }
    }

    /// The timeouts this follower has yielded to more central voters since it last heard a leader.
    pub fn yielded(&self) -> u32 {
        self.yielded
    }

    /// A leader's period: ages one period and returns `true` every `base_periods`, when the leader
    /// judges its quorum (Raft §6.2 CheckQuorum on the election-timeout cadence). A leader is its
    /// own contact, so its silence as a follower starts over.
    pub fn leader_period(&mut self, timing: &ElectionTiming) -> bool {
        self.silent_periods = 0;
        self.idle_periods = self.idle_periods.saturating_add(1);
        if self.idle_periods < timing.base_periods.max(1) {
            return false;
        }
        self.idle_periods = 0;
        true
    }

    /// Resets the age and the silence: a sole voter's period, or a role change.
    pub fn reset(&mut self) {
        self.idle_periods = 0;
        self.silent_periods = 0;
    }

    /// Re-baselines the contact after a campaign, so the campaign's own vote and append echoes do
    /// not retrigger a fresh one next period.
    pub fn rebaseline(&mut self, contact: u64) {
        self.seen_contact = contact;
    }

    /// How many campaigns this timer has fired.
    pub fn attempts(&self) -> u32 {
        self.attempt
    }

    /// Periods since the last leader contact, or the last campaign or quorum check.
    pub fn idle_periods(&self) -> u32 {
        self.idle_periods
    }
}

#[cfg(test)]
mod tests {
    //! slates' timing-law tests (`crates/cluster/src/timing.rs` at `c4e2c52`), on [`ExchangeRtt`],
    //! the RFC 9002 estimator they were written against, plus the durable-acknowledgement term,
    //! the median-and-MAD estimator under the same law, and the unified round budget.
    use super::*;
    use crate::{RoundAnchors, RoundBudget};

    /// A millisecond in nanoseconds, so the samples read as round times.
    const MS: u64 = 1_000_000;
    /// slates' daemon heartbeat, its owner period: 100 ms.
    const HEARTBEAT: u64 = 100 * MS;
    /// The loopback round trips slates measured on 2026-09-13: SWIM p99 17 ms, consensus broadcast
    /// p50 11 ms and p99 33 ms.
    const LOOPBACK_SAMPLES_MS: [u64; 4] = [11, 17, 11, 33];
    /// An inter-region path of 80 ms ± 20 ms one way.
    const WAN_SAMPLES_MS: [u64; 6] = [160, 200, 120, 160, 190, 130];
    /// The same path once the estimate has settled within its ± 40 ms spread.
    const WAN_SETTLED_CYCLE_MS: [u64; 6] = [160, 200, 140, 180, 120, 160];

    fn path_of(samples_ms: &[u64]) -> ExchangeRtt {
        let mut path = ExchangeRtt::new();
        for sample in samples_ms {
            path.on_sample(sample * MS);
        }
        path
    }

    fn derive(paths: &[&ExchangeRtt]) -> ElectionTiming {
        ElectionTiming::derive(HEARTBEAT, 0, paths.iter().copied())
    }

    #[test]
    fn with_no_measured_voter_path_the_timing_is_the_floor() {
        let timing = derive(&[]);
        assert_eq!(timing, ElectionTiming::floor());
        assert_eq!(timing.base_periods, 10);
        assert_eq!(timing.span_periods, 10);
        assert_eq!(timing.samples, 0);
    }

    #[test]
    fn a_path_inside_the_heartbeat_leaves_the_timing_at_the_floor() {
        let lan = path_of(&LOOPBACK_SAMPLES_MS);
        let timing = derive(&[&lan]);
        assert!(timing.broadcast_rtt_tail_ns < HEARTBEAT);
        assert_eq!(timing.base_periods, ElectionTiming::floor().base_periods);
        assert_eq!(timing.span_periods, ElectionTiming::floor().span_periods);
        assert_eq!(timing.samples, 4, "the floor was measured, not defaulted");
    }

    #[test]
    fn a_wan_path_raises_the_base_to_ten_times_its_tail_in_whole_periods() {
        let wan = path_of(&WAN_SAMPLES_MS);
        let timing = derive(&[&wan]);
        let tail = wan.tail_ns().unwrap();
        assert!(tail > HEARTBEAT, "a WAN tail exceeds one period: {tail}");
        assert_eq!(
            timing.base_periods,
            u32::try_from((10 * tail).div_ceil(HEARTBEAT)).unwrap()
        );
        assert!(u64::from(timing.base_periods) * HEARTBEAT >= 10 * tail);
        assert_eq!(timing.broadcast_rtt_tail_ns, tail);
    }

    #[test]
    fn the_durable_acknowledgement_joins_the_tail_once_a_path_is_measured() {
        let wan = path_of(&WAN_SAMPLES_MS);
        let tail = wan.tail_ns().unwrap();
        let durable = 40 * MS;
        let timing = ElectionTiming::derive(HEARTBEAT, durable, [&wan]);
        assert_eq!(timing.broadcast_rtt_tail_ns, tail + durable);
        assert_eq!(
            timing.base_periods,
            u32::try_from((10 * (tail + durable)).div_ceil(HEARTBEAT)).unwrap()
        );
        // With nothing measured there is no broadcast to lengthen: the floor stands.
        assert_eq!(
            ElectionTiming::derive::<ExchangeRtt>(HEARTBEAT, durable, std::iter::empty()),
            derive(&[])
        );
    }

    #[test]
    fn the_window_holds_a_repairs_worth_of_batches_on_the_slowest_path() {
        const BATCH: usize = 4_367;
        assert_eq!(
            ElectionTiming::floor().window_budget(HEARTBEAT, BATCH),
            BATCH
        );
        let lan = derive(&[&path_of(&LOOPBACK_SAMPLES_MS)]);
        assert_eq!(lan.window_budget(HEARTBEAT, BATCH), BATCH);
        let wan = path_of(&WAN_SAMPLES_MS);
        let timing = derive(&[&wan]);
        let tail = wan.tail_ns().unwrap();
        let batches = usize::try_from((2 * tail).div_ceil(HEARTBEAT)).unwrap();
        assert_eq!(timing.window_budget(HEARTBEAT, BATCH), BATCH * batches);
        assert!(batches >= 3);
    }

    #[test]
    fn the_span_follows_the_paths_variation_and_settles_to_the_floor() {
        let early = path_of(&WAN_SAMPLES_MS);
        let timing = derive(&[&early]);
        let spread = early.spread_ns().unwrap();
        assert!(spread > HEARTBEAT, "the early variation is above a period");
        assert_eq!(
            timing.span_periods,
            u32::try_from((10 * spread).div_ceil(HEARTBEAT)).unwrap()
        );
        let mut settled = ExchangeRtt::new();
        for sample in WAN_SETTLED_CYCLE_MS.iter().cycle().take(40) {
            settled.on_sample(sample * MS);
        }
        let converged = derive(&[&settled]);
        assert!(settled.spread_ns().unwrap() < HEARTBEAT);
        assert_eq!(converged.span_periods, 10);
        assert!(converged.base_periods > 10);
    }

    #[test]
    fn the_slowest_voter_path_bounds_the_broadcast() {
        let near = path_of(&LOOPBACK_SAMPLES_MS);
        let far = path_of(&WAN_SAMPLES_MS);
        let mixed = derive(&[&near, &far]);
        let far_only = derive(&[&far]);
        assert_eq!(mixed.base_periods, far_only.base_periods);
        assert_eq!(mixed.broadcast_rtt_tail_ns, far.tail_ns().unwrap());
        assert_eq!(mixed.samples, near.samples() + far.samples());
    }

    #[test]
    fn a_voter_without_a_sample_contributes_nothing() {
        let fresh = ExchangeRtt::new();
        let lan = path_of(&LOOPBACK_SAMPLES_MS);
        let timing = derive(&[&fresh, &lan]);
        assert_eq!(timing.base_periods, 10);
        assert_eq!(fresh.tail_ns(), None);
    }

    /// The law takes either estimator. A path that answered three probes seconds late moves the
    /// RFC 9002 estimate's tail by seconds and the median and MAD's not at all: the difference the
    /// timed simulation of note 32 §3.7 weighs.
    #[test]
    fn the_law_takes_either_estimator_and_late_answers_move_only_the_smoothed_one() {
        let mut median = PathRtt::new();
        let mut smoothed = ExchangeRtt::new();
        for _ in 0..crate::PATH_WINDOW {
            median.on_sample(5 * MS);
            smoothed.on_sample(5 * MS);
        }
        for late in [2_900, 1_400, 800] {
            median.on_sample(late * MS);
            smoothed.on_sample(late * MS);
        }
        let by_median = ElectionTiming::derive(HEARTBEAT, 0, [&median]);
        let by_smoothed = ElectionTiming::derive(HEARTBEAT, 0, [&smoothed]);
        assert_eq!(
            by_median.base_periods, 10,
            "three late answers of 16 move nothing"
        );
        assert!(
            by_smoothed.base_periods > 100,
            "the smoothed tail carries the late answers: {}",
            by_smoothed.base_periods
        );
    }

    fn ages_without_firing(
        timer: &mut ElectionTimer,
        contact: u64,
        timing: &ElectionTiming,
        local: u64,
        periods: u32,
    ) -> bool {
        (0..periods)
            .all(|_| timer.follower_period(contact, timing, local, 0) != FollowerStep::Campaign)
    }

    #[test]
    fn a_follower_campaigns_after_its_jittered_timeout() {
        let timing = ElectionTiming::floor();
        let local = 3;
        let timeout = timing.timeout_periods(local, 0);
        assert!(
            timeout >= timing.base_periods && timeout < timing.base_periods + timing.span_periods
        );
        let mut timer = ElectionTimer::new();
        assert!(ages_without_firing(
            &mut timer,
            0,
            &timing,
            local,
            timeout - 1
        ));
        assert_eq!(
            timer.follower_period(0, &timing, local, 0),
            FollowerStep::Campaign
        );
        assert_eq!(timer.attempts(), 1);
    }

    #[test]
    fn contact_resets_the_follower_and_the_next_attempt_draws_afresh() {
        let timing = ElectionTiming::floor();
        let local = 3;
        let mut timer = ElectionTimer::new();
        let first = timing.timeout_periods(local, 0);
        assert!(ages_without_firing(
            &mut timer,
            0,
            &timing,
            local,
            first - 1
        ));
        assert_eq!(
            timer.follower_period(0, &timing, local, 0),
            FollowerStep::Campaign
        );
        assert!(ages_without_firing(&mut timer, 0, &timing, local, 5));
        assert_eq!(
            timer.follower_period(1, &timing, local, 0),
            FollowerStep::Follow
        );
        assert_eq!(timer.idle_periods(), 0);
        let second = timing.timeout_periods(local, 1);
        assert!(ages_without_firing(
            &mut timer,
            1,
            &timing,
            local,
            second - 1
        ));
        assert_eq!(
            timer.follower_period(1, &timing, local, 0),
            FollowerStep::Campaign
        );
    }

    /// Over every pair of 32 ids and every attempt offset (4,960 trials of 64 shared attempts),
    /// independent draws collide about once in `span`: a mean of 6.4 per 64, and a maximum of 24 has
    /// probability 2.2 × 10⁻⁵ (slates' derivation).
    #[test]
    fn two_nodes_draws_stay_independent_across_shared_attempts() {
        let timing = ElectionTiming::floor();
        let ids: Vec<u64> = (1..=16)
            .chain((1..=16).map(|seed: u64| seed.wrapping_mul(0x2545_f491_4f6c_dd1d)))
            .collect();
        let mut worst = 0;
        let mut total = 0;
        let mut trials = 0;
        for (index, left) in ids.iter().enumerate() {
            for right in ids.iter().skip(index + 1) {
                for offset in 0..timing.span_periods {
                    let collisions = (0..64u32)
                        .filter(|attempt| {
                            timing.timeout_periods(*left, *attempt)
                                == timing.timeout_periods(*right, attempt + offset)
                        })
                        .count();
                    worst = worst.max(collisions);
                    total += collisions;
                    trials += 1;
                }
            }
        }
        assert!(
            worst < 24,
            "a pair collided on {worst} of 64 shared attempts"
        );
        let mean_tenths = total * 10 / trials;
        assert!(
            (54..=74).contains(&mean_tenths),
            "{mean_tenths} tenths per 64"
        );
    }

    #[test]
    fn a_leader_checks_its_quorum_every_base_period() {
        let floor = ElectionTiming::floor();
        let mut timer = ElectionTimer::new();
        let fired = (1..=20).filter(|_| timer.leader_period(&floor)).count();
        assert_eq!(fired, 2);
        let wan = derive(&[&path_of(&WAN_SAMPLES_MS)]);
        let mut timer = ElectionTimer::new();
        let first = (1..=100).find(|_| timer.leader_period(&wan)).unwrap();
        assert_eq!(first, wan.base_periods);
    }

    #[test]
    fn a_follower_yields_one_timeout_per_rank() {
        let timing = ElectionTiming::floor();
        let local = 9;
        let mut timer = ElectionTimer::new();
        let mut fired = Vec::new();
        for _ in 0..3 {
            let mut periods = 0;
            loop {
                periods += 1;
                let campaign =
                    timer.follower_period(0, &timing, local, 2) == FollowerStep::Campaign;
                if campaign || timer.idle_periods() == 0 {
                    fired.push(campaign);
                    break;
                }
                assert!(periods < 1_000);
            }
        }
        assert_eq!(fired, vec![false, false, true]);
        assert_eq!(
            timer.follower_period(1, &timing, local, 2),
            FollowerStep::Follow
        );
        assert_eq!(timer.yielded(), 0);
        let mut rank_zero = ElectionTimer::new();
        let mut periods = 0;
        while rank_zero.follower_period(0, &timing, local, 0) != FollowerStep::Campaign {
            periods += 1;
            assert!(periods < 1_000);
        }
    }

    #[test]
    fn a_followers_lease_lapses_at_the_minimum_election_timeout() {
        let timing = ElectionTiming::floor();
        let local = 9;
        let mut timer = ElectionTimer::new();
        let steps: Vec<FollowerStep> = (1..=timing.base_periods)
            .map(|_| timer.follower_period(0, &timing, local, 2))
            .collect();
        let (last, before) = steps.split_last().unwrap();
        assert!(before.iter().all(|step| *step == FollowerStep::Follow));
        assert_eq!(*last, FollowerStep::LeaderLapsed);
        let mut periods = timing.base_periods;
        loop {
            periods += 1;
            match timer.follower_period(0, &timing, local, 2) {
                FollowerStep::Campaign => break,
                step => assert_eq!(step, FollowerStep::LeaderLapsed, "period {periods}"),
            }
            assert!(periods < 1_000);
        }
        assert_eq!(timer.yielded(), 2);
        assert_eq!(
            timer.follower_period(1, &timing, local, 2),
            FollowerStep::Follow
        );
        assert_eq!(
            timer.follower_period(1, &timing, local, 2),
            FollowerStep::Follow
        );
    }

    #[test]
    fn the_quorum_round_trip_is_the_majoritys_farthest_path() {
        let path = |rtt_ms: u64| {
            let mut path = ExchangeRtt::new();
            for _ in 0..8 {
                path.on_sample(rtt_ms * MS);
            }
            path
        };
        let near = path(72);
        let middle = path(162);
        let far = path(262);
        let three = quorum_priority([Some(&middle), Some(&near)], 3);
        assert_eq!(three.quorum_ns, near.smoothed_ns());
        assert_eq!(Some(three.spread_ns), near.spread_ns());
        let five = quorum_priority([Some(&far), None, Some(&near), Some(&middle)], 5);
        assert_eq!(five.quorum_ns, middle.smoothed_ns());
        assert_eq!(
            quorum_priority([None, None, Some(&near), None], 5),
            ElectionPriority::default()
        );
        assert_eq!(
            quorum_priority::<ExchangeRtt>(std::iter::empty(), 1),
            ElectionPriority::default()
        );
    }

    #[test]
    fn a_priority_outranks_only_beyond_both_spreads() {
        let near = ElectionPriority {
            quorum_ns: 70 * MS,
            spread_ns: 10 * MS,
        };
        let far = ElectionPriority {
            quorum_ns: 160 * MS,
            spread_ns: 40 * MS,
        };
        let close = ElectionPriority {
            quorum_ns: 90 * MS,
            spread_ns: 20 * MS,
        };
        assert!(near.outranks(&far));
        assert!(!far.outranks(&near));
        assert!(!near.outranks(&close), "overlapping intervals tie");
        assert!(
            !ElectionPriority::default().outranks(&far),
            "unknown never outranks"
        );
    }

    /// slates' round-budget cases under the unified law, with slates' ceiling: the deadline plus
    /// [`ELECTION_MARGIN`] periods, so a measured round is exactly slates' budget. An unmeasured
    /// round is given the whole ceiling, as focal's law gives it.
    #[test]
    fn a_round_budget_opens_to_the_measured_tail_within_its_ceiling() {
        let anchors = RoundAnchors {
            heartbeat_ns: HEARTBEAT,
            stall_periods: 2,
            polls_per_period: 10,
            lookahead: (3, 4),
        };
        let ceiling =
            |tail: Option<u64>| HEARTBEAT.max(tail.unwrap_or(0)) + ELECTION_MARGIN * HEARTBEAT;
        assert_eq!(anchors.poll_interval_ns(), HEARTBEAT / 10);
        let lan_tail = path_of(&LOOPBACK_SAMPLES_MS).tail_ns();
        let lan = RoundBudget::derive(&anchors, lan_tail, ceiling(lan_tail));
        assert_eq!(
            lan.deadline_ns, HEARTBEAT,
            "a tail inside a period changes nothing"
        );
        assert_eq!(lan.max_deadline_ns(), HEARTBEAT + 10 * HEARTBEAT);
        assert_eq!(lan.stall_window_ns, 2 * HEARTBEAT);
        let wan = path_of(&WAN_SAMPLES_MS).tail_ns();
        let far = RoundBudget::derive(&anchors, wan, ceiling(wan));
        assert_eq!(far.deadline_ns, wan.unwrap(), "the base opens to the tail");
        assert_eq!(far.max_deadline_ns(), wan.unwrap() + 10 * HEARTBEAT);
        let unmeasured = RoundBudget::derive(&anchors, None, ceiling(None));
        assert_eq!(unmeasured.deadline_ns, ceiling(None));
        assert_eq!(unmeasured.max_extensions, 0);
    }
}
