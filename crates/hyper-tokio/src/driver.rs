//! The endpoint's driver: one owner, one socket, one timer.

use std::future::{Future as _, poll_fn};
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Instant;

use hyper_transport::{Budget, Classes, Directory, Endpoint, Event, Lateness, SocketCharge};
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
    /// What the socket's queue is learned to charge, and the room in force (hyper-transport's
    /// `SocketCharge`; `docs/transport.md` §4i).
    measure: SocketCharge,
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
        mut endpoint: Endpoint<C, B, D>,
        socket: std::net::UdpSocket,
        io: Io,
    ) -> Result<Self, Error> {
        // The socket's queue holds what the endpoint's windows may grant (hyper-transport's socket
        // room): asked for their sum, and the endpoint told what the kernel gave, so what its peers
        // may send unread never passes what the queue takes while the owner does not read.
        // The queue is asked for the windows' demand, the least it could need, and the room is
        // told from what the kernel granted and what it is measured to charge: until a message is
        // measured, the first windows admission lets in.
        let ask = hyper_transport::socket_buffer_ask(endpoint.receive_demand());
        crate::sys::set_recv_buffer(&socket, usize::try_from(ask).unwrap_or(usize::MAX))?;
        let unmeasured = endpoint.unmeasured_room();
        endpoint.set_receive_capacity(unmeasured);
        // Stamped where the kernel stamps, so a drain knows what was queued when it began.
        let socket = Socket::new(socket, io, true)?;
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
            measure: SocketCharge::new(unmeasured),
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
    /// each turn takes what had arrived when it began (`drain`), then fires the timers due, sends,
    /// and hands out the next event, or parks. What arrives meanwhile is the next turn's: the
    /// socket's room holds whatever its peers' windows let them send unread (hyper-transport's
    /// socket room), so taking it later loses nothing, and the sends wait behind the backlog the
    /// turn found, not behind whatever more its peers kept sending.
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

    /// Takes, a batch at a time, the datagrams that were queued when the drain began, the rest of
    /// the batch that reached the first that was not, until the socket has none or [`TURNS`]
    /// batches are taken. What was queued is told by the kernel's receive stamps where the socket
    /// has them: one stamped after the drain began arrived since, and a stamp is late, never
    /// early. A socket without counts bytes: as many as it held at the start
    /// ([`Socket::queued_bytes`]: every queued datagram's on Windows, so the drain ends once it
    /// has taken them). Where the platform says neither, [`TURNS`] batches bound it. A drain to
    /// empty kept the driver reading while a peer kept sending, its sends, the peer's credit among
    /// them, waiting behind (hyper-rt-transport's driver measured it: `docs/transport.md` §4h).
    #[allow(
        clippy::disallowed_methods,
        reason = "hyper-tokio is the runtime adapter: it reads the host's clock for the sans-io crates it drives"
    )]
    fn drain(&mut self, context: &mut Context<'_>) -> Result<(), Error> {
        let began = self.socket.clock().now_ns();
        let held = if self.socket.stats().kernel_stamps {
            None
        } else {
            self.socket.queued_bytes().ok()
        };
        // The queue's account before the drain, and the window its peers had consumed: a drain
        // that took exactly one datagram and left the queue charged nothing read that datagram's
        // charge (hyper-transport's `SocketCharge`, the singleton rule).
        let before = self.socket.account()?;
        if let Some(before) = before {
            self.measure.before(before);
        }
        let stream_before = self.endpoint.stream_bytes_received_shared();
        let mut read = 0usize;
        let mut datagrams = 0usize;
        let mut emptied = false;
        for _ in 0..TURNS {
            let Poll::Ready(ready) = self.socket.poll_ready(context, false) else {
                emptied = true;
                break;
            };
            if !ready.readable {
                emptied = true;
                break;
            }
            let endpoint = &mut self.endpoint;
            let mut past = false;
            let taken = self.socket.receive(|arrival, bytes| {
                past |= arrival.kernel && arrival.at_ns > began;
                read = read.saturating_add(bytes.len());
                endpoint.handle_datagram(Instant::now(), arrival.from, None, bytes);
            })?;
            datagrams = datagrams.saturating_add(taken);
            if taken == 0 {
                emptied = true;
                break;
            }
            if past || held.is_some_and(|held| read >= held) {
                break;
            }
        }
        let consumed = self
            .endpoint
            .stream_bytes_received_shared()
            .saturating_sub(stream_before);
        let after = match before {
            Some(before) if self.measure.wants_after(&before, datagrams, emptied) => {
                self.socket.account()?
            }
            _ => None,
        };
        self.measure.after(before, after, consumed, emptied);
        let room = self
            .measure
            .room(self.endpoint.unprompted_shared())
            .unwrap_or_else(|| self.endpoint.unmeasured_room());
        self.endpoint.set_receive_capacity(room);
        self.measure.set_room(room);
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
