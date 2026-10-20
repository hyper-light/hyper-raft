//! macOS: the kernel's receive timestamp and the host's monotonic clock, which neither std nor
//! tokio exposes.
//!
//! - `SO_TIMESTAMP_MONOTONIC` (<sys/socket.h>): XNU attaches `mach_absolute_time()` when UDP input
//!   queues the datagram on the socket (`bsd/netinet/ip_input.c`, `ip_savecontrol`, called from
//!   `udp_input`), as an `SCM_TIMESTAMP_MONOTONIC` control message carrying a `uint64_t`
//!   (`docs/research/timing.md`). Read with `recvmsg(2)` and the `CMSG_*` walk of cmsg(3), through
//!   libc, whose declarations its `ctest` suite checks against the headers; only
//!   `SCM_TIMESTAMP_MONOTONIC` is declared here, as hyper-timing-trace declares it, because libc
//!   0.2.189 does not, and the stamp test fails on a wrong value.
//! - `mach_absolute_time` scaled by `mach_timebase_info` (<mach/mach_time.h>): the clock the stamps
//!   are on, `CLOCK_UPTIME_RAW`, one clock for every process on the host.
//! - `hw.tbfrequency` (sysctl(3), `CTLTYPE_QUAD`): the kernel's ticks a second, which its stamps
//!   count. XNU's timebase is 10⁹ over it in lowest terms, so a native process's ticks are the
//!   kernel's; a process Rosetta 2 translates is shown a timebase of 1/1 and its own clock in
//!   nanoseconds, while the stamps stay in Apple silicon's 24 MHz ticks, so a stamp is converted
//!   at the kernel's rate, never the process's (docs/research/timing.md, "Rosetta 2 and the
//!   kernel's ticks"). Read with the process's timebase, a translated process's
//!   stamps came out at 3/125 of the clock read around them, 13.25 days before it where measured.

#![allow(unsafe_code)]

use std::io;
use std::mem::{size_of, zeroed};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};
use std::os::fd::RawFd;
use std::ptr;

/// `SCM_TIMESTAMP_MONOTONIC`, 0x04 in XNU's `bsd/sys/socket.h`, which libc 0.2.189 does not
/// declare.
const SCM_TIMESTAMP_MONOTONIC: libc::c_int = 0x04;

/// The control buffer, in 8-byte words for `cmsghdr`'s alignment: `CMSG_SPACE(sizeof(uint64_t))`,
/// 12 bytes of header rounded to 4-byte alignment and 8 of stamp, 20, in three words.
const CONTROL_WORDS: usize = 3;

/// Format: nanoseconds in a second (SI units), the unit `hw.tbfrequency` counts ticks in.
const SECOND_NS: u64 = 1_000_000_000;

/// The host's monotonic clock as nanoseconds, and the kernel's ticks its receive stamps count.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Clock {
    /// `mach_timebase_info`: nanoseconds are this process's ticks × `numer` / `denom`; neither is
    /// zero.
    numer: u64,
    denom: u64,
    /// `hw.tbfrequency`: the kernel's ticks a second, which its stamps count; never zero.
    kernel_hz: u64,
}

impl Clock {
    #[expect(
        deprecated,
        reason = "libc deprecates its Mach declarations in favour of the mach2 crate, which is not \
                  a dependency; the declarations themselves are unchanged in libc"
    )]
    pub(crate) fn new() -> io::Result<Self> {
        let mut info = libc::mach_timebase_info { numer: 0, denom: 0 };
        // SAFETY: `info` is a live, writable `mach_timebase_info` for the call, which writes that
        // one structure (<mach/mach_time.h>).
        let status = unsafe { libc::mach_timebase_info(&raw mut info) };
        if status != libc::KERN_SUCCESS || info.numer == 0 || info.denom == 0 {
            return Err(io::Error::other("mach_timebase_info failed"));
        }
        Ok(Self {
            numer: u64::from(info.numer),
            denom: u64::from(info.denom),
            kernel_hz: kernel_hz()?,
        })
    }

    /// Now, in nanoseconds.
    #[expect(deprecated, reason = "as in `new`")]
    pub(crate) fn now_ns(&self) -> u64 {
        // SAFETY: takes no argument and reads the timebase register (<mach/mach_time.h>).
        let ticks = unsafe { libc::mach_absolute_time() };
        self.ticks_ns(ticks)
    }

    /// One tick in nanoseconds, rounded up: no two readings are closer apart. The larger of this
    /// process's tick, `numer / denom`, and the kernel's, a second over `hw.tbfrequency`: one for a
    /// native process, while a translated one's clock steps at the hardware's tick though its
    /// timebase says 1/1 (41 and 42 ns apart on the M5 Max, 2026-10-20, a million steps).
    pub(crate) fn tick_ns(&self) -> u64 {
        let rounded_up = |numer: u64, denom: u64| {
            numer
                .saturating_add(denom.saturating_sub(1))
                .checked_div(denom)
                .unwrap_or(1)
        };
        rounded_up(self.numer, self.denom).max(rounded_up(SECOND_NS, self.kernel_hz))
    }

    /// `ticks` of this process's × `numer` / `denom`, in 128 bits so it cannot overflow before the
    /// division.
    fn ticks_ns(&self, ticks: u64) -> u64 {
        let ns = u128::from(ticks)
            .checked_mul(u128::from(self.numer))
            .and_then(|scaled| scaled.checked_div(u128::from(self.denom)))
            .unwrap_or(u128::MAX);
        u64::try_from(ns).unwrap_or(u64::MAX)
    }

    /// `ticks` of the kernel's (a receive stamp) as nanoseconds on this clock: × 10⁹ /
    /// `hw.tbfrequency`, in 128 bits. For a native process this is `ticks_ns` to the bit, the
    /// timebase being 10⁹ over the frequency in lowest terms, so both are one fraction rounded down.
    pub(crate) fn kernel_ns(&self, ticks: u64) -> u64 {
        let ns = u128::from(ticks)
            .checked_mul(u128::from(SECOND_NS))
            .and_then(|scaled| scaled.checked_div(u128::from(self.kernel_hz)))
            .unwrap_or(u128::MAX);
        u64::try_from(ns).unwrap_or(u64::MAX)
    }
}

/// The kernel's ticks a second, `hw.tbfrequency` (an 8-byte quad, bsd/kern/kern_mib.c at
/// apple-oss-distributions/xnu xnu-12377.121.6, lines 683-689 and 986); refused when the call is,
/// or it reads zero.
fn kernel_hz() -> io::Result<u64> {
    let mut hz: u64 = 0;
    let mut length = size_of::<u64>();
    // SAFETY: the name is NUL-terminated; the out pointer and its length name one live local
    // `u64`, the sysctl's type; no new value is written.
    let status = unsafe {
        libc::sysctlbyname(
            c"hw.tbfrequency".as_ptr(),
            ptr::addr_of_mut!(hz).cast(),
            &raw mut length,
            ptr::null_mut(),
            0,
        )
    };
    if status != 0 {
        return Err(io::Error::last_os_error());
    }
    if length != size_of::<u64>() || hz == 0 {
        return Err(io::Error::other("hw.tbfrequency read no rate"));
    }
    Ok(hz)
}

/// `MSIZE` (`bsd/arm/param.h`, `bsd/i386/param.h`): an mbuf's own size. A received datagram is
/// queued as its address's mbuf, its control's and its data's, each charged at least this.
const MSIZE: u64 = 256;
/// `M16KCLBYTES` (`bsd/arm/param.h`, `bsd/i386/param.h`): XNU's largest cluster, the "jumbo
/// cluster", the most storage a receiving driver gives a datagram's data in one mbuf.
const M16KCLBYTES: u64 = 16_384;
/// `sb_efficiency` (`bsd/kern/uipc_socket2.c`): a socket buffer's mbuf storage is limited to this
/// many times its byte limit (`sbreserve`: `sb_mbmax = cc * sb_efficiency`).
const SB_EFFICIENCY: u64 = 8;

/// The least XNU may charge one queued datagram against its byte limit, beyond what `FIONREAD`
/// reads: `sbspace` is the smaller of what is left of the byte limit and of the mbuf limit, and a
/// datagram's mbufs are its address's, its control's and its data's, the data's at most a jumbo
/// cluster (hyper-rt's `udp/macos.rs` reads the same account).
pub(crate) fn message_floor() -> u64 {
    MSIZE
        .saturating_mul(3)
        .saturating_add(M16KCLBYTES)
        .checked_div(SB_EFFICIENCY)
        .unwrap_or(u64::MAX)
}

/// The bytes waiting in `fd`'s receive queue as `FIONREAD` counts them: on XNU every queued
/// datagram's, with their addresses and control (`soioctl`: `so_rcv.sb_cc`).
pub(crate) fn queued_bytes(fd: RawFd) -> io::Result<usize> {
    let mut value: libc::c_int = 0;
    // SAFETY: FIONREAD writes one `c_int` through the pointer; `value` is a live local of that size.
    let status = unsafe { libc::ioctl(fd, libc::FIONREAD, ptr::addr_of_mut!(value)) };
    if status != 0 {
        return Err(io::Error::last_os_error());
    }
    usize::try_from(value).map_err(|_| io::Error::other("FIONREAD read back negative"))
}

/// The receive buffer the kernel reports for `fd` (`SO_RCVBUF`).
pub(crate) fn recv_buffer(fd: RawFd) -> io::Result<usize> {
    let mut value: libc::c_int = 0;
    let mut length = libc::socklen_t::try_from(size_of::<libc::c_int>()).unwrap_or(0);
    // SAFETY: `value` and `length` are live locals of the sizes passed; getsockopt writes at most
    // `length` bytes into `value` and the new length into `length`.
    let status = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_RCVBUF,
            ptr::addr_of_mut!(value).cast(),
            &raw mut length,
        )
    };
    if status != 0 {
        return Err(io::Error::last_os_error());
    }
    usize::try_from(value).map_err(|_| io::Error::other("SO_RCVBUF read back negative"))
}

/// Asks the kernel for a receive buffer of `bytes` on `fd` ([`super::set_recv_buffer`]):
/// `SO_RCVBUF`, which macOS refuses past `kern.ipc.maxsockbuf` (`ENOBUFS`), so a refused size is
/// halved and asked again, at most [`super::BUFFER_ASKS`] times. The buffer reported after.
pub(crate) fn set_recv_buffer(fd: RawFd, bytes: usize) -> io::Result<usize> {
    let mut value = libc::c_int::try_from(bytes).unwrap_or(libc::c_int::MAX);
    let length = libc::socklen_t::try_from(size_of::<libc::c_int>()).unwrap_or(0);
    let mut refusal = None;
    for _ in 0..super::BUFFER_ASKS {
        // SAFETY: `value` is a live `c_int` and the length passed is its size; setsockopt only
        // reads it.
        let status = unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_RCVBUF,
                ptr::addr_of!(value).cast(),
                length,
            )
        };
        if status == 0 {
            return recv_buffer(fd);
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::ENOBUFS | libc::EINVAL) => {
                refusal = Some(error);
                value = value.checked_div(2).unwrap_or(0);
                if value == 0 {
                    break;
                }
            }
            _ => return Err(error),
        }
    }
    Err(refusal.unwrap_or_else(|| io::Error::other("no receive buffer was granted")))
}

/// Asks the kernel to stamp each datagram `fd` receives. Whether it agreed.
pub(crate) fn enable_stamps(fd: RawFd) -> bool {
    let on: libc::c_int = 1;
    let length = libc::socklen_t::try_from(size_of::<libc::c_int>()).unwrap_or(0);
    // SAFETY: `on` is a live `c_int` and `length` its size, which bounds what setsockopt reads.
    let status = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_TIMESTAMP_MONOTONIC,
            ptr::addr_of!(on).cast(),
            length,
        )
    };
    status == 0
}

/// One datagram received: who sent it, how long it is, and the kernel's stamp, its
/// `mach_absolute_time` in its own ticks, when there is one.
pub(crate) struct Received {
    pub(crate) from: SocketAddr,
    pub(crate) length: usize,
    pub(crate) stamp: Option<u64>,
}

/// Receives one datagram into `buffer` without blocking (the socket is non-blocking). A datagram
/// from an address that is not an internet one, or one the buffer truncated, is `None`: the
/// protocols above treat it as lost.
pub(crate) fn receive(fd: RawFd, buffer: &mut [u8]) -> io::Result<Option<Received>> {
    // SAFETY: `sockaddr_storage` is plain integers; all zeroes is a valid value.
    let mut name: libc::sockaddr_storage = unsafe { zeroed() };
    let mut control = [0u64; CONTROL_WORDS];
    let mut iov = libc::iovec {
        iov_base: buffer.as_mut_ptr().cast(),
        iov_len: buffer.len(),
    };
    // SAFETY: an all-zero `msghdr` is valid: null pointers with zero lengths.
    let mut message: libc::msghdr = unsafe { zeroed() };
    message.msg_name = ptr::addr_of_mut!(name).cast();
    message.msg_namelen =
        libc::socklen_t::try_from(size_of::<libc::sockaddr_storage>()).unwrap_or(0);
    message.msg_iov = &raw mut iov;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen =
        libc::socklen_t::try_from(size_of::<[u64; CONTROL_WORDS]>()).unwrap_or(0);
    // SAFETY: `message` points at `name`, `iov` (which covers `buffer`) and `control`, all live
    // and writable for the call, each with its length; recvmsg writes within them.
    let read = unsafe { libc::recvmsg(fd, &raw mut message, libc::MSG_DONTWAIT) };
    let Ok(length) = usize::try_from(read) else {
        return Err(io::Error::last_os_error());
    };
    if message.msg_flags & libc::MSG_TRUNC != 0 {
        return Ok(None);
    }
    let Some(from) = read_name(&name) else {
        return Ok(None);
    };
    Ok(Some(Received {
        from,
        length,
        stamp: stamp_of(&message),
    }))
}

/// The address the kernel wrote into `storage`, if it is an internet address.
fn read_name(storage: &libc::sockaddr_storage) -> Option<SocketAddr> {
    match libc::c_int::from(storage.ss_family) {
        libc::AF_INET => {
            // SAFETY: the family says the kernel wrote a `sockaddr_in`, which the storage holds.
            let name = unsafe { &*ptr::from_ref(storage).cast::<libc::sockaddr_in>() };
            Some(SocketAddr::V4(SocketAddrV4::new(
                Ipv4Addr::from(name.sin_addr.s_addr.to_ne_bytes()),
                u16::from_be(name.sin_port),
            )))
        }
        libc::AF_INET6 => {
            // SAFETY: as above, for `sockaddr_in6`.
            let name = unsafe { &*ptr::from_ref(storage).cast::<libc::sockaddr_in6>() };
            Some(SocketAddr::V6(SocketAddrV6::new(
                Ipv6Addr::from(name.sin6_addr.s6_addr),
                u16::from_be(name.sin6_port),
                name.sin6_flowinfo,
                name.sin6_scope_id,
            )))
        }
        _ => None,
    }
}

/// The `SCM_TIMESTAMP_MONOTONIC` stamp among the control messages recvmsg left in `message`;
/// `None` when the kernel attached none or truncated it.
fn stamp_of(message: &libc::msghdr) -> Option<u64> {
    let whole = usize::try_from(
        // SAFETY: `CMSG_LEN` is arithmetic on its argument and reads no memory.
        unsafe { libc::CMSG_LEN(u32::try_from(size_of::<u64>()).unwrap_or(u32::MAX)) },
    )
    .ok()?;
    // SAFETY: `message` was filled by recvmsg, whose control buffer and length are its own;
    // CMSG_FIRSTHDR and CMSG_NXTHDR return headers that lie whole within `msg_controllen`, or null
    // (cmsg(3)), and a stamp message whose `cmsg_len` covers the stamp has it at CMSG_DATA, read
    // unaligned.
    unsafe {
        let mut at = libc::CMSG_FIRSTHDR(message);
        while let Some(header) = at.as_ref() {
            if header.cmsg_level == libc::SOL_SOCKET
                && header.cmsg_type == SCM_TIMESTAMP_MONOTONIC
                && usize::try_from(header.cmsg_len).is_ok_and(|length| length >= whole)
            {
                return Some(ptr::read_unaligned(libc::CMSG_DATA(at).cast::<u64>()));
            }
            at = libc::CMSG_NXTHDR(message, at);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::Clock;

    /// Apple silicon's kernel rate, 24 MHz, which a native process's timebase (125/3) describes
    /// and a translated one's (1/1) does not (both read on the M5 Max, 2026-10-20).
    const KERNEL_HZ: u64 = 24_000_000;

    fn clock(numer: u64, denom: u64, kernel_hz: u64) -> Clock {
        Clock {
            numer,
            denom,
            kernel_hz,
        }
    }

    /// Stamps the sweeps convert: the edges, a tick's neighbours, the M5 Max's uptime when the
    /// stamp was measured in its ticks (28,155,671,796,901), and multiples of 2⁶² − 57 wrapped
    /// over the whole range.
    fn stamps() -> impl Iterator<Item = u64> {
        [0, 1, 2, 3, 4, KERNEL_HZ, 28_155_671_796_901, u64::MAX]
            .into_iter()
            .chain((1..4_000u64).map(|i| i.wrapping_mul(4_611_686_018_427_387_847)))
    }

    /// A native process converts a stamp as it did before the kernel's rate was read, to the
    /// bit: its timebase is the kernel's, so the two are one fraction (125/3 against 24 MHz on
    /// Apple silicon, 1/1 against 10⁹ on Intel), and its tick is the one it was.
    #[test]
    fn a_native_process_converts_a_stamp_as_before() {
        for (native, tick) in [
            (clock(125, 3, KERNEL_HZ), 42),
            (clock(1, 1, 1_000_000_000), 1),
        ] {
            for stamp in stamps() {
                assert_eq!(
                    native.kernel_ns(stamp),
                    native.ticks_ns(stamp),
                    "{native:?}"
                );
            }
            assert_eq!(native.tick_ns(), tick);
        }
    }

    /// A translated process, shown a timebase of 1/1 against the kernel's 24 MHz, converts a
    /// stamp at 125/3: the stamp measured between the process's readings 1,173,152,991,501,583
    /// and 1,173,152,991,549,999 ns lands between them, where its timebase put it at 3/125 of
    /// them. Its clock steps at the kernel's tick.
    #[test]
    fn a_translated_process_converts_a_stamp_at_the_kernels_rate() {
        let translated = clock(1, 1, KERNEL_HZ);
        let stamp = translated.kernel_ns(28_155_671_796_901);
        assert!(
            (1_173_152_991_501_583..=1_173_152_991_549_999).contains(&stamp),
            "{stamp}"
        );
        assert_eq!(translated.ticks_ns(28_155_671_796_901), 28_155_671_796_901);
        assert_eq!(translated.kernel_ns(3), 125);
        assert_eq!(translated.kernel_ns(KERNEL_HZ), 1_000_000_000);
        assert_eq!(
            translated.kernel_ns(u64::MAX),
            u64::MAX,
            "saturates past u64"
        );
        assert_eq!(translated.tick_ns(), 42);
    }

    /// The same instant read by a native process and a translated one: a stamp converts to the
    /// same nanoseconds in both.
    #[test]
    fn a_translated_process_and_a_native_one_read_one_stamp() {
        let native = clock(125, 3, KERNEL_HZ);
        let translated = clock(1, 1, KERNEL_HZ);
        for stamp in stamps() {
            assert_eq!(translated.kernel_ns(stamp), native.kernel_ns(stamp));
        }
    }
}
