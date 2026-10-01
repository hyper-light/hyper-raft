//! Waits charged to progress (mantle note 32 T39; focal `transport.rs` `carried` and `frame.rs`
//! `read_payload_arriving`, made sans-io).
//!
//! An exchange is not given a fixed wall time. It is judged once a period, and given up only when
//! a period moved less than a datagram ([`LEAST_PROGRESS`]), so "a megabyte crosses a path of
//! 4 Mbit/s ... between endpoints that give a request one second" (focal 27 §7), and an exchange
//! whose peer stopped taking it ends within a period or two on any path. Two phases:
//!
//! - **Asking**: the request is being carried and the reply's prefix has not arrived. A period is
//!   charged what the connection sent and did not lose in it. The exchange ends at the end of a
//!   period that sent less than a datagram, or in which the peer sent nothing at all, and at the
//!   end of one that began when all the
//!   exchanges on the connection had to send beside it, and its own bytes, had been sent: the peer
//!   had the request and a period to answer it. What was sent is progress only while the peer is
//!   heard: a live peer acknowledges what it receives within its `max_ack_delay` (RFC 9000 §13.2.1,
//!   25 ms by default, §18.2), so a period that heard nothing is silence, and what the sender put
//!   into it (the flight in the air, then the probe timeout's probes, which RFC 9002 §6.2.4 sends
//!   whether or not the peer lives) moved nothing. A period is to be longer than the peer's
//!   acknowledgement delay; focal's `carried` counted bytes sent alone.
//! - **Answering**: the reply's prefix arrived and its body is arriving. A period must bring a
//!   datagram's worth of the connection's received bytes or the body's end, and the body is given no
//!   longer than its residency: what its bytes take at the least a live sender delivers, two
//!   datagrams a round trip, the smallest congestion window QUIC keeps (RFC 9002 §7.2), measured
//!   against the longest round trip seen while it arrives; one period at least.

use std::time::{Duration, Instant};

use hyper_timing::RoundBudget;

use crate::Refusal;
use crate::credit::MIN_DATAGRAM;

/// The least a period must move: one datagram of the smallest size a QUIC path carries
/// (RFC 9000 §14; focal `frame.rs`'s `LEAST_PROGRESS`).
pub const LEAST_PROGRESS: u64 = MIN_DATAGRAM;
/// The smallest congestion window QUIC keeps is two datagrams (RFC 9002 §7.2,
/// `kMinimumWindow = 2 * max_datagram_size`): what a live sender delivers a round trip at least.
const MINIMUM_WINDOW_DATAGRAMS: u64 = 2;

/// A progress-charged deadline: the period at which an exchange is judged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    period: Duration,
}

impl Progress {
    /// A deadline judged every `period`, which is what the peer is given to answer once it has
    /// the request.
    pub fn new(period: Duration) -> Result<Self, Refusal> {
        if period.is_zero() {
            return Err(Refusal::Configuration);
        }
        Ok(Self { period })
    }
    /// The period a round's budget opens with (hyper-timing's law for focal and slates): the tail
    /// of the exchanges with these peers, or the ceiling while none is measured.
    pub fn from_budget(budget: &RoundBudget) -> Self {
        let period = Duration::from_nanos(budget.deadline_ns.max(1));
        Self { period }
    }
    /// The period.
    pub fn period(&self) -> Duration {
        self.period
    }
}

/// What the connection has moved so far, as the judgement reads it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Moved {
    /// Bytes the connection sent and did not find lost.
    pub(crate) sent: u64,
    /// Bytes the connection received.
    pub(crate) received: u64,
}

#[derive(Clone, Copy, Debug)]
enum Phase {
    Asking {
        /// What the exchanges on the connection, this one included, had to send.
        owed: u64,
        /// What the periods so far were charged.
        charged: u64,
        /// Whether the period now running began with every owed byte charged.
        had: bool,
    },
    Answering {
        began: Instant,
        /// The body's whole length: what its residency is priced by.
        total: u64,
        /// The body's length still to arrive.
        remaining: u64,
        longest: Duration,
    },
    /// Nothing is being waited on: a served exchange whose request has arrived and whose reply
    /// has not begun is the owner's to answer.
    Idle,
}

/// One exchange's wait.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Carry {
    period: Duration,
    next: Instant,
    before: Moved,
    phase: Phase,
}

impl Carry {
    /// A wait judged every period of `progress` from `now`, with nothing to wait on yet.
    pub(crate) fn idle(progress: Progress, now: Instant, moved: Moved) -> Self {
        Self {
            period: progress.period,
            next: now.checked_add(progress.period).unwrap_or(now),
            before: moved,
            phase: Phase::Idle,
        }
    }
    /// The exchange is asking: `owed` bytes are to be sent on the connection, its own included.
    pub(crate) fn asking(&mut self, now: Instant, moved: Moved, owed: u64) {
        self.restart(now, moved);
        self.phase = Phase::Asking {
            owed,
            charged: 0,
            had: false,
        };
    }
    /// The exchange is answered: `remaining` bytes of body are to arrive.
    pub(crate) fn answering(&mut self, now: Instant, moved: Moved, remaining: u64, rtt: Duration) {
        self.restart(now, moved);
        self.phase = Phase::Answering {
            began: now,
            total: remaining,
            remaining,
            longest: rtt,
        };
    }
    /// Nothing is waited on until the exchange asks or is answered again.
    pub(crate) fn rest(&mut self) {
        self.phase = Phase::Idle;
    }
    /// `bytes` of the body arrived and were read.
    pub(crate) fn arrived(&mut self, bytes: u64) {
        if let Phase::Answering { remaining, .. } = &mut self.phase {
            *remaining = remaining.saturating_sub(bytes);
        }
    }
    /// The owner is not reading the body it is answered with: a period in which it did not ask
    /// for more is no evidence against the sender, so the wait starts afresh from `now`, the rest of
    /// the body priced anew.
    pub(crate) fn hold(&mut self, now: Instant, moved: Moved) {
        self.restart(now, moved);
        if let Phase::Answering {
            began,
            total,
            remaining,
            ..
        } = &mut self.phase
        {
            *began = now;
            *total = *remaining;
        }
    }
    fn restart(&mut self, now: Instant, moved: Moved) {
        self.before = moved;
        self.next = now.checked_add(self.period).unwrap_or(now);
    }
    /// When the wait is next judged, if anything is waited on.
    pub(crate) fn due(&self) -> Option<Instant> {
        (!matches!(self.phase, Phase::Idle)).then_some(self.next)
    }
    /// Judge the wait at `now`, if a judgement is due: `moved` is what the connection has moved,
    /// `held` what its exchanges have to send now, `rtt` its round trip now.
    pub(crate) fn judge(
        &mut self,
        now: Instant,
        moved: Moved,
        held: u64,
        rtt: Duration,
    ) -> Result<(), Refusal> {
        if now < self.next || matches!(self.phase, Phase::Idle) {
            return Ok(());
        }
        let before = std::mem::replace(&mut self.before, moved);
        self.next = now.checked_add(self.period).unwrap_or(now);
        match &mut self.phase {
            Phase::Asking { owed, charged, had } => {
                let sent = moved.sent.saturating_sub(before.sent);
                let heard = moved.received.saturating_sub(before.received);
                if *had || sent < LEAST_PROGRESS || heard == 0 {
                    return Err(Refusal::Stalled);
                }
                *charged = charged.saturating_add(sent);
                *owed = (*owed).max(held);
                *had = *charged >= *owed;
                Ok(())
            }
            Phase::Answering {
                began,
                total,
                remaining,
                longest,
            } => {
                *longest = (*longest).max(rtt);
                let received = moved.received.saturating_sub(before.received);
                let residency = residency(*total, *longest).max(self.period);
                let spent = now.saturating_duration_since(*began) > residency;
                if received < LEAST_PROGRESS.min(*remaining) || spent {
                    return Err(Refusal::Stalled);
                }
                Ok(())
            }
            Phase::Idle => Ok(()),
        }
    }
}

/// How long `bytes` may take to arrive over a path whose round trip is `rtt`, at the least a live
/// sender delivers: two datagrams of the least size a round trip (focal `frame.rs` `residency`).
pub(crate) fn residency(bytes: u64, rtt: Duration) -> Duration {
    let per_round_trip = LEAST_PROGRESS.saturating_mul(MINIMUM_WINDOW_DATAGRAMS);
    let round_trips = bytes.div_ceil(per_round_trip);
    rtt.saturating_mul(u32::try_from(round_trips).unwrap_or(u32::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PERIOD: Duration = Duration::from_millis(100);
    fn moved(sent: u64, received: u64) -> Moved {
        Moved { sent, received }
    }

    /// focal's `carried`: a megabyte over a path that carries a megabit a second is given its eight
    /// seconds and more; it is never cut off at a fixed time while it moves.
    #[test]
    fn a_slow_transfer_that_moves_is_never_cut_off() {
        let start = Instant::now();
        let mut carry = Carry::idle(Progress::new(PERIOD).unwrap(), start, moved(0, 0));
        carry.asking(start, moved(0, 0), 1_000_000);
        // 12,500 bytes a period: a megabit a second.
        for period in 1..=80u64 {
            let now = start + PERIOD * u32::try_from(period).unwrap();
            let sent = 12_500 * period;
            carry
                .judge(
                    now,
                    moved(sent, period * 100),
                    1_000_000 - sent.min(1_000_000),
                    PERIOD,
                )
                .unwrap();
        }
        // Everything was sent by the 80th period: the peer had the request and the 81st to answer.
        let now = start + PERIOD * 81;
        assert_eq!(
            carry.judge(now, moved(1_000_000 + 1_200, 8_200), 0, PERIOD),
            Err(Refusal::Stalled)
        );
    }

    /// A peer that died leaves the sender sending into silence: the flight in the air, then the
    /// probe timeout's probes, which RFC 9002 §6.2.4 sends whether or not the peer lives, a
    /// datagram or two each, at a backoff that doubles. Counted as progress, they kept an exchange
    /// to a killed peer alive for as many periods as held a probe: two periods on one run of the
    /// end-to-end scenario, four on another.
    #[test]
    fn what_is_sent_into_silence_is_not_progress() {
        let start = Instant::now();
        let mut carry = Carry::idle(Progress::new(PERIOD).unwrap(), start, moved(0, 0));
        carry.asking(start, moved(0, 0), 1_000_000);
        assert_eq!(
            carry.judge(start + PERIOD, moved(13_776, 0), 1_000_000, PERIOD),
            Err(Refusal::Stalled)
        );
    }

    #[test]
    fn a_period_that_moves_less_than_a_datagram_ends_the_exchange() {
        let start = Instant::now();
        let mut carry = Carry::idle(Progress::new(PERIOD).unwrap(), start, moved(0, 0));
        carry.asking(start, moved(500, 0), 10_000);
        // Not yet due: no judgement.
        carry
            .judge(start + PERIOD / 2, moved(500, 0), 10_000, PERIOD)
            .unwrap();
        assert_eq!(
            carry.judge(start + PERIOD, moved(500 + 1_199, 300), 10_000, PERIOD),
            Err(Refusal::Stalled)
        );
    }

    #[test]
    fn an_answer_is_given_its_residency_and_must_keep_arriving() {
        let start = Instant::now();
        let rtt = Duration::from_millis(40);
        let mut carry = Carry::idle(Progress::new(PERIOD).unwrap(), start, moved(0, 0));
        carry.answering(start, moved(0, 0), 24_000, rtt);
        // 24,000 bytes are ten round trips of two datagrams: 400 ms.
        assert_eq!(residency(24_000, rtt), Duration::from_millis(400));
        for period in 1..=4u32 {
            carry
                .judge(
                    start + PERIOD * period,
                    moved(0, 2_400 * u64::from(period)),
                    0,
                    rtt,
                )
                .unwrap();
            carry.arrived(2_400);
        }
        // Past its residency.
        assert_eq!(
            carry.judge(start + PERIOD * 5, moved(0, 12_000), 0, rtt),
            Err(Refusal::Stalled)
        );
        // A body that stops arriving ends at the period that brought less than a datagram.
        let mut carry = Carry::idle(Progress::new(PERIOD).unwrap(), start, moved(0, 0));
        carry.answering(start, moved(0, 0), 24_000, rtt);
        assert_eq!(
            carry.judge(start + PERIOD, moved(0, 100), 0, rtt),
            Err(Refusal::Stalled)
        );
    }

    #[test]
    fn an_idle_wait_is_never_due() {
        let start = Instant::now();
        let carry = Carry::idle(Progress::new(PERIOD).unwrap(), start, moved(0, 0));
        assert_eq!(carry.due(), None);
        assert_eq!(Progress::new(Duration::ZERO), Err(Refusal::Configuration));
    }
}
