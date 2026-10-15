//! Windows: the host's monotonic clock, `QueryPerformanceCounter` scaled by
//! `QueryPerformanceFrequency` (profileapi.h), through windows-sys. Every process on the host reads
//! the same counter, which is also the clock of Winsock's receive timestamps where a NIC driver
//! gives them (`docs/research/timing.md`, "Winsock timestamping"); the adapter does not ask for
//! those, since only a miniport driver that reports timestamping capabilities stamps, which no
//! virtual NIC and no loopback path does, and stamps a datagram where it reads it instead.

#![allow(unsafe_code)]

use std::io;
use std::mem::size_of;

use windows_sys::Win32::Networking::WinSock::{
    SO_RCVBUF, SOCKET, SOCKET_ERROR, SOL_SOCKET, getsockopt, setsockopt,
};
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

    /// One count in nanoseconds, a second over the frequency rounded up: no two readings are closer
    /// apart.
    pub(crate) fn tick_ns(&self) -> u64 {
        let frequency = u128::from(self.frequency);
        let tick = NANOS_PER_SECOND
            .saturating_add(frequency.saturating_sub(1))
            .checked_div(frequency)
            .unwrap_or(1);
        u64::try_from(tick).unwrap_or(u64::MAX)
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

/// The receive buffer Winsock reports for `socket` (`SO_RCVBUF`).
pub(crate) fn recv_buffer(socket: u64) -> io::Result<usize> {
    let socket = SOCKET::try_from(socket).map_err(|_| io::Error::other("a socket handle"))?;
    let mut value: i32 = 0;
    let mut length = i32::try_from(size_of::<i32>()).unwrap_or(i32::MAX);
    // SAFETY: `value` and `length` are live locals of the sizes passed; getsockopt writes at most
    // `length` bytes into `value` and the new length into `length`.
    let status = unsafe {
        getsockopt(
            socket,
            SOL_SOCKET,
            SO_RCVBUF,
            (&raw mut value).cast::<u8>(),
            &raw mut length,
        )
    };
    if status == SOCKET_ERROR {
        return Err(io::Error::last_os_error());
    }
    usize::try_from(value).map_err(|_| io::Error::other("SO_RCVBUF read back negative"))
}

/// Asks Winsock for a receive buffer of `bytes` on `socket` ([`super::set_recv_buffer`]):
/// `SO_RCVBUF`, a size it refuses halved and asked again, at most [`super::BUFFER_ASKS`] times.
/// The buffer reported after.
pub(crate) fn set_recv_buffer(socket: u64, bytes: usize) -> io::Result<usize> {
    let handle = SOCKET::try_from(socket).map_err(|_| io::Error::other("a socket handle"))?;
    let mut value = i32::try_from(bytes).unwrap_or(i32::MAX);
    let length = i32::try_from(size_of::<i32>()).unwrap_or(i32::MAX);
    let mut refusal = None;
    for _ in 0..super::BUFFER_ASKS {
        // SAFETY: `value` is a live `i32` and the length passed is its size; setsockopt only
        // reads it.
        let status = unsafe {
            setsockopt(
                handle,
                SOL_SOCKET,
                SO_RCVBUF,
                (&raw const value).cast::<u8>(),
                length,
            )
        };
        if status != SOCKET_ERROR {
            return recv_buffer(socket);
        }
        refusal = Some(io::Error::last_os_error());
        value = value.checked_div(2).unwrap_or(0);
        if value == 0 {
            break;
        }
    }
    Err(refusal.unwrap_or_else(|| io::Error::other("no receive buffer was granted")))
}
