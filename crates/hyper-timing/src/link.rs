//! A link's failure detector, estimated from its heartbeats (`docs/timing.md` §2.2, §2.6).
//!
//! [`LinkEstimator`] is the receiving side of one node pair's heartbeat stream. It is fed each
//! heartbeat's sequence number and arrival time on the receiver's monotonic clock (the kernel's
//! receive stamp where the caller has one, §2.4), and it keeps:
//! - NFD-E's expected arrival (Chen, Toueg and Aguilera 2002; Toueg's DSN 2002 workshop slides):
//!   `EA_{s} = (1/n) Σ (A_i − iη) + sη` over the `n` most recent heartbeats received, so the
//!   freshness point of heartbeat `s` is `τ_s = EA_s + α`;
//! - the delay's mean and variance (§2.6, item 7: mean and variance, never the median and MAD);
//! - the loss `p_L` from the sequence numbers, the Jeffreys posterior mean `(k + ½)/(m + 1)`;
//! - the window `n = min(n_G, n_A)` at the link's interval (§2.6, item 2), computed online;
//! - the trust NFD-E gives: trusted while the latest heartbeat `h` is fresh, until `τ_{h+1}`.
//!
//! **The delay's variance.** What the detector compares with its margin is a heartbeat's arrival
//! less its expected arrival, `A_i − EA_i`; Theorem 7 bounds the probability that it passes `α`
//! through Cantelli's inequality on that quantity. Its variance is `V(D)(1 + 1/n)` for independent
//! delays with a window of `n`, and it holds no offset or drift between the two hosts' clocks,
//! which `EA` follows and a plain variance of `A_i − iη` would count as delay: two oscillators
//! apart by RFC 5905's 15 ppm drift 54 ms an hour, more than any delay the traces saw. The
//! estimator's `V(D)` is the variance of those prediction errors over the link's whole history,
//! not the window's (§2.6, item 3: the variance lives in stalls a short window has not seen).
//!
//! **What the history has not yet seen** (§3, item 3). A link with little history underestimates
//! `V(D)` until it has seen a stall. No prior is invented for it. Instead the estimator counts the
//! chance that the next heartbeat is later than every one it has seen, which for exchangeable
//! samples is exactly `1/(m + 1)` after `m` of them whatever their distribution (the probability
//! that the last of `m + 1` is the largest, the first record indicator's law: Rényi 1962; Arnold,
//! Balakrishnan and Nagaraja, *Records*, 1998, ch. 2), with `m` the history's independent
//! heartbeats, `m = count / τ_int`. A heartbeat that late is, to Theorem 7, as good as lost:
//! `Pr(lost or later than x) ≤ p + (1 − p)·V/(V + x²)` with `p = 1 − (1 − p_L)(1 − 1/(m + 1))` and
//! `V` the variance of the delays within the range seen, which is exactly Theorem 7's factor with
//! `p` for `p_L`. What this leaves open is the sampling error of `V` within that range, which a
//! heavy tail skews low (`docs/timing.md` §3, item 3). So
//! the configurator is fed `p` as the loss: early, a margin of one heartbeat cannot promise
//! better than `1/(m + 1)`, and the configurator spreads the margin over more heartbeats until the
//! history has shown more. The estimator refuses to configure until it has the evidence `m` needs:
//! two prediction errors (a variance) and an integrated autocorrelation time it has measured
//! ([`Refusal`]).
//!
//! **The window** (§2.6, item 2), at the interval the link sends at:
//! - `n_A`: the window at which the Allan deviation of the window means stops falling (Allan 1966),
//!   the shortest within the statistical tolerance `1/√(2(K−1))` of the least, over windows
//!   `1, 2, 4, …` kept online: each level holds the sum of its unfinished window and the running
//!   sum of squared differences of consecutive window means, so a heartbeat costs one step a level.
//! - `n_G = ⌈τ_int·V/G²⌉`: the window whose mean is within the timer's resolution `G`. `τ_int` is
//!   read from the same levels: the variance of an `m`-mean of a series is `V·τ_int/m` once `m`
//!   is long against its correlation (Sokal 1997, §3), and the Allan variance of non-overlapping
//!   `m`-means is that variance less the covariance of neighbouring means, which vanishes there; so
//!   `τ̂(m) = m·σ²_A(m)/V` at the shortest `m ≥ c·τ̂(m)` (Madras and Sokal's self-consistent window,
//!   `c = 6`). The Allan form is the one a drift does not inflate.
//!
//! **The window's bound**, the ring's capacity, derived. The expected arrival is a mean over the
//! window, centred `(n − 1)/2` heartbeats back, and predicts `(n + 1)/2` past that centre. Two
//! clocks within RFC 5905's frequency tolerance `PHI` of true time (15 ppm, §7.2) drift apart by up
//! to `2·PHI·η` a heartbeat, so the expected arrival lags a drifting pair by up to
//! `PHI·η·(n + 1)`. A window whose lag passes `G` averages over a link that has moved by more than
//! the timer can observe, which is what `n_A` exists to stop; so `n + 1 ≤ G / (PHI·η)`. The
//! configurator never sets `η` below `G` (its floor), so no link's window passes
//! `1/PHI − 1 =` [`WINDOW_LIMIT`] (66,665), and a link's ring holds `G/(PHI·η) − 1` prefix sums of
//! eight bytes, set when it is built: 3,332 (27 KiB) for Linux's 1 ms tick at a 20 ms interval,
//! 33,332 (267 KiB) for macOS's half-the-wait coalescing, at most 533 KiB. A pair of clocks that
//! drift faster than `PHI` shows it in the Allan deviation, and `n_A` binds first.
//!
//! The estimator is sans-io: it is fed `now`, arrivals and the measured floors, and returns events
//! and deadlines. Once built, a heartbeat, a poll and a read allocate nothing (`benches/estimator.rs`,
//! `docs/benchmarks.md`).

use std::time::Duration;

use crate::qos::{Costs, Detector, Floors, LinkBehaviour, configure, detector_at};

/// RFC 5905 §7.2, `PHI`: the frequency tolerance NTP assumes of a clock, 15 ppm, in parts per
/// [`MILLION`].
const PHI_PER_MILLION: u64 = 15;
/// The unit of [`PHI_PER_MILLION`].
const MILLION: u64 = 1_000_000;
/// The longest window any link's estimator can hold: `n + 1 ≤ G/(PHI·η)` and `η ≥ G`, so
/// `n ≤ 1/PHI − 1` (the module's derivation).
pub const WINDOW_LIMIT: u64 = (MILLION / PHI_PER_MILLION).saturating_sub(1);
/// The Allan levels kept, windows `1, 2, 4, …, 2^(LEVELS−1)`: every power of two up to
/// [`WINDOW_LIMIT`], so `n_A` can reach any window a link can hold.
const LEVELS: usize = (u64::BITS - WINDOW_LIMIT.leading_zeros()) as usize;
/// The windows a level needs before its deviation enters the comparison: seven. Its relative
/// uncertainty `1/√(2(K−1))` (Allan 1966; IEEE Std 1139) must be finer than the fall a doubling of
/// the window makes for white noise, `1 − 1/√2`, or the comparison cannot tell a falling curve from
/// a flat one: `K > 1 + 1/(2(1 − 1/√2)²) ≈ 6.83` (a test computes it).
const ALLAN_WINDOWS: u64 = 7;
/// Madras and Sokal's window constant: `τ_int` is summed to the first window `M ≥ c·τ_int(M)`, with
/// `c ≈ 6` for a correlation that decays as an exponential (Sokal 1997, §3).
const SOKAL_C: f64 = 6.0;
/// The largest offset a heartbeat may have from its link's anchor, in nanoseconds, so that the sum
/// of a full window of them fits the `i64` the ring sums in: `i64::MAX / WINDOW_LIMIT`, about 38
/// hours. An offset that large is a sender that restarted its schedule; the caller re-anchors with
/// [`LinkEstimator::retime`].
const OFFSET_LIMIT: u64 = i64::MAX.unsigned_abs() / WINDOW_LIMIT;

/// Where a sender's schedule sits on the receiver's clock: heartbeat `seq` was scheduled at `at_ns`.
/// Known when the two share a clock (one host, as the traces did); then the estimator measures
/// `E(D)` itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Schedule {
    /// A heartbeat's sequence number.
    pub seq: u64,
    /// When it was scheduled, on the receiver's monotonic clock, nanoseconds.
    pub at_ns: u64,
}

/// Why an estimator could not be built or could not take a heartbeat.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EstimateError {
    /// A zero heartbeat interval.
    ZeroInterval,
    /// A zero timer granularity: nothing is measured yet ([`crate::Lateness`]).
    ZeroGranularity,
    /// A heartbeat whose offset from the link's anchor is past what a window can sum: a sender
    /// that restarted its schedule. The heartbeat is not taken.
    OutOfRange,
}

impl std::fmt::Display for EstimateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::ZeroInterval => "a heartbeat interval of zero",
            Self::ZeroGranularity => "a timer granularity of zero",
            Self::OutOfRange => "a heartbeat too far from its link's schedule to take",
        })
    }
}

impl std::error::Error for EstimateError {}

/// Why the estimator does not configure a detector yet: the evidence it lacks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Fewer than two prediction errors: no variance.
    TooFewHeartbeats,
    /// The integrated autocorrelation time is not measured: no Allan level with
    /// enough windows reaches Madras and Sokal's self-consistent window at this interval, so the
    /// history's independent heartbeats, and the chance of a heartbeat later than all of them, are
    /// unknown. A link this correlated at its interval is sending too often to be told apart from
    /// itself; at the correlation-time floor (§2.6, item 6) its heartbeats are independent and
    /// `τ_int` is measured within a few dozen of them.
    CorrelationUnmeasured,
    /// The configurator found no detector ([`configure`]).
    Unconfigurable,
}

/// A change in what the detector believes of the sender.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// A fresh heartbeat arrived while the sender was suspected or not yet trusted.
    Trusted,
    /// The latest heartbeat's freshness passed with no newer one: the sender is suspected.
    Suspected,
}

/// What the detector believes of the sender now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trust {
    /// No margin is configured yet, so nothing is believed.
    Unconfigured,
    /// The sender is trusted until `until_ns`, the next heartbeat's freshness point.
    Trusted {
        /// The freshness point on the receiver's clock.
        until_ns: u64,
    },
    /// The sender is suspected.
    Suspected,
}

/// The window and the bounds it is the least of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Window {
    /// The heartbeats the expected arrival is averaged over.
    pub length: u64,
    /// `n_G`, once `τ_int` is measured.
    pub granularity: Option<u64>,
    /// `n_A`, once a level has enough windows.
    pub allan: Option<u64>,
    /// The drift bound `G/(PHI·η) − 1`, at most the ring's capacity.
    pub drift: u64,
}

/// What the estimator holds now.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Estimates {
    /// Heartbeats taken.
    pub received: u64,
    /// Heartbeats never taken among those the sequence numbers say were sent.
    pub lost: u64,
    /// `p_L`, the Jeffreys posterior mean `(k + ½)/(m + 1)`.
    pub loss: f64,
    /// The chance the next heartbeat is later than every one seen, `1/(m + 1)` over the history's
    /// independent heartbeats; `None` until `τ_int` is measured.
    pub unseen: Option<f64>,
    /// `E(D)` over the window, where the sender's [`Schedule`] is known.
    pub mean_delay: Option<Duration>,
    /// `√V(D)`, the deviation of the prediction errors over the history; `None` before two.
    pub delay_deviation: Option<Duration>,
    /// `τ_int` in heartbeats at this interval, once measured.
    pub correlation: Option<f64>,
    /// The window.
    pub window: Window,
}

/// A configured detector: the one to run now and the one to move to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Configuration {
    /// What the configurator was fed.
    pub link: LinkBehaviour,
    /// The detector at the link's current interval: its margin is the one in force.
    pub current: Detector,
    /// The detector minimizing unavailability over every interval the floors allow: where its
    /// interval differs, the link should move to it ([`LinkEstimator::retime`]).
    pub best: Detector,
}

/// One Allan level: windows of `2^j` heartbeats.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Level {
    /// A finished window's sum waiting for its neighbour, to make one window of the next level.
    half: Option<i128>,
    /// The latest finished window's mean.
    previous: Option<f64>,
    /// The sum of squared differences of consecutive window means.
    squares: f64,
    /// The differences summed.
    pairs: u64,
}

impl Level {
    /// The Allan variance and the windows it is over, once there are [`ALLAN_WINDOWS`].
    fn variance(&self) -> Option<(f64, u64)> {
        let windows = self.pairs.saturating_add(1);
        (windows >= ALLAN_WINDOWS && self.pairs > 0)
            .then(|| (0.5 * self.squares / self.pairs as f64, windows))
    }
}

/// The Allan levels of a link's offsets at one interval.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Allan {
    levels: [Level; LEVELS],
}

impl Allan {
    const fn new() -> Self {
        Self {
            levels: [Level {
                half: None,
                previous: None,
                squares: 0.0,
                pairs: 0,
            }; LEVELS],
        }
    }

    /// One offset: a finished window at level 0, and at each level above whose window it finishes.
    fn push(&mut self, offset: i64) {
        let mut carry = Some(i128::from(offset));
        let mut width = 1.0f64;
        for level in &mut self.levels {
            let Some(sum) = carry.take() else { break };
            // i128 → f64 rounds past 2⁵³ ns of summed offset; the mean keeps its leading digits.
            let mean = sum as f64 / width;
            if let Some(previous) = level.previous {
                level.squares += (mean - previous) * (mean - previous);
                level.pairs = level.pairs.saturating_add(1);
            }
            level.previous = Some(mean);
            carry = match level.half.take() {
                None => {
                    level.half = Some(sum);
                    None
                }
                // Two windows of up to 2¹⁶ offsets of at most 2⁶³: far inside an i128.
                Some(first) => Some(first.wrapping_add(sum)),
            };
            width *= 2.0;
        }
    }

    /// The levels with enough windows, as `(window, Allan variance, windows)`, up to `limit`.
    fn qualified(&self, limit: u64) -> impl Iterator<Item = (u64, f64, u64)> + Clone + '_ {
        self.levels
            .iter()
            .enumerate()
            .map_while(move |(j, level)| {
                let window = u32::try_from(j)
                    .ok()
                    .and_then(|j| 1u64.checked_shl(j))
                    .filter(|w| *w <= limit)?;
                Some((window, level))
            })
            .filter_map(|(window, level)| level.variance().map(|(v, k)| (window, v, k)))
    }

    /// `n_A`: the shortest window whose Allan deviation is within the statistical tolerance of the
    /// least one's, `1/√(2(K−1))` relative for the least's `K` windows.
    fn window(&self, limit: u64) -> Option<u64> {
        let levels = self.qualified(limit);
        let (_, least, windows) = levels.clone().min_by(|a, b| a.1.total_cmp(&b.1))?;
        let spread = (2.0 * windows.saturating_sub(1) as f64).sqrt();
        let tolerance = least.sqrt() * (1.0 + 1.0 / spread);
        levels
            .filter(|(_, variance, _)| variance.sqrt() <= tolerance)
            .map(|(window, ..)| window)
            .next()
    }

    /// `τ_int` at the shortest level that holds Madras and Sokal's window, `m ≥ c·τ̂(m)`, with
    /// `τ̂(m) = m·σ²_A(m)/V`; `None` when no level does yet. A link whose prediction errors have no
    /// variance has a `τ_int` of one.
    fn correlation(&self, variance: f64) -> Option<f64> {
        self.qualified(u64::MAX).find_map(|(window, allan, _)| {
            let m = window as f64;
            let tau = if variance > 0.0 {
                (m * allan / variance).max(1.0)
            } else {
                1.0
            };
            (m >= SOKAL_C * tau).then_some(tau)
        })
    }
}

/// Welford's running mean and sum of squared deviations (Welford 1962): numerically stable, two
/// words.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Moments {
    count: u64,
    mean: f64,
    squares: f64,
}

impl Moments {
    fn add(&mut self, value: f64) {
        self.count = self.count.saturating_add(1);
        let step = value - self.mean;
        self.mean += step / self.count as f64;
        self.squares += step * (value - self.mean);
    }

    fn variance(&self) -> Option<f64> {
        (self.count >= 2).then(|| self.squares / self.count.saturating_sub(1) as f64)
    }
}

/// The receiving side of one node pair's heartbeat stream (the module's documentation).
#[derive(Clone, Debug)]
pub struct LinkEstimator {
    interval_ns: u64,
    granularity_ns: u64,
    schedule: Option<Schedule>,
    /// Where offsets are measured from: the schedule, or the first heartbeat at this interval.
    anchor: Option<Schedule>,
    /// Prefix sums of the offsets taken at this interval, `capacity + 1` of them, wrapping: a
    /// window's sum is a difference of two, exact while it fits an `i64` ([`OFFSET_LIMIT`]).
    sums: Vec<i64>,
    /// Offsets taken at this interval.
    taken: u64,
    allan: Allan,
    window: Window,
    correlation: Option<f64>,
    errors: Moments,
    first_seq: Option<u64>,
    highest: Option<u64>,
    received: u64,
    margin_ns: Option<u64>,
    trust: Trust,
    last_arrival_ns: Option<u64>,
    configured_at: u64,
}

/// `duration` in nanoseconds, saturating at `u64::MAX` (584 years).
fn nanos(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

/// `value` rounded up to a whole count no larger than `limit`, from a finite non-negative real;
/// `limit` for anything larger or not finite.
fn whole_up_to(value: f64, limit: u64) -> u64 {
    // u64 → f64 rounds only past 2⁵³, and the limit here is at most WINDOW_LIMIT.
    if value.is_nan() || value >= limit as f64 {
        return limit;
    }
    let up = value.ceil().max(0.0);
    let mut count = 0u64;
    let mut bit = 1u64
        .checked_shl(u64::BITS.saturating_sub(limit.leading_zeros()).min(63))
        .unwrap_or(0);
    // Bisect `up` into an integer without a narrowing cast.
    while bit > 0 {
        let candidate = count | bit;
        if (candidate as f64) <= up {
            count = candidate;
        }
        bit >>= 1;
    }
    count.min(limit)
}

/// The drift bound `G/(PHI·η) − 1`, at least one and at most [`WINDOW_LIMIT`].
fn drift_bound(granularity_ns: u64, interval_ns: u64) -> u64 {
    let scaled = u128::from(granularity_ns).saturating_mul(u128::from(MILLION));
    let per = u128::from(interval_ns).saturating_mul(u128::from(PHI_PER_MILLION));
    let bound = scaled.checked_div(per).unwrap_or(0).saturating_sub(1);
    u64::try_from(bound)
        .unwrap_or(u64::MAX)
        .clamp(1, WINDOW_LIMIT)
}

/// A time in `i128` nanoseconds on a `u64` clock: zero before its start, `u64::MAX` past its end.
fn clock(at: i128) -> u64 {
    u64::try_from(at.max(0)).unwrap_or(u64::MAX)
}

impl LinkEstimator {
    /// An estimator for heartbeats every `interval`, on a receiver whose timer granularity is
    /// `granularity` (`G`, [`crate::Lateness`]), and with the sender's schedule on this clock where
    /// it is known. The ring is sized here, the one allocation ([`WINDOW_LIMIT`]'s derivation).
    pub fn new(
        interval: Duration,
        granularity: Duration,
        schedule: Option<Schedule>,
    ) -> Result<Self, EstimateError> {
        let interval_ns = nanos(interval);
        let granularity_ns = nanos(granularity);
        if interval_ns == 0 {
            return Err(EstimateError::ZeroInterval);
        }
        if granularity_ns == 0 {
            return Err(EstimateError::ZeroGranularity);
        }
        let capacity = drift_bound(granularity_ns, interval_ns);
        let slots = usize::try_from(capacity.saturating_add(1)).unwrap_or(usize::MAX);
        Ok(Self {
            interval_ns,
            granularity_ns,
            schedule,
            anchor: schedule,
            sums: vec![0; slots],
            taken: 0,
            allan: Allan::new(),
            window: Window {
                length: 0,
                granularity: None,
                allan: None,
                drift: capacity,
            },
            correlation: None,
            errors: Moments::default(),
            first_seq: None,
            highest: None,
            received: 0,
            margin_ns: None,
            trust: Trust::Unconfigured,
            last_arrival_ns: None,
            configured_at: 0,
        })
    }

    /// The heartbeats now come every `interval`, from the next one taken, with the sender's new
    /// schedule where it is known. The window and the Allan levels start again, since both are at
    /// the interval; the loss, the history of prediction errors and the margin in force carry over
    /// until the next [`configure`](Self::configure). The ring is resized for the new interval: an
    /// allocation, once a change of interval.
    pub fn retime(
        &mut self,
        interval: Duration,
        schedule: Option<Schedule>,
    ) -> Result<(), EstimateError> {
        let fresh = Self::new(
            interval,
            Duration::from_nanos(self.granularity_ns),
            schedule,
        )?;
        self.interval_ns = fresh.interval_ns;
        self.schedule = schedule;
        self.anchor = schedule;
        self.sums = fresh.sums;
        self.taken = 0;
        self.allan = fresh.allan;
        self.window = fresh.window;
        self.correlation = None;
        self.configured_at = self.received;
        Ok(())
    }

    /// The receiver's granularity `G` moved ([`crate::Lateness`]): `n_G` and the drift bound follow
    /// it, the drift bound never past the ring's capacity. A zero granularity is ignored.
    pub fn set_granularity(&mut self, granularity: Duration) {
        let granularity_ns = nanos(granularity);
        if granularity_ns > 0 {
            self.granularity_ns = granularity_ns;
            self.update_window();
        }
    }

    /// Heartbeat `seq`, stamped `arrival_ns` on the receiver's monotonic clock. A heartbeat no newer
    /// than the latest is not taken: a duplicate, or one that came after a newer one, which the loss
    /// keeps counted as lost (the conservative side of `p_L`). Returns the trust it changed.
    ///
    /// The caller feeds every heartbeat stamped before a deadline before it polls that deadline
    /// ([`poll`](Self::poll)): a stamp is when the kernel received it, however late it is read.
    pub fn on_heartbeat(
        &mut self,
        seq: u64,
        arrival_ns: u64,
    ) -> Result<Option<Event>, EstimateError> {
        if self.highest.is_some_and(|highest| seq <= highest) {
            return Ok(None);
        }
        let anchor = *self.anchor.get_or_insert(Schedule {
            seq,
            at_ns: arrival_ns,
        });
        let offset = Self::offset(anchor, self.interval_ns, seq, arrival_ns)?;
        self.last_arrival_ns = Some(arrival_ns);
        if let Some(predicted) = self.window_mean() {
            self.errors.add(offset as f64 - predicted);
        }
        self.take(offset);
        self.first_seq.get_or_insert(seq);
        self.highest = Some(seq);
        self.received = self.received.saturating_add(1);
        Ok(self.refresh(arrival_ns))
    }

    /// `A − σ`: the arrival less its schedule, from `anchor`, in nanoseconds.
    fn offset(
        anchor: Schedule,
        interval_ns: u64,
        seq: u64,
        arrival_ns: u64,
    ) -> Result<i64, EstimateError> {
        let steps = i128::from(seq).wrapping_sub(i128::from(anchor.seq));
        let offset = steps
            .checked_mul(i128::from(interval_ns))
            .and_then(|scheduled| i128::from(arrival_ns).checked_sub(scheduled))
            .and_then(|delay| delay.checked_sub(i128::from(anchor.at_ns)))
            .and_then(|offset| i64::try_from(offset).ok())
            .ok_or(EstimateError::OutOfRange)?;
        if offset.unsigned_abs() > OFFSET_LIMIT {
            return Err(EstimateError::OutOfRange);
        }
        Ok(offset)
    }

    /// Folds an offset into the ring and the Allan levels, and moves the window.
    fn take(&mut self, offset: i64) {
        let slots = u64::try_from(self.sums.len()).unwrap_or(u64::MAX).max(1);
        let latest = self.slot(self.taken, slots);
        let next = self.taken.saturating_add(1);
        let sum = latest.wrapping_add(offset);
        if let Some(slot) = usize::try_from(next.checked_rem(slots).unwrap_or(0))
            .ok()
            .and_then(|at| self.sums.get_mut(at))
        {
            *slot = sum;
        }
        self.taken = next;
        self.allan.push(offset);
        self.update_window();
    }

    /// The prefix sum after `count` offsets.
    fn slot(&self, count: u64, slots: u64) -> i64 {
        usize::try_from(count.checked_rem(slots).unwrap_or(0))
            .ok()
            .and_then(|at| self.sums.get(at))
            .copied()
            .unwrap_or(0)
    }

    /// The window's length and the sum of its offsets, nanoseconds; `None` before an offset.
    fn window_sum(&self) -> Option<(u64, i64)> {
        let length = self.window.length.min(self.taken);
        if length == 0 {
            return None;
        }
        let slots = u64::try_from(self.sums.len()).unwrap_or(u64::MAX).max(1);
        let newest = self.slot(self.taken, slots);
        let oldest = self.slot(self.taken.saturating_sub(length), slots);
        // Wrapping: the window's true sum fits an i64 (OFFSET_LIMIT), so the difference is exact.
        Some((length, newest.wrapping_sub(oldest)))
    }

    /// The window's mean offset, nanoseconds; `None` before an offset.
    fn window_mean(&self) -> Option<f64> {
        let (length, sum) = self.window_sum()?;
        Some(sum as f64 / length as f64)
    }

    /// The window from the levels, the variance and `G`: `min(n_G, n_A, drift)` once `τ_int` is
    /// measured, and until then every offset taken, up to the drift bound (NFD-E's window filling to
    /// `n`).
    fn update_window(&mut self) {
        let capacity = u64::try_from(self.sums.len().saturating_sub(1)).unwrap_or(1);
        let drift = drift_bound(self.granularity_ns, self.interval_ns).min(capacity);
        let variance = self.errors.variance();
        self.correlation = variance.and_then(|v| self.allan.correlation(v));
        let allan = self.allan.window(drift);
        let granularity = self.correlation.zip(variance).map(|(tau, v)| {
            let g = self.granularity_ns as f64;
            whole_up_to(tau * v / (g * g), drift).max(1)
        });
        let length = match (granularity, allan) {
            (Some(n_g), Some(n_a)) => n_g.min(n_a),
            (Some(n_g), None) => n_g,
            _ => self.taken,
        };
        self.window = Window {
            length: length.clamp(1, drift),
            granularity,
            allan,
            drift,
        };
    }

    /// `EA_seq`, nanoseconds on the receiver's clock, from the window as it stands.
    fn expected_arrival(&self, seq: u64) -> Option<i128> {
        let anchor = self.anchor?;
        let (length, sum) = self.window_sum()?;
        // Truncated to the nanosecond: finer than any clock this runs on.
        let mean = i128::from(sum.checked_div(i64::try_from(length).ok()?)?);
        let steps = i128::from(seq).wrapping_sub(i128::from(anchor.seq));
        let scheduled = steps
            .checked_mul(i128::from(self.interval_ns))?
            .checked_add(i128::from(anchor.at_ns))?;
        scheduled.checked_add(mean)
    }

    /// The freshness point of the heartbeat after the latest: `τ_{h+1} = EA_{h+1} + α`.
    fn fresh_until(&self) -> Option<u64> {
        let margin = self.margin_ns?;
        let next = self.highest?.checked_add(1)?;
        let until = self
            .expected_arrival(next)?
            .checked_add(i128::from(margin))?;
        Some(clock(until))
    }

    /// The trust after a heartbeat stamped `arrival_ns`: trusted while it arrived before the next
    /// freshness point.
    fn refresh(&mut self, arrival_ns: u64) -> Option<Event> {
        let until = self.fresh_until()?;
        let was_trusted = matches!(self.trust, Trust::Trusted { .. });
        if arrival_ns < until {
            self.trust = Trust::Trusted { until_ns: until };
            (!was_trusted).then_some(Event::Trusted)
        } else {
            self.trust = Trust::Suspected;
            was_trusted.then_some(Event::Suspected)
        }
    }

    /// The trust under a new margin: a trusted sender is trusted to the new freshness point, a
    /// suspected one stays suspected until a heartbeat comes, and on the first configuration the
    /// latest heartbeat is fresh or not by its own stamp.
    fn retrusted(&self) -> Trust {
        let until = self.fresh_until();
        match (self.trust, until, self.last_arrival_ns) {
            (Trust::Trusted { .. }, Some(until_ns), _) => Trust::Trusted { until_ns },
            (Trust::Unconfigured, Some(until_ns), Some(at)) if at < until_ns => {
                Trust::Trusted { until_ns }
            }
            (Trust::Unconfigured, ..) => Trust::Suspected,
            (trust, ..) => trust,
        }
    }

    /// The time is `now_ns`: the sender is suspected once the latest heartbeat's freshness passed.
    pub fn poll(&mut self, now_ns: u64) -> Option<Event> {
        match self.trust {
            Trust::Trusted { until_ns } if now_ns >= until_ns => {
                self.trust = Trust::Suspected;
                Some(Event::Suspected)
            }
            _ => None,
        }
    }

    /// When to [`poll`](Self::poll) next: the freshness point, while the sender is trusted.
    pub fn deadline(&self) -> Option<u64> {
        match self.trust {
            Trust::Trusted { until_ns } => Some(until_ns),
            _ => None,
        }
    }

    /// What the detector believes of the sender now.
    pub fn trust(&self) -> Trust {
        self.trust
    }

    /// The heartbeat interval the estimator is at.
    pub fn interval(&self) -> Duration {
        Duration::from_nanos(self.interval_ns)
    }

    /// The margin in force, once configured.
    pub fn margin(&self) -> Option<Duration> {
        self.margin_ns.map(Duration::from_nanos)
    }

    /// Whether the estimates have renewed since the last configuration: a window's worth of
    /// heartbeats taken, the span over which the expected arrival is renewed once, or never
    /// configured. Chen et al.'s adaptive detector reconfigures as its estimates move (§6); the
    /// window is how far they move between independent readings.
    pub fn reconfigure_due(&self) -> bool {
        self.margin_ns.is_none()
            || self.received.saturating_sub(self.configured_at) >= self.window.length
    }

    /// `p_L`, the Jeffreys posterior mean `(k + ½)/(m + 1)` after `k` lost of `m` sent, the sent
    /// counted from the first sequence number taken to the latest (Jeffreys 1946; Brown, Cai and
    /// DasGupta 2001).
    fn loss(&self) -> (u64, f64) {
        let sent = match (self.first_seq, self.highest) {
            (Some(first), Some(highest)) => highest.saturating_sub(first).saturating_add(1),
            _ => 0,
        };
        let lost = sent.saturating_sub(self.received);
        (lost, (lost as f64 + 0.5) / (sent as f64 + 1.0))
    }

    /// The chance the next heartbeat is later than every one in the history: `1/(m + 1)` over its
    /// `m = count / τ_int` independent prediction errors.
    fn unseen(&self) -> Option<f64> {
        let tau = self.correlation?;
        Some(1.0 / (self.errors.count as f64 / tau + 1.0))
    }

    /// What the estimator holds now.
    pub fn estimates(&self) -> Estimates {
        let (lost, loss) = self.loss();
        let mean_delay = self
            .schedule
            .and(self.window_mean())
            .and_then(|mean| Duration::try_from_secs_f64(mean.max(0.0) / 1e9).ok());
        Estimates {
            received: self.received,
            lost,
            loss,
            unseen: self.unseen(),
            mean_delay,
            delay_deviation: self
                .errors
                .variance()
                .and_then(|v| Duration::try_from_secs_f64(v.sqrt() / 1e9).ok()),
            correlation: self.correlation,
            window: self.window,
        }
    }

    /// What the configurator is fed: the loss `p = 1 − (1 − p_L)(1 − 1/(m + 1))`, the mean delay
    /// (zero where the sender's schedule is unknown: Chen et al.'s NFD-E bound on detection is then
    /// past `E(D)`, and the configured interval and margin do not depend on it) and the deviation of
    /// the prediction errors.
    pub fn behaviour(&self) -> Result<LinkBehaviour, Refusal> {
        let variance = self.errors.variance().ok_or(Refusal::TooFewHeartbeats)?;
        let unseen = self.unseen().ok_or(Refusal::CorrelationUnmeasured)?;
        let estimates = self.estimates();
        Ok(LinkBehaviour {
            loss: 1.0 - (1.0 - estimates.loss) * (1.0 - unseen),
            mean_delay: estimates.mean_delay.unwrap_or(Duration::ZERO),
            delay_deviation: Duration::try_from_secs_f64(variance.sqrt() / 1e9)
                .map_err(|_| Refusal::Unconfigurable)?,
        })
    }

    /// Configures the detector from the estimates as they stand, within the measured `floors` and
    /// for an election and node failures costing `costs`. The margin for the current interval takes
    /// force at once; the best interval is returned for the caller to move the link to. The
    /// configurator's search allocates nothing.
    pub fn configure(&mut self, costs: &Costs, floors: &Floors) -> Result<Configuration, Refusal> {
        let link = self.behaviour()?;
        let current =
            detector_at(&link, costs, floors, self.interval()).ok_or(Refusal::Unconfigurable)?;
        let best = configure(&link, costs, floors).ok_or(Refusal::Unconfigurable)?;
        self.margin_ns = Some(nanos(current.margin));
        self.configured_at = self.received;
        self.trust = self.retrusted();
        Ok(Configuration {
            link,
            current,
            best,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_constants_follow_from_their_derivations() {
        assert_eq!(WINDOW_LIMIT, 66_665);
        assert_eq!(LEVELS, 17);
        let (low, high) = (1u64 << (LEVELS - 1), 1u64 << LEVELS);
        assert!(low <= WINDOW_LIMIT && WINDOW_LIMIT < high);
        let fall = 1.0 - 1.0 / 2f64.sqrt();
        let least = 1.0 + 1.0 / (2.0 * fall * fall);
        assert_eq!(ALLAN_WINDOWS as f64, least.ceil());
        let full = u128::from(OFFSET_LIMIT) * u128::from(WINDOW_LIMIT);
        assert!(full <= u128::from(i64::MAX.unsigned_abs()));
    }

    #[test]
    fn the_drift_bound_is_g_over_phi_eta_less_one() {
        // Linux's 1 ms tick at a 20 ms interval.
        assert_eq!(drift_bound(1_000_000, 20_000_000), 3_332);
        // macOS's coalescing at half the wait.
        assert_eq!(drift_bound(25_000_000, 50_000_000), 33_332);
        assert_eq!(drift_bound(1, u64::MAX), 1);
        assert_eq!(drift_bound(u64::MAX, 1), WINDOW_LIMIT);
    }

    #[test]
    fn whole_counts_round_up_within_the_limit() {
        assert_eq!(whole_up_to(0.0, 10), 0);
        assert_eq!(whole_up_to(2.1, 10), 3);
        assert_eq!(whole_up_to(10.0, 10), 10);
        assert_eq!(whole_up_to(1e300, 10), 10);
        assert_eq!(whole_up_to(f64::NAN, 10), 10);
        assert_eq!(whole_up_to(65_535.5, WINDOW_LIMIT), 65_536);
    }

    use proptest::prelude::*;

    const MS: u64 = 1_000_000;

    fn costs() -> Costs {
        Costs {
            election: Duration::from_micros(400),
            mtbf: Duration::from_secs(3_600),
        }
    }

    fn floors(interval: Duration) -> Floors {
        Floors {
            granularity: Duration::from_micros(50),
            sender: Duration::from_micros(50),
            correlation: interval,
        }
    }

    /// A xorshift stream (Marsaglia 2003): deterministic test noise.
    fn noise(state: &mut u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state
    }

    /// The Allan variance of `x` at windows of `m` as the trace analyser computes it, in one pass
    /// over the whole series.
    fn batch_allan(x: &[i64], m: usize) -> Option<(f64, u64)> {
        let means: Vec<f64> = x
            .chunks_exact(m)
            .map(|c| c.iter().map(|&v| v as f64).sum::<f64>() / m as f64)
            .collect();
        if (means.len() as u64) < ALLAN_WINDOWS {
            return None;
        }
        let sum: f64 = means.windows(2).map(|w| (w[1] - w[0]).powi(2)).sum();
        Some((0.5 * sum / (means.len() - 1) as f64, means.len() as u64))
    }

    proptest! {
        /// The online levels hold what the analyser's batch computation finds, at every window.
        #[test]
        fn the_allan_levels_are_the_batch_computation(
            x in prop::collection::vec(-1_000_000_000i64..1_000_000_000, 1..700)
        ) {
            let mut allan = Allan::new();
            for &v in &x {
                allan.push(v);
            }
            for (j, level) in allan.levels.iter().enumerate() {
                let m = 1usize << j;
                let expected = if m <= x.len() { batch_allan(&x, m) } else { None };
                match (level.variance(), expected) {
                    (Some((a, k)), Some((b, kb))) => {
                        prop_assert_eq!(k, kb);
                        prop_assert!((a - b).abs() <= 1e-9 * b.abs().max(1.0), "{} {}", a, b);
                    }
                    (None, None) => {}
                    other => prop_assert!(false, "level {}: {:?}", j, other),
                }
            }
        }

        /// The window's sum is the sum of the latest `n` offsets, its mean predicts each heartbeat,
        /// the history's variance is the two-pass variance of those prediction errors, and the loss
        /// is Jeffreys' from the sequence numbers, under losses, duplicates and reordering.
        #[test]
        fn the_estimates_are_their_definitions(
            steps in prop::collection::vec((0u64..4, 0u64..3 * MS, 0u8..8), 2..400),
            granularity in 1u64..20 * MS,
            shared in any::<bool>(),
        ) {
            let interval = 10 * MS;
            let schedule = Schedule { seq: 100, at_ns: 7 * MS };
            let mut link = LinkEstimator::new(
                Duration::from_nanos(interval),
                Duration::from_nanos(granularity),
                shared.then_some(schedule),
            ).unwrap();
            let (mut seq, mut offsets, mut errors) = (100u64, Vec::new(), Vec::new());
            let (mut sent, mut received) = (0u64, 0u64);
            let mut first_seq = None;
            // Unshared clocks: offsets are measured from the first heartbeat's arrival.
            let first = steps[0].1;
            let anchor = (seq, 7 * MS + if shared { 0 } else { first });
            for (gap, delay, replay) in steps {
                // A replay of an older heartbeat is not taken.
                if replay == 0 && received > 0 {
                    prop_assert_eq!(link.on_heartbeat(seq - 1, u64::MAX / 2).unwrap(), None);
                }
                seq += gap + u64::from(received > 0);
                let arrival = 7 * MS + (seq - anchor.0) * interval + delay;
                let before = link.window_sum();
                link.on_heartbeat(seq, arrival).unwrap();
                let offset = arrival as i64 - anchor.1 as i64 - ((seq - anchor.0) * interval) as i64;
                if let Some((length, sum)) = before {
                    let n = length as usize;
                    let expected: i64 = offsets.iter().rev().take(n).sum();
                    prop_assert_eq!(sum, expected);
                    errors.push(offset as f64 - sum as f64 / length as f64);
                }
                offsets.push(offset);
                let first_taken = *first_seq.get_or_insert(seq);
                sent = seq - first_taken + 1;
                received += 1;
                let window = link.estimates().window;
                prop_assert!(window.length >= 1 && window.length <= window.drift);
            }
            let estimates = link.estimates();
            prop_assert_eq!(estimates.received, received);
            prop_assert_eq!(estimates.lost, sent - received);
            let jeffreys = ((sent - received) as f64 + 0.5) / (sent as f64 + 1.0);
            prop_assert!((estimates.loss - jeffreys).abs() < 1e-12);
            if errors.len() >= 2 {
                let mean = errors.iter().sum::<f64>() / errors.len() as f64;
                let variance = errors.iter().map(|e| (e - mean).powi(2)).sum::<f64>()
                    / (errors.len() - 1) as f64;
                let held = link.errors.variance().unwrap();
                prop_assert!((held - variance).abs() <= 1e-6 * variance.max(1.0), "{} {}", held, variance);
            }
        }
    }

    /// Heartbeats every `interval` with independent delays of `base` plus up to `spread`.
    fn white(link: &mut LinkEstimator, count: u64, base: u64, spread: u64, state: &mut u64) {
        let interval = link.interval_ns;
        let start = link.highest.map_or(0, |h| h + 1);
        for seq in start..start + count {
            let delay = base + noise(state) % spread;
            link.on_heartbeat(seq, seq * interval + delay).unwrap();
        }
    }

    #[test]
    fn the_estimator_refuses_until_it_has_its_evidence() {
        let interval = Duration::from_millis(50);
        let mut link = LinkEstimator::new(interval, Duration::from_micros(50), None).unwrap();
        assert_eq!(
            link.configure(&costs(), &floors(interval)),
            Err(Refusal::TooFewHeartbeats)
        );
        let mut state = 0x2545_F491_4F6C_DD1D;
        white(&mut link, 4, MS, MS, &mut state);
        assert_eq!(
            link.configure(&costs(), &floors(interval)),
            Err(Refusal::CorrelationUnmeasured),
            "no Allan level has its seven windows"
        );
        assert_eq!(link.trust(), Trust::Unconfigured);
        white(&mut link, 200, MS, MS, &mut state);
        let configured = link.configure(&costs(), &floors(interval)).unwrap();
        let estimates = link.estimates();
        // Independent delays: τ_int near one, and the unseen chance one in the history's count.
        let tau = estimates.correlation.unwrap();
        assert!(tau < 2.0, "τ_int {tau}");
        let unseen = estimates.unseen.unwrap();
        assert!((unseen - 1.0 / (203.0 / tau + 1.0)).abs() < 1e-12);
        // The configurator was fed the loss with the unseen chance in it.
        let fed = 1.0 - (1.0 - estimates.loss) * (1.0 - unseen);
        assert!((configured.link.loss - fed).abs() < 1e-15);
        assert_eq!(configured.current.interval, interval);
        assert_eq!(link.margin(), Some(configured.current.margin));
        assert!(matches!(
            link.trust(),
            Trust::Trusted { .. } | Trust::Suspected
        ));
    }

    #[test]
    fn a_zero_interval_or_granularity_builds_nothing() {
        let ms = Duration::from_millis(1);
        assert_eq!(
            LinkEstimator::new(Duration::ZERO, ms, None).err(),
            Some(EstimateError::ZeroInterval)
        );
        assert_eq!(
            LinkEstimator::new(ms, Duration::ZERO, None).err(),
            Some(EstimateError::ZeroGranularity)
        );
    }

    #[test]
    fn a_heartbeat_past_the_window_sum_is_refused_and_not_taken() {
        let mut link =
            LinkEstimator::new(Duration::from_millis(1), Duration::from_millis(1), None).unwrap();
        link.on_heartbeat(0, 1_000).unwrap();
        assert_eq!(
            link.on_heartbeat(1, u64::MAX),
            Err(EstimateError::OutOfRange)
        );
        assert_eq!(link.estimates().received, 1);
        link.on_heartbeat(1, 1_000 + MS).unwrap();
        assert_eq!(link.estimates().received, 2);
    }

    #[test]
    fn trust_lasts_to_the_next_freshness_point_and_a_fresh_heartbeat_restores_it() {
        let interval = Duration::from_millis(50);
        let mut link = LinkEstimator::new(
            interval,
            Duration::from_micros(50),
            Some(Schedule { seq: 0, at_ns: 0 }),
        )
        .unwrap();
        let mut state = 0x9E37_79B9_7F4A_7C15;
        white(&mut link, 300, 2 * MS, MS, &mut state);
        link.configure(&costs(), &floors(interval)).unwrap();
        let mean = link.estimates().mean_delay.unwrap();
        assert!(mean >= Duration::from_millis(2) && mean <= Duration::from_millis(3));
        let alpha = link.margin().unwrap();
        // The next heartbeat, on time.
        let h = link.highest.unwrap() + 1;
        let at = h * 50 * MS + 2 * MS;
        link.on_heartbeat(h, at).unwrap();
        let until = link.deadline().unwrap();
        // τ_{h+1} = σ_{h+1} + mean of the window + α, to the nanosecond the mean is truncated to.
        let (length, sum) = link.window_sum().unwrap();
        let expected = (h + 1) * 50 * MS + (sum / length as i64) as u64 + alpha.as_nanos() as u64;
        assert_eq!(until, expected);
        assert_eq!(link.poll(until - 1), None);
        assert_eq!(link.poll(until), Some(Event::Suspected));
        assert_eq!(link.poll(until + 1), None, "one event a suspicion");
        assert_eq!(link.trust(), Trust::Suspected);
        // The sender was alive: its next heartbeat restores trust.
        assert_eq!(
            link.on_heartbeat(h + 1, until + MS),
            Ok(Some(Event::Trusted))
        );
        assert!(matches!(link.trust(), Trust::Trusted { .. }));
    }

    #[test]
    fn a_new_interval_restarts_the_window_and_keeps_the_history() {
        let interval = Duration::from_millis(50);
        let mut link = LinkEstimator::new(interval, Duration::from_micros(50), None).unwrap();
        let mut state = 7;
        white(&mut link, 300, MS, MS, &mut state);
        link.configure(&costs(), &floors(interval)).unwrap();
        let before = link.estimates();
        link.retime(Duration::from_millis(100), None).unwrap();
        let after = link.estimates();
        assert_eq!(after.received, before.received);
        assert_eq!(after.delay_deviation, before.delay_deviation);
        assert_eq!(after.correlation, None, "τ_int is per interval");
        assert_eq!(
            link.margin(),
            Some(before.window).map(|_| link.margin().unwrap())
        );
        assert!(link.reconfigure_due() || link.margin().is_some());
        // Heartbeats at the new interval, numbered on from the old ones.
        let start = link.highest.unwrap() + 1;
        for seq in start..start + 100 {
            link.on_heartbeat(seq, seq * 100 * MS + MS).unwrap();
        }
        assert!(link.estimates().correlation.is_some());
        assert_eq!(link.estimates().lost, 0);
    }
}
