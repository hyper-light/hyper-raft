//! Careful Resume (RFC 9959): a connection starts from half the capacity an earlier connection to
//! the same remote measured, once its own first round trip confirms the path, and retreats on the
//! first congestion (`docs/research/quic-overhead.md` §1)
//!
//! A connection observes what it delivers each round trip (§3.1). At its close the endpoint keeps
//! the observation for the remote's IP address in a [`CongestionMemory`], and hands it to the next
//! connection to that address, one connection at a time. That connection runs the phases of §3.2
//! to §3.5 over its congestion controller's window.

use std::collections::VecDeque;
use std::net::IpAddr;

use super::RttEstimator;
use crate::congestion::Controller;
use crate::{Duration, Instant};

/// What a connection measured of its path, for a later connection to the same remote (§3.1)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Saved {
    /// The bytes acknowledged in one round trip, the most the connection saw: `saved_cwnd`
    pub(crate) cwnd: u64,
    /// The connection's minimum RTT: `saved_rtt`
    pub(crate) rtt: Duration,
    /// When the measurement was taken, from which its lifetime runs (§2.4)
    pub(crate) at: Instant,
}

/// The measurements an endpoint holds, one per remote IP address (§3.1: "A sender MUST NOT retain
/// more than one set of CC parameters for a Remote Endpoint"), at most `capacity`, the oldest
/// replaced first
pub(crate) struct CongestionMemory {
    entries: VecDeque<(IpAddr, Saved)>,
    capacity: usize,
}

impl CongestionMemory {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            capacity,
        }
    }

    /// Takes the measurement for `remote`, if one younger than `lifetime` is held: a taken
    /// measurement is out of the memory while one connection uses it, so no second connection
    /// starts from it at once (§3.2)
    pub(crate) fn take(
        &mut self,
        remote: IpAddr,
        now: Instant,
        lifetime: Duration,
    ) -> Option<Saved> {
        let at = self.entries.iter().position(|(ip, _)| *ip == remote)?;
        let (_, saved) = self.entries.remove(at)?;
        (now.saturating_duration_since(saved.at) <= lifetime).then_some(saved)
    }

    /// Keeps `saved` for `remote`, in place of any measurement held for it
    pub(crate) fn put(&mut self, remote: IpAddr, saved: Saved) {
        if self.capacity == 0 {
            return;
        }
        if let Some(at) = self.entries.iter().position(|(ip, _)| *ip == remote) {
            self.entries.remove(at);
        }
        while self.entries.len() >= self.capacity {
            self.entries.pop_front();
        }
        self.entries.push_back((remote, saved));
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
}

/// The bytes acknowledged in each round trip of a connection, and the most of them (§3.1: "This
/// could be computed by measuring the volume of data acknowledged in one RTT")
#[derive(Debug, Default)]
struct Observer {
    /// When the current round trip's count began
    start: Option<Instant>,
    acked: u64,
    /// The most acknowledged in one round trip, and when that round trip ended
    best: Option<(u64, Instant)>,
}

impl Observer {
    fn on_ack(&mut self, now: Instant, bytes: u64, rtt: Duration) {
        let start = *self.start.get_or_insert(now);
        self.acked = self.acked.saturating_add(bytes);
        if now.saturating_duration_since(start) >= rtt {
            if self.best.is_none_or(|(best, _)| self.acked > best) {
                self.best = Some((self.acked, now));
            }
            self.start = Some(now);
            self.acked = 0;
        }
    }
}

/// A phase of RFC 9959 §3
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Normal congestion control: nothing saved to resume from, or done with it
    Off,
    /// §3.2: the initial window under normal congestion control, until the data sent in the first
    /// round trip is acknowledged without congestion. `initial_end` is set by the first
    /// acknowledgement in any space to the largest ack-eliciting Data packet sent before it, or
    /// none; `ready` that every such packet is acknowledged.
    Reconnaissance {
        initial_end: Option<Option<u64>>,
        ready: bool,
    },
    /// §3.3: the window at the jump. `first` is the first Data packet sent in the phase, `last`
    /// the last, `pipe` the PipeSize.
    Unvalidated {
        first: u64,
        last: Option<u64>,
        pipe: u64,
        entered: Instant,
    },
    /// §3.4: normal congestion control until `last`, the last packet sent while Unvalidated, is
    /// acknowledged
    Validating { last: u64, pipe: u64 },
    /// §3.5: half the PipeSize, not increased, until `last` is acknowledged
    Retreat { last: u64, pipe: u64 },
}

/// One connection's Careful Resume
#[derive(Debug)]
pub(crate) struct CarefulResume {
    phase: Phase,
    /// The measurement this connection was given, returned at its close unless it retreated
    given: Option<Saved>,
    /// The configured maximum jump (§2.4's `max_jump`)
    max_jump: u64,
    observer: Observer,
}

/// RFC 9959 §3.1: a measurement below four initial windows "would not justify" Careful Resume, and
/// the sender "can choose to not save" it; hyper-quic does not
const SAVE_FLOOR_WINDOWS: u64 = 4;

/// RFC 9959 §3.2 and §4.2.1: a minimum RTT at or below half the saved RTT stops Careful Resume, and
/// one past ten times it "is indicative of a path change"
const RTT_TOO_SMALL_DIVISOR: u32 = 2;
/// See [`RTT_TOO_SMALL_DIVISOR`]
const RTT_PATH_CHANGE_FACTOR: u32 = 10;

/// RFC 9959 §3.3: "jump_cwnd MUST be no more than half of the saved_cwnd"
const JUMP_DIVISOR: u64 = 2;

/// RFC 9959 §3.5: on leaving Safe Retreat, "ssthresh MUST be set to no larger than the most
/// recently measured PipeSize * Beta", Beta 0.5 by default; and on entering it the window is "no
/// more than (PipeSize/2)"
const RETREAT_DIVISOR: u64 = 2;

impl CarefulResume {
    /// A connection given `saved` from an earlier one, or none
    pub(crate) fn new(saved: Option<Saved>, max_jump: u64) -> Self {
        Self {
            phase: match saved {
                Some(_) => Phase::Reconnaissance {
                    initial_end: None,
                    ready: false,
                },
                None => Phase::Off,
            },
            given: saved,
            max_jump,
            observer: Observer::default(),
        }
    }

    /// Whether the controller's window is held as it is, not grown by acknowledgements: in the
    /// Unvalidated Phase (the window is the jump's) and in Safe Retreat (§3.5: "The CWND MUST NOT
    /// be increased")
    pub(crate) fn holds_window(&self) -> bool {
        matches!(
            self.phase,
            Phase::Unvalidated { .. } | Phase::Retreat { .. }
        )
    }

    /// The pacing rate in the Unvalidated Phase, bytes a second: one window a smoothed round trip
    /// (§3.3: "All packets sent in the Unvalidated Phase MUST use pacing based on the current
    /// RTT"; §4.3's ITT = RTT × MPS / jump_cwnd)
    pub(crate) fn pacing_rate(&self, window: u64, rtt: Duration) -> Option<u64> {
        if !matches!(self.phase, Phase::Unvalidated { .. }) {
            return None;
        }
        let nanos = rtt.as_nanos().max(1);
        let rate = u128::from(window)
            .saturating_mul(1_000_000_000)
            .checked_div(nanos)?;
        Some(u64::try_from(rate).unwrap_or(u64::MAX))
    }

    /// An acknowledgement was processed: `bytes` newly acknowledged ack-eliciting bytes, the Data
    /// space's largest acknowledged packet and largest ack-eliciting packet sent, the bytes in
    /// flight after it
    pub(crate) fn on_ack(
        &mut self,
        now: Instant,
        bytes: u64,
        largest_data_acked: Option<u64>,
        data_sent: Option<u64>,
        in_flight: u64,
        rtt: &RttEstimator,
        controller: &mut dyn Controller,
    ) {
        self.observer.on_ack(now, bytes, rtt.get());
        let acked_past = |pn: u64| largest_data_acked.is_some_and(|largest| largest >= pn);
        match &mut self.phase {
            Phase::Off => {}
            Phase::Reconnaissance { initial_end, ready } => {
                *ready |= match *initial_end.get_or_insert(data_sent) {
                    Some(end) => acked_past(end),
                    None => true,
                };
            }
            Phase::Unvalidated {
                first,
                last,
                pipe,
                entered,
            } => {
                *pipe = pipe.saturating_add(bytes);
                let (first, last, pipe) = (*first, *last, *pipe);
                if acked_past(first) || now.saturating_duration_since(*entered) > rtt.get() {
                    self.leave_unvalidated(last, pipe, in_flight, controller);
                }
            }
            Phase::Validating { last, pipe } => {
                *pipe = pipe.saturating_add(bytes);
                if acked_past(*last) {
                    self.phase = Phase::Off;
                }
            }
            Phase::Retreat { last, pipe } => {
                *pipe = pipe.saturating_add(bytes);
                if acked_past(*last) {
                    controller.set_ssthresh(*pipe / RETREAT_DIVISOR);
                    self.phase = Phase::Off;
                }
            }
        }
    }

    /// The congestion controller reacted to congestion (loss or ECN-CE): Reconnaissance stops using
    /// Careful Resume (§3.2), the Unvalidated and Validating Phases retreat (§3.3, §3.4)
    pub(crate) fn on_congestion(&mut self, controller: &mut dyn Controller) {
        match self.phase {
            Phase::Off | Phase::Retreat { .. } => {}
            Phase::Reconnaissance { .. } => self.phase = Phase::Off,
            Phase::Unvalidated {
                first, last, pipe, ..
            } => {
                // Nothing sent since the jump: nothing of it to wait for
                self.retreat(last.unwrap_or(first), pipe, controller);
            }
            Phase::Validating { last, pipe } => self.retreat(last, pipe, controller),
        }
    }

    /// §3.5: the saved measurement is deleted and the window halved from the PipeSize
    fn retreat(&mut self, last: u64, pipe: u64, controller: &mut dyn Controller) {
        self.given = None;
        let window = controller.window().min(pipe / RETREAT_DIVISOR);
        controller.set_window(window);
        self.phase = Phase::Retreat { last, pipe };
    }

    /// The window would hold back the next datagram. In Reconnaissance with the initial data
    /// acknowledged, this is the deferred jump (§3.2: "this transition MAY be deferred to the time
    /// at which more data is sent than would have been normally permitted by the CC algorithm"):
    /// returns whether the window grew
    pub(crate) fn on_window_blocked(
        &mut self,
        now: Instant,
        next_data_pn: u64,
        in_flight: u64,
        rtt: &RttEstimator,
        controller: &mut dyn Controller,
    ) -> bool {
        let (Phase::Reconnaissance { ready: true, .. }, Some(saved)) = (self.phase, self.given)
        else {
            return false;
        };
        // §3.2 and §4.2.1: the path must still be the saved one
        let too_small = saved
            .rtt
            .checked_div(RTT_TOO_SMALL_DIVISOR)
            .is_some_and(|half| rtt.min() <= half);
        let path_changed = rtt.get() > saved.rtt.saturating_mul(RTT_PATH_CHANGE_FACTOR);
        let jump = (saved.cwnd / JUMP_DIVISOR).min(self.max_jump);
        if too_small || path_changed || jump <= controller.window() || !controller.set_window(jump)
        {
            self.phase = Phase::Off;
            return false;
        }
        self.phase = Phase::Unvalidated {
            first: next_data_pn,
            last: None,
            pipe: in_flight,
            entered: now,
        };
        true
    }

    /// A datagram whose last packet is Data packet `pn` was sent, leaving `in_flight` bytes in
    /// flight: §3.3's first exit, "when the flight_size equals the CWND"
    pub(crate) fn on_data_sent(
        &mut self,
        pn: u64,
        in_flight: u64,
        controller: &mut dyn Controller,
    ) {
        if let Phase::Unvalidated { last, pipe, .. } = &mut self.phase {
            *last = Some(pn);
            let pipe = *pipe;
            if in_flight >= controller.window() {
                self.leave_unvalidated(Some(pn), pipe, in_flight, controller);
            }
        }
    }

    /// §3.3, on any exit event: "if the flight_size is less than the IW or if the flight_size is
    /// less than or equal to the PipeSize", the window becomes the PipeSize (not below the IW) and
    /// Careful Resume ends; otherwise the window becomes the flight size and Validating follows
    fn leave_unvalidated(
        &mut self,
        last: Option<u64>,
        pipe: u64,
        in_flight: u64,
        controller: &mut dyn Controller,
    ) {
        let iw = controller.initial_window();
        match last {
            Some(last) if in_flight >= iw && in_flight > pipe => {
                controller.set_window(in_flight);
                self.phase = Phase::Validating { last, pipe };
            }
            _ => {
                controller.set_window(pipe.max(iw));
                self.phase = Phase::Off;
            }
        }
    }

    /// The path changed (a migration): Careful Resume stops (§3.2)
    pub(crate) fn on_path_change(&mut self) {
        self.phase = Phase::Off;
        self.given = None;
        self.observer = Observer::default();
    }

    /// What this connection leaves for the next one to its remote: its own measurement if it
    /// reached four initial windows (§3.1), else the one it was given and did not retreat from
    pub(crate) fn observed(&self, initial_window: u64, min_rtt: Duration) -> Option<Saved> {
        let floor = initial_window.saturating_mul(SAVE_FLOOR_WINDOWS);
        match self.observer.best {
            Some((cwnd, at)) if cwnd >= floor => Some(Saved {
                cwnd,
                rtt: min_rtt,
                at,
            }),
            _ => self.given,
        }
    }

    #[cfg(test)]
    pub(crate) fn phase_name(&self) -> &'static str {
        match self.phase {
            Phase::Off => "off",
            Phase::Reconnaissance { .. } => "reconnaissance",
            Phase::Unvalidated { .. } => "unvalidated",
            Phase::Validating { .. } => "validating",
            Phase::Retreat { .. } => "retreat",
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use super::*;
    use crate::congestion::{NewReno, NewRenoConfig};

    const SECOND: Duration = Duration::from_secs(1);
    /// NewReno's default initial window at QUIC's 1,200-byte minimum (RFC 9002 §7.2)
    const IW: u64 = 12_000;

    fn controller(now: Instant) -> NewReno {
        NewReno::new(NewRenoConfig::default(), now, 1200)
    }

    fn rtt(sample: Duration) -> RttEstimator {
        let mut rtt = RttEstimator::new(Duration::from_millis(333));
        rtt.update(Duration::ZERO, sample);
        rtt
    }

    fn saved(cwnd: u64, at: Instant) -> Saved {
        Saved {
            cwnd,
            rtt: SECOND,
            at,
        }
    }

    /// A connection given `cwnd` whose initial data (Data packet 0) is acknowledged
    fn ready(cwnd: u64, now: Instant, controller: &mut NewReno) -> CarefulResume {
        let mut resume = CarefulResume::new(Some(saved(cwnd, now)), u64::MAX);
        resume.on_ack(now, 0, None, Some(0), 0, &rtt(SECOND), controller);
        assert_eq!(resume.phase_name(), "reconnaissance");
        resume.on_ack(now, 1_000, Some(0), Some(0), 0, &rtt(SECOND), controller);
        resume
    }

    #[test]
    fn the_memory_keeps_one_measurement_per_address_for_its_lifetime() {
        let now = Instant::now();
        let (a, b) = (
            IpAddr::from(Ipv4Addr::LOCALHOST),
            IpAddr::from([10, 0, 0, 1]),
        );
        let mut memory = CongestionMemory::new(1);
        memory.put(a, saved(1, now));
        memory.put(a, saved(2, now));
        assert_eq!(memory.len(), 1);
        // One past the bound replaces the oldest
        memory.put(b, saved(3, now));
        assert_eq!(memory.take(a, now, SECOND), None);
        // A taken measurement is out until it is put back
        assert_eq!(memory.take(b, now, SECOND), Some(saved(3, now)));
        assert_eq!(memory.take(b, now, SECOND), None);
        // Past its lifetime it is dropped
        memory.put(b, saved(4, now));
        assert_eq!(memory.take(b, now + 2 * SECOND, SECOND), None);
        assert_eq!(memory.len(), 0);
    }

    #[test]
    fn a_blocked_window_jumps_to_half_the_measurement_once_the_initial_data_is_acknowledged() {
        let now = Instant::now();
        let mut cc = controller(now);
        let mut resume = CarefulResume::new(Some(saved(100_000, now)), u64::MAX);
        // Before the initial data is acknowledged, no jump
        assert!(!resume.on_window_blocked(now, 1, IW, &rtt(SECOND), &mut cc));
        let mut resume = ready(100_000, now, &mut cc);
        assert!(resume.on_window_blocked(now, 10, IW, &rtt(SECOND), &mut cc));
        assert_eq!(cc.window(), 50_000);
        assert_eq!(resume.phase_name(), "unvalidated");
        assert!(resume.holds_window());
        // Paced at one window a round trip
        assert_eq!(resume.pacing_rate(50_000, SECOND), Some(50_000));
    }

    #[test]
    fn max_jump_bounds_the_jump() {
        let now = Instant::now();
        let mut cc = controller(now);
        let mut resume = ready(100_000, now, &mut cc);
        resume.max_jump = 30_000;
        assert!(resume.on_window_blocked(now, 10, IW, &rtt(SECOND), &mut cc));
        assert_eq!(cc.window(), 30_000);
    }

    #[test]
    fn a_different_path_or_a_small_measurement_stops_careful_resume() {
        let now = Instant::now();
        for (sample, cwnd) in [
            // RTT at most half the saved RTT
            (Duration::from_millis(500), 100_000),
            // RTT past ten times the saved RTT
            (Duration::from_secs(11), 100_000),
            // Half the measurement no more than the window
            (SECOND, 2 * IW),
        ] {
            let mut cc = controller(now);
            let mut resume = ready(cwnd, now, &mut cc);
            assert!(!resume.on_window_blocked(now, 10, IW, &rtt(sample), &mut cc));
            assert_eq!(resume.phase_name(), "off");
            assert_eq!(cc.window(), IW);
        }
    }

    #[test]
    fn congestion_before_the_jump_stops_and_after_it_retreats_to_half_the_pipe() {
        let now = Instant::now();
        let mut cc = controller(now);
        let mut resume = CarefulResume::new(Some(saved(100_000, now)), u64::MAX);
        resume.on_congestion(&mut cc);
        assert_eq!(resume.phase_name(), "off");
        // The measurement it never used goes back
        assert_eq!(resume.observed(IW, SECOND), Some(saved(100_000, now)));

        let mut resume = ready(100_000, now, &mut cc);
        assert!(resume.on_window_blocked(now, 10, 20_000, &rtt(SECOND), &mut cc));
        resume.on_data_sent(12, 30_000, &mut cc);
        resume.on_congestion(&mut cc);
        assert_eq!(resume.phase_name(), "retreat");
        assert_eq!(cc.window(), 10_000);
        assert!(resume.holds_window());
        // The measurement is deleted (§3.5)
        assert_eq!(resume.observed(IW, SECOND), None);
        // The last packet sent while Unvalidated is acknowledged: ssthresh from the PipeSize
        resume.on_ack(now, 4_000, Some(12), Some(12), 0, &rtt(SECOND), &mut cc);
        assert_eq!(resume.phase_name(), "off");
        assert_eq!(cc.metrics().ssthresh, Some(12_000));
    }

    #[test]
    fn the_unvalidated_phase_ends_after_a_round_trip_at_the_pipe_or_the_flight() {
        let now = Instant::now();
        // Little in flight at the exit: the window becomes the PipeSize and Careful Resume ends
        let mut cc = controller(now);
        let mut resume = ready(100_000, now, &mut cc);
        assert!(resume.on_window_blocked(now, 10, 14_000, &rtt(SECOND), &mut cc));
        resume.on_data_sent(11, 16_000, &mut cc);
        resume.on_ack(now, 8_000, Some(10), Some(11), 8_000, &rtt(SECOND), &mut cc);
        assert_eq!(resume.phase_name(), "off");
        assert_eq!(cc.window(), 22_000);

        // A full flight: the window becomes the flight and Validating follows until the last
        // packet sent while Unvalidated is acknowledged
        let mut cc = controller(now);
        let mut resume = ready(100_000, now, &mut cc);
        assert!(resume.on_window_blocked(now, 10, 12_000, &rtt(SECOND), &mut cc));
        resume.on_data_sent(40, 50_000, &mut cc);
        assert_eq!(resume.phase_name(), "validating");
        assert_eq!(cc.window(), 50_000);
        assert!(!resume.holds_window());
        resume.on_ack(
            now,
            1_000,
            Some(39),
            Some(40),
            49_000,
            &rtt(SECOND),
            &mut cc,
        );
        assert_eq!(resume.phase_name(), "validating");
        resume.on_ack(
            now,
            1_000,
            Some(40),
            Some(40),
            48_000,
            &rtt(SECOND),
            &mut cc,
        );
        assert_eq!(resume.phase_name(), "off");
    }

    #[test]
    fn a_connection_saves_what_it_delivered_a_round_trip_from_four_initial_windows() {
        let now = Instant::now();
        let mut cc = controller(now);
        let mut resume = CarefulResume::new(None, u64::MAX);
        // 47,000 bytes in the first round trip: below four initial windows, not saved
        resume.on_ack(now, 47_000, Some(1), Some(1), 0, &rtt(SECOND), &mut cc);
        resume.on_ack(now + SECOND, 0, Some(1), Some(1), 0, &rtt(SECOND), &mut cc);
        assert_eq!(resume.observed(IW, SECOND), None);
        // 60,000 in the next: saved, with the minimum RTT and when it was measured
        resume.on_ack(
            now + SECOND,
            60_000,
            Some(2),
            Some(2),
            0,
            &rtt(SECOND),
            &mut cc,
        );
        resume.on_ack(
            now + 2 * SECOND,
            0,
            Some(2),
            Some(2),
            0,
            &rtt(SECOND),
            &mut cc,
        );
        assert_eq!(
            resume.observed(IW, Duration::from_millis(900)),
            Some(Saved {
                cwnd: 60_000,
                rtt: Duration::from_millis(900),
                at: now + 2 * SECOND,
            })
        );
    }
}
