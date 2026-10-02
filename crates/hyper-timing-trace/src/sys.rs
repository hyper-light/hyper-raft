//! The operating-system interface the recorder needs and std does not give: a monotonic clock as
//! nanoseconds two processes share, the kernel's receive timestamp of a datagram, a socket wait with
//! a sub-millisecond timeout, a positional write with the platform's full flush, and the load
//! average.
//!
//! Every call goes through a binding whose types are checked against the platform's headers: rustix
//! where it has a safe call, libc (whose `ctest` suite checks its declarations against each
//! platform's headers) for the rest. Nothing is declared by hand but `SCM_TIMESTAMP_MONOTONIC`,
//! which libc 0.2.189 does not declare; the receive-timestamp test reads a stamp through it, so a
//! wrong value fails that test.
//!
//! - Linux: `clock_gettime(CLOCK_MONOTONIC)` (rustix; clock_gettime(2)). `SO_TIMESTAMPNS`, whose
//!   `SCM_TIMESTAMPNS` control message carries a `struct timespec` in `CLOCK_REALTIME` taken when
//!   the kernel received the datagram (socket(7)): `setsockopt(2)` and `recvmsg(2)` with the
//!   `CMSG_*` walk of cmsg(3) (libc; rustix's `recvmsg` drops control messages it does not know,
//!   timestamps among them, and has no timestamp option). `ppoll(2)` (rustix's `poll`), whose
//!   timeout is a `struct timespec`: in `fs/select.c` select and ppoll both end their wait in
//!   `poll_schedule_timeout` with the slack `select_estimate_accuracy` gives, so the wait ends no
//!   earlier than asked and up to the thread's timer slack late (PR_SET_TIMERSLACK(2const)), as
//!   select's did, without `FD_SETSIZE`. `fdatasync(2)` (rustix). The load average from
//!   `sysinfo(2)` (rustix), the kernel's `avenrun` in `SI_LOAD_SHIFT` (16) fixed point, the
//!   numbers `/proc/loadavg` and glibc's `getloadavg(3)` print to two decimals.
//! - macOS: `mach_absolute_time` scaled by `mach_timebase_info` (libc; <mach/mach_time.h>).
//!   `SO_TIMESTAMP_MONOTONIC` (<sys/socket.h>), whose `SCM_TIMESTAMP_MONOTONIC` (0x04) control
//!   message carries a `uint64_t` the kernel takes with `mach_absolute_time()` as UDP input appends
//!   the datagram to the socket (XNU `bsd/netinet/ip_input.c`, `ip_savecontrol`, called from
//!   `udp_input` in `udp_usrreq.c`): `setsockopt(2)`, `recvmsg(2)` and the `CMSG_*` walk (libc).
//!   `select(2)` (rustix), which on macOS has no `ppoll` and whose `poll(2)` takes whole
//!   milliseconds. `fcntl(F_FULLFSYNC)` (rustix's `fcntl_fullfsync`), which asks the drive to
//!   flush to the media. `getloadavg(3)` (libc).
//!
//! Elsewhere (Windows, open item 5 of `docs/timing.md` §3) the recorder does not run: every call
//! but the load average refuses with [`io::ErrorKind::Unsupported`], and `analyse` still works.
#![allow(unsafe_code)]

pub(crate) use os::{
    Clock, KERNEL_CLOCK, SOCKET_WAIT, enable_receive_timestamps, load_average, preferred_block,
    recv_stamped, wait_readable, write_and_flush,
};

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod os {
    use std::fs::File;
    use std::io;
    use std::net::UdpSocket;
    use std::os::fd::AsRawFd;

    /// Nanoseconds in a second, the unit of `struct timespec`'s `tv_nsec` (POSIX <time.h>).
    #[cfg(target_os = "linux")]
    const NANOS_PER_SECOND: u64 = 1_000_000_000;

    /// Nanoseconds of a `struct timespec` as one count, `None` when negative or past `u64`.
    #[cfg(target_os = "linux")]
    fn timespec_ns(sec: i64, nsec: i64) -> Option<u64> {
        u64::try_from(sec)
            .ok()?
            .checked_mul(NANOS_PER_SECOND)?
            .checked_add(u64::try_from(nsec).ok()?)
    }

    /// The socket option that asks for a receive timestamp on every datagram: `SO_TIMESTAMPNS`
    /// (socket(7)).
    #[cfg(target_os = "linux")]
    const SO_STAMP: libc::c_int = libc::SO_TIMESTAMPNS;
    /// The control message type that carries it: `SCM_TIMESTAMPNS` (socket(7)).
    #[cfg(target_os = "linux")]
    const SCM_STAMP: libc::c_int = libc::SCM_TIMESTAMPNS;
    /// The stamp `SCM_TIMESTAMPNS` carries.
    #[cfg(target_os = "linux")]
    type Stamp = libc::timespec;

    /// `SO_TIMESTAMP_MONOTONIC` (<sys/socket.h>).
    #[cfg(target_os = "macos")]
    const SO_STAMP: libc::c_int = libc::SO_TIMESTAMP_MONOTONIC;
    /// `SCM_TIMESTAMP_MONOTONIC`, 0x04 in <sys/socket.h> (XNU `bsd/sys/socket.h`), which libc
    /// 0.2.189 does not declare.
    #[cfg(target_os = "macos")]
    const SCM_STAMP: libc::c_int = 0x04;
    /// The stamp `SCM_TIMESTAMP_MONOTONIC` carries: `mach_absolute_time()` ticks.
    #[cfg(target_os = "macos")]
    type Stamp = u64;

    /// `CMSG_SPACE(sizeof(Stamp))` (cmsg(3)): the one control message the socket is asked for.
    const CONTROL_BYTES: usize = {
        // The payload is 8 or 16 bytes, which `c_uint` holds.
        let payload = size_of::<Stamp>() as libc::c_uint;
        // SAFETY: `CMSG_SPACE` is arithmetic on its argument and reads no memory; libc declares
        // every `CMSG_*` macro `unsafe`.
        let space = unsafe { libc::CMSG_SPACE(payload) };
        space as usize
    };

    /// The control-message buffer: `CONTROL_BYTES` aligned as `struct cmsghdr`, which
    /// `CMSG_FIRSTHDR` and `CMSG_NXTHDR` assume of `msg_control` (cmsg(3)).
    #[repr(C)]
    union Control {
        _align: libc::cmsghdr,
        _bytes: [u8; CONTROL_BYTES],
    }

    /// The monotonic clock both processes read, as nanoseconds.
    pub(crate) struct Clock {
        /// `mach_timebase_info`'s ratio: nanoseconds are ticks × `numer` / `denom`.
        #[cfg(target_os = "macos")]
        numer: u64,
        /// See `numer`; never zero.
        #[cfg(target_os = "macos")]
        denom: u64,
    }

    impl Clock {
        /// The clock, with its scale on macOS.
        #[cfg(target_os = "linux")]
        pub(crate) fn new() -> io::Result<Self> {
            Ok(Self {})
        }

        /// The clock, with its scale on macOS.
        #[cfg(target_os = "macos")]
        #[expect(
            deprecated,
            reason = "libc deprecates its Mach declarations in favour of the mach2 crate, which is \
                      not a dependency; the declarations themselves are unchanged in libc"
        )]
        pub(crate) fn new() -> io::Result<Self> {
            let mut info = libc::mach_timebase_info { numer: 0, denom: 0 };
            // SAFETY: `info` is a live, writable `mach_timebase_info` for the call, which writes
            // that one structure (<mach/mach_time.h>).
            let status = unsafe { libc::mach_timebase_info(&raw mut info) };
            if status != libc::KERN_SUCCESS || info.denom == 0 {
                return Err(io::Error::other(format!(
                    "mach_timebase_info failed: {status}"
                )));
            }
            Ok(Self {
                numer: u64::from(info.numer),
                denom: u64::from(info.denom),
            })
        }

        /// Now, nanoseconds on the monotonic clock.
        #[cfg(target_os = "linux")]
        pub(crate) fn now(&self) -> io::Result<u64> {
            use rustix::time::{ClockId, DynamicClockId, clock_gettime_dynamic};
            // The fallible form: rustix's `clock_gettime` asserts on a failure instead.
            let now = clock_gettime_dynamic(DynamicClockId::Known(ClockId::Monotonic))?;
            timespec_ns(now.tv_sec, now.tv_nsec)
                .ok_or_else(|| io::Error::other("CLOCK_MONOTONIC out of range"))
        }

        /// Now, nanoseconds on the monotonic clock.
        #[cfg(target_os = "macos")]
        #[expect(deprecated, reason = "as in `new`")]
        pub(crate) fn now(&self) -> io::Result<u64> {
            // SAFETY: takes no argument and reads the timebase register (<mach/mach_time.h>).
            let ticks = unsafe { libc::mach_absolute_time() };
            Ok(self.ticks_ns(ticks))
        }

        /// A kernel timestamp as nanoseconds: on macOS `mach_absolute_time` ticks on the
        /// monotonic clock; on Linux already nanoseconds of `CLOCK_REALTIME`.
        pub(crate) fn kernel_ns(&self, raw: u64) -> u64 {
            #[cfg(target_os = "macos")]
            {
                self.ticks_ns(raw)
            }
            #[cfg(target_os = "linux")]
            {
                raw
            }
        }

        /// `ticks` × `numer` / `denom`, in 128 bits so it cannot overflow before the division.
        #[cfg(target_os = "macos")]
        fn ticks_ns(&self, ticks: u64) -> u64 {
            let ns = u128::from(ticks)
                .checked_mul(u128::from(self.numer))
                .and_then(|scaled| scaled.checked_div(u128::from(self.denom)))
                .unwrap_or(u128::MAX);
            u64::try_from(ns).unwrap_or(u64::MAX)
        }
    }

    /// Which clock [`recv_stamped`]'s kernel timestamps are on.
    pub(crate) const KERNEL_CLOCK: &str = if cfg!(target_os = "macos") {
        "monotonic (mach_absolute_time)"
    } else {
        "realtime (CLOCK_REALTIME)"
    };

    /// The call [`wait_readable`] waits in.
    pub(crate) const SOCKET_WAIT: &str = if cfg!(target_os = "macos") {
        "select"
    } else {
        "ppoll"
    };

    /// Asks the kernel to stamp each datagram `socket` receives.
    pub(crate) fn enable_receive_timestamps(socket: &UdpSocket) -> io::Result<()> {
        let on: libc::c_int = 1;
        let len = libc::socklen_t::try_from(size_of::<libc::c_int>()).map_err(io::Error::other)?;
        // SAFETY: the descriptor is borrowed from `socket`, open for the call; `on` is a live
        // `int` and `len` its size, which bounds what the call reads.
        let status = unsafe {
            libc::setsockopt(
                socket.as_raw_fd(),
                libc::SOL_SOCKET,
                SO_STAMP,
                (&raw const on).cast(),
                len,
            )
        };
        if status != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Receives one datagram into `buf` without blocking past what the socket's mode says: its
    /// length and the kernel's receive timestamp, raw, when the kernel attached one.
    pub(crate) fn recv_stamped(
        socket: &UdpSocket,
        buf: &mut [u8],
    ) -> io::Result<(usize, Option<u64>)> {
        let mut control = Control {
            _bytes: [0; CONTROL_BYTES],
        };
        let mut iov = libc::iovec {
            iov_base: buf.as_mut_ptr().cast(),
            iov_len: buf.len(),
        };
        #[cfg(target_os = "linux")]
        let controllen = CONTROL_BYTES;
        #[cfg(target_os = "macos")]
        let controllen = libc::socklen_t::try_from(CONTROL_BYTES).map_err(io::Error::other)?;
        let mut msg = libc::msghdr {
            msg_name: std::ptr::null_mut(),
            msg_namelen: 0,
            msg_iov: &raw mut iov,
            msg_iovlen: 1,
            msg_control: (&raw mut control).cast(),
            msg_controllen: controllen,
            msg_flags: 0,
        };
        // SAFETY: the descriptor is borrowed from `socket`, open for the call. `msg` points at
        // `iov`, which covers `buf`, and at `control`, `msg_controllen` bytes aligned as
        // `cmsghdr`; all are live and writable for the call. No name buffer is given.
        let read = unsafe { libc::recvmsg(socket.as_raw_fd(), &raw mut msg, 0) };
        let Ok(read) = usize::try_from(read) else {
            return Err(io::Error::last_os_error());
        };
        Ok((read, stamp_of(&msg)))
    }

    /// The receive timestamp among the control messages `recvmsg` left in `msg`.
    fn stamp_of(msg: &libc::msghdr) -> Option<u64> {
        let base = msg.msg_control as usize;
        // SAFETY: `msg` is the header `recvmsg` filled: `msg_control` is the live `Control` buffer
        // and `msg_controllen` the bytes the kernel wrote there. `CMSG_FIRSTHDR` returns the buffer
        // when those bytes hold a whole header, and null otherwise (cmsg(3)).
        let mut header = unsafe { libc::CMSG_FIRSTHDR(msg) };
        loop {
            // SAFETY: `header` is null or a header `CMSG_FIRSTHDR` or `CMSG_NXTHDR` returned, which
            // lies whole in the bytes the kernel wrote (cmsg(3)).
            let cmsg = unsafe { header.as_ref() }?;
            if cmsg.cmsg_level == libc::SOL_SOCKET && cmsg.cmsg_type == SCM_STAMP {
                return stamp_in(cmsg, header, base);
            }
            // SAFETY: `msg` is as above and `header` a non-null header in its buffer, as
            // `CMSG_NXTHDR` requires; it returns the next header that lies whole within
            // `msg_controllen` bytes, or null (cmsg(3)).
            header = unsafe { libc::CMSG_NXTHDR(msg, header) };
        }
    }

    /// The stamp `cmsg`, at `header` in the control buffer at `base`, carries; `None` when the
    /// kernel truncated it (`MSG_CTRUNC`), which leaves a `cmsg_len` short of the stamp.
    fn stamp_in(cmsg: &libc::cmsghdr, header: *mut libc::cmsghdr, base: usize) -> Option<u64> {
        #[cfg(target_os = "linux")]
        let len: usize = cmsg.cmsg_len;
        #[cfg(target_os = "macos")]
        let len = usize::try_from(cmsg.cmsg_len).ok()?;
        // SAFETY: `CMSG_DATA` is arithmetic on the header's address, which lies in the buffer.
        let data = unsafe { libc::CMSG_DATA(header) };
        let end = (data as usize)
            .checked_sub(base)?
            .checked_add(size_of::<Stamp>())?;
        if len < stamp_len()? || end > CONTROL_BYTES {
            return None;
        }
        // SAFETY: `data` .. `data + size_of::<Stamp>()` lies in the buffer (checked above) and the
        // kernel wrote the whole stamp there (`cmsg_len` covers it); `CMSG_DATA` need not be
        // aligned for `Stamp`, so the read is unaligned.
        let stamp = unsafe { std::ptr::read_unaligned(data.cast::<Stamp>()) };
        #[cfg(target_os = "linux")]
        return timespec_ns(stamp.tv_sec, stamp.tv_nsec);
        #[cfg(target_os = "macos")]
        return Some(stamp);
    }

    /// `CMSG_LEN(sizeof(Stamp))`: the `cmsg_len` of a whole stamp message.
    fn stamp_len() -> Option<usize> {
        let payload = libc::c_uint::try_from(size_of::<Stamp>()).ok()?;
        // SAFETY: `CMSG_LEN` is arithmetic on its argument and reads no memory.
        let len = unsafe { libc::CMSG_LEN(payload) };
        usize::try_from(len).ok()
    }

    /// Waits until `socket` is readable or `timeout_ns` passes: `true` when readable. An interrupted
    /// wait ends as a timeout.
    #[cfg(target_os = "linux")]
    pub(crate) fn wait_readable(socket: &UdpSocket, timeout_ns: u64) -> io::Result<bool> {
        use rustix::event::{PollFd, PollFlags, Timespec, poll};
        let timeout = Timespec {
            tv_sec: i64::try_from(timeout_ns / NANOS_PER_SECOND).map_err(io::Error::other)?,
            tv_nsec: i64::try_from(timeout_ns % NANOS_PER_SECOND).map_err(io::Error::other)?,
        };
        let mut fds = [PollFd::new(socket, PollFlags::IN)];
        match poll(&mut fds, Some(&timeout)) {
            Ok(ready) => Ok(ready > 0),
            Err(rustix::io::Errno::INTR) => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    /// The descriptors a stack `fd_set` holds: `FD_SETSIZE` (<sys/select.h>).
    #[cfg(target_os = "macos")]
    const FD_SETSIZE: usize = libc::FD_SETSIZE;
    /// The words of a `FD_SETSIZE`-bit set.
    #[cfg(target_os = "macos")]
    const FD_SET_WORDS: usize = FD_SETSIZE / (8 * size_of::<rustix::event::FdSetElement>());

    /// Waits until `socket` is readable or `timeout_ns` passes: `true` when readable. An interrupted
    /// wait ends as a timeout.
    #[cfg(target_os = "macos")]
    pub(crate) fn wait_readable(socket: &UdpSocket, timeout_ns: u64) -> io::Result<bool> {
        use rustix::event::{FdSetElement, Timespec, fd_set_insert, select};
        let fd = socket.as_raw_fd();
        // The set is a fixed `FD_SETSIZE` bits on the stack; rustix indexes it by `fd` and asserts
        // it holds `fd + 1` bits, so a descriptor past it is refused here, before either.
        if usize::try_from(fd).map_err(io::Error::other)? >= FD_SETSIZE {
            return Err(io::Error::other("descriptor past FD_SETSIZE"));
        }
        let mut set = [FdSetElement::default(); FD_SET_WORDS];
        fd_set_insert(&mut set, fd);
        // select's timeout is a `struct timeval`: whole microseconds, rounded up so the wait is
        // never shorter than asked (rustix rounds a part microsecond up too; whole ones pass
        // exactly, and `tv_usec` stays below a second as select(2) requires).
        let micros = timeout_ns.div_ceil(1_000);
        let timeout = Timespec {
            tv_sec: i64::try_from(micros / 1_000_000).map_err(io::Error::other)?,
            tv_nsec: i64::try_from(micros % 1_000_000 * 1_000).map_err(io::Error::other)?,
        };
        let nfds = fd
            .checked_add(1)
            .ok_or_else(|| io::Error::other("descriptor"))?;
        // SAFETY: the one descriptor in the set is borrowed from `socket`, open for the call; the
        // set holds `nfds` bits (checked above).
        match unsafe { select(nfds, Some(&mut set), None, None, Some(&timeout)) } {
            Ok(ready) => Ok(ready > 0),
            Err(rustix::io::Errno::INTR) => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    /// The file's preferred I/O size (`st_blksize`, stat(2)).
    pub(crate) fn preferred_block(file: &File) -> io::Result<usize> {
        use std::os::unix::fs::MetadataExt;
        usize::try_from(file.metadata()?.blksize()).map_err(io::Error::other)
    }

    /// Writes `block` at the start of `file` and flushes it with the platform's full flush:
    /// `fdatasync` on Linux, `F_FULLFSYNC` on macOS.
    pub(crate) fn write_and_flush(file: &File, block: &[u8]) -> io::Result<()> {
        use std::os::unix::fs::FileExt;
        file.write_all_at(block, 0)?;
        #[cfg(target_os = "linux")]
        rustix::fs::fdatasync(file)?;
        #[cfg(target_os = "macos")]
        rustix::fs::fcntl_fullfsync(file)?;
        Ok(())
    }

    /// The 1-, 5- and 15-minute load averages: `sysinfo(2)`'s `loads`, fixed point with
    /// `SI_LOAD_SHIFT` (16) fractional bits (linux/kernel.h).
    #[cfg(target_os = "linux")]
    pub(crate) fn load_average() -> [f64; 3] {
        /// `1 << SI_LOAD_SHIFT`.
        const SI_LOAD_SCALE: f64 = 65_536.0;
        // sysinfo(2) fails only with EFAULT, which rustix's own buffer rules out.
        let loads = rustix::system::sysinfo().loads;
        loads.map(|load| load as f64 / SI_LOAD_SCALE)
    }

    /// The 1-, 5- and 15-minute load averages, zeros when the system gives none.
    #[cfg(target_os = "macos")]
    pub(crate) fn load_average() -> [f64; 3] {
        let mut loads = [0.0f64; 3];
        // SAFETY: `loads` holds three doubles, the count passed, which bounds what the call writes.
        let filled = unsafe { libc::getloadavg(loads.as_mut_ptr(), 3) };
        if filled != 3 {
            return [0.0; 3];
        }
        loads
    }
}

/// The recorder on a platform it does not run on: each call refuses.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod os {
    use std::fs::File;
    use std::io;
    use std::net::UdpSocket;

    /// Why every call here refuses.
    const REFUSAL: &str = "the trace recorder runs on Linux and macOS (docs/timing.md §3, item 5)";

    fn refused() -> io::Error {
        io::Error::new(io::ErrorKind::Unsupported, REFUSAL)
    }

    /// The monotonic clock both processes read; never made here.
    pub(crate) struct Clock;

    impl Clock {
        /// Refuses.
        pub(crate) fn new() -> io::Result<Self> {
            Err(refused())
        }
        /// Refuses.
        pub(crate) fn now(&self) -> io::Result<u64> {
            Err(refused())
        }
        /// The raw value: no kernel stamps here.
        pub(crate) fn kernel_ns(&self, raw: u64) -> u64 {
            raw
        }
    }

    /// No kernel timestamps here.
    pub(crate) const KERNEL_CLOCK: &str = "none";
    /// No socket wait here.
    pub(crate) const SOCKET_WAIT: &str = "none";

    /// Refuses.
    pub(crate) fn enable_receive_timestamps(_socket: &UdpSocket) -> io::Result<()> {
        Err(refused())
    }

    /// Refuses.
    pub(crate) fn recv_stamped(
        _socket: &UdpSocket,
        _buf: &mut [u8],
    ) -> io::Result<(usize, Option<u64>)> {
        Err(refused())
    }

    /// Refuses.
    pub(crate) fn wait_readable(_socket: &UdpSocket, _timeout_ns: u64) -> io::Result<bool> {
        Err(refused())
    }

    /// Refuses.
    pub(crate) fn preferred_block(_file: &File) -> io::Result<usize> {
        Err(refused())
    }

    /// Refuses.
    pub(crate) fn write_and_flush(_file: &File, _block: &[u8]) -> io::Result<()> {
        Err(refused())
    }

    /// Zeros: the system gives none here.
    pub(crate) fn load_average() -> [f64; 3] {
        [0.0; 3]
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use std::net::UdpSocket;

    use super::*;

    /// Datagrams the stamp test sends, so consecutive stamps can be compared.
    const DATAGRAMS: u64 = 16;
    /// The wait for a datagram already sent on loopback: a second, ten thousand times the 100 µs
    /// loopback delay the traces measured (`docs/benchmarks.md`, "Heartbeat traces").
    const WAIT_NS: u64 = 1_000_000_000;

    /// A receiver with kernel stamps on, and a sender connected to it.
    fn pair() -> (UdpSocket, UdpSocket) {
        let receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
        enable_receive_timestamps(&receiver).unwrap();
        let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
        sender.connect(receiver.local_addr().unwrap()).unwrap();
        (receiver, sender)
    }

    /// Waits for a datagram sent on loopback and reads it with its stamp.
    fn recv(socket: &UdpSocket, buf: &mut [u8]) -> (usize, Option<u64>) {
        assert!(wait_readable(socket, WAIT_NS).unwrap(), "no datagram");
        recv_stamped(socket, buf).unwrap()
    }

    /// Now on the clock the kernel's stamps are on (`KERNEL_CLOCK`): the monotonic clock on macOS,
    /// `CLOCK_REALTIME` on Linux.
    fn kernel_clock_now(clock: &Clock) -> u64 {
        if cfg!(target_os = "macos") {
            clock.now().unwrap()
        } else {
            let since = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap();
            u64::try_from(since.as_nanos()).unwrap()
        }
    }

    /// Each datagram's kernel stamp is read, on the clock `KERNEL_CLOCK` names: no earlier than
    /// the send began, no later than the read returned, and no earlier than the stamp of the
    /// datagram before it.
    #[test]
    fn a_datagram_carries_its_kernel_receive_stamp() {
        let clock = Clock::new().unwrap();
        let (receiver, sender) = pair();
        let mut buf = [0u8; 64];
        let mut previous = 0;
        for i in 0..DATAGRAMS {
            let before = kernel_clock_now(&clock);
            sender.send(&i.to_le_bytes()).unwrap();
            let (read, raw) = recv(&receiver, &mut buf);
            let after = kernel_clock_now(&clock);
            assert_eq!(read, 8);
            assert_eq!(buf[..8], i.to_le_bytes());
            let stamp = clock.kernel_ns(raw.expect("the kernel stamped the datagram"));
            assert!(
                before <= stamp && stamp <= after,
                "stamp {stamp} outside [{before}, {after}]"
            );
            assert!(stamp >= previous, "stamp {stamp} before {previous}");
            previous = stamp;
        }
    }

    /// A socket the timestamp option was not set on gives its datagrams no stamp.
    #[test]
    fn an_unstamped_socket_reads_no_stamp() {
        let receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
        let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
        sender.connect(receiver.local_addr().unwrap()).unwrap();
        sender.send(b"beat").unwrap();
        let mut buf = [0u8; 64];
        assert_eq!(recv(&receiver, &mut buf), (4, None));
    }

    /// A wait on a socket with nothing to read ends on its timeout, no earlier than asked (the
    /// recorder's lateness is the time past it); one with a datagram waiting ends at once.
    #[test]
    fn a_wait_ends_no_earlier_than_asked() {
        let clock = Clock::new().unwrap();
        let (receiver, sender) = pair();
        // Asked durations from the recorder's sweep (main.rs, `SWEEP_NS`): its least, Linux's
        // default timer slack, and a millisecond tick past it.
        for asked in [1_000, 50_000, 2_000_000] {
            let began = clock.now().unwrap();
            assert!(!wait_readable(&receiver, asked).unwrap());
            let waited = clock.now().unwrap() - began;
            assert!(waited >= asked, "asked {asked} ns, woke after {waited} ns");
        }
        sender.send(b"beat").unwrap();
        assert!(wait_readable(&receiver, WAIT_NS).unwrap());
    }

    /// The macOS clock is `CLOCK_UPTIME_RAW`, which Apple documents as `mach_absolute_time()`
    /// scaled by the timebase (clock_gettime(3)): a reading lies between two of it, so the
    /// timebase read through libc is the right ratio.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_macos_clock_is_uptime_raw() {
        let clock = Clock::new().unwrap();
        let uptime = || {
            let mut now = libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            // SAFETY: `now` is a live, writable `timespec` for the call.
            let status = unsafe { libc::clock_gettime(libc::CLOCK_UPTIME_RAW, &raw mut now) };
            assert_eq!(status, 0);
            u64::try_from(now.tv_sec).unwrap() * 1_000_000_000 + u64::try_from(now.tv_nsec).unwrap()
        };
        let before = uptime();
        let now = clock.now().unwrap();
        let after = uptime();
        assert!(
            before <= now && now <= after,
            "{now} outside [{before}, {after}]"
        );
    }

    /// A block written and fully flushed is in the file, read back through another handle.
    #[test]
    fn a_flushed_block_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("flush.log");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        let block = vec![0xa5; preferred_block(&file).unwrap()];
        assert!(!block.is_empty());
        write_and_flush(&file, &block).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), block);
    }

    /// The load averages are finite and not negative.
    #[test]
    fn the_load_average_is_a_load() {
        assert!(
            load_average()
                .iter()
                .all(|load| load.is_finite() && *load >= 0.0)
        );
    }
}

#[cfg(all(test, not(any(target_os = "linux", target_os = "macos"))))]
mod tests {
    use std::fs::File;
    use std::io;
    use std::net::UdpSocket;

    use super::*;

    /// Off Linux and macOS each recorder call refuses as unsupported, and the load average is
    /// zeros.
    #[test]
    fn the_recorder_refuses_elsewhere() {
        let unsupported = |error: io::Error| error.kind() == io::ErrorKind::Unsupported;
        assert!(Clock::new().map(|_| ()).is_err_and(unsupported));
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        assert!(enable_receive_timestamps(&socket).is_err_and(unsupported));
        assert!(wait_readable(&socket, 1_000).is_err_and(unsupported));
        assert!(recv_stamped(&socket, &mut [0; 8]).is_err_and(unsupported));
        let dir = tempfile::tempdir().unwrap();
        let file = File::create(dir.path().join("flush.log")).unwrap();
        assert!(write_and_flush(&file, &[0; 8]).is_err_and(unsupported));
        assert!(preferred_block(&file).is_err_and(unsupported));
        assert_eq!(load_average(), [0.0; 3]);
    }
}
