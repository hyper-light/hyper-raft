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
//!
//! **One set of buffers, and sends straight from the endpoint's.** Every socket receives into one
//! [`Ring`] the driver lends whichever it reads, so a socket a peer costs its descriptor and a few
//! headers, not a batch of 64 KiB buffers: a peer's socket had held two, and so carried two
//! datagrams a system call. The endpoint makes up to `SEGMENTS_A_TRANSMIT` of one connection's
//! datagrams at once, which the socket sends from the driver's buffer in one segmented call where
//! the kernel segments (`UDP_SEGMENT`), so a peer's bulk leaves, and arrives through `UDP_GRO`, as
//! a few large messages, where it left a datagram a slot of a two-slot outbox (quinn's driver and
//! `quinn-udp` send a connection's batch so; measured in `docs/transport.md` §4h).

use std::future::{Future, poll_fn};
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use hyper_rt::RtError;
use hyper_rt::futures::{Sleep, sleep};
use hyper_rt::readiness::Ready;
use hyper_rt::udp::{Batched, Io, IoStats, MAX_BATCH, Ring, UdpSocket};
use hyper_transport::{Budget, Classes, Directory, Endpoint, Event, Lateness, PeerId};

/// Derived: the turns one poll of [`Driver::event`] takes before it yields to the shard, the bound
/// hyper-tokio takes from tokio's cooperative budget (128 operations a task a poll,
/// `tokio::task::coop`), kept so a consumer moving between the two drivers keeps one step's work.
pub const TURNS: usize = 128;

/// The shard's clock now, nanoseconds.
fn shard_now() -> Result<u64, RtError> {
    hyper_rt::registry::with_current(|context| context.now_ns()).ok_or(RtError::NotOnShardThread)
}

/// Bound: the datagrams one turn takes from the endpoint and sends before the turn's other work:
/// one system call's most ([`MAX_BATCH`]).
const DATAGRAMS_A_TURN: usize = MAX_BATCH;

/// Format: the datagrams of one connection's the endpoint makes at once, `UDP_MAX_SEGMENTS`
/// (`include/linux/udp.h`), the most one segmented send carries; a socket that does not segment
/// sends them a datagram a call.
const SEGMENTS_A_TRANSMIT: usize = 64;

/// A transmit its socket took only part of: its rest waits for the socket to write, and nothing the
/// endpoint makes is sent past it.
struct Held {
    to: SocketAddr,
    bytes: Vec<u8>,
    /// The bytes of it sent.
    done: usize,
    /// Each datagram's size but the last's.
    segment: usize,
}

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
    /// A transmit its socket took only part of, waiting for it to write: at most one.
    held: Option<Held>,
    /// The buffer the endpoint makes each transmit in, kept between transmits.
    outgoing: Vec<u8>,
    /// The receive buffers every socket is lent as it is read.
    ring: Ring,
}

impl<C: Classes, B: Budget<C::Class>, D: Directory<Role = C::Role>> Driver<C, B, D> {
    /// A driver for `endpoint` on a socket bound to `address`, its port shared with a socket for
    /// each peer where the platform lets it (the module's doc). Called on a shard thread.
    pub fn bind(endpoint: Endpoint<C, B, D>, address: SocketAddr, io: Io) -> Result<Self, RtError> {
        let ring = Ring::new(io)?;
        match Batched::bind_shared_lent(address, false) {
            Ok(socket) => {
                let local = socket.local_addr()?;
                let mut driver = Self::with_ring(endpoint, socket, ring)?;
                driver.shared = Some(local);
                Ok(driver)
            }
            Err(RtError::BadConfig { .. }) => Self::with_ring(
                endpoint,
                Batched::lent(UdpSocket::bind(address)?, false)?,
                ring,
            ),
            Err(error) => Err(error),
        }
    }

    /// A driver for `endpoint` on `socket`, whose sockets receive into a ring of `socket`'s batch.
    /// Called on a shard thread.
    pub fn new(endpoint: Endpoint<C, B, D>, socket: Batched) -> Result<Self, RtError> {
        let ring = Ring::new(Io {
            batch: socket.batch(),
        })?;
        Self::with_ring(endpoint, socket, ring)
    }

    #[allow(
        clippy::disallowed_methods,
        reason = "the runtime adapter anchors the endpoint's Instant to the shard's clock once"
    )]
    fn with_ring(
        mut endpoint: Endpoint<C, B, D>,
        socket: Batched,
        ring: Ring,
    ) -> Result<Self, RtError> {
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
            ring,
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

    /// What the sockets of the peers it holds now have done, summed; a flag is set where any of
    /// theirs is.
    pub fn peer_stats(&self) -> IoStats {
        self.peers.iter().fold(IoStats::default(), |sum, peer| {
            sum.plus(&peer.socket.stats())
        })
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
        // A socket is awaited writable while its outbox holds datagrams or it holds the transmit
        // the driver waits on.
        let held = self.held.as_ref().map(|held| held.to);
        let listener_holds = held.is_some_and(|to| {
            self.peers
                .binary_search_by_key(&to, |peer| peer.remote)
                .is_err()
        });
        if self.socket.pending() || listener_holds {
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
            if peer.socket.pending() || held == Some(peer.remote) {
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
            let Ok(mut socket) = Batched::bind_shared_lent(local, false) else {
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
            if self.socket.receive_with(&mut self.ring, &mut deliver)? == 0 {
                break;
            }
        }
        for peer in &mut self.peers {
            if !peer.ready {
                continue;
            }
            let mut emptied = false;
            for _ in 0..TURNS {
                if peer.socket.receive_with(&mut self.ring, &mut deliver)? == 0 {
                    emptied = true;
                    break;
                }
            }
            // A socket left holding datagrams is drained again at the next turn without a wait.
            peer.ready = !emptied;
        }
        Ok(())
    }

    /// Sends what the endpoint transmits, each transmit straight from the driver's buffer on its
    /// peer's socket or the listener ([`Batched::send_segments`]); whether the turn's bound was
    /// reached with the endpoint likely to have more. A transmit its socket took only part of waits,
    /// held, until the socket can write, and nothing is sent past it: none is dropped.
    fn send(&mut self, now: Instant) -> bool {
        if let Some(held) = self.held.take()
            && let Some(rest) = self.send_held(held)
        {
            self.held = Some(rest);
            return false;
        }
        let mut datagrams = 0usize;
        let mut more = false;
        while datagrams < DATAGRAMS_A_TURN {
            let mut outgoing = std::mem::take(&mut self.outgoing);
            outgoing.clear();
            let Some(transmit) =
                self.endpoint
                    .poll_transmit_batch(now, SEGMENTS_A_TRANSMIT, &mut outgoing)
            else {
                self.outgoing = outgoing;
                break;
            };
            outgoing.truncate(transmit.size);
            let segment = transmit.segment_size.unwrap_or(transmit.size).max(1);
            datagrams = datagrams.saturating_add(outgoing.len().div_ceil(segment));
            let held = Held {
                to: transmit.destination,
                bytes: outgoing,
                done: 0,
                segment,
            };
            if let Some(rest) = self.send_held(held) {
                self.held = Some(rest);
                return false;
            }
            more = datagrams >= DATAGRAMS_A_TURN;
        }
        more
    }

    /// Sends what `held` has left on its socket: none left, its buffer kept for the next transmit,
    /// or the rest when the socket took no more.
    fn send_held(&mut self, mut held: Held) -> Option<Held> {
        let rest = held.bytes.get(held.done..).unwrap_or(&[]);
        let sent = self
            .socket_for(held.to)
            .send_segments(held.to, rest, held.segment);
        held.done = held
            .done
            .saturating_add(sent.saturating_mul(held.segment))
            .min(held.bytes.len());
        if held.done < held.bytes.len() {
            return Some(held);
        }
        self.outgoing = held.bytes;
        None
    }

    /// The socket `to`'s datagrams leave by: its peer's, or the listener's.
    fn socket_for(&mut self, to: SocketAddr) -> &mut Batched {
        match self
            .peers
            .binary_search_by_key(&to, |peer| peer.remote)
            .ok()
            .and_then(|at| self.peers.get_mut(at))
        {
            Some(peer) => &mut peer.socket,
            None => &mut self.socket,
        }
    }
}
