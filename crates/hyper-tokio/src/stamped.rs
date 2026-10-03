//! A standard UDP socket's datagrams with when they arrived, for an owner that waits on its socket
//! itself and runs no tokio (hyper-raft-e2e's and hyper-durable-e2e's members): the kernel's
//! receive stamp on the host's monotonic clock where the platform gives one, as a
//! [`crate::PlaneSocket`]'s arrivals are ([`crate::clock`]), and the read's time elsewhere.
//!
//! A datagram that waited in the socket while its reader was stopped is stamped when it arrived,
//! not when it was read. That is what a heartbeat's echo needs: the hold it states, from the echoed
//! heartbeat's arrival to the echo's send, then covers the time the heartbeat sat in the socket,
//! and the peer's round trip is the path's. Stamped at the read, the stop was counted as the path's,
//! and a stopped member's peers' `T_E`, which their round trips to it enter, stood at up to 2.1 s
//! (`docs/timing.md` §2.8, "The echo").

use std::io::{self, ErrorKind};
use std::net::UdpSocket;

use crate::Error;
use crate::clock::{Arrival, Clock};
#[cfg(target_os = "linux")]
use crate::sys::linux;
#[cfg(target_os = "macos")]
use crate::sys::macos;

/// What one [`Stamped::receive`] took from the socket.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Taken {
    /// A datagram of this many bytes, now at the front of the buffer, and when and where from it
    /// arrived.
    Datagram(usize, Arrival),
    /// A datagram the protocols above take as lost: one the buffer truncated, or from an address
    /// that is not an internet one. It is gone from the socket.
    Unreadable,
}

/// The receive stamps of a standard UDP socket its owner drives.
pub struct Stamped {
    clock: Clock,
    kernel: bool,
    /// The latest stamp handed over: the socket's queue is first in, first out, so none after is
    /// earlier.
    latest_ns: u64,
    #[cfg(target_os = "linux")]
    headers: linux::Headers,
    #[cfg(target_os = "linux")]
    received: Vec<linux::Received>,
}

impl Stamped {
    /// Asks the kernel to stamp `socket`'s datagrams ([`kernel`](Self::kernel) says whether it
    /// agreed), on the host's monotonic clock.
    pub fn new(socket: &UdpSocket) -> Result<Self, Error> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        use std::os::fd::AsRawFd;
        #[cfg(target_os = "linux")]
        let kernel = linux::enable_stamps(socket.as_raw_fd());
        #[cfg(target_os = "macos")]
        let kernel = macos::enable_stamps(socket.as_raw_fd());
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let kernel = {
            let _ = socket;
            false
        };
        Ok(Self {
            clock: Clock::new()?,
            kernel,
            latest_ns: 0,
            #[cfg(target_os = "linux")]
            headers: linux::Headers::new(1),
            #[cfg(target_os = "linux")]
            received: Vec::with_capacity(1),
        })
    }

    /// The clock the arrivals are stamped on: the owner's `now` for what it judges by them.
    pub fn clock(&self) -> &Clock {
        &self.clock
    }

    /// Whether the kernel stamps the socket's datagrams; when it does not, a datagram is stamped
    /// when it is read.
    pub fn kernel(&self) -> bool {
        self.kernel
    }

    /// The next datagram queued on `socket`, read into `buffer` without waiting whatever the
    /// socket's mode on Linux and macOS (`MSG_DONTWAIT`), and in its mode elsewhere: `None` when none
    /// is queued.
    pub fn receive(&mut self, socket: &UdpSocket, buffer: &mut [u8]) -> io::Result<Option<Taken>> {
        match self.take(socket, buffer) {
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                Ok(None)
            }
            other => other.map(Some),
        }
    }

    #[cfg(target_os = "linux")]
    fn take(&mut self, socket: &UdpSocket, buffer: &mut [u8]) -> io::Result<Taken> {
        use std::os::fd::AsRawFd;
        self.received.clear();
        linux::receive(
            socket.as_raw_fd(),
            &mut [buffer],
            &mut self.headers,
            &mut self.received,
        )?;
        // Both clocks after the receive, the realtime one first, as the plane socket reads them
        // (`crate::clock`): a stamp is carried over by its age, late and never early.
        let realtime_ns = if self.kernel {
            linux::realtime_ns()?
        } else {
            0
        };
        let read_ns = linux::monotonic_ns()?;
        let Some(taken) = self.received.first().copied() else {
            return Ok(Taken::Unreadable);
        };
        let stamp = taken
            .stamp
            .map(|stamp| read_ns.saturating_sub(realtime_ns.saturating_sub(stamp)));
        Ok(Taken::Datagram(
            taken.length,
            self.arrival(taken.from, stamp, read_ns),
        ))
    }

    #[cfg(target_os = "macos")]
    fn take(&mut self, socket: &UdpSocket, buffer: &mut [u8]) -> io::Result<Taken> {
        use std::os::fd::AsRawFd;
        let received = macos::receive(socket.as_raw_fd(), buffer)?;
        let read_ns = self.clock.now_ns();
        let Some(taken) = received else {
            return Ok(Taken::Unreadable);
        };
        let stamp = taken.stamp.map(|ticks| self.clock.ticks_ns(ticks));
        Ok(Taken::Datagram(
            taken.length,
            self.arrival(taken.from, stamp, read_ns),
        ))
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    fn take(&mut self, socket: &UdpSocket, buffer: &mut [u8]) -> io::Result<Taken> {
        let (length, from) = socket.recv_from(buffer)?;
        let read_ns = self.clock.now_ns();
        Ok(Taken::Datagram(length, self.arrival(from, None, read_ns)))
    }

    /// A stamp handed over: within the read's time and no earlier than the one before it, as the
    /// plane socket's are.
    fn arrival(&mut self, from: std::net::SocketAddr, stamp: Option<u64>, read_ns: u64) -> Arrival {
        let at_ns = stamp.unwrap_or(read_ns).min(read_ns).max(self.latest_ns);
        self.latest_ns = at_ns;
        Arrival {
            from,
            at_ns,
            kernel: stamp.is_some(),
        }
    }
}
