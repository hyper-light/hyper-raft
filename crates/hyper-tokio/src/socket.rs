//! One UDP socket on tokio's reactor, batched: the datagrams to send wait in a bounded outbox and
//! leave in as few system calls as the platform allows; received ones are taken a batch at a
//! time.
//!
//! - Linux: `sendmmsg(2)` and `recvmmsg(2)`, each message a segmented send (`UDP_SEGMENT`) or a
//!   coalesced receive (`UDP_GRO`) where the kernel has them ([`crate::sys`]).
//! - macOS and Windows: one datagram a system call through tokio (macOS through `recvmsg(2)` when
//!   the socket is stamped). macOS has no public batched UDP call; Windows' segmentation and
//!   coalescing (`UDP_SEND_MSG_SIZE`, `UDP_RECV_MAX_COALESCED_SIZE`) are not used yet
//!   (docs/transport.md §4b).
//!
//! A socket that is stamped hands each datagram over with when it arrived on the host's
//! monotonic clock: the kernel's receive stamp on Linux and macOS, the read's time on Windows
//! ([`crate::clock`]).

use std::io::{self, ErrorKind};
use std::net::{Ipv4Addr, SocketAddr};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::task::{Context, Poll};

use tokio::net::UdpSocket;

use crate::Error;
use crate::clock::{Arrival, Clock};
#[cfg(target_os = "linux")]
use crate::sys::linux;
#[cfg(target_os = "macos")]
use crate::sys::macos;

/// The receive buffer: `GRO_LEGACY_MAX_SIZE` (`include/linux/netdevice.h`, 65,536 bytes), the
/// most the kernel coalesces into one UDP receive, which also holds the largest single UDP
/// payload (65,527 bytes over IPv6, 65,507 over IPv4; RFC 768, RFC 8200). A datagram never
/// arrives truncated.
pub const RECEIVE_BYTES: usize = 1 << 16;
/// `UIO_MAXIOV` (`sendmmsg(2)`, `recvmmsg(2)`): the most messages one call takes; a larger batch
/// would be cut by the kernel.
pub const MAX_BATCH: usize = 1_024;

/// How a socket batches; the owner's configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Io {
    /// The most datagrams one system call carries, either way, and so the outbox's bound: 1 to
    /// [`MAX_BATCH`]. Receiving holds this many buffers of [`RECEIVE_BYTES`] on Linux and one
    /// elsewhere; sending holds this many datagrams.
    pub batch: usize,
}

/// What a socket has done.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IoStats {
    /// Datagrams handed to the kernel.
    pub sent: u64,
    /// System calls that sent them.
    pub send_calls: u64,
    /// Datagrams received, each coalesced one counted once per datagram it held.
    pub received: u64,
    /// System calls that received them.
    pub receive_calls: u64,
    /// Datagrams the kernel refused to send, dropped as lost (QUIC recovers them; the plane
    /// retransmits nothing by design).
    pub send_errors: u64,
    /// Datagrams dropped because the outbox was full and the socket would not take more.
    pub outbox_full: u64,
    /// Receive errors read past: a reset or unreachable report for an earlier send.
    pub receive_errors: u64,
    /// Whether segmented sends are in use.
    pub gso: bool,
    /// Whether coalesced receives are in use.
    pub gro: bool,
    /// Whether the kernel stamps each datagram received (Linux's `SO_TIMESTAMPNS`, macOS's
    /// `SO_TIMESTAMP_MONOTONIC`); otherwise a datagram is stamped when it is read.
    pub kernel_stamps: bool,
}

/// A datagram waiting in the outbox.
pub(crate) struct Slot {
    pub(crate) bytes: Vec<u8>,
    pub(crate) to: SocketAddr,
}

/// Whether the outbox drained.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Sent {
    /// Everything queued has left.
    Drained,
    /// The socket would not take more; wait until it is writable.
    Blocked,
}

/// A receive error a UDP socket reports for an earlier datagram that went astray, which says
/// nothing about the socket: Windows reports a send to a closed port as a reset on the next
/// receive (`WSAECONNRESET`, the `recvfrom` reference), and an ICMP report can surface as
/// refused or unreachable.
fn astray(kind: ErrorKind) -> bool {
    matches!(
        kind,
        ErrorKind::ConnectionReset
            | ErrorKind::ConnectionRefused
            | ErrorKind::HostUnreachable
            | ErrorKind::NetworkUnreachable
            | ErrorKind::Interrupted
    )
}

/// Runs `work`, which registers with tokio and panics when no runtime with the needed driver is
/// current, behind an unwind boundary (mantle CLAUDE.md §1: a dependency that can panic is called
/// behind one).
pub(crate) fn registered<T>(work: impl FnOnce() -> io::Result<T>) -> Result<T, Error> {
    if tokio::runtime::Handle::try_current().is_err() {
        return Err(Error::Runtime);
    }
    match catch_unwind(AssertUnwindSafe(work)) {
        Ok(result) => result.map_err(Error::from),
        Err(_) => Err(Error::Runtime),
    }
}

/// The socket.
pub(crate) struct Socket {
    udp: UdpSocket,
    batch: usize,
    slots: Vec<Slot>,
    /// Slots filled, and of those, the ones sent.
    queued: usize,
    sent: usize,
    buffers: Vec<Vec<u8>>,
    stats: IoStats,
    clock: Clock,
    /// The latest arrival handed over: a socket's queue is first in, first out, so none after it
    /// arrived before it.
    latest_ns: u64,
    #[cfg(target_os = "linux")]
    linux: Linux,
}

#[cfg(target_os = "linux")]
struct Linux {
    send: linux::Headers,
    receive: linux::Headers,
    received: Vec<linux::Received>,
}

impl Socket {
    /// Takes `socket` onto tokio's reactor; it must be called within a runtime with I/O enabled.
    /// With `stamps`, the kernel is asked to stamp each datagram it receives, where it can.
    pub(crate) fn new(socket: std::net::UdpSocket, io: Io, stamps: bool) -> Result<Self, Error> {
        if io.batch == 0 || io.batch > MAX_BATCH {
            return Err(Error::Configuration);
        }
        socket.set_nonblocking(true)?;
        let udp = registered(|| UdpSocket::from_std(socket))?;
        let unspecified = SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0));
        let slots = (0..io.batch)
            .map(|_| Slot {
                bytes: Vec::new(),
                to: unspecified,
            })
            .collect();
        #[cfg(target_os = "linux")]
        let (linux, buffers, stats) = {
            use std::os::fd::AsRawFd;
            let offload = linux::offload(udp.as_raw_fd());
            let stats = IoStats {
                gso: offload.gso,
                gro: offload.gro,
                kernel_stamps: stamps && linux::enable_stamps(udp.as_raw_fd()),
                ..IoStats::default()
            };
            let linux = Linux {
                send: linux::Headers::new(io.batch),
                receive: linux::Headers::new(io.batch),
                received: Vec::with_capacity(io.batch),
            };
            (linux, vec![vec![0; RECEIVE_BYTES]; io.batch], stats)
        };
        #[cfg(target_os = "macos")]
        let (buffers, stats) = {
            use std::os::fd::AsRawFd;
            let stats = IoStats {
                kernel_stamps: stamps && macos::enable_stamps(udp.as_raw_fd()),
                ..IoStats::default()
            };
            (vec![vec![0; RECEIVE_BYTES]], stats)
        };
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let (buffers, stats) = {
            let _ = stamps;
            (vec![vec![0; RECEIVE_BYTES]], IoStats::default())
        };
        Ok(Self {
            udp,
            batch: io.batch,
            slots,
            queued: 0,
            sent: 0,
            buffers,
            stats,
            clock: Clock::new()?,
            latest_ns: 0,
            #[cfg(target_os = "linux")]
            linux,
        })
    }

    pub(crate) fn local_addr(&self) -> Result<SocketAddr, Error> {
        Ok(self.udp.local_addr()?)
    }

    pub(crate) fn stats(&self) -> IoStats {
        self.stats
    }

    /// The clock the socket stamps on.
    pub(crate) fn clock(&self) -> &Clock {
        &self.clock
    }

    /// A stamp handed over: within the read's time and no earlier than the one before it.
    fn arrival(&mut self, from: SocketAddr, stamp: Option<u64>, read_ns: u64) -> Arrival {
        let at_ns = stamp.unwrap_or(read_ns).min(read_ns).max(self.latest_ns);
        self.latest_ns = at_ns;
        Arrival {
            from,
            at_ns,
            kernel: stamp.is_some(),
        }
    }

    /// Whether datagrams wait for the socket.
    pub(crate) fn pending(&self) -> bool {
        self.sent < self.queued
    }

    /// The next free slot, if the outbox has one; [`Socket::commit`] queues it.
    pub(crate) fn slot(&mut self) -> Option<&mut Slot> {
        if self.sent == self.queued {
            self.sent = 0;
            self.queued = 0;
        }
        self.slots.get_mut(self.queued)
    }

    /// Queues the slot [`Socket::slot`] gave.
    pub(crate) fn commit(&mut self) {
        self.queued = self.queued.saturating_add(1).min(self.batch);
    }

    /// Queues a copy of `bytes` for `to`; a full outbox drops it and counts it.
    pub(crate) fn queue(&mut self, to: SocketAddr, bytes: &[u8]) {
        let Some(slot) = self.slot() else {
            self.stats.outbox_full = self.stats.outbox_full.saturating_add(1);
            return;
        };
        slot.bytes.clear();
        slot.bytes.extend_from_slice(bytes);
        slot.to = to;
        self.commit();
    }

    /// Sends what is queued, as far as the socket takes it. A datagram the kernel refuses is
    /// dropped and counted: the protocols above recover a lost one.
    pub(crate) fn send(&mut self) -> Sent {
        while self.sent < self.queued {
            match self.send_some() {
                Ok(count) => {
                    let count = count.max(1);
                    self.sent = self.sent.saturating_add(count).min(self.queued);
                    self.stats.sent = self.stats.sent.saturating_add(as_u64(count));
                    self.stats.send_calls = self.stats.send_calls.saturating_add(1);
                }
                Err((error, _)) if error.kind() == ErrorKind::WouldBlock => return Sent::Blocked,
                Err((error, slots)) => {
                    self.refused(&error);
                    let slots = slots.max(1);
                    self.sent = self.sent.saturating_add(slots).min(self.queued);
                    self.stats.send_errors = self.stats.send_errors.saturating_add(as_u64(slots));
                }
            }
        }
        self.sent = 0;
        self.queued = 0;
        Sent::Drained
    }

    #[cfg(target_os = "linux")]
    fn refused(&mut self, error: &io::Error) {
        // EIO from a segmented send: the device cannot segment (its checksum offload is off,
        // udp(7)); later sends go one datagram a message.
        if self.stats.gso && error.raw_os_error() == Some(libc::EIO) {
            self.stats.gso = false;
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn refused(&mut self, _: &io::Error) {}

    /// One system call's worth of the queue; how many slots left, or the error and how many
    /// slots it concerned.
    #[cfg(target_os = "linux")]
    fn send_some(&mut self) -> Result<usize, (io::Error, usize)> {
        use std::os::fd::AsRawFd;
        let fd = self.udp.as_raw_fd();
        let slots = self.slots.get(self.sent..self.queued).unwrap_or(&[]);
        let gso = self.stats.gso;
        let headers = &mut self.linux.send;
        let mut failed = 1;
        let result = self.udp.try_io(tokio::io::Interest::WRITABLE, || {
            linux::send(fd, slots, gso, headers).map_err(|refused| {
                failed = refused.slots;
                refused.error
            })
        });
        result.map_err(|error| (error, failed))
    }

    #[cfg(not(target_os = "linux"))]
    fn send_some(&mut self) -> Result<usize, (io::Error, usize)> {
        let Some(slot) = self.slots.get(self.sent) else {
            return Ok(1);
        };
        self.udp
            .try_send_to(&slot.bytes, slot.to)
            .map(|_| 1)
            .map_err(|error| (error, 1))
    }

    /// Ready to read, or to send when `writing`: registers the task's waker otherwise.
    pub(crate) fn poll_ready(&self, context: &mut Context<'_>, writing: bool) -> Poll<Ready> {
        let readable = matches!(self.udp.poll_recv_ready(context), Poll::Ready(_));
        let writable = writing && matches!(self.udp.poll_send_ready(context), Poll::Ready(_));
        if readable || writable {
            Poll::Ready(Ready { readable, writable })
        } else {
            Poll::Pending
        }
    }

    /// Takes one batch of datagrams that have arrived, handing each to `deliver` with its arrival;
    /// returns how many, 0 when none had. Through the reactor's readiness: what a wake for the
    /// socket's readiness takes, the readiness cleared when the socket is found empty.
    pub(crate) fn receive(
        &mut self,
        deliver: impl FnMut(Arrival, &mut [u8]),
    ) -> Result<usize, Error> {
        self.receive_batch(Through::Reactor, deliver)
    }

    /// [`receive`](Self::receive), asking the kernel whatever the reactor last saw: what an owner
    /// reads before it judges a time, which must take every datagram the kernel stamped before it.
    /// The reactor's readiness is what it saw at its last turn, and a datagram that came after
    /// (through a stop of the process, say) is in the socket with a stamp before the owner's time
    /// while the reactor says the socket is empty. Where the stamp is the read's (Windows, or a
    /// kernel that refused the stamps), a datagram not yet read has no stamp before the owner's
    /// time, and the reactor's readiness serves.
    pub(crate) fn receive_queued(
        &mut self,
        deliver: impl FnMut(Arrival, &mut [u8]),
    ) -> Result<usize, Error> {
        let through = if self.stats.kernel_stamps {
            Through::Kernel
        } else {
            Through::Reactor
        };
        self.receive_batch(through, deliver)
    }

    fn receive_batch(
        &mut self,
        through: Through,
        mut deliver: impl FnMut(Arrival, &mut [u8]),
    ) -> Result<usize, Error> {
        let mut delivered = 0usize;
        for _ in 0..self.batch {
            match self.receive_some(through, &mut deliver) {
                Ok(0) => break,
                Ok(count) => {
                    delivered = delivered.saturating_add(count);
                    if cfg!(target_os = "linux") {
                        // One recvmmsg took the batch.
                        break;
                    }
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) if astray(error.kind()) => {
                    self.stats.receive_errors = self.stats.receive_errors.saturating_add(1);
                }
                Err(error) => return Err(Error::from(error)),
            }
        }
        self.stats.received = self.stats.received.saturating_add(as_u64(delivered));
        Ok(delivered)
    }

    #[cfg(target_os = "linux")]
    fn receive_some(
        &mut self,
        through: Through,
        deliver: &mut impl FnMut(Arrival, &mut [u8]),
    ) -> io::Result<usize> {
        use std::os::fd::AsRawFd;
        let fd = self.udp.as_raw_fd();
        let Self {
            udp,
            buffers,
            linux,
            stats,
            ..
        } = self;
        linux.received.clear();
        let mut take = || linux::receive(fd, buffers, &mut linux.receive, &mut linux.received);
        match through {
            Through::Reactor => udp.try_io(tokio::io::Interest::READABLE, take),
            Through::Kernel => take(),
        }?;
        stats.receive_calls = stats.receive_calls.saturating_add(1);
        // Both clocks once a batch, after the receive: a stamp is carried over by its age. The
        // realtime clock first: a thread preempted between the two reads then makes the age's end
        // later, so a stamp comes out late, never early (the order mattered: read the other way, a
        // throttled container's preemption put a stamp before its datagram was sent).
        let realtime_ns = if stats.kernel_stamps {
            linux::realtime_ns()?
        } else {
            0
        };
        let read_ns = linux::monotonic_ns()?;
        let mut delivered = 0usize;
        let mut buffers = std::mem::take(&mut self.buffers);
        let received = std::mem::take(&mut self.linux.received);
        for (taken, buffer) in received.iter().zip(buffers.iter_mut()) {
            let Some(bytes) = buffer.get_mut(..taken.length) else {
                continue;
            };
            let stamp = taken
                .stamp
                .map(|stamp| read_ns.saturating_sub(realtime_ns.saturating_sub(stamp)));
            let arrival = self.arrival(taken.from, stamp, read_ns);
            match taken.segment {
                Some(size) => {
                    for datagram in bytes.chunks_mut(size) {
                        deliver(arrival, datagram);
                        delivered = delivered.saturating_add(1);
                    }
                }
                None => {
                    deliver(arrival, bytes);
                    delivered = delivered.saturating_add(1);
                }
            }
        }
        self.buffers = buffers;
        self.linux.received = received;
        Ok(delivered)
    }

    #[cfg(target_os = "macos")]
    fn receive_some(
        &mut self,
        through: Through,
        deliver: &mut impl FnMut(Arrival, &mut [u8]),
    ) -> io::Result<usize> {
        if !self.stats.kernel_stamps {
            return self.receive_portable(deliver);
        }
        use std::os::fd::AsRawFd;
        let fd = self.udp.as_raw_fd();
        let mut buffers = std::mem::take(&mut self.buffers);
        let result = match buffers.first_mut() {
            None => Ok(None),
            Some(buffer) => match through {
                Through::Reactor => self
                    .udp
                    .try_io(tokio::io::Interest::READABLE, || macos::receive(fd, buffer)),
                Through::Kernel => macos::receive(fd, buffer),
            },
        };
        let delivered = match result {
            Err(error) => Err(error),
            Ok(received) => {
                self.stats.receive_calls = self.stats.receive_calls.saturating_add(1);
                let read_ns = self.clock.now_ns();
                if let Some(taken) = received {
                    let stamp = taken.stamp.map(|ticks| self.clock.ticks_ns(ticks));
                    let arrival = self.arrival(taken.from, stamp, read_ns);
                    if let Some(bytes) = buffers.first_mut().and_then(|b| b.get_mut(..taken.length))
                    {
                        deliver(arrival, bytes);
                    }
                }
                Ok(1)
            }
        };
        self.buffers = buffers;
        delivered
    }

    #[cfg(not(target_os = "linux"))]
    fn receive_portable(
        &mut self,
        deliver: &mut impl FnMut(Arrival, &mut [u8]),
    ) -> io::Result<usize> {
        let mut buffers = std::mem::take(&mut self.buffers);
        let result = match buffers.first_mut() {
            None => Ok(0),
            Some(buffer) => self.udp.try_recv_from(buffer).map(|(length, from)| {
                self.stats.receive_calls = self.stats.receive_calls.saturating_add(1);
                let arrival = self.arrival(from, None, self.clock.now_ns());
                if let Some(bytes) = buffer.get_mut(..length) {
                    deliver(arrival, bytes);
                }
                1
            }),
        };
        self.buffers = buffers;
        result
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    fn receive_some(
        &mut self,
        _through: Through,
        deliver: &mut impl FnMut(Arrival, &mut [u8]),
    ) -> io::Result<usize> {
        self.receive_portable(deliver)
    }
}

/// Whom a receive asks whether the socket holds a datagram.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Through {
    /// The reactor's readiness, as of its last turn.
    Reactor,
    /// The kernel itself: a receive that does not block, whatever the reactor last saw.
    Kernel,
}

/// Which way the socket is ready.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Ready {
    pub(crate) readable: bool,
    pub(crate) writable: bool,
}

fn as_u64(count: usize) -> u64 {
    u64::try_from(count).unwrap_or(u64::MAX)
}
