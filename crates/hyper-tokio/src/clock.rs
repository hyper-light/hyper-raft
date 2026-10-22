//! The host's monotonic clock as nanoseconds, and the receive stamps of a plane socket on it.
//!
//! A heartbeat is judged by when the kernel received it, not when its owner read it
//! (`docs/timing.md` §2.4): a receiver that wakes late must not blame the sender for its own
//! lateness. The stamps and the owner's `now` are therefore on one clock, the host's monotonic one,
//! which every process on the host reads alike (so two processes' times compare, as the end-to-end
//! tests compare them):
//! - Linux: `CLOCK_MONOTONIC`. Its receive stamps (`SO_TIMESTAMPNS`) are `CLOCK_REALTIME`, so a
//!   stamp is carried over by the offset between the clocks, read after each receive, the realtime
//!   one first so that a preemption between the reads makes the stamp late and never early. A slew
//!   moves both clocks alike and leaves the offset; a step of the realtime clock between a
//!   datagram's arrival and its read moves the read's offset and not the stamp's, so a stamp is
//!   carried by the least offset it may have been taken on (`Carry`), and held within the read's
//!   time and no earlier than the stamp of the datagram read before it from the socket, whose
//!   queue is first in, first out.
//! - macOS: `mach_absolute_time`, the clock `SO_TIMESTAMP_MONOTONIC` stamps on, the stamps in the
//!   kernel's ticks and converted at its rate (`hw.tbfrequency`), which is the process's timebase
//!   only where the process runs natively (`sys::macos`).
//! - Windows: `QueryPerformanceCounter`. Winsock's receive timestamps (`SIO_TIMESTAMPING`, Windows
//!   10 build 20348) are taken by a NIC miniport driver that reports timestamping capabilities, and
//!   none is taken on loopback or a virtual NIC (`docs/research/timing.md`), so a datagram is
//!   stamped when it is read: the portable path. Its cost is the read's delay counted as the
//!   sender's, in the delays the detector measures and so in its margin.

use std::time::Duration;

use crate::Error;

#[cfg(target_os = "linux")]
use crate::sys::linux;
#[cfg(target_os = "macos")]
use crate::sys::macos;
#[cfg(windows)]
use crate::sys::windows;

/// The host's monotonic clock, nanoseconds.
#[derive(Clone, Copy, Debug)]
pub struct Clock {
    #[cfg(target_os = "macos")]
    os: macos::Clock,
    #[cfg(windows)]
    os: windows::Clock,
    /// Elsewhere: an instant this process counts from, so times compare within the process only.
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    origin: std::time::Instant,
}

impl Clock {
    /// The clock, with its scale where the platform has one.
    pub fn new() -> Result<Self, Error> {
        #[cfg(target_os = "linux")]
        {
            linux::monotonic_ns()?;
            Ok(Self {})
        }
        #[cfg(target_os = "macos")]
        {
            Ok(Self {
                os: macos::Clock::new()?,
            })
        }
        #[cfg(windows)]
        {
            Ok(Self {
                os: windows::Clock::new()?,
            })
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
        {
            Ok(Self {
                origin: std::time::Instant::now(),
            })
        }
    }

    /// Now, nanoseconds. Zero if the platform refuses the clock it gave at [`Clock::new`], which
    /// none documents doing.
    pub fn now_ns(&self) -> u64 {
        #[cfg(target_os = "linux")]
        {
            linux::monotonic_ns().unwrap_or(0)
        }
        #[cfg(any(target_os = "macos", windows))]
        {
            self.os.now_ns()
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
        {
            u64::try_from(self.origin.elapsed().as_nanos()).unwrap_or(u64::MAX)
        }
    }

    /// The least step the clock's readings take, which bounds how late a wait read exactly on time
    /// was (`hyper_timing::Lateness`). On macOS a tick of `mach_absolute_time`, `numer / denom`
    /// nanoseconds (125/3 on Apple silicon), rounded up, or the kernel's tick where that is larger
    /// (a process Rosetta 2 translates is shown 1/1, its clock stepping 125/3); on Windows a count of
    /// `QueryPerformanceCounter`, a second over `QueryPerformanceFrequency`, rounded up. Linux
    /// states no step for `CLOCK_MONOTONIC`'s readings: `clock_getres(2)` reports its timers'
    /// resolution, a jiffy where the kernel has no high-resolution timers, and a wait's lateness
    /// does not stay above that (a deadline just before a tick is late by little). There, and
    /// elsewhere, where std states none for `Instant`, it is the readings' unit, a nanosecond; a
    /// counter that steps coarser (an arm64 timer at 24 or 25 MHz, Hyper-V's 100 ns reference
    /// clock) is then understated, which matters only to an owner whose every wait reads within
    /// that step of its deadline.
    pub fn resolution(&self) -> Duration {
        #[cfg(any(target_os = "macos", windows))]
        {
            Duration::from_nanos(self.os.tick_ns())
        }
        #[cfg(not(any(target_os = "macos", windows)))]
        {
            Duration::from_nanos(1)
        }
    }

    /// A macOS kernel stamp, the kernel's `mach_absolute_time` in its own ticks, as nanoseconds on
    /// this clock: at the kernel's rate, which a translated process's timebase is not.
    #[cfg(target_os = "macos")]
    pub(crate) fn kernel_ns(&self, ticks: u64) -> u64 {
        self.os.kernel_ns(ticks)
    }
}

/// Where and when a datagram arrived.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Arrival {
    /// The address it came from.
    pub from: std::net::SocketAddr,
    /// When it arrived, nanoseconds on the socket's [`Clock`]: the kernel's receive stamp where
    /// there is one, else when it was read.
    pub at_ns: u64,
    /// Whether `at_ns` is the kernel's stamp.
    pub kernel: bool,
}

/// Linux's receive stamps, `CLOCK_REALTIME`, carried to `CLOCK_MONOTONIC` (the module's note).
///
/// The two clocks differ by an offset that the kernel's frequency corrections leave alone, since
/// they slew both alike (`clock_gettime(2)`: `CLOCK_MONOTONIC` "is affected by the incremental
/// adjustments performed by adjtime(3) and NTP"), and that only a step of the realtime clock moves
/// (`settimeofday(2)`, `adjtimex(2)`'s `ADJ_SETOFFSET`). A stamp is carried by the offset it was
/// taken on. That offset is not read with it: the kernel stamps when the datagram arrives and the
/// clocks are read after the receive that takes it, and a step between moves the read's offset
/// and not the stamp's. Carried by the read's offset alone, a stamp taken before a forward step
/// came out the step early. Docker Desktop's Linux VM steps its realtime clock forward by 0.28 to
/// 0.85 ms every 30 s, and that is what put a stamp before its datagram's send
/// (`docs/benchmarks.md`, "hyper-tokio: kernel stamps across a realtime step").
///
/// So a stamp is carried by the least offset read since the read before the last one that found
/// the socket empty, and to no earlier than that read began. A datagram that arrived after that
/// read was stamped on one of those offsets or on one read since, and the least is at most its
/// own: late by a step at most, never early, and exact when no step came between. One that arrived
/// before it (queued behind a full batch, or behind a read that reported the socket empty from a
/// stale readiness: tokio's `try_io` reports `WouldBlock` without asking the kernel while its
/// readiness is clear) is held to that read, which is after its arrival: late, never early. Each
/// offset is read realtime first, so a preemption between the two reads lowers it and a stamp comes
/// out late by the preemption, never early. One case is beyond the rule: two steps between two
/// reads, back and then forward, with a datagram arriving between them; the offset it was stamped
/// on was never read, and it is carried early by the lesser step, no earlier than the read the
/// rule holds it to.
#[cfg(any(target_os = "linux", test))]
#[derive(Clone, Copy, Debug)]
pub(crate) struct Carry {
    /// `CLOCK_REALTIME − CLOCK_MONOTONIC` at the read before the latest, and that read's monotonic
    /// time.
    previous: (i128, u64),
    /// The same at the latest read.
    current: (i128, u64),
    /// The least offset read since the read before the last one that found the socket empty.
    floor: i128,
    /// The monotonic time of the read before the last one that found the socket empty. Zero until
    /// a read finds the socket empty: a datagram queued before the carry began has no read before
    /// it.
    since_ns: u64,
}

#[cfg(any(target_os = "linux", test))]
impl Carry {
    /// A carry whose clocks were read as `realtime_ns` and then `monotonic_ns`.
    pub(crate) fn new(realtime_ns: u64, monotonic_ns: u64) -> Self {
        let read = (offset(realtime_ns, monotonic_ns), monotonic_ns);
        Self {
            previous: read,
            current: read,
            floor: read.0,
            since_ns: 0,
        }
    }

    /// A receive's clocks, read after it, the realtime one first.
    pub(crate) fn read(&mut self, realtime_ns: u64, monotonic_ns: u64) {
        let offset = offset(realtime_ns, monotonic_ns);
        self.previous = self.current;
        self.current = (offset, monotonic_ns);
        self.floor = self.floor.min(offset);
    }

    /// The receive last read found the socket empty (it took less than it asked for, or nothing):
    /// a datagram read from now on arrived after the read before it.
    pub(crate) fn emptied(&mut self) {
        self.floor = self.previous.0.min(self.current.0);
        self.since_ns = self.previous.1;
    }

    /// A stamp taken by the receive last read, on the monotonic clock; `None` before the clock's
    /// zero or past `u64`.
    pub(crate) fn carry(&self, stamp_ns: u64) -> Option<u64> {
        let at = u64::try_from(i128::from(stamp_ns).checked_sub(self.floor)?).ok()?;
        Some(at.max(self.since_ns))
    }
}

/// `realtime − monotonic`, which no `u64` pair overflows in an `i128`.
#[cfg(any(target_os = "linux", test))]
fn offset(realtime_ns: u64, monotonic_ns: u64) -> i128 {
    i128::from(realtime_ns)
        .checked_sub(i128::from(monotonic_ns))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::Carry;

    /// The realtime clock's offset from the monotonic one at the start of each test.
    const OFFSET: u64 = 1_000_000_000_000;
    /// A step of the realtime clock forward: Docker Desktop's Linux VM (6.12.76-linuxkit, 2026-10-16)
    /// stepped `CLOCK_REALTIME` forward by 0.28 to 0.85 ms every 30 s, with `CLOCK_MONOTONIC`
    /// untouched (`docs/benchmarks.md`, "hyper-tokio: kernel stamps across a realtime step").
    const STEP: u64 = 500_000;

    /// A datagram that arrived before the realtime clock stepped forward, and was read after, is
    /// carried to when it arrived, not the step earlier: the stamp is on the clock before the
    /// step, the read's realtime after it.
    #[test]
    fn a_stamp_taken_before_a_forward_step_is_not_carried_early() {
        let mut carry = Carry::new(OFFSET + 1_000, 1_000);
        // An empty read, then the datagram arrives at 5 ms, the clock steps at 10 ms, and the read
        // at 20 ms takes it.
        carry.read(OFFSET + 2_000, 2_000);
        carry.emptied();
        let arrived = 5_000_000;
        let stamp = OFFSET + arrived;
        let read = 20_000_000;
        carry.read(OFFSET + STEP + read, read);
        let at = carry.carry(stamp).unwrap();
        assert!(at >= arrived, "carried to {at}, {} ns early", arrived - at);
        assert!(at <= read);
    }

    /// A datagram left queued by a read that took a full batch, the clock stepping forward
    /// before that read, is carried by the offset it was stamped on at the read that takes it.
    #[test]
    fn a_stamp_queued_across_a_full_read_and_a_step_is_not_carried_early() {
        let mut carry = Carry::new(OFFSET, 0);
        carry.read(OFFSET + 1_000, 1_000);
        carry.emptied();
        let arrived = 2_000_000;
        carry.read(OFFSET + STEP + 3_000_000, 3_000_000);
        carry.read(OFFSET + STEP + 4_000_000, 4_000_000);
        let at = carry.carry(OFFSET + arrived).unwrap();
        assert!(at >= arrived, "carried to {at}, {} ns early", arrived - at);
    }

    /// After a forward step, a datagram that arrives once the socket was found empty twice since
    /// is carried exactly: the offset before the step no longer counts.
    #[test]
    fn a_stamp_after_a_step_and_two_empty_reads_is_carried_exactly() {
        let mut carry = Carry::new(OFFSET, 0);
        carry.read(OFFSET + STEP + 1_000_000, 1_000_000);
        carry.emptied();
        carry.read(OFFSET + STEP + 2_000_000, 2_000_000);
        carry.emptied();
        let arrived = 3_000_000;
        carry.read(OFFSET + STEP + 4_000_000, 4_000_000);
        assert_eq!(carry.carry(OFFSET + STEP + arrived), Some(arrived));
    }

    /// A step back is carried late, never early, for a datagram stamped before it, and exactly
    /// for one stamped after it.
    #[test]
    fn a_backward_step_carries_late_or_exactly() {
        let mut carry = Carry::new(OFFSET, 0);
        carry.read(OFFSET + 1_000_000, 1_000_000);
        carry.emptied();
        let before = 2_000_000;
        carry.read(OFFSET - STEP + 3_000_000, 3_000_000);
        let at = carry.carry(OFFSET + before).unwrap();
        assert_eq!(at, before + STEP);
        let after = 3_500_000;
        carry.read(OFFSET - STEP + 4_000_000, 4_000_000);
        assert_eq!(carry.carry(OFFSET - STEP + after), Some(after));
    }

    /// A read that reports the socket empty while a datagram waits (tokio's stale readiness), the
    /// clock having stepped forward between that datagram's arrival and the read before, holds the
    /// datagram to the read before: late, never early.
    #[test]
    fn a_stamp_behind_a_stale_empty_read_is_held_to_the_read_before() {
        let mut carry = Carry::new(OFFSET, 0);
        let arrived = 500_000;
        // The clock steps at 0.8 ms; the read at 1 ms takes a full batch of earlier datagrams.
        carry.read(OFFSET + STEP + 1_000_000, 1_000_000);
        carry.read(OFFSET + STEP + 2_000_000, 2_000_000);
        carry.emptied();
        carry.read(OFFSET + STEP + 3_000_000, 3_000_000);
        let at = carry.carry(OFFSET + arrived).unwrap();
        assert_eq!(at, 1_000_000);
        assert!(at >= arrived);
    }

    /// No stamp is carried earlier than the read before the last one that found the socket empty:
    /// every datagram read since arrived after it.
    #[test]
    fn no_stamp_is_carried_before_the_socket_was_last_found_empty() {
        let mut carry = Carry::new(OFFSET, 0);
        carry.read(OFFSET + 1_000_000, 1_000_000);
        carry.read(OFFSET + 2_000_000, 2_000_000);
        carry.emptied();
        carry.read(OFFSET + 3_000_000, 3_000_000);
        assert_eq!(carry.carry(OFFSET + 10), Some(1_000_000));
    }
}
