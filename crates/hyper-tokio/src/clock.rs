//! The host's monotonic clock as nanoseconds, and the receive stamps of a plane socket on it.
//!
//! A heartbeat is judged by when the kernel received it, not when its owner read it
//! (`docs/timing.md` §2.4): a receiver that wakes late must not blame the sender for its own
//! lateness. The stamps and the owner's `now` are therefore on one clock, the host's monotonic one,
//! which every process on the host reads alike (so two processes' times compare, as the end-to-end
//! tests compare them):
//! - Linux: `CLOCK_MONOTONIC`. Its receive stamps (`SO_TIMESTAMPNS`) are `CLOCK_REALTIME`, so a
//!   stamp is carried over by its age: read with both clocks after the receive, it is
//!   `monotonic − (realtime − stamp)`, held within the read's time and no earlier than the stamp of
//!   the datagram read before it from the socket, whose queue is first in, first out. A step of the
//!   realtime clock between a datagram's arrival and its read moves only that stamp, within those
//!   limits; a slew moves it by its rate times the age, at most 500 ppm (`adjtimex(2)`'s
//!   `MAXFREQ`).
//! - macOS: `mach_absolute_time`, the clock `SO_TIMESTAMP_MONOTONIC` stamps on.
//! - Windows: `QueryPerformanceCounter`. Winsock's receive timestamps (`SIO_TIMESTAMPING`, Windows
//!   10 build 20348) are taken by a NIC miniport driver that reports timestamping capabilities, and
//!   none is taken on loopback or a virtual NIC (`docs/research/timing.md`), so a datagram is
//!   stamped when it is read: the portable path. Its cost is the read's delay counted as the
//!   sender's, in the delays the detector measures and so in its margin.

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

    /// A macOS kernel stamp, `mach_absolute_time` ticks, as nanoseconds on this clock.
    #[cfg(target_os = "macos")]
    pub(crate) fn ticks_ns(&self, ticks: u64) -> u64 {
        self.os.ticks_ns(ticks)
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
