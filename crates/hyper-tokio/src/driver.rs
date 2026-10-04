//! The endpoint's driver: one owner, one socket, one timer.

use std::future::{Future as _, poll_fn};
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Instant;

use hyper_transport::{Budget, Classes, Directory, Endpoint, Event, Lateness};
use tokio::time::Sleep;

use crate::socket::{Io, IoStats, Sent, Socket, registered};
use crate::{Clock, Error};

/// The turns one poll of [`Driver::event`] takes before it yields to the runtime: tokio's
/// cooperative budget, 128 operations a task a poll (`tokio::task::coop`, `Budget::initial`).
/// A turn takes a batch of datagrams, fires a timer or sends a batch, so a busy socket never
/// holds the executor past that.
pub const TURNS: usize = 128;

/// Drives an [`Endpoint`] on tokio: owns its UDP socket and its timer, feeds it datagrams and
/// timeouts with the time they happened, sends what it transmits, and hands its events to the
/// owner one at a time from [`Driver::event`].
///
/// The owner holds the driver and calls the endpoint through [`Driver::endpoint`] between events;
/// nothing is shared, and there is no task, channel or lock inside. What the owner's calls queue
/// is sent at its next [`Driver::event`] or [`Driver::flush`].
pub struct Driver<C: Classes, B: Budget<C::Class>, D: Directory<Role = C::Role>> {
    endpoint: Endpoint<C, B, D>,
    socket: Socket,
    /// The timer, boxed because tokio's `Sleep` is not `Unpin`; reset, never rebuilt.
    sleep: Pin<Box<Sleep>>,
    /// The deadline the timer is armed for.
    armed: Option<Instant>,
    /// How late the timer fires: the owner's timer granularity `G`, which the endpoint tunes its
    /// receive windows under (`docs/timing.md` §2.4).
    late: Lateness,
    /// The instant the lateness fold's nanoseconds count from.
    epoch: Instant,
}

impl<C: Classes, B: Budget<C::Class>, D: Directory<Role = C::Role>> Driver<C, B, D> {
    /// A driver for `endpoint` on a socket bound to `address`. Called within a tokio runtime with
    /// its I/O and time drivers enabled; otherwise [`Error::Runtime`].
    pub fn bind(endpoint: Endpoint<C, B, D>, address: SocketAddr, io: Io) -> Result<Self, Error> {
        let socket = std::net::UdpSocket::bind(address)?;
        Self::new(endpoint, socket, io)
    }

    /// A driver for `endpoint` on `socket`, which it takes onto tokio's reactor.
    #[allow(
        clippy::disallowed_methods,
        reason = "hyper-tokio is the runtime adapter: it reads the host's clock for the sans-io crates it drives"
    )]
    pub fn new(
        endpoint: Endpoint<C, B, D>,
        socket: std::net::UdpSocket,
        io: Io,
    ) -> Result<Self, Error> {
        let socket = Socket::new(socket, io, false)?;
        // The fold reads std's `Instant`, which is the clock `Clock` reads: `CLOCK_MONOTONIC`,
        // `CLOCK_UPTIME_RAW` and `QueryPerformanceCounter` (std's `Instant` documentation).
        let resolution = Clock::new()?.resolution();
        let sleep = registered(|| {
            Ok(Box::pin(tokio::time::sleep_until(
                tokio::time::Instant::now(),
            )))
        })?;
        Ok(Self {
            endpoint,
            socket,
            sleep,
            armed: None,
            late: Lateness::new(resolution),
            epoch: Instant::now(),
        })
    }

    /// The endpoint, for the owner's calls: `connect`, `open`, `write_body`, `read_body`,
    /// `reply`, `end`, `send_frame` and the rest.
    pub fn endpoint(&mut self) -> &mut Endpoint<C, B, D> {
        &mut self.endpoint
    }

    /// The address the socket is bound to.
    pub fn local_addr(&self) -> Result<SocketAddr, Error> {
        self.socket.local_addr()
    }

    /// What the socket has done.
    pub fn stats(&self) -> IoStats {
        self.socket.stats()
    }

    /// Sends what the endpoint has to send now, as far as the socket takes it without waiting;
    /// the rest goes at the next [`Driver::event`].
    #[allow(
        clippy::disallowed_methods,
        reason = "hyper-tokio is the runtime adapter: it reads the host's clock for the sans-io crates it drives"
    )]
    pub fn flush(&mut self) {
        let _ = self.send(Instant::now());
    }

    /// The endpoint's next event. Until one is ready it sends what the endpoint transmits, takes
    /// the datagrams that arrive and fires its timers, parked on the socket and the timer in
    /// between.
    ///
    /// Cancel-safe: everything it has taken is in the endpoint before it awaits, so dropping it
    /// (as `tokio::select!` does with the branch that lost) loses nothing.
    pub async fn event(&mut self) -> Result<Event<C>, Error> {
        poll_fn(|context| self.poll_event(context)).await
    }

    /// [`Driver::event`] as a poll. An event already queued is handed out at once. Otherwise
    /// each turn takes what has arrived, up to [`TURNS`] batches, so that the socket's receive
    /// buffer is drained before anything is surfaced; then fires the timers due, sends, and hands
    /// out the next event, or parks.
    #[allow(
        clippy::disallowed_methods,
        reason = "hyper-tokio is the runtime adapter: it reads the host's clock for the sans-io crates it drives"
    )]
    pub fn poll_event(&mut self, context: &mut Context<'_>) -> Poll<Result<Event<C>, Error>> {
        // Events already queued go first: the owner's work on them is what the next datagrams
        // and window updates wait for, and handing them out costs no system call.
        if let Some(event) = self.endpoint.poll_event() {
            return Poll::Ready(Ok(event));
        }
        for _ in 0..TURNS {
            self.drain(context)?;
            let now = Instant::now();
            let due = self.endpoint.poll_timeout();
            if due.is_some_and(|due| due <= now) {
                self.endpoint.handle_timeout(now);
            }
            let more = self.send(now);
            if let Some(event) = self.endpoint.poll_event() {
                return Poll::Ready(Ok(event));
            }
            if more {
                continue;
            }
            if self
                .socket
                .poll_ready(context, self.socket.pending())
                .is_ready()
            {
                continue;
            }
            let Some(due) = self.endpoint.poll_timeout() else {
                return Poll::Pending;
            };
            if self.armed != Some(due) {
                self.sleep
                    .as_mut()
                    .reset(tokio::time::Instant::from_std(due));
                self.armed = Some(due);
            }
            if self.sleep.as_mut().poll(context).is_pending() {
                return Poll::Pending;
            }
            self.fired(due);
            self.armed = None;
        }
        // The budget is spent with work still to do: come back after the runtime's other tasks.
        context.waker().wake_by_ref();
        Poll::Pending
    }

    /// The timer armed for `due` fired: its lateness is folded into `G`, and the endpoint told.
    #[allow(
        clippy::disallowed_methods,
        reason = "hyper-tokio is the runtime adapter: it reads the host's clock for the sans-io crates it drives"
    )]
    fn fired(&mut self, due: Instant) {
        let nanos = |at: Instant| {
            u64::try_from(at.saturating_duration_since(self.epoch).as_nanos()).unwrap_or(u64::MAX)
        };
        // A full fold keeps the mean it has: 2^64 waits, or nanoseconds, are past any process.
        let _ = self.late.on_wait(nanos(due), nanos(Instant::now()));
        if let Some(granularity) = self.late.granularity() {
            self.endpoint.set_granularity(granularity);
        }
    }

    /// Takes the datagrams that have arrived, a batch at a time, until the socket has none or
    /// [`TURNS`] batches are taken.
    #[allow(
        clippy::disallowed_methods,
        reason = "hyper-tokio is the runtime adapter: it reads the host's clock for the sans-io crates it drives"
    )]
    fn drain(&mut self, context: &mut Context<'_>) -> Result<(), Error> {
        for _ in 0..TURNS {
            let Poll::Ready(ready) = self.socket.poll_ready(context, false) else {
                return Ok(());
            };
            if !ready.readable {
                return Ok(());
            }
            let endpoint = &mut self.endpoint;
            let taken = self.socket.receive(|arrival, bytes| {
                endpoint.handle_datagram(Instant::now(), arrival.from, None, bytes);
            })?;
            if taken == 0 {
                return Ok(());
            }
        }
        Ok(())
    }

    /// Moves what the endpoint transmits into the outbox and sends it; whether the outbox filled,
    /// so that the endpoint may have more.
    fn send(&mut self, now: Instant) -> bool {
        if self.socket.pending() && self.socket.send() == Sent::Blocked {
            return false;
        }
        let mut filled = true;
        while let Some(slot) = self.socket.slot() {
            let Some(transmit) = self.endpoint.poll_transmit(now, &mut slot.bytes) else {
                filled = false;
                break;
            };
            slot.bytes.truncate(transmit.size);
            slot.to = transmit.destination;
            self.socket.commit();
        }
        self.socket.send() == Sent::Drained && filled
    }
}
