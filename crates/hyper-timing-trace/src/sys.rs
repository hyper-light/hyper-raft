//! The operating-system interface the recorder needs and std does not give: a monotonic clock as
//! nanoseconds two processes share, the kernel's receive timestamp of a datagram, a socket wait with
//! a sub-millisecond timeout, the platform's full flush, and the load average.
//!
//! - Linux: `clock_gettime(CLOCK_MONOTONIC)` (clock_gettime(2)); `SO_TIMESTAMPNS`, whose
//!   `SCM_TIMESTAMPNS` control message carries a `struct timespec` in `CLOCK_REALTIME` taken when
//!   the kernel received the datagram (socket(7)); `select(2)`, whose timeout is a `struct timeval`
//!   and which ends no earlier than asked and up to the thread's timer slack late
//!   (PR_SET_TIMERSLACK(2const)); `fdatasync(2)`.
//! - macOS: `mach_absolute_time` scaled by `mach_timebase_info` (<mach/mach_time.h>);
//!   `SO_TIMESTAMP_MONOTONIC` (0x0800, <sys/socket.h>), whose `SCM_TIMESTAMP_MONOTONIC` (0x04)
//!   control message carries a `uint64_t` the kernel takes with `mach_absolute_time()` as UDP input
//!   appends the datagram to the socket (XNU `bsd/netinet/ip_input.c`, `ip_savecontrol`, called
//!   from `udp_input` in `udp_usrreq.c`); `select(2)`; `fcntl(F_FULLFSYNC)` (51, <sys/fcntl.h>),
//!   which asks the drive to flush to the media.
//! - Both: `recvmsg(2)` and `getloadavg(3)`.
//!
//! Elsewhere (Windows, open item 5 of `docs/timing.md` §3) the recorder does not run.
#![allow(unsafe_code)]

use std::io;
use std::os::fd::RawFd;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("the trace recorder runs on Linux and macOS (docs/timing.md §3, item 5)");

/// `struct iovec` (<sys/uio.h>): the same on both.
#[repr(C)]
struct IoVec {
    base: *mut u8,
    len: usize,
}

/// `struct msghdr`, Linux (glibc and musl on LP64): lengths are `size_t`.
#[cfg(target_os = "linux")]
#[repr(C)]
struct MsgHdr {
    name: *mut u8,
    namelen: u32,
    iov: *mut IoVec,
    iovlen: usize,
    control: *mut u8,
    controllen: usize,
    flags: i32,
}

/// `struct msghdr`, macOS (<sys/socket.h>): `msg_iovlen` is an `int`, `msg_controllen` a
/// `socklen_t`.
#[cfg(target_os = "macos")]
#[repr(C)]
struct MsgHdr {
    name: *mut u8,
    namelen: u32,
    iov: *mut IoVec,
    iovlen: i32,
    control: *mut u8,
    controllen: u32,
    flags: i32,
}

/// `struct timeval`: `tv_usec` is a `long` on Linux and an `int` (`__darwin_suseconds_t`) on macOS,
/// padded to 16 bytes either way.
#[repr(C)]
struct TimeVal {
    sec: i64,
    #[cfg(target_os = "linux")]
    usec: i64,
    #[cfg(target_os = "macos")]
    usec: i32,
}

/// `struct timespec` on LP64 Linux.
#[cfg(target_os = "linux")]
#[repr(C)]
struct TimeSpec {
    sec: i64,
    nsec: i64,
}

/// `struct mach_timebase_info` (<mach/mach_time.h>).
#[cfg(target_os = "macos")]
#[repr(C)]
struct Timebase {
    numer: u32,
    denom: u32,
}

/// `SOL_SOCKET`: 1 on Linux (asm-generic/socket.h), 0xffff on macOS (<sys/socket.h>).
#[cfg(target_os = "linux")]
const SOL_SOCKET: i32 = 1;
/// `SOL_SOCKET`: 1 on Linux (asm-generic/socket.h), 0xffff on macOS (<sys/socket.h>).
#[cfg(target_os = "macos")]
const SOL_SOCKET: i32 = 0xffff;
/// `SO_TIMESTAMPNS` (`SO_TIMESTAMPNS_OLD`, 35, asm-generic/socket.h); its control message type
/// `SCM_TIMESTAMPNS` is the same number.
#[cfg(target_os = "linux")]
const SO_STAMP: i32 = 35;
/// `SCM_TIMESTAMPNS` = `SO_TIMESTAMPNS` (asm-generic/socket.h).
#[cfg(target_os = "linux")]
const SCM_STAMP: i32 = 35;
/// `SO_TIMESTAMP_MONOTONIC`, 0x0800 (<sys/socket.h>).
#[cfg(target_os = "macos")]
const SO_STAMP: i32 = 0x0800;
/// `SCM_TIMESTAMP_MONOTONIC`, 0x04 (<sys/socket.h>).
#[cfg(target_os = "macos")]
const SCM_STAMP: i32 = 0x04;
/// `CLOCK_MONOTONIC`, 1 (linux/time.h).
#[cfg(target_os = "linux")]
const CLOCK_MONOTONIC: i32 = 1;
/// `F_FULLFSYNC`, 51 (<sys/fcntl.h>).
#[cfg(target_os = "macos")]
const F_FULLFSYNC: i32 = 51;
/// The size of `struct cmsghdr` rounded to the control-message alignment: `size_t` + two `int`s,
/// aligned to `size_t`, on Linux (`CMSG_ALIGN`); three 32-bit fields aligned to 4 on macOS
/// (`__DARWIN_ALIGN32`).
#[cfg(target_os = "linux")]
const CMSG_HEADER: usize = 16;
/// See the Linux definition.
#[cfg(target_os = "macos")]
const CMSG_HEADER: usize = 12;
/// The control-message alignment: `size_t` on Linux, 4 bytes on macOS.
#[cfg(target_os = "linux")]
const CMSG_ALIGN: usize = 8;
/// See the Linux definition.
#[cfg(target_os = "macos")]
const CMSG_ALIGN: usize = 4;
/// `FD_SETSIZE`, 1024 on both: `fd_set` is that many bits.
const FD_SETSIZE: usize = 1024;

unsafe extern "C" {
    fn setsockopt(fd: i32, level: i32, name: i32, value: *const u8, len: u32) -> i32;
    fn recvmsg(fd: i32, msg: *mut MsgHdr, flags: i32) -> isize;
    fn select(
        nfds: i32,
        read: *mut u8,
        write: *mut u8,
        error: *mut u8,
        timeout: *mut TimeVal,
    ) -> i32;
    fn getloadavg(loads: *mut f64, count: i32) -> i32;
    #[cfg(target_os = "linux")]
    fn clock_gettime(clock: i32, now: *mut TimeSpec) -> i32;
    #[cfg(target_os = "linux")]
    fn fdatasync(fd: i32) -> i32;
    #[cfg(target_os = "macos")]
    fn mach_absolute_time() -> u64;
    #[cfg(target_os = "macos")]
    fn mach_timebase_info(info: *mut Timebase) -> i32;
    #[cfg(target_os = "macos")]
    fn fcntl(fd: i32, command: i32, ...) -> i32;
}

/// The monotonic clock both processes read, as nanoseconds.
pub struct Clock {
    #[cfg(target_os = "macos")]
    numer: u64,
    #[cfg(target_os = "macos")]
    denom: u64,
}

impl Clock {
    /// The clock, with its scale on macOS.
    pub fn new() -> io::Result<Self> {
        #[cfg(target_os = "macos")]
        {
            let mut info = Timebase { numer: 0, denom: 0 };
            // SAFETY: `info` is a live, writable `mach_timebase_info` for the call.
            let status = unsafe { mach_timebase_info(&raw mut info) };
            if status != 0 || info.denom == 0 {
                return Err(io::Error::other("mach_timebase_info failed"));
            }
            Ok(Self {
                numer: u64::from(info.numer),
                denom: u64::from(info.denom),
            })
        }
        #[cfg(target_os = "linux")]
        Ok(Self {})
    }

    /// Now, nanoseconds on the monotonic clock.
    pub fn now(&self) -> u64 {
        #[cfg(target_os = "macos")]
        {
            // SAFETY: takes no argument and reads the timebase register.
            let ticks = unsafe { mach_absolute_time() };
            self.ticks_ns(ticks)
        }
        #[cfg(target_os = "linux")]
        {
            let mut now = TimeSpec { sec: 0, nsec: 0 };
            // SAFETY: `now` is a live, writable `timespec` for the call.
            let status = unsafe { clock_gettime(CLOCK_MONOTONIC, &raw mut now) };
            if status != 0 {
                return 0;
            }
            u64::try_from(now.sec)
                .unwrap_or(0)
                .saturating_mul(1_000_000_000)
                .saturating_add(u64::try_from(now.nsec).unwrap_or(0))
        }
    }

    /// A kernel timestamp as nanoseconds: on macOS `mach_absolute_time` ticks on the monotonic
    /// clock; on Linux already nanoseconds of `CLOCK_REALTIME`.
    pub fn kernel_ns(&self, raw: u64) -> u64 {
        #[cfg(target_os = "macos")]
        {
            self.ticks_ns(raw)
        }
        #[cfg(target_os = "linux")]
        {
            raw
        }
    }

    #[cfg(target_os = "macos")]
    fn ticks_ns(&self, ticks: u64) -> u64 {
        let ns = u128::from(ticks) * u128::from(self.numer) / u128::from(self.denom.max(1));
        u64::try_from(ns).unwrap_or(u64::MAX)
    }
}

/// Which clock [`recv_stamped`]'s kernel timestamps are on.
pub const KERNEL_CLOCK: &str = if cfg!(target_os = "macos") {
    "monotonic (mach_absolute_time)"
} else {
    "realtime (CLOCK_REALTIME)"
};

/// Asks the kernel to stamp each datagram `fd` receives.
pub fn enable_receive_timestamps(fd: RawFd) -> io::Result<()> {
    let on: i32 = 1;
    // SAFETY: `on` is a live `int` for the call and its size is passed with it.
    let status = unsafe {
        setsockopt(
            fd,
            SOL_SOCKET,
            SO_STAMP,
            (&raw const on).cast(),
            u32::try_from(size_of::<i32>()).unwrap_or(4),
        )
    };
    if status != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Receives one datagram into `buf` without blocking past what the socket's mode says: its length
/// and the kernel's receive timestamp, raw, when the kernel attached one.
pub fn recv_stamped(fd: RawFd, buf: &mut [u8]) -> io::Result<(usize, Option<u64>)> {
    let mut control = [0u64; 8];
    let mut iov = IoVec {
        base: buf.as_mut_ptr(),
        len: buf.len(),
    };
    #[cfg(target_os = "linux")]
    let (iovlen, controllen) = (1usize, size_of_val(&control));
    #[cfg(target_os = "macos")]
    let (iovlen, controllen) = (1i32, u32::try_from(size_of_val(&control)).unwrap_or(0));
    let mut msg = MsgHdr {
        name: std::ptr::null_mut(),
        namelen: 0,
        iov: &raw mut iov,
        iovlen,
        control: control.as_mut_ptr().cast(),
        controllen,
        flags: 0,
    };
    // SAFETY: `msg` points at `iov`, which covers `buf`, and at `control`, all live and writable
    // for the call with their lengths stated; no name buffer is given.
    let read = unsafe { recvmsg(fd, &raw mut msg, 0) };
    let Ok(read) = usize::try_from(read) else {
        return Err(io::Error::last_os_error());
    };
    let filled = usize::try_from(msg.controllen).unwrap_or(0);
    let bytes: Vec<u8> = control.iter().flat_map(|word| word.to_ne_bytes()).collect();
    Ok((read, stamp_of(bytes.get(..filled).unwrap_or(&[]))))
}

/// The receive timestamp among the control messages `control` holds.
fn stamp_of(control: &[u8]) -> Option<u64> {
    let mut at = 0usize;
    while let Some(header) = control.get(at..at.checked_add(CMSG_HEADER)?) {
        #[cfg(target_os = "linux")]
        let length = usize::try_from(u64::from_ne_bytes(header.get(..8)?.try_into().ok()?)).ok()?;
        #[cfg(target_os = "macos")]
        let length = usize::try_from(u32::from_ne_bytes(header.get(..4)?.try_into().ok()?)).ok()?;
        let word = |from: usize| -> Option<i32> {
            Some(i32::from_ne_bytes(
                header.get(from..from.checked_add(4)?)?.try_into().ok()?,
            ))
        };
        #[cfg(target_os = "linux")]
        let (level, kind) = (word(8)?, word(12)?);
        #[cfg(target_os = "macos")]
        let (level, kind) = (word(4)?, word(8)?);
        let data = control.get(at.checked_add(CMSG_HEADER)?..at.checked_add(length)?)?;
        if level == SOL_SOCKET && kind == SCM_STAMP {
            #[cfg(target_os = "linux")]
            {
                let sec = u64::from_ne_bytes(data.get(..8)?.try_into().ok()?);
                let nsec = u64::from_ne_bytes(data.get(8..16)?.try_into().ok()?);
                return Some(sec.checked_mul(1_000_000_000)?.checked_add(nsec)?);
            }
            #[cfg(target_os = "macos")]
            return Some(u64::from_ne_bytes(data.get(..8)?.try_into().ok()?));
        }
        let step = length.checked_add(CMSG_ALIGN - 1)? / CMSG_ALIGN * CMSG_ALIGN;
        if step == 0 {
            return None;
        }
        at = at.checked_add(step)?;
    }
    None
}

/// Waits until `fd` is readable or `timeout_ns` passes: `true` when readable.
pub fn wait_readable(fd: RawFd, timeout_ns: u64) -> io::Result<bool> {
    let index = usize::try_from(fd).map_err(|_| io::Error::other("negative descriptor"))?;
    if index >= FD_SETSIZE {
        return Err(io::Error::other("descriptor past FD_SETSIZE"));
    }
    // `fd_set` is FD_SETSIZE bits, descriptor `d` at bit `d mod 8` of byte `d / 8` on a
    // little-endian machine for both the `long` (glibc) and `int` (Darwin) word arrays.
    let mut set = [0u8; FD_SETSIZE / 8];
    if let Some(byte) = set.get_mut(index / 8) {
        *byte |= 1 << (index % 8);
    }
    let micros = timeout_ns.div_ceil(1_000);
    let mut timeout = TimeVal {
        sec: i64::try_from(micros / 1_000_000).unwrap_or(i64::MAX),
        usec: (micros % 1_000_000).try_into().unwrap_or(0),
    };
    // SAFETY: `set` is a whole `fd_set` and `timeout` a whole `timeval`, both live and writable
    // for the call; the other sets are null, which select(2) allows.
    let ready = unsafe {
        select(
            fd.saturating_add(1),
            set.as_mut_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &raw mut timeout,
        )
    };
    match ready {
        r if r > 0 => Ok(true),
        0 => Ok(false),
        _ => {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                Ok(false)
            } else {
                Err(error)
            }
        }
    }
}

/// The platform's full flush of `fd`: `fdatasync` on Linux, `F_FULLFSYNC` on macOS.
pub fn full_flush(fd: RawFd) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    // SAFETY: `fd` is an open file the caller owns for the call.
    let status = unsafe { fdatasync(fd) };
    #[cfg(target_os = "macos")]
    // SAFETY: `fd` is an open file the caller owns for the call; F_FULLFSYNC takes no argument.
    let status = unsafe { fcntl(fd, F_FULLFSYNC) };
    if status != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// The 1-, 5- and 15-minute load averages, zeros when the system gives none.
pub fn load_average() -> [f64; 3] {
    let mut loads = [0.0f64; 3];
    // SAFETY: `loads` holds three doubles, the count passed.
    let filled = unsafe { getloadavg(loads.as_mut_ptr(), 3) };
    if filled != 3 {
        return [0.0; 3];
    }
    loads
}
