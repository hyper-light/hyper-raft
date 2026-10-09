//! LocalRuntime::block_on must deliver socket readiness while another task keeps yielding.
//! The watchdog wakes the root and stops the busy task on failure; it does not supply readiness.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::disallowed_macros,
    clippy::disallowed_methods,
    clippy::missing_panics_doc
)]

use std::future::{Future, poll_fn};
use std::pin::pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::task::{Poll, Waker};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use hyper_rt::runtime::{LocalRuntime, RuntimeConfig};
use hyper_rt::udp::UdpSocket;

// The existing channel and cross-thread wake fixtures' failure bound, never an ordering delay.
const WAIT: Duration = Duration::from_secs(5);
static STOP_BUSY: AtomicBool = AtomicBool::new(false);
static BUSY_STARTED: AtomicBool = AtomicBool::new(false);
const PAYLOAD: &[u8] = b"ready while another task yields";

fn config() -> RuntimeConfig {
    RuntimeConfig {
        shards: 1,
        tasks_per_shard: 64,
        timers_per_shard: 64,
        interests_per_shard: 64,
        ring_entries: 64,
        step_budget_ns: 1_000_000_000,
        timer_tick_ns: 100_000,
        batch: 64,
        pin: false,
        cores: Vec::new(),
        page_bytes: 4096,
        spin_ns: 0,
        wake_tracking: None,
    }
}

struct Watchdog {
    done: Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl Watchdog {
    fn new(root: Waker) -> Result<Self, String> {
        let (done, ended) = channel();
        let thread = thread::Builder::new()
            .name("rt-readiness-watchdog".to_owned())
            .spawn(move || {
                if ended.recv_timeout(WAIT).is_err() {
                    STOP_BUSY.store(true, Ordering::Release);
                    // The root can report failure even if the socket's readiness never wakes it.
                    root.wake();
                }
            })
            .map_err(|error| format!("watchdog start: {error}"))?;
        Ok(Self {
            done,
            thread: Some(thread),
        })
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        STOP_BUSY.store(true, Ordering::Release);
        let _ = self.done.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[test]
#[cfg_attr(miri, ignore)] // Native socket readiness opens kqueue, epoll or AFD.
fn block_on_delivers_readiness_before_a_cooperatively_busy_task_is_stopped() {
    STOP_BUSY.store(false, Ordering::Release);
    BUSY_STARTED.store(false, Ordering::Release);
    let mut rt = LocalRuntime::new(&config()).unwrap();
    let outcome = rt.block_on(async {
        // A duplicated descriptor observes the same kernel receive queue without consuming it.
        // Adoption is the public socket-activation API already exercised by tests/udp.rs.
        let observed = std::net::UdpSocket::bind("127.0.0.1:0")
            .map_err(|error| format!("receiver bind: {error}"))?;
        observed
            .set_nonblocking(true)
            .map_err(|error| format!("nonblocking receiver: {error}"))?;
        let socket = UdpSocket::adopt(
            observed
                .try_clone()
                .map_err(|error| format!("receiver duplicate: {error}"))?
                .into(),
        )
        .map_err(|error| format!("receiver adoption: {error}"))?;
        let peer = std::net::UdpSocket::bind("127.0.0.1:0")
            .map_err(|error| format!("peer bind: {error}"))?;
        peer.set_nonblocking(true)
            .map_err(|error| format!("nonblocking peer: {error}"))?;
        let mut readiness = pin!(socket.readable());
        let root = poll_fn(|cx| {
            Poll::Ready(match readiness.as_mut().poll(cx) {
                Poll::Pending => Ok(cx.waker().clone()),
                Poll::Ready(result) => Err(format!("empty socket must first wait: {result:?}")),
            })
        })
        .await?;
        let _watchdog = Watchdog::new(root)?;
        hyper_rt::futures::spawn_detached(async {
            BUSY_STARTED.store(true, Ordering::Release);
            while !STOP_BUSY.load(Ordering::Acquire) {
                hyper_rt::futures::yield_now().await;
            }
        })
        .map_err(|error| format!("busy task admission: {error}"))?;
        while !BUSY_STARTED.load(Ordering::Acquire) {
            if STOP_BUSY.load(Ordering::Acquire) {
                return Err("busy task did not start inside the watchdog".to_owned());
            }
            hyper_rt::futures::yield_now().await;
        }
        let address = observed
            .local_addr()
            .map_err(|error| format!("receiver address: {error}"))?;
        let sent = peer
            .send_to(PAYLOAD, address)
            .map_err(|error| format!("loopback send: {error}"))?;
        if sent != PAYLOAD.len() {
            return Err("loopback send did not accept the complete datagram".to_owned());
        }
        let mut peeked = [0; PAYLOAD.len()];
        loop {
            if STOP_BUSY.load(Ordering::Acquire) {
                return Err("loopback data did not arrive inside the watchdog".to_owned());
            }
            match observed.peek_from(&mut peeked) {
                Ok((n, _)) if peeked.get(..n) == Some(PAYLOAD) => break,
                Ok(other) => return Err(format!("loopback peek changed the datagram: {other:?}")),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    hyper_rt::futures::yield_now().await;
                }
                Err(error) => return Err(format!("loopback peek: {error}")),
            }
        }
        // The root no longer self-wakes. Only the other task yields continuously. The
        // watchdog's eventual wake is a failure, never a successful readiness delivery.
        poll_fn(|cx| {
            if STOP_BUSY.load(Ordering::Acquire) {
                return Poll::Ready(Err(
                    "readiness starved until the busy task was stopped".to_owned()
                ));
            }
            readiness
                .as_mut()
                .poll(cx)
                .map(|result| result.map_err(|error| format!("readiness: {error}")))
        })
        .await?;
        let before_stop = !STOP_BUSY.load(Ordering::Acquire);
        let mut received = [0; PAYLOAD.len()];
        let delivered = socket
            .try_recv_from(&mut received)
            .map_err(|error| format!("ready receive: {error}"))?
            .ok_or("ready socket had no datagram")?;
        if received.get(..delivered.0) != Some(PAYLOAD) {
            return Err("the readiness delivery changed the datagram".to_owned());
        }
        Ok::<_, String>(before_stop)
    });
    // block_on cancels its remaining tasks; dropping the runtime closes every owned socket
    // before the public verdict. The watchdog guard joins its thread on every root exit.
    drop(rt);
    assert!(
        outcome
            .expect("block_on completed")
            .expect("public socket delivery"),
        "the I/O completed before the watchdog stopped the yielding task"
    );
}
