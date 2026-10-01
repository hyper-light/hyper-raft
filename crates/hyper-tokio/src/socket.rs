//! One UDP socket on tokio's reactor, batched: the datagrams to send wait in a bounded outbox and
//! leave in as few system calls as the platform allows; received ones are taken a batch at a
//! time.
//!
//! - Linux: `sendmmsg(2)` and `recvmmsg(2)`, each message a segmented send (`UDP_SEGMENT`) or a
//!   coalesced receive (`UDP_GRO`) where the kernel has them ([`crate::sys`]).
//! - macOS and Windows: one datagram a system call through tokio. macOS has no public batched
//!   UDP call; Windows' segmentation and coalescing (`UDP_SEND_MSG_SIZE`,
//!   `UDP_RECV_MAX_COALESCED_SIZE`) are not used yet (docs/transport.md §4b).

use std::io::{self, ErrorKind};
use std::net::{Ipv4Addr, SocketAddr};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::task::{Context, Poll};

use tokio::net::UdpSocket;

use crate::Error;
#[cfg(target_os = "linux")]
use crate::sys::linux;

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
    pub(crate) fn new(socket: std::net::UdpSocket, io: Io) -> Result<Self, Error> {
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
                ..IoStats::default()
            };
            let linux = Linux {
                send: linux::Headers::new(io.batch),
                receive: linux::Headers::new(io.batch),
                received: Vec::with_capacity(io.batch),
            };
            (linux, vec![vec![0; RECEIVE_BYTES]; io.batch], stats)
        };
        #[cfg(not(target_os = "linux"))]
        let (buffers, stats) = (vec![vec![0; RECEIVE_BYTES]], IoStats::default());
        Ok(Self {
            udp,
            batch: io.batch,
            slots,
            queued: 0,
            sent: 0,
            buffers,
            stats,
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

    /// Takes one batch of datagrams that have arrived, handing each to `deliver`; returns how
    /// many, 0 when none had.
    pub(crate) fn receive(
        &mut self,
        mut deliver: impl FnMut(SocketAddr, &mut [u8]),
    ) -> Result<usize, Error> {
        let mut delivered = 0usize;
        for _ in 0..self.batch {
            match self.receive_some(&mut deliver) {
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
        deliver: &mut impl FnMut(SocketAddr, &mut [u8]),
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
        udp.try_io(tokio::io::Interest::READABLE, || {
            linux::receive(fd, buffers, &mut linux.receive, &mut linux.received)
        })?;
        stats.receive_calls = stats.receive_calls.saturating_add(1);
        let mut delivered = 0usize;
        for (received, buffer) in linux.received.iter().zip(buffers.iter_mut()) {
            let Some(bytes) = buffer.get_mut(..received.length) else {
                continue;
            };
            match received.segment {
                Some(size) => {
                    for datagram in bytes.chunks_mut(size) {
                        deliver(received.from, datagram);
                        delivered = delivered.saturating_add(1);
                    }
                }
                None => {
                    deliver(received.from, bytes);
                    delivered = delivered.saturating_add(1);
                }
            }
        }
        Ok(delivered)
    }

    #[cfg(not(target_os = "linux"))]
    fn receive_some(
        &mut self,
        deliver: &mut impl FnMut(SocketAddr, &mut [u8]),
    ) -> io::Result<usize> {
        let Some(buffer) = self.buffers.first_mut() else {
            return Ok(0);
        };
        let (length, from) = self.udp.try_recv_from(buffer)?;
        self.stats.receive_calls = self.stats.receive_calls.saturating_add(1);
        if let Some(bytes) = buffer.get_mut(..length) {
            deliver(from, bytes);
        }
        Ok(1)
    }
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
