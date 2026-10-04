//! Copa (Arun and Balakrishnan, "Copa: Practical Delay-Based Congestion Control for the Internet",
//! NSDI 2018), with the changes slates' and focal's measurements made to it (docs/transport.md §4d).
//!
//! Copa aims at the rate `1/(δ·d_q)` packets a second, where `d_q` is the queueing delay it measures:
//! the least round trip of the last half smoothed round trip (`RTTstanding`) less the least of the last
//! ten seconds (`RTTmin`). Below that rate the window grows and above it the window shrinks, by
//! `v/(δ·cwnd)` packets for each packet acknowledged; the velocity `v` doubles once the window has moved
//! one way for three round trips (§2.1). Alone, the queue cycles from empty to about `2.5/δ` packets and
//! back every five round trips (§3). A loss is no signal by itself: it may be noise. When the queue has
//! not nearly emptied in the last round trips ([`MODE_WINDOW_SRTTS`]) another sender is filling it,
//! and Copa competes: `1/δ` grows each round trip without a loss and halves on a loss, until the queue
//! empties again (§2.2).
//!
//! From slates' law, which fixed its details from the paper, the authors' implementation (genericCC)
//! and mvfst: integer arithmetic throughout, Nichols' windowed filters, and RFC 9002 §7.8 bounding only
//! the window's growth, so a window the sender does not fill still shrinks.
//!
//! From focal's measurements (focal's record F39):
//! - slow start judges a doubling by what was sent after the last one;
//! - a round trip moves the window by half of itself at most ([`CopaConfig::stride`]);
//! - an explicit congestion mark (ECN-CE) is answered as a classic sender answers congestion, and for
//!   ten seconds after a mark past slow start the window grows as a classic sender's does.
//!
//! focal's derivations for the competing mode (focal b18: the mode judged over five round trips, a
//! delay sample judged by the window its packet was sent under, `1/δ` raised by `d_q/RTTstanding`)
//! and three variants of the mode's exit and its competing growth were measured over focal's grids
//! by a rule fixed before the runs; none met it, and the law is focal's from before b18
//! (docs/transport.md §4d).
//!
//! The law states its pacing, `2·cwnd/RTTstanding` (§2.1), and the connection's pacer sends at it
//! (`Controller::pacing_rate`): the paper's analysis of Copa's own cycle (§3) assumes it, and paced
//! at the connection's own five quarters of the window a smoothed round trip, Copa alone emptied its
//! queue every five round trips on one of focal's five paths.
//!
//! Open (docs/transport.md §4d, with the numbers): without a queue manager at long round trips Copa
//! takes more than the incumbents' bar (focal's finding 1); the mode test, judging one empty moment
//! as a queue that empties, leaves Copa competing after its competitor has gone (focal's finding 3);
//! under a single queue that CoDel manages, the manager empties the queue and hides the competition
//! from the mode test (focal's finding 4).

use std::any::Any;

use super::{Controller, ControllerMetrics};
use crate::connection::RttEstimator;
use crate::{Duration, Instant};

/// RFC 9002 §7.2: the initial window is ten datagrams...
const INITIAL_WINDOW_DATAGRAMS: u64 = 10;
/// ...limited to the larger of 14,720 bytes and two datagrams.
const INITIAL_WINDOW_LIMIT: u64 = 14_720;
/// RFC 9002 §7.2: the least window, in datagrams.
const MINIMUM_WINDOW_DATAGRAMS: u64 = 2;
/// Copa §2.1: `RTTmin` is the least round trip of the last ten seconds.
const MIN_RTT_WINDOW_NS: u64 = 10_000_000_000;
/// Copa §2.2: the queue must have been nearly empty "in the last 5 RTTs", the period of Copa's own
/// oscillation (§3: the queue oscillates "between having 0 and 2.5/δ̂ packets every five RTTs"). The
/// paper measures `RTTmax`, which scales "nearly", over four, and genericCC (`rtt-window.cc`), slates
/// and focal took four for both. focal's A1 took five, the cycle's period; measured with its other
/// derivations over focal's grids, it was not taken (docs/transport.md §4d).
const MODE_WINDOW_SRTTS: u64 = 4;
/// Copa §2.2: the queue is nearly empty when the least round trip of the mode's window is within a
/// tenth of that window's spread above `RTTmin`.
const NEARLY_EMPTY_FRACTION: u64 = 10;
/// Copa §2.1: the velocity doubles once the window has moved one way this many round trips running.
const VELOCITY_DIRECTION_THRESHOLD: u32 = 3;
/// Copa §2.2: the default mode's δ = 0.5, held as `1/δ`.
const DEFAULT_INV_DELTA: u64 = 2;
/// A round trip moves the window by half of itself at most. The velocity of the paper and of slates is
/// bounded by `cwnd·δ` packets, by which one round trip moves the window by all of itself; what the
/// window's change does to the queue is heard of a round trip later, so a window that falls for the
/// queue it built falls to its floor, and one that grows to the path's rate grows past it. focal
/// measured half to answer soonest over its paths, and to carry within a thousandth of the most: at
/// 100 Mbit/s and 100 ms, 95% of the path where the whole carried 67%.
const DEFAULT_STRIDE: u64 = 2;
/// What a mark multiplies the window by: a classic sender's answer to congestion, which a sender of
/// ECT(0) gives a mark (RFC 3168 §5; RFC 9002 §B.1's `kLossReductionFactor`). Of the backoffs the RFCs
/// give (RFC 3168 and RFC 9002: 1/2; RFC 9438: 7/10; RFC 8511's experimental β_ecn: 4/5), focal
/// measured the gentler ones to leave NewReno and CUBIC under CoDel less than nine tenths of what they
/// carry beside their own kind or CUBIC: at 1 Mbit/s and 100 ms over eight seeds, 7/10 left NewReno
/// 0.84 of it and 4/5 left CUBIC 0.79.
const DEFAULT_MARK_BACKOFF: MarkBackoff = MarkBackoff {
    numerator: 1,
    denominator: 2,
};

/// Copa §2.1: "the sender paces packets at a rate of 2·cwnd/RTTstanding packets per second", double
/// the window's rate "to accommodate imperfections in pacing".
const PACING_MULTIPLE: u128 = 2;
/// Nanoseconds in a second: the unit rates are stated per.
const NANOS_PER_SECOND: u128 = 1_000_000_000;

/// RFC 9002 §7.2's initial window for datagrams of `datagram` bytes.
fn initial_window(datagram: u64) -> u64 {
    INITIAL_WINDOW_DATAGRAMS
        .saturating_mul(datagram)
        .min(INITIAL_WINDOW_LIMIT.max(MINIMUM_WINDOW_DATAGRAMS.saturating_mul(datagram)))
}

/// One sample of a filter: when, and what.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Sample {
    time: u64,
    value: u64,
}

/// The greatest sample of the last `window` of the caller's clock, kept in three samples (Nichols'
/// filter, as Linux `lib/win_minmax.c`): constant space whatever the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct WindowedMax {
    best: Sample,
    second: Sample,
    third: Sample,
}

impl WindowedMax {
    fn new(time: u64, value: u64) -> Self {
        let sample = Sample { time, value };
        Self {
            best: sample,
            second: sample,
            third: sample,
        }
    }

    fn get(&self) -> u64 {
        self.best.value
    }

    fn update(&mut self, time: u64, window: u64, value: u64) -> u64 {
        let sample = Sample { time, value };
        if value >= self.best.value || time.saturating_sub(self.third.time) > window {
            *self = Self::new(time, value);
            return value;
        }
        if value >= self.second.value {
            self.second = sample;
            self.third = sample;
        } else if value >= self.third.value {
            self.third = sample;
        }
        self.age(sample, window);
        self.best.value
    }

    /// As time passes, a best older than the window gives way to the next; and a quarter of the window
    /// without a new second, or half without a new third, takes the sample for it, so the three stay
    /// spread over the window.
    fn age(&mut self, sample: Sample, window: u64) {
        let elapsed = sample.time.saturating_sub(self.best.time);
        if elapsed > window {
            self.shift(sample);
            if sample.time.saturating_sub(self.best.time) > window {
                self.shift(sample);
            }
        } else if self.second.time == self.best.time && elapsed > window.checked_div(4).unwrap_or(0)
        {
            self.second = sample;
            self.third = sample;
        } else if self.third.time == self.second.time
            && elapsed > window.checked_div(2).unwrap_or(0)
        {
            self.third = sample;
        }
    }

    fn shift(&mut self, sample: Sample) {
        self.best = self.second;
        self.second = self.third;
        self.third = sample;
    }
}

/// The least sample of the last `window`: the same filter over the order turned round.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct WindowedMin(WindowedMax);

impl WindowedMin {
    fn new(time: u64, value: u64) -> Self {
        Self(WindowedMax::new(time, u64::MAX.saturating_sub(value)))
    }

    fn get(&self) -> u64 {
        u64::MAX.saturating_sub(self.0.get())
    }

    fn update(&mut self, time: u64, window: u64, value: u64) -> u64 {
        u64::MAX.saturating_sub(self.0.update(time, window, u64::MAX.saturating_sub(value)))
    }
}

/// The least of `filter`'s window with `value` taken in, the filter begun with it if it had none.
fn least(filter: &mut Option<WindowedMin>, now: u64, window: u64, value: u64) -> u64 {
    match filter {
        Some(filter) => filter.update(now, window, value),
        None => filter.insert(WindowedMin::new(now, value)).get(),
    }
}

/// The greatest of `filter`'s window with `value` taken in, the filter begun with it if it had none.
fn most(filter: &mut Option<WindowedMax>, now: u64, window: u64, value: u64) -> u64 {
    match filter {
        Some(filter) => filter.update(now, window, value),
        None => filter.insert(WindowedMax::new(now, value)).get(),
    }
}

/// Which way the window moved over the last round trip (Copa §2.1's velocity rule).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Direction {
    Up,
    Down,
}

/// A fraction the window is multiplied by: `numerator/denominator`, at most one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MarkBackoff {
    numerator: u64,
    denominator: u64,
}

/// One acknowledged packet, as the law sees it. Times are nanoseconds of the law's clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Acked {
    now: u64,
    /// From the packet's sending to now.
    rtt: u64,
    /// The smoothed round trip.
    srtt: u64,
    bytes: u64,
    /// The sender had nothing more to send than its window let it.
    window_limited: bool,
}

/// What the filters make of one round-trip sample.
#[derive(Clone, Copy, Debug)]
struct Delays {
    /// The least round trip of the last ten seconds.
    rtt_min: u64,
    /// The least of the last half smoothed round trip.
    standing: u64,
    /// The least and the greatest of the mode's window.
    recent_min: u64,
    recent_max: u64,
}

impl Delays {
    /// The queueing delay `d_q`: `RTTstanding` less `RTTmin`.
    fn queueing(&self) -> u64 {
        self.standing.saturating_sub(self.rtt_min)
    }

    /// Copa §2.2: the queue nearly emptied in the mode's window.
    fn nearly_empty(&self) -> bool {
        let spread = self.recent_max.saturating_sub(self.rtt_min);
        self.recent_min == self.rtt_min
            || self.recent_min
                < self
                    .rtt_min
                    .saturating_add(spread.checked_div(NEARLY_EMPTY_FRACTION).unwrap_or(0))
    }
}

/// The law, on a clock of nanoseconds the caller counts.
#[derive(Clone, Debug)]
struct Law {
    datagram: u64,
    initial: u64,
    window: u64,
    /// `1/δ` as configured and as it is, in whole packets: focal's law, whose halving drops the
    /// fraction.
    default_inv_delta: u64,
    inv_delta: u64,
    /// What part of the window a round trip moves it by at most, as its inverse.
    stride: u64,
    competitive: bool,
    slow_start: bool,
    last_double: Option<u64>,
    min_rtt: Option<WindowedMin>,
    standing_rtt: Option<WindowedMin>,
    mode_min: Option<WindowedMin>,
    mode_max: Option<WindowedMax>,
    velocity: u64,
    direction: Direction,
    same_direction: u32,
    /// When the direction was last judged, and the window then.
    direction_mark: Option<(u64, u64)>,
    /// What of a step was less than a byte, kept for the next.
    step_remainder: u64,
    last_loss_update: u64,
    last_increase_update: u64,
    /// When the last mark was answered: a mark of what was sent before then is part of the round trip
    /// it answered (RFC 9002 §7.3.2).
    mark_recovery: Option<u64>,
    /// When a mark last came after slow start: what the window grows as a classic sender's for
    /// ([`Law::grows_as_a_classic_sender`]).
    marked_after_slow_start: Option<u64>,
    /// Whether a round trip went down since `1/δ` was last raised or the competitive mode began: the
    /// target held the window back.
    held_back: bool,
    mark_backoff: MarkBackoff,
}

impl Law {
    fn new(datagram: u64, config: &CopaConfig) -> Self {
        let datagram = datagram.max(1);
        let initial = initial_window(datagram);
        let inv_delta = config.inv_delta.max(1);
        Self {
            datagram,
            initial,
            window: initial,
            default_inv_delta: inv_delta,
            inv_delta,
            stride: config.stride.max(1),
            competitive: false,
            slow_start: true,
            last_double: None,
            min_rtt: None,
            standing_rtt: None,
            mode_min: None,
            mode_max: None,
            velocity: 1,
            direction: Direction::Up,
            same_direction: 0,
            direction_mark: None,
            step_remainder: 0,
            last_loss_update: 0,
            last_increase_update: 0,
            mark_recovery: None,
            marked_after_slow_start: None,
            held_back: false,
            mark_backoff: config.mark_backoff,
        }
    }

    fn minimum_window(&self) -> u64 {
        MINIMUM_WINDOW_DATAGRAMS.saturating_mul(self.datagram)
    }

    /// `2·cwnd/RTTstanding` in bytes a second (§2.1), once a standing round trip is measured; the
    /// connection's own rule until then.
    fn pacing_rate(&self) -> Option<u64> {
        let standing = self.standing_rtt?.get().max(1);
        let rate = u128::from(self.window)
            .saturating_mul(PACING_MULTIPLE)
            .saturating_mul(NANOS_PER_SECOND)
            .checked_div(u128::from(standing))
            .map_or(u64::MAX, |rate| u64::try_from(rate).unwrap_or(u64::MAX));
        Some(rate)
    }

    /// The path takes datagrams of another size (RFC 9002 §7.2): the window stays in bytes, and is the
    /// least window at that size at least.
    fn set_datagram(&mut self, datagram: u64) {
        self.datagram = datagram.max(1);
        self.window = self.window.max(self.minimum_window());
    }

    fn on_ack(&mut self, acked: Acked) {
        let srtt = acked.srtt.max(1);
        let delays = self.delays(acked.now, acked.rtt, srtt);
        let queueing = delays.queueing();
        self.update_mode(acked.now, srtt, &delays);
        let sent = acked.now.saturating_sub(acked.rtt);
        let increase = self.below_target(queueing, delays.standing);
        if increase && !acked.window_limited {
            // RFC 9002 §7.8: a window that is not used does not grow. It still shrinks: slates' guard
            // once skipped every update, and a window slow start had overshot stayed frozen while
            // losses kept the sender from looking window-limited (1.5 MB at a 250 kB product).
            return;
        }
        if self.slow_start && increase {
            self.double(acked.now, sent, srtt);
            return;
        }
        self.steer(acked, srtt, increase);
    }

    /// The round-trip sample through the four filters.
    fn delays(&mut self, now: u64, rtt: u64, srtt: u64) -> Delays {
        let mode_window = srtt.saturating_mul(MODE_WINDOW_SRTTS);
        Delays {
            rtt_min: least(&mut self.min_rtt, now, MIN_RTT_WINDOW_NS, rtt),
            standing: least(
                &mut self.standing_rtt,
                now,
                srtt.checked_div(2).unwrap_or(0),
                rtt,
            ),
            recent_min: least(&mut self.mode_min, now, mode_window, rtt),
            recent_max: most(&mut self.mode_max, now, mode_window, rtt),
        }
    }

    /// Whether the rate the sample measured, the window over `RTTstanding`, is at or below the target
    /// `1/(δ·d_q)`; in bytes, `window·d_q ≤ (1/δ)·datagram·RTTstanding`. An empty queue is below any
    /// target. The products of three 64-bit values fit 192 bits and not 128: the target saturates
    /// where it would not fit.
    fn below_target(&self, queueing: u64, standing: u64) -> bool {
        if queueing == 0 {
            return true;
        }
        let held = u128::from(self.window).saturating_mul(u128::from(queueing));
        let target = u128::from(self.inv_delta)
            .saturating_mul(u128::from(self.datagram))
            .saturating_mul(u128::from(standing));
        held <= target
    }

    /// Slow start doubles the window once what was sent after the last doubling is heard of. The queue
    /// a doubling builds is seen by what was sent after it, a round trip of sending later and a round
    /// trip of acknowledging after that. A doubling judged by what was sent before the last one doubles
    /// once more than the path holds: at 100 Mbit/s and 100 ms focal measured the window to come to
    /// 3.07 MB where the path and its queue hold 2.5 MB, the queue overflowing for as long as the
    /// transfer lasted, and what was asked beside it taking four round trips.
    fn double(&mut self, now: u64, sent: u64, srtt: u64) {
        match self.last_double {
            None => self.last_double = Some(now),
            Some(then) if sent > then.saturating_add(srtt) => {
                self.window = self.window.saturating_mul(2);
                self.last_double = Some(now);
            }
            Some(_) => {}
        }
    }

    /// Past slow start, or on a decrease: the window moves toward the target by `v/(δ·cwnd)` packets
    /// for the packet acknowledged; for ten seconds after a mark past slow start, it grows by a classic
    /// sender's datagram a round trip instead (RFC 9002 §B.5's `max_datagram_size · acked / cwnd`).
    fn steer(&mut self, acked: Acked, srtt: u64, increase: bool) {
        self.update_direction(acked.now, srtt);
        let wanted = if increase {
            Direction::Up
        } else {
            Direction::Down
        };
        if wanted != self.direction && self.velocity > 1 {
            // A reversal at speed starts over at one (mvfst `changeDirection`).
            self.direction = wanted;
            self.velocity = 1;
            self.same_direction = 0;
            self.direction_mark = Some((acked.now, self.window));
        }
        let gain = if increase && self.grows_as_a_classic_sender(acked.now) {
            1
        } else {
            self.velocity.saturating_mul(self.inv_delta)
        };
        let step = self.step(acked.bytes, gain);
        if increase {
            self.window = self.window.saturating_add(step);
        } else {
            self.slow_start = false;
            self.window = self.window.saturating_sub(step).max(self.minimum_window());
        }
    }

    /// `bytes` acknowledged move the window by `bytes·datagram·gain/cwnd`, the gain in packets; what
    /// is less than a byte is kept for the next step.
    fn step(&mut self, bytes: u64, gain: u64) -> u64 {
        let numerator = u128::from(bytes)
            .saturating_mul(u128::from(self.datagram))
            .saturating_mul(u128::from(gain))
            .saturating_add(u128::from(self.step_remainder));
        let denominator = u128::from(self.window.max(1));
        self.step_remainder = numerator
            .checked_rem(denominator)
            .and_then(|rest| u64::try_from(rest).ok())
            .unwrap_or(0);
        numerator
            .checked_div(denominator)
            .and_then(|step| u64::try_from(step).ok())
            .unwrap_or(u64::MAX)
    }

    /// Once a smoothed round trip: a direction held three times doubles the velocity and a change makes
    /// it one ([`Law::velocity_cap`] bounds it).
    fn update_direction(&mut self, now: u64, srtt: u64) {
        let Some((then, window_then)) = self.direction_mark else {
            self.direction_mark = Some((now, self.window));
            return;
        };
        if now.saturating_sub(then) < srtt {
            return;
        }
        let direction = if self.window > window_then {
            Direction::Up
        } else {
            Direction::Down
        };
        if self.window < window_then {
            self.held_back = true;
        }
        if direction == self.direction {
            self.same_direction = self.same_direction.saturating_add(1);
            if self.same_direction >= VELOCITY_DIRECTION_THRESHOLD {
                self.velocity = self.velocity.saturating_mul(2);
            }
        } else {
            self.velocity = 1;
            self.same_direction = 0;
        }
        self.velocity = self.velocity.min(self.velocity_cap());
        self.direction = direction;
        self.direction_mark = Some((now, self.window));
    }

    /// The velocity at most: `cwnd·δ` packets (genericCC `update_amt`), by which one round trip moves
    /// the window by all of itself, divided by the stride.
    fn velocity_cap(&self) -> u64 {
        u128::from(self.window.checked_div(self.datagram).unwrap_or(0))
            .checked_div(u128::from(self.inv_delta))
            .and_then(|packets| packets.checked_div(u128::from(self.stride)))
            .map_or(1, |cap| u64::try_from(cap).unwrap_or(u64::MAX))
            .max(1)
    }

    /// Copa §2.2: a queue that nearly emptied in the mode's window is Copa's own, and `1/δ` is the
    /// default; one that did not is another sender's, and competing, `1/δ` rises a packet each round
    /// trip without a loss.
    ///
    /// While the window grows as a classic sender's, a datagram a round trip, the target rises only
    /// after a round trip it held the window back. Raised a packet a round trip beside it, the target
    /// was never reached, the window never fell, and the queue Copa keeps never emptied for the mode to
    /// see Copa alone: alone at 100 Mbit/s and 20 ms under CoDel, focal measured Copa to take itself for
    /// competing (§2.2's test misjudges Copa's own queue now and then), raise `1/δ` to 58 and fill the
    /// queue to CoDel's target, a mark every three to six seconds. Held back, the window falls by Copa's
    /// own step, which empties the queue Copa alone keeps.
    fn update_mode(&mut self, now: u64, srtt: u64, delays: &Delays) {
        if delays.nearly_empty() {
            self.competitive = false;
            self.inv_delta = self.default_inv_delta;
            return;
        }
        if !self.competitive {
            self.competitive = true;
            self.last_increase_update = now;
            self.held_back = false;
        }
        if now.saturating_sub(self.last_increase_update) > srtt
            && now.saturating_sub(self.last_loss_update) > srtt
            && (self.held_back || !self.grows_as_a_classic_sender(now))
        {
            self.inv_delta = self.inv_delta.saturating_add(1);
            self.last_increase_update = now;
            self.held_back = false;
        }
    }

    /// Whether the window grows as a classic sender's does, a datagram a round trip: within the window
    /// Copa keeps its least round trip over (§2.1, ten seconds) of a mark that came after slow start. A
    /// queue manager keeps the queue short for every sender, so the senders filling it to the manager's
    /// target leave Copa's queue nearly empty and its competing mode unseen; the marks are what Copa
    /// sees of them. Copa's own growth, `v/δ` datagrams a round trip, took back after every mark the
    /// share a classic sender regrows a datagram a round trip, more of it the longer the round trip.
    /// Growing as a classic sender after its marks, competing Copa left NewReno and CUBIC 0.89 of what
    /// they carry beside their own kind at 1 Mbit/s and 100 ms under CoDel in focal's measurement. A
    /// mark in slow start is Copa's own doubling past the manager's target, which ending slow start
    /// answers. A window of four round trips (the mode's) let Copa grow by its own step between the
    /// marks of a long path and take from NewReno more than CUBIC does.
    fn grows_as_a_classic_sender(&self, now: u64) -> bool {
        self.marked_after_slow_start
            .is_some_and(|answered| now.saturating_sub(answered) <= MIN_RTT_WINDOW_NS)
    }

    /// A loss halves `1/δ` while Copa competes, once a round trip at most; otherwise it is no signal
    /// (§2.2: a loss may be noise, and a mode judged competing on a lossy path is no proof of a
    /// competitor). Persistent congestion leaves the least window.
    fn on_loss(&mut self, now: u64, srtt: u64, persistent: bool) {
        if self.competitive && now.saturating_sub(self.last_loss_update) > srtt {
            self.halve_target(now);
        }
        if persistent {
            self.window = self.minimum_window();
            self.slow_start = false;
        }
    }

    /// Competing, Copa's own rule for congestion: `1/δ` halves, never below the default.
    fn halve_target(&mut self, now: u64) {
        self.inv_delta = self
            .inv_delta
            .checked_div(2)
            .unwrap_or(0)
            .max(self.default_inv_delta);
        self.last_loss_update = now;
    }

    /// A mark (ECN-CE) of what was sent at `sent`: a queue manager on the path judged its queue longer
    /// than it wants it. A loss Copa may take for noise (§2.2), but a mark is never random: it is
    /// congestion, and a sender that marks its datagrams ECN-capable answers it as a classic sender
    /// answers congestion (RFC 3168 §5, RFC 9002 §7.1). A mark of what was sent before the last one was
    /// answered is part of that round trip (RFC 9002 §7.3.2). Otherwise slow start ends (§7.3.1), `1/δ`
    /// halves while Copa competes, a direction up turns down at velocity one, and the window is
    /// multiplied by the backoff, never below the least window, the next round trip's direction judged
    /// from there. Marks round trip after round trip take the window down by the backoff each round
    /// trip until they stop or the window is the least: the response to persistent marking.
    fn on_mark(&mut self, now: u64, sent: u64) {
        if self.mark_recovery.is_some_and(|answered| sent <= answered) {
            return;
        }
        self.mark_recovery = Some(now);
        // A mark in slow start is Copa's own doubling past the manager's target, which ending slow
        // start answers; one after it says the manager's queue stands above its target while Copa aims
        // at its own short queue: other senders fill it.
        if !self.slow_start {
            self.marked_after_slow_start = Some(now);
        }
        self.slow_start = false;
        if self.competitive {
            self.halve_target(now);
        }
        if self.direction == Direction::Up && self.velocity > 1 {
            self.direction = Direction::Down;
            self.velocity = 1;
            self.same_direction = 0;
        }
        self.back_off();
        self.direction_mark = Some((now, self.window));
    }

    /// The window multiplied by the mark backoff, never above what it was nor below the least window.
    fn back_off(&mut self) {
        let backed_off = u128::from(self.window)
            .saturating_mul(u128::from(self.mark_backoff.numerator))
            .checked_div(u128::from(self.mark_backoff.denominator))
            .and_then(|window| u64::try_from(window).ok())
            .unwrap_or(self.window);
        self.window = backed_off.min(self.window).max(self.minimum_window());
    }
}

/// Copa as a path's congestion controller: the law on the connection's clock
#[derive(Debug, Clone)]
pub struct Copa {
    law: Law,
    /// What the law's clock counts from.
    began: Instant,
    /// The smoothed round trip at the last acknowledgement, for a loss.
    srtt: u64,
}

impl Copa {
    /// A controller for one path, its clock counting from `now`
    pub fn new(config: CopaConfig, now: Instant, current_mtu: u16) -> Self {
        Self {
            law: Law::new(u64::from(current_mtu), &config),
            began: now,
            srtt: 0,
        }
    }

    /// Whether the law is in slow start
    pub fn in_slow_start(&self) -> bool {
        self.law.slow_start
    }

    /// Whether the law judges another sender to be filling the queue and competes for it
    pub fn competitive(&self) -> bool {
        self.law.competitive
    }

    /// `1/δ`, in whole packets: about how many packets of queue the law aims to keep
    pub fn inv_delta(&self) -> u64 {
        self.law.inv_delta
    }

    /// How many times its step a packet acknowledged moves the window by
    pub fn velocity(&self) -> u64 {
        self.law.velocity
    }

    fn at(&self, now: Instant) -> u64 {
        nanos(now.saturating_duration_since(self.began))
    }
}

/// A duration in nanoseconds, the most a `u64` holds where it holds more.
fn nanos(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

impl Controller for Copa {
    fn on_ack(
        &mut self,
        now: Instant,
        sent: Instant,
        bytes: u64,
        app_limited: bool,
        rtt: &RttEstimator,
    ) {
        self.srtt = nanos(rtt.get());
        let acked = Acked {
            now: self.at(now),
            rtt: nanos(now.saturating_duration_since(sent)),
            srtt: self.srtt,
            bytes,
            window_limited: !app_limited,
        };
        self.law.on_ack(acked);
    }

    /// The connection raises a mark with no bytes lost and no persistence (`process_ecn`), once for
    /// each acknowledgement whose count of marks grew, naming the latest packet it acknowledges as
    /// `sent`; a loss names the bytes it lost.
    fn on_congestion_event(
        &mut self,
        now: Instant,
        sent: Instant,
        is_persistent_congestion: bool,
        lost_bytes: u64,
    ) {
        let at = self.at(now);
        if lost_bytes == 0 && !is_persistent_congestion {
            self.law.on_mark(at, self.at(sent));
        } else {
            self.law.on_loss(at, self.srtt, is_persistent_congestion);
        }
    }

    fn on_mtu_update(&mut self, new_mtu: u16) {
        self.law.set_datagram(u64::from(new_mtu));
    }

    fn window(&self) -> u64 {
        self.law.window
    }

    fn pacing_rate(&self) -> Option<u64> {
        self.law.pacing_rate()
    }

    fn metrics(&self) -> ControllerMetrics {
        ControllerMetrics {
            congestion_window: self.law.window,
            ssthresh: None,
            // qlog states it in bits a second.
            pacing_rate: self.law.pacing_rate().map(|bytes| bytes.saturating_mul(8)),
        }
    }

    fn clone_box(&self) -> Box<dyn Controller> {
        Box::new(self.clone())
    }

    fn initial_window(&self) -> u64 {
        self.law.initial
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }
}

/// Configuration for the [`Copa`] congestion controller
#[derive(Debug, Clone)]
pub struct CopaConfig {
    inv_delta: u64,
    stride: u64,
    mark_backoff: MarkBackoff,
}

impl CopaConfig {
    /// `1/δ`: about how many packets of queue the sender aims to keep (Copa §2.1)
    ///
    /// Copa §2.2's default mode takes 2, the default; mvfst runs 25 (δ = 0.04) for live video.
    pub fn inv_delta(&mut self, value: u64) -> &mut Self {
        self.inv_delta = value;
        self
    }

    /// What part of the window a round trip moves it by at most, as its inverse
    ///
    /// The paper's velocity bound moves the window by all of itself in a round trip (1). The default,
    /// 2, is focal's measurement: half answered soonest over its paths.
    pub fn stride(&mut self, value: u64) -> &mut Self {
        self.stride = value;
        self
    }

    /// What an explicit congestion mark multiplies the window by, `numerator/denominator`, at most one
    ///
    /// The default, 1/2, is a classic sender's answer to congestion (RFC 3168 §5, RFC 9002 §B.1), and
    /// focal's measurement: the gentler backoffs the RFCs give left NewReno and CUBIC under CoDel less
    /// than nine tenths of what they carry beside their own kind or CUBIC.
    pub fn mark_backoff(&mut self, numerator: u64, denominator: u64) -> &mut Self {
        self.mark_backoff = MarkBackoff {
            numerator,
            denominator,
        };
        self
    }
}

impl Default for CopaConfig {
    fn default() -> Self {
        Self {
            inv_delta: DEFAULT_INV_DELTA,
            stride: DEFAULT_STRIDE,
            mark_backoff: DEFAULT_MARK_BACKOFF,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::congestion::Congestion;

    const DATAGRAM: u64 = 1_000;
    const MS: u64 = 1_000_000;
    /// `1/δ` at the default.
    const DEFAULT: u64 = DEFAULT_INV_DELTA;

    fn new_law() -> Law {
        Law::new(DATAGRAM, &CopaConfig::default())
    }

    fn ack(law: &mut Law, now: u64, rtt: u64, window_limited: bool) {
        law.on_ack(Acked {
            now,
            rtt,
            srtt: 100 * MS,
            bytes: DATAGRAM,
            window_limited,
        });
    }

    /// A queue that never nearly empties from 500 ms on: 80 to 120 ms over a least round trip of
    /// 100 ms, a sample each 10 ms.
    fn compete(law: &mut Law) {
        ack(law, 0, 100 * MS, true);
        for step in 1..=200 {
            let rtt = if step % 2 == 0 { 180 * MS } else { 220 * MS };
            ack(law, 500 * MS + step * 10 * MS, rtt, true);
        }
    }

    #[test]
    fn the_initial_window_is_rfc_9002s() {
        assert_eq!(initial_window(1_200), 12_000);
        assert_eq!(initial_window(1_472), 14_720);
        // Large datagrams: limited to the larger of 14,720 bytes and two datagrams.
        assert_eq!(initial_window(9_000), 18_000);
        assert_eq!(initial_window(1_600), 14_720);
        let copa = Copa::new(CopaConfig::default(), Instant::now(), 1_200);
        assert_eq!(copa.initial_window(), 12_000);
        assert_eq!(copa.window(), 12_000);
    }

    #[test]
    fn copa_paces_at_twice_its_window_over_the_standing_round_trip() {
        let mut law = new_law();
        // No standing round trip yet: the connection's own rule.
        assert_eq!(law.pacing_rate(), None);
        ack(&mut law, 0, 100 * MS, true);
        // 10,000 bytes over a standing 100 ms, doubled: 200,000 bytes a second.
        assert_eq!(law.window, 10 * DATAGRAM);
        assert_eq!(law.pacing_rate(), Some(200_000));
        let copa = Copa::new(CopaConfig::default(), Instant::now(), 1_200);
        assert_eq!(copa.pacing_rate(), None);
        assert_eq!(copa.metrics().pacing_rate, None);
    }

    #[test]
    fn an_empty_queue_doubles_the_window_once_what_was_sent_after_the_last_doubling_is_heard_of() {
        let mut law = new_law();
        let start = law.window;
        assert_eq!(start, 10 * DATAGRAM);
        // Heard of at 0: what is acknowledged until 200 ms was sent within a round trip of it.
        for step in 0..=20 {
            ack(&mut law, step * 10 * MS, 100 * MS, true);
            assert_eq!(law.window, start, "at {step}");
        }
        ack(&mut law, 210 * MS, 100 * MS, true);
        assert_eq!(law.window, 2 * start);
        for step in 22..=41 {
            ack(&mut law, step * 10 * MS, 100 * MS, true);
            assert_eq!(law.window, 2 * start, "at {step}");
        }
        ack(&mut law, 420 * MS, 100 * MS, true);
        assert_eq!(law.window, 4 * start);
        assert!(law.slow_start);
    }

    #[test]
    fn a_queue_past_the_target_shrinks_the_window() {
        let mut law = new_law();
        ack(&mut law, 0, 100 * MS, true);
        let before = law.window;
        // 100 ms of queue at ten packets: the target is 20 packets a second, and ten packets in
        // 200 ms are 50.
        for step in 1..=5 {
            ack(&mut law, 100 * MS + step * MS, 200 * MS, true);
        }
        assert!(!law.slow_start);
        assert!(law.window < before);
        assert_eq!(law.standing_rtt.map(|filter| filter.get()), Some(200 * MS));
    }

    #[test]
    fn a_window_the_sender_does_not_fill_shrinks_and_never_grows() {
        let mut law = new_law();
        ack(&mut law, 0, 100 * MS, false);
        let before = law.window;
        for step in 1..=5 {
            ack(&mut law, 100 * MS + step * MS, 200 * MS, false);
        }
        assert!(law.window < before);
        let shrunk = law.window;
        for step in 1..=50 {
            ack(&mut law, 10_000 * MS + step * MS, 100 * MS, false);
        }
        assert!(law.window <= shrunk);
    }

    #[test]
    fn a_queue_that_never_empties_is_competed_for() {
        let mut law = new_law();
        compete(&mut law);
        assert!(law.competitive);
        let raised = law.inv_delta;
        assert!(raised > DEFAULT, "{raised}");
        law.on_loss(3_000 * MS, 100 * MS, false);
        assert_eq!(law.inv_delta, (raised / 2).max(DEFAULT));
        // Within the same round trip a second loss says nothing more.
        law.on_loss(3_001 * MS, 100 * MS, false);
        assert_eq!(law.inv_delta, (raised / 2).max(DEFAULT));
        // The queue empties: the default again.
        for step in 1..=100 {
            ack(&mut law, 4_000 * MS + step * 10 * MS, 100 * MS, true);
        }
        assert!(!law.competitive);
        assert_eq!(law.inv_delta, DEFAULT);
    }

    #[test]
    fn competing_copa_raises_its_target_a_packet_a_round_trip() {
        // A queue of 50 ms over a least round trip of 100 ms that never empties: competing, `1/δ`
        // rises a packet each round trip without a loss (§2.2).
        let mut law = new_law();
        ack(&mut law, 0, 100 * MS, true);
        let mut raises = 0;
        let mut before = law.inv_delta;
        for step in 1..=300 {
            ack(&mut law, 500 * MS + step * 10 * MS, 150 * MS, true);
            if law.inv_delta != before {
                assert!(law.competitive);
                assert_eq!(law.inv_delta - before, 1);
                raises += 1;
                before = law.inv_delta;
            }
        }
        // A raise at the first sample more than a smoothed round trip (100 ms) after the last, one in
        // eleven samples of 10 ms, from the mode's first judgement at 510 ms to 3,500 ms.
        assert_eq!(raises, 27);
        // Halved on a loss.
        let raised = law.inv_delta;
        law.on_loss(4_000 * MS, 100 * MS, false);
        assert_eq!(law.inv_delta, (raised / 2).max(DEFAULT));
    }

    #[test]
    fn the_mode_is_judged_over_four_round_trips() {
        // The queue nearly empty at 0, standing since: within four smoothed round trips of it the
        // default holds; past them Copa competes.
        let mut law = new_law();
        ack(&mut law, 0, 100 * MS, true);
        for step in 1..=40 {
            ack(&mut law, step * 10 * MS, 150 * MS, true);
            assert!(!law.competitive, "{step}");
        }
        ack(&mut law, 410 * MS, 150 * MS, true);
        assert!(law.competitive);
    }

    #[test]
    fn a_loss_is_no_signal_unless_it_persists() {
        let mut law = new_law();
        for step in 0..=50 {
            ack(&mut law, step * 10 * MS, 100 * MS, true);
        }
        let window = law.window;
        law.on_loss(600 * MS, 100 * MS, false);
        assert_eq!(law.window, window);
        law.on_loss(900 * MS, 100 * MS, true);
        assert_eq!(law.window, 2 * DATAGRAM);
        assert!(!law.slow_start);
    }

    #[test]
    fn the_window_is_never_below_two_datagrams_and_follows_their_size() {
        let mut law = new_law();
        ack(&mut law, 0, 100 * MS, true);
        // 4.9 s of queue: the target is two packets and a little.
        for step in 1..=90 {
            ack(&mut law, 100 * MS + step * MS, 5_000 * MS, true);
            assert!(law.window >= 2 * DATAGRAM);
        }
        assert!(law.window < 3 * DATAGRAM, "{}", law.window);
        law.on_loss(200 * MS, 100 * MS, true);
        assert_eq!(law.window, 2 * DATAGRAM);
        law.set_datagram(1_400);
        assert_eq!(law.window, 2_800);
        law.set_datagram(0);
        assert_eq!(law.window, 2_800);
    }

    #[test]
    fn nothing_overflows_at_the_bounds_of_what_it_is_told() {
        for (datagram, inv_delta) in [(0, 0), (u64::MAX, u64::MAX), (1, u64::MAX), (u64::MAX, 1)] {
            let mut config = CopaConfig::default();
            config.inv_delta(inv_delta).stride(0).mark_backoff(0, 0);
            let mut law = Law::new(datagram, &config);
            for (now, rtt, srtt, bytes) in [
                (0, 0, 0, 0),
                (u64::MAX, u64::MAX, u64::MAX, u64::MAX),
                (1, u64::MAX, 1, u64::MAX),
                (u64::MAX, 1, u64::MAX, 1),
                (u64::MAX, 0, 1, u64::MAX),
            ] {
                for window_limited in [true, false] {
                    law.on_ack(Acked {
                        now,
                        rtt,
                        srtt,
                        bytes,
                        window_limited,
                    });
                    law.on_loss(now, srtt, window_limited);
                    law.on_mark(now, rtt);
                    assert!(law.window >= 2);
                }
            }
        }
    }

    #[test]
    fn the_filters_forget_what_has_aged_out_of_their_window() {
        let mut most = WindowedMax::new(0, 10);
        assert_eq!(most.update(1, 100, 5), 10);
        assert_eq!(most.update(50, 100, 7), 10);
        assert_eq!(most.update(90, 100, 6), 10);
        // The best is a hundred old and more: the next best stands.
        assert_eq!(most.update(101, 100, 1), 7);
        assert_eq!(most.update(151, 100, 1), 6);
        assert_eq!(most.update(400, 100, 2), 2);
        assert_eq!(most.update(401, 100, 9), 9);
        let mut least = WindowedMin::new(0, 10);
        assert_eq!(least.update(1, 100, 50), 10);
        assert_eq!(least.update(60, 100, 20), 10);
        assert_eq!(least.update(101, 100, 90), 20);
        assert_eq!(least.update(102, 100, 3), 3);
        assert_eq!(least.get(), 3);
        // A window of nothing keeps the last sample alone.
        let mut none = WindowedMax::new(0, 10);
        assert_eq!(none.update(1, 0, 4), 4);
    }

    #[test]
    fn the_controller_counts_from_when_it_was_built() {
        let began = Instant::now();
        let mut controller = Congestion::Copa(CopaConfig::default()).build(began, 1_200);
        assert_eq!(controller.window(), 12_000);
        assert_eq!(controller.initial_window(), 12_000);
        controller.on_mtu_update(1_400);
        assert_eq!(controller.window(), 12_000);
        // A clock behind the beginning counts as the beginning.
        controller.on_congestion_event(began, began, true, 1_200);
        assert_eq!(controller.window(), 2_800);
        assert_eq!(controller.metrics().congestion_window, 2_800);
        let copy = controller.clone_box();
        assert_eq!(copy.window(), 2_800);
        let copa = controller.into_any().downcast::<Copa>().unwrap();
        assert!(!copa.in_slow_start());
        assert!(!copa.competitive());
        assert_eq!(copa.inv_delta(), DEFAULT_INV_DELTA);
        assert_eq!(copa.velocity(), 1);
    }

    #[test]
    fn a_mark_ends_slow_start_and_steps_the_window_down_once_a_round_trip() {
        let mut law = new_law();
        for step in 0..=25 {
            ack(&mut law, step * 10 * MS, 100 * MS, true);
        }
        assert!(law.slow_start);
        let before = law.window;
        // The backoff of a mark: half the window.
        law.on_mark(300 * MS, 250 * MS);
        assert!(!law.slow_start);
        assert_eq!(law.window, before / 2);
        // A mark of what was sent before that answer is the same round trip.
        law.on_mark(320 * MS, 300 * MS);
        assert_eq!(law.window, before / 2);
        // One of what was sent after it is the next.
        law.on_mark(420 * MS, 310 * MS);
        assert_eq!(law.window, before / 4);
        // Never below the least window.
        for round in 0..100 {
            law.on_mark(500 * MS + round * 100 * MS, 450 * MS + round * 100 * MS);
            assert!(law.window >= 2 * DATAGRAM);
        }
        assert_eq!(law.window, 2 * DATAGRAM);
    }

    #[test]
    fn a_mark_while_competing_halves_the_target_and_the_window_as_a_classic_sender_does() {
        let mut law = new_law();
        compete(&mut law);
        assert!(law.competitive);
        let raised = law.inv_delta;
        assert!(raised > DEFAULT, "{raised}");
        let window = law.window;
        law.on_mark(3_000 * MS, 2_900 * MS);
        assert_eq!(law.inv_delta, (raised / 2).max(DEFAULT));
        // Copa halves its window as a classic sender does for a mark (RFC 9002 §B.1).
        assert_eq!(law.window, (window / 2).max(2 * DATAGRAM));
        // The same round trip says nothing more.
        law.on_mark(3_010 * MS, 2_950 * MS);
        assert_eq!(law.inv_delta, (raised / 2).max(DEFAULT));
        assert_eq!(law.window, (window / 2).max(2 * DATAGRAM));
    }

    #[test]
    fn marks_held_round_trip_after_round_trip_take_the_window_down_by_the_backoff_to_the_least() {
        // A law whose queue empties (the default mode), marked once each round trip: the declared
        // response to persistent marking halves the window each round trip, to the least window.
        let mut law = new_law();
        for step in 0..=40 {
            ack(&mut law, step * 10 * MS, 100 * MS, true);
        }
        assert!(!law.competitive);
        let mut window = law.window;
        for round in 0..40_u64 {
            let begins = 1_000 * MS + round * 100 * MS;
            law.on_mark(begins + 99 * MS, begins);
            window = (window / 2).max(2 * DATAGRAM);
            assert_eq!(law.window, window, "round {round}");
        }
        assert_eq!(law.window, 2 * DATAGRAM);
        assert!(!law.slow_start && !law.competitive);
    }

    /// What a round trip of acknowledgements, the window's bytes from `from`, grows the window by, and
    /// the window it began from.
    fn round_trip(law: &mut Law, from: u64) -> (u64, u64) {
        let before = law.window;
        let whole = before / DATAGRAM;
        for step in 0..whole {
            ack(law, from + step * MS, 100 * MS, true);
        }
        law.on_ack(Acked {
            now: from + whole * MS,
            rtt: 100 * MS,
            srtt: 100 * MS,
            bytes: before % DATAGRAM,
            window_limited: true,
        });
        (law.window - before, before)
    }

    /// Whether a round trip that began at `before` and grew by `grown` grew by `packets` datagrams a
    /// round trip: a window's bytes acknowledged, each step `packets·datagram²/cwnd` with the window
    /// between `before` and `before + packets·datagram` as it grows, the remainder carried, so the
    /// growth lies within a byte of `[packets·datagram·before/(before + packets·datagram),
    /// packets·datagram]`.
    fn grew_by(grown: u64, before: u64, packets: u64) -> bool {
        let most = packets * DATAGRAM;
        let least = most * before / (before + most);
        (least.saturating_sub(1)..=most + 1).contains(&grown)
    }

    #[test]
    fn after_a_mark_past_slow_start_the_window_grows_a_datagram_a_round_trip_for_ten_seconds() {
        let mut law = new_law();
        for step in 0..=80 {
            ack(&mut law, step * 10 * MS, 100 * MS, true);
        }
        assert_eq!(law.window, 80 * DATAGRAM);
        // A mark in slow start is Copa's own doubling past the manager's target: slow start ends and
        // Copa grows by its own step after it, `v/δ` datagrams a round trip, two at velocity one.
        law.on_mark(900 * MS, 850 * MS);
        assert!(!law.slow_start);
        let (grown, before) = round_trip(&mut law, 1_000 * MS);
        assert!(
            grew_by(grown, before, 2),
            "Copa's own step: {grown} from {before}"
        );
        // One after slow start: for ten seconds a datagram a round trip.
        law.on_mark(1_100 * MS, 1_050 * MS);
        let (grown, before) = round_trip(&mut law, 1_200 * MS);
        assert!(
            grew_by(grown, before, 1),
            "a datagram: {grown} from {before}"
        );
        let (grown, before) = round_trip(&mut law, 11_000 * MS);
        assert!(
            grew_by(grown, before, 1),
            "a datagram: {grown} from {before}"
        );
        // Past ten seconds from it, Copa's own step again.
        let (grown, before) = round_trip(&mut law, 11_200 * MS);
        assert!(
            grew_by(grown, before, 2),
            "Copa's own step: {grown} from {before}"
        );
    }

    #[test]
    fn competing_as_a_classic_sender_copa_raises_its_target_only_after_a_round_trip_held_back() {
        let mut law = new_law();
        ack(&mut law, 0, 100 * MS, true);
        // A mark in slow start ends it, and begins no classic growth.
        law.on_mark(10 * MS, 5 * MS);
        // A queue a millisecond or three over the least that never nearly empties: Copa competes.
        let low = |step: u64| {
            if step.is_multiple_of(2) {
                101 * MS
            } else {
                103 * MS
            }
        };
        for step in 2..=150 {
            ack(&mut law, step * 10 * MS, low(step), true);
        }
        assert!(law.competitive);
        // Set where a longer competition would have raised it, a packet a round trip.
        let raised = 9;
        law.inv_delta = raised;
        // A mark past slow start: `1/δ` halves, and for ten seconds the window grows as a classic
        // sender's.
        law.on_mark(1_510 * MS, 1_505 * MS);
        let halved = law.inv_delta;
        assert_eq!(halved, (raised / 2).max(DEFAULT));
        // Below its target the window grows round trip after round trip and the target stays: raised
        // beside a window that grows a datagram a round trip, it would never be reached, and the queue
        // never empty. A queue nine to ten milliseconds over the least for the mode's four round trips
        // keeps the mode competing as the samples rise: risen to 80 ms over, "nearly empty" is within
        // 8 ms of the least, and a sample of the low queue still in the window would end the mode.
        for step in 152..=300 {
            let rtt = match step {
                ..=250 => low(step),
                _ if step.is_multiple_of(2) => 109 * MS,
                _ => 110 * MS,
            };
            ack(&mut law, step * 10 * MS, rtt, true);
            assert!(law.competitive, "{step}");
            assert_eq!(law.inv_delta, halved, "{step}");
        }
        // The queue rises past what the target allows: a round trip goes down, and the round trip after
        // it `1/δ` rises, by a packet.
        let before = law.window;
        for step in 301..=360_u64 {
            let rtt = if step.is_multiple_of(2) {
                170 * MS
            } else {
                180 * MS
            };
            ack(&mut law, step * 10 * MS, rtt, true);
            assert!(law.competitive, "{step}");
            if law.inv_delta > halved {
                assert!(law.window < before, "{} {before}", law.window);
                assert_eq!(law.inv_delta - halved, 1);
                return;
            }
        }
        panic!("held back, and `1/δ` stayed {}", law.inv_delta);
    }

    #[test]
    fn the_controller_takes_an_event_without_lost_bytes_for_a_mark() {
        let began = Instant::now();
        let mut controller = Congestion::Copa(CopaConfig::default()).build(began, 1_200);
        let window = controller.window();
        let later = began + Duration::from_millis(100);
        controller.on_congestion_event(later, began, false, 0);
        assert_eq!(controller.window(), window / 2);
        // The same round trip again: nothing more.
        controller.on_congestion_event(later + Duration::from_millis(1), began, false, 0);
        assert_eq!(controller.window(), window / 2);
        // A loss in the default mode is still no signal.
        controller.on_congestion_event(later + Duration::from_millis(200), later, false, 1_200);
        assert_eq!(controller.window(), window / 2);
        let copa = controller.into_any().downcast::<Copa>().unwrap();
        assert!(!copa.in_slow_start());
    }
}
