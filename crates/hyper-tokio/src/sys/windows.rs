//! Windows: the host's monotonic clock, `QueryPerformanceCounter` scaled by
//! `QueryPerformanceFrequency` (profileapi.h), through windows-sys. Every process on the host reads
//! the same counter, which is also the clock of Winsock's receive timestamps where a NIC driver
//! gives them (`docs/research/timing.md`, "Winsock timestamping"); the adapter does not ask for
//! those, since only a miniport driver that reports timestamping capabilities stamps, which no
//! virtual NIC and no loopback path does, and stamps a datagram where it reads it instead.

#![allow(unsafe_code)]

use std::io;

use windows_sys::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

/// Nanoseconds in a second.
const NANOS_PER_SECOND: u128 = 1_000_000_000;

/// The host's monotonic clock as nanoseconds.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Clock {
    /// Counts a second; never zero.
    frequency: u64,
}

impl Clock {
    pub(crate) fn new() -> io::Result<Self> {
        let mut frequency: i64 = 0;
        // SAFETY: `frequency` is a live, writable `i64` (`LARGE_INTEGER`) for the call, which
        // writes that one value; the call cannot fail on Windows XP and later (profileapi.h).
        let ok = unsafe { QueryPerformanceFrequency(&raw mut frequency) };
        let frequency = u64::try_from(frequency).unwrap_or(0);
        if ok == 0 || frequency == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { frequency })
    }

    /// Now, in nanoseconds.
    pub(crate) fn now_ns(&self) -> u64 {
        let mut counter: i64 = 0;
        // SAFETY: `counter` is a live, writable `i64` for the call, which writes that one value.
        unsafe { QueryPerformanceCounter(&raw mut counter) };
        let ns = u128::try_from(counter)
            .unwrap_or(0)
            .checked_mul(NANOS_PER_SECOND)
            .and_then(|scaled| scaled.checked_div(u128::from(self.frequency)))
            .unwrap_or(u128::MAX);
        u64::try_from(ns).unwrap_or(u64::MAX)
    }
}
