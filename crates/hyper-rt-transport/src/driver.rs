//! The endpoint's driver: one owner, its sockets, one timer.
//!
//! **A socket for each peer.** Where the platform lets sockets share a port so that a connected
//! one takes its peer's datagrams (Linux and the BSDs; `UdpSocket::bind_shared`), the driver's
//! listener shares its port, and each connection the endpoint identifies gets a socket of its own,
//! bound to that port and connected to its peer: the peer's datagrams then wait in a receive queue
//! of their own, as a TCP connection's do, and the connection's windows are funded from that
//! queue's room alone (hyper-transport's `Endpoint::set_peer_room`). A node's receive capacity so
//! grows with the connections it holds, at the kernel's default buffer each, with nothing tuned and
//! no privilege asked: one shared socket of Linux's default 208 KiB held about three connections'
//! first windows. The listener keeps the handshakes and any peer whose socket could not be made.
//! Elsewhere (Windows, the simulation fabric) the listener takes every peer.

use std::future::{Future, poll_fn};
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use hyper_rt::RtError;
use hyper_rt::futures::{Sleep, sleep};
use hyper_rt::readiness::Ready;
use hyper_rt::udp::{Batched, Io, IoStats, MAX_BATCH, Sent};
use hyper_transport::{Budget, Classes, Directory, Endpoint, Event, Lateness, PeerId};

/// Derived: the turns one poll of [`Driver::event`] takes before it yields to the shard, the bound
/// hyper-tokio takes from tokio's cooperative budget (128 operations a task a poll,
/// `tokio::task::coop`), kept so a consumer moving between the two drivers keeps one step's work.
pub const TURNS: usize = 128;

/// The shard's clock now, nanoseconds.
fn shard_now() -> Result<u64, RtError> {
    hyper_rt::registry::with_current(|context| context.now_ns()).ok_or(RtError::NotOnShardThread)
}

/// Bound: the datagrams one turn takes from the endpoint into the outboxes before the turn's other
/// work: one system call's most ([`MAX_BATCH`]).
const TRANSMITS_A_TURN: usize = MAX_BATCH;

/// Bound: a peer socket's batch, datagrams a system call each way. Each socket holds its receive
/// buffers ([`hyper_rt::udp::RECEIVE_BYTES`] each, for the kernel's coalesced receives) for as long
/// as its peer is connected, so a peer's is two, against the listener's batch.
const PEER_IO: Io = Io { batch: 2 };

/// A socket connected to one of the endpoint's peers, sharing the listener's port.
struct PeerSocket {
    peer: PeerId,
    remote: SocketAddr,
    socket: Batched,
    readable: Option<Ready>,
    writable: Option<Ready>,
    /// Its readiness fired, or it was just made: drained at the next turn.
    ready: bool,
}

/// Drives an [`Endpoint`] on a hyper-rt shard (module doc of the crate).
pub struct Driver<C: Classes, B: Budget<C::Class>, D: Directory<Role = C::Role>> {
    endpoint: Endpoint<C, B, D>,
    socket: Batched,
    /// The shard clock's reading and the instant anchored to it.
    anchor: (u64, Instant),
    /// The armed readiness waits, kept between polls (a fired one is dropped and made again).
    readable: Option<Ready>,
    writable: Option<Ready>,
    /// The timer, and the deadline it is armed for.
    timer: Option<(Instant, Sleep)>,
    /// How late the timer fires: the owner's granularity `G` (hyper-raft docs/timing.md §2.4).
    late: Lateness,
    /// The listener's address where its port is shared: the address each peer's socket binds.
    shared: Option<SocketAddr>,
    /// The peers' sockets, sorted by the address each is connected to; at most the admission's bound
    /// on connections.
    peers: Vec<PeerSocket>,
    /// The endpoint's connection generation the peers' sockets were last matched to.
    seen: u64,
    /// The endpoint's connections as last listed ([`Endpoint::established`]).
    listed: Vec<(PeerId, SocketAddr)>,
    /// A datagram the endpoint made, and where it goes, waiting for its socket's outbox to have room:
    /// at most one.
    held: Option<(SocketAddr, Vec<u8>)>,
    /// The buffer the endpoint makes each datagram in, kept between datagrams.
    outgoing: Vec<u8>,
}

impl<C: Classes, B: Budget<C::Class>, D: Directory<Role = C::Role>> Driver<C, B, D> {
    /// A driver for `endpoint` on a socket bound to `address`, its port shared with a socket for
    /// each peer where the platform lets it (the module's doc). Called on a shard thread.
    pub fn bind(endpoint: Endpoint<C, B, D>, address: SocketAddr, io: Io) -> Result<Self, RtError> {
        match Batched::bind_shared(address, io, false) {
            Ok(socket) => {
                let local = socket.local_addr()?;
                let mut driver = Self::new(endpoint, socket)?;
                driver.shared = Some(local);
                Ok(driver)
            }
            Err(RtError::BadConfig { .. }) => {
                Self::new(endpoint, Batched::bind(address, io, false)?)
            }
            Err(error) => Err(error),
        }
    }

    /// A driver for `endpoint` on `socket`. Called on a shard thread.
    #[allow(
        clippy::disallowed_methods,
        reason = "the runtime adapter anchors the endpoint's Instant to the shard's clock once"
    )]
    pub fn new(mut endpoint: Endpoint<C, B, D>, socket: Batched) -> Result<Self, RtError> {
        // The socket's queue holds what the endpoint's windows may grant (hyper-transport's socket
        // room): asked for their sum, and the endpoint told what the kernel gave, so what its peers
        // may send unread never passes what the queue takes while the owner does not read.
        let ask = hyper_transport::socket_buffer_ask(endpoint.receive_demand());
        let reported = socket
            .socket()
            .set_recv_buffer_bytes(usize::try_from(ask).unwrap_or(usize::MAX))?;
        endpoint.set_receive_capacity(hyper_transport::socket_room(
            u64::try_from(reported).unwrap_or(u64::MAX),
        ));
        let anchor = (shard_now()?, Instant::now());
        let resolution = Duration::from_nanos(hyper_rt::machine::clock::resolution_ns());
        Ok(Self {
            endpoint,
            socket,
            anchor,
            readable: None,
            writable: None,
            timer: None,
            late: Lateness::new(resolution),
            shared: None,
            peers: Vec::new(),
            seen: 0,
            listed: Vec::new(),
            held: None,
            outgoing: Vec::new(),
        })
    }

    /// The instant of the shard clock's reading `ns`.
    fn instant(&self, ns: u64) -> Instant {
        let (anchor_ns, anchor) = self.anchor;
        anchor
            .checked_add(Duration::from_nanos(ns.saturating_sub(anchor_ns)))
            .unwrap_or(anchor)
    }

    /// The shard clock's reading of `at`.
    fn ns(&self, at: Instant) -> u64 {
        let (anchor_ns, anchor) = self.anchor;
        let since =
            u64::try_from(at.saturating_duration_since(anchor).as_nanos()).unwrap_or(u64::MAX);
        anchor_ns.saturating_add(since)
    }

    /// Now, as an instant on the shard's clock.
    fn now(&self) -> Result<Instant, RtError> {
        Ok(self.instant(shard_now()?))
    }

    /// The endpoint, for the owner's calls.
    pub fn endpoint(&mut self) -> &mut Endpoint<C, B, D> {
        &mut self.endpoint
    }

    /// The address the socket is bound to.
    pub fn local_addr(&self) -> Result<SocketAddr, RtError> {
        self.socket.local_addr()
    }

    /// What the listener's socket has done.
    pub fn stats(&self) -> IoStats {
        self.socket.stats()
    }

    /// The sockets the driver holds connected to its peers (the module's doc).
    pub fn peer_sockets(&self) -> usize {
        self.peers.len()
    }

    /// Now on the endpoint's clock: the instant the owner passes to the endpoint's calls (`connect`,
    /// `ask`), on the shard's clock as the driver's own are.
    pub fn clock(&self) -> Result<Instant, RtError> {
        self.now()
    }

    /// Sends what the endpoint has to send now, as far as the socket takes it without waiting.
    pub fn flush(&mut self) -> Result<(), RtError> {
        let now = self.now()?;
        let _ = self.send(now);
        Ok(())
    }

    /// The endpoint's next event (hyper-tokio's `Driver::event`). Cancel-safe.
    pub async fn event(&mut self) -> Result<Event<C>, RtError> {
        poll_fn(|context| self.poll_event(context)).await
    }

    /// [`Driver::event`] as a poll: an event already queued goes at once; otherwise each turn takes what
    /// arrived, fires the timers due, sends, and hands out the next event, or arms the socket's readiness
    /// and the timer and parks.
    pub fn poll_event(&mut self, context: &mut Context<'_>) -> Poll<Result<Event<C>, RtError>> {
        if let Some(event) = self.endpoint.poll_event() {
            return Poll::Ready(Ok(event));
        }
        for _ in 0..TURNS {
            if let Err(error) = self.reconcile() {
                return Poll::Ready(Err(error));
            }
            if let Err(error) = self.drain() {
                return Poll::Ready(Err(error));
            }
            let now = match self.now() {
                Ok(now) => now,
                Err(error) => return Poll::Ready(Err(error)),
            };
            if self.endpoint.poll_timeout().is_some_and(|due| due <= now) {
                self.endpoint.handle_timeout(now);
            }
            let more = self.send(now);
            if let Some(event) = self.endpoint.poll_event() {
                return Poll::Ready(Ok(event));
            }
            if more {
                continue;
            }
            match self.poll_ready(context) {
                Ok(true) => continue,
                Ok(false) => {}
                Err(error) => return Poll::Ready(Err(error)),
            }
            let Some(due) = self.endpoint.poll_timeout() else {
                return Poll::Pending;
            };
            match self.poll_timer(context, due, now) {
                Ok(true) => {}
                Ok(false) => return Poll::Pending,
                Err(error) => return Poll::Ready(Err(error)),
            }
        }
        // The budget is spent with work still to do: come back after the shard's other tasks.
        context.waker().wake_by_ref();
        Poll::Pending
    }

    /// Polls the sockets' readiness waits, arming them as needed; whether one fired.
    fn poll_ready(&mut self, context: &mut Context<'_>) -> Result<bool, RtError> {
        let mut fired = false;
        let readable = self.readable.get_or_insert_with(|| self.socket.readable());
        if let Poll::Ready(outcome) = Pin::new(readable).poll(context) {
            self.readable = None;
            outcome?;
            fired = true;
        }
        if self.socket.pending() {
            let writable = self.writable.get_or_insert_with(|| self.socket.writable());
            if let Poll::Ready(outcome) = Pin::new(writable).poll(context) {
                self.writable = None;
                outcome?;
                fired = true;
            }
        }
        for peer in &mut self.peers {
            if !peer.ready {
                let readable = peer.readable.get_or_insert_with(|| peer.socket.readable());
                if let Poll::Ready(outcome) = Pin::new(readable).poll(context) {
                    peer.readable = None;
                    outcome?;
                    peer.ready = true;
                    fired = true;
                }
            }
            if peer.socket.pending() {
                let writable = peer.writable.get_or_insert_with(|| peer.socket.writable());
                if let Poll::Ready(outcome) = Pin::new(writable).poll(context) {
                    peer.writable = None;
                    outcome?;
                    fired = true;
                }
            }
        }
        Ok(fired)
    }

    /// Matches the peers' sockets to the endpoint's connections, when they changed: a socket for
    /// each connection that has none (bound to the listener's shared port and connected to its peer,
    /// its receive queue asked for what one connection's windows may need, the connection's windows
    /// moved to that queue's room), and none for a connection that went. A socket the platform
    /// refuses leaves its peer on the listener. At most the admission's bound on connections.
    fn reconcile(&mut self) -> Result<(), RtError> {
        let Some(local) = self.shared else {
            return Ok(());
        };
        let generation = self.endpoint.connection_generation();
        if generation == self.seen {
            return Ok(());
        }
        self.seen = generation;
        self.endpoint.established(&mut self.listed);
        let listed = &self.listed;
        self.peers.retain(|socket| {
            listed
                .iter()
                .any(|(peer, remote)| *peer == socket.peer && *remote == socket.remote)
        });
        let ask = usize::try_from(hyper_transport::socket_buffer_ask(
            self.endpoint.peer_demand(),
        ))
        .unwrap_or(usize::MAX);
        for &(peer, remote) in &self.listed {
            if self
                .peers
                .iter()
                .any(|socket| socket.peer == peer && socket.remote == remote)
            {
                continue;
            }
            let Ok(mut socket) = Batched::bind_shared(local, PEER_IO, false) else {
                continue;
            };
            if socket.connect(remote).is_err() {
                continue;
            }
            let reported = socket.socket().set_recv_buffer_bytes(ask)?;
            let room = hyper_transport::socket_room(u64::try_from(reported).unwrap_or(u64::MAX));
            if self.endpoint.set_peer_room(peer, room).is_err() {
                continue;
            }
            let at = self
                .peers
                .binary_search_by_key(&remote, |socket| socket.remote)
                .unwrap_or_else(|at| at);
            self.peers.insert(
                at,
                PeerSocket {
                    peer,
                    remote,
                    socket,
                    readable: None,
                    writable: None,
                    ready: true,
                },
            );
        }
        Ok(())
    }

    /// Polls the timer for `due`, arming it anew when the deadline moved; whether it fired.
    fn poll_timer(
        &mut self,
        context: &mut Context<'_>,
        due: Instant,
        now: Instant,
    ) -> Result<bool, RtError> {
        if self.timer.as_ref().is_none_or(|(armed, _)| *armed != due) {
            let wait =
                u64::try_from(due.saturating_duration_since(now).as_nanos()).unwrap_or(u64::MAX);
            self.timer = Some((due, sleep(wait)));
        }
        let Some((_, timer)) = self.timer.as_mut() else {
            return Ok(false);
        };
        match Pin::new(timer).poll(context) {
            Poll::Ready(outcome) => {
                self.timer = None;
                outcome?;
                self.fired(due)?;
                Ok(true)
            }
            Poll::Pending => Ok(false),
        }
    }

    /// The timer armed for `due` fired: its lateness folded into `G`, and the endpoint told.
    fn fired(&mut self, due: Instant) -> Result<(), RtError> {
        let woke = shard_now()?;
        // A full fold keeps the mean it has: 2^64 waits are past any process.
        let _ = self.late.on_wait(self.ns(due), woke);
        if let Some(granularity) = self.late.granularity() {
            self.endpoint.set_granularity(granularity);
        }
        Ok(())
    }

    /// Takes the datagrams that have arrived, a batch at a time, until none or [`TURNS`] batches: the
    /// listener's, and each peer socket's whose readiness fired.
    fn drain(&mut self) -> Result<(), RtError> {
        let (anchor_ns, anchor) = self.anchor;
        let endpoint = &mut self.endpoint;
        let mut deliver = |arrival: hyper_rt::udp::Arrival, bytes: &mut [u8]| {
            let at = anchor
                .checked_add(Duration::from_nanos(
                    arrival.at_ns.saturating_sub(anchor_ns),
                ))
                .unwrap_or(anchor);
            endpoint.handle_datagram(at, arrival.from, None, bytes);
        };
        for _ in 0..TURNS {
            if self.socket.receive(&mut deliver)? == 0 {
                break;
            }
        }
        for peer in &mut self.peers {
            if !peer.ready {
                continue;
            }
            let mut emptied = false;
            for _ in 0..TURNS {
                if peer.socket.receive(&mut deliver)? == 0 {
                    emptied = true;
                    break;
                }
            }
            // A socket left holding datagrams is drained again at the next turn without a wait.
            peer.ready = !emptied;
        }
        Ok(())
    }

    /// Moves what the endpoint transmits into the outboxes, each datagram into its peer's socket's or
    /// the listener's, and sends them; whether the turn's bound was reached with the endpoint likely
    /// to have more. A datagram whose outbox is full and stays so waits, held, until its socket can
    /// write: none is dropped.
    fn send(&mut self, now: Instant) -> bool {
        if let Some((to, bytes)) = self.held.take() {
            if !self.queue(to, &bytes) {
                self.held = Some((to, bytes));
                let _ = self.flush_all();
                return false;
            }
            self.outgoing = bytes;
        }
        let mut taken = 0;
        let mut more = false;
        while taken < TRANSMITS_A_TURN {
            let mut outgoing = std::mem::take(&mut self.outgoing);
            outgoing.clear();
            let Some(transmit) = self.endpoint.poll_transmit(now, &mut outgoing) else {
                self.outgoing = outgoing;
                break;
            };
            taken = taken.saturating_add(1);
            outgoing.truncate(transmit.size);
            if !self.queue(transmit.destination, &outgoing) {
                self.held = Some((transmit.destination, outgoing));
                break;
            }
            self.outgoing = outgoing;
            more = taken == TRANSMITS_A_TURN;
        }
        self.flush_all() && more
    }

    /// Queues `bytes` for `to` in its peer's socket's outbox, or the listener's; whether it had room.
    fn queue(&mut self, to: SocketAddr, bytes: &[u8]) -> bool {
        let socket = match self
            .peers
            .binary_search_by_key(&to, |peer| peer.remote)
            .ok()
            .and_then(|at| self.peers.get_mut(at))
        {
            Some(peer) => &mut peer.socket,
            None => &mut self.socket,
        };
        if socket.queue(to, bytes) {
            return true;
        }
        // Full: what it holds goes now, and the datagram is queued if that made room.
        socket.send() == Sent::Drained && socket.queue(to, bytes)
    }

    /// Sends what every outbox holds; whether all drained.
    fn flush_all(&mut self) -> bool {
        let mut drained = !self.socket.pending() || self.socket.send() == Sent::Drained;
        for peer in &mut self.peers {
            if peer.socket.pending() && peer.socket.send() == Sent::Blocked {
                drained = false;
            }
        }
        drained
    }
}
