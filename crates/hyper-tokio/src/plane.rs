//! The datagram plane's socket: its own, beside the endpoint's (note 32 §3.5).

use std::future::poll_fn;
use std::net::SocketAddr;
use std::task::{Context, Poll};

use hyper_datagram::{Fence, Opened, PeerId, Plane, Refusal};

use crate::Error;
use crate::socket::{Io, IoStats, Socket};

/// Carries a [`Plane`]'s datagrams on a UDP socket on tokio. The owner keeps the plane and lends
/// it to each call; the socket keeps only its bounded outbox and receive buffers.
pub struct PlaneSocket {
    socket: Socket,
}

impl PlaneSocket {
    /// A socket bound to `address`, within a tokio runtime with I/O enabled.
    pub fn bind(address: SocketAddr, io: Io) -> Result<Self, Error> {
        let socket = std::net::UdpSocket::bind(address)?;
        Self::new(socket, io)
    }

    /// The plane's socket on `socket`, which it takes onto tokio's reactor.
    pub fn new(socket: std::net::UdpSocket, io: Io) -> Result<Self, Error> {
        Ok(Self {
            socket: Socket::new(socket, io)?,
        })
    }

    /// The address the socket is bound to.
    pub fn local_addr(&self) -> Result<SocketAddr, Error> {
        self.socket.local_addr()
    }

    /// What the socket has done.
    pub fn stats(&self) -> IoStats {
        self.socket.stats()
    }

    /// Seals what `plane` has queued and sends it: each peer's datagram to the address `route`
    /// names. A peer `route` does not know, or one the plane refuses to seal for (its key past
    /// its confidentiality limit, say), is handed to `refused`; the messages are dropped either
    /// way, as the plane retransmits nothing. Past the outbox's bound, with the socket full, a
    /// datagram is dropped and counted ([`IoStats::outbox_full`]); what the socket would not take
    /// yet goes at the next [`PlaneSocket::flush`] or [`PlaneSocket::receive`].
    pub fn flush(
        &mut self,
        plane: &mut Plane,
        mut route: impl FnMut(PeerId) -> Option<SocketAddr>,
        mut refused: impl FnMut(PeerId, Option<Refusal>),
    ) {
        let _ = self.socket.send();
        let socket = &mut self.socket;
        plane.flush(|peer, sealed| match sealed {
            Err(refusal) => refused(peer, Some(refusal)),
            Ok(datagram) => match route(peer) {
                None => refused(peer, None),
                Some(to) => {
                    if socket.slot().is_none() {
                        let _ = socket.send();
                    }
                    socket.queue(to, datagram);
                }
            },
        });
        let _ = self.socket.send();
    }

    /// Waits for datagrams and opens one batch of them with `plane` under `fence`, handing each
    /// result to `deliver` with the address it came from; returns how many. Sends what the outbox
    /// holds whenever the socket takes it. Cancel-safe: a batch is taken and delivered without an
    /// await in between.
    pub async fn receive(
        &mut self,
        plane: &mut Plane,
        fence: &dyn Fence,
        mut deliver: impl FnMut(SocketAddr, Result<Opened<'_>, Refusal>),
    ) -> Result<usize, Error> {
        poll_fn(|context| self.poll_receive(context, plane, fence, &mut deliver)).await
    }

    fn poll_receive(
        &mut self,
        context: &mut Context<'_>,
        plane: &mut Plane,
        fence: &dyn Fence,
        deliver: &mut impl FnMut(SocketAddr, Result<Opened<'_>, Refusal>),
    ) -> Poll<Result<usize, Error>> {
        for _ in 0..crate::TURNS {
            let ready = match self.socket.poll_ready(context, self.socket.pending()) {
                Poll::Ready(ready) => ready,
                Poll::Pending => return Poll::Pending,
            };
            if ready.writable {
                let _ = self.socket.send();
            }
            if ready.readable {
                let count = self.socket.receive(|from, datagram| {
                    deliver(from, plane.open(datagram, fence));
                })?;
                if count > 0 {
                    return Poll::Ready(Ok(count));
                }
            }
        }
        context.waker().wake_by_ref();
        Poll::Pending
    }
}
