//! Awaiting a socket's readiness through the shard's driver (§4.10a, §4.6): a future registers
//! one-shot interest — readable, or writable — with the current shard's driver on its first poll and
//! yields; the driver's completion (or the simulation fabric's wake) re-queues the task, and the next
//! poll returns ready so the caller retries its non-blocking syscall. One path serves both sockets:
//! the UDP datagram socket awaits readability (a real fd, or a simulated fabric port), and the TCP
//! stream awaits readability for `read`/`accept` and writability for a `write` whose send buffer
//! filled. The driver decides how the edge is watched (kqueue `EVFILT_READ`/`EVFILT_WRITE`, epoll
//! `EPOLLIN`/`EPOLLOUT`); the future is the same either way, which is why it lives here and not in the
//! socket modules. [`readable`] is public because a bridge queue's doorbell is the same edge: the
//! virtio-fs device (`slates-bridge-virtiofs`, §4.6) awaits the kick descriptor its VMM handed it —
//! an eventfd or a pipe — through the shard's driver exactly as a socket is awaited, and drains it
//! itself; the driver seam's doc names "the bridge queues" as a user of `wait` for this reason.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use crate::error::RtError;
use crate::registry;
use crate::waker::polling_task;

/// Which readiness edge a caller awaits.
#[derive(Clone, Copy, Debug)]
enum Interest {
    /// The socket has data to read, or a listener has a connection to accept.
    Readable,
    /// The socket has send-buffer space for a write (or connect) that returned `EAGAIN`/`EINPROGRESS`: a TCP
    /// write, or a UDP send the kernel had no room for (every platform's driver arms it: kqueue
    /// `EVFILT_WRITE`, epoll `EPOLLOUT`, IOCP's AFD send poll).
    Writable,
}

/// Awaits one readiness edge on `raw` through the shard's driver: it registers one-shot interest on
/// the first poll and yields; the driver's completion re-queues the task, and the next poll is ready
/// so the caller retries the non-blocking syscall (a spurious wake just retries).
#[derive(Debug)]
pub struct Ready {
    target: Target,
    interest: Interest,
    armed: bool,
}

/// What a readiness wait watches: an OS handle through the shard's driver, or a simulated socket through
/// its desk (docs/runtime.md §11).
#[derive(Clone, Copy, Debug)]
pub(crate) enum Target {
    /// A descriptor or socket the driver watches.
    Os(i32),
    /// A simulated socket's index on the shard's desk.
    Sim(u16),
}

impl Future for Ready {
    type Output = Result<(), RtError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), RtError>> {
        let Some(word) = polling_task(cx.waker()) else {
            // A foreign waker cannot be registered with the driver; degrade to a retry (the caller's loop copes).
            return Poll::Ready(Ok(()));
        };
        if self.armed {
            // Woken: by the driver, or by the loop handing back a registration the driver refused.
            return Poll::Ready(
                match registry::with_current(|ctx| ctx.take_interest_refusal(word)).flatten() {
                    Some(refusal) => Err(refusal),
                    None => Ok(()),
                },
            );
        }
        let writable = matches!(self.interest, Interest::Writable);
        let registered = match self.target {
            Target::Os(raw) => {
                registry::with_current(|ctx| ctx.register_interest(raw, writable, word))
            }
            Target::Sim(index) => Some(crate::sim::sim_register(index, writable, word)),
        };
        match registered {
            Some(Ok(())) => {
                self.armed = true;
                Poll::Pending
            }
            Some(Err(e)) => Poll::Ready(Err(e)),
            None => Poll::Ready(Err(RtError::NotOnShardThread)),
        }
    }
}

/// Awaits `raw`'s readability once (a real socket fd, a simulated fabric port, or a bridge
/// queue's doorbell descriptor).
pub async fn readable(raw: i32) -> Result<(), RtError> {
    ready(Target::Os(raw), false).await
}

/// Awaits `raw`'s writability once (a real socket whose send buffer filled, or a connect in progress).
pub(crate) async fn writable(raw: i32) -> Result<(), RtError> {
    ready(Target::Os(raw), true).await
}

/// One readiness edge of `target`.
pub(crate) fn ready(target: Target, writable: bool) -> Ready {
    Ready {
        target,
        interest: if writable {
            Interest::Writable
        } else {
            Interest::Readable
        },
        armed: false,
    }
}
