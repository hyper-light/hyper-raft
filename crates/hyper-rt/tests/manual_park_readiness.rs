//! A skipped public park must not postpone nonblocking readiness retrieval forever.
//! Native Pending and exact kernel-peek facts precede the repeated step/park calls.

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
use std::sync::mpsc::{Sender, TryRecvError, channel};
use std::task::{Poll, Waker};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use hyper_rt::runtime::{LocalRuntime, RuntimeConfig};
use hyper_rt::udp::UdpSocket;

// The existing native channel/readiness fixtures' failure bound, never an ordering delay.
const WAIT: Duration = Duration::from_secs(5);
const PAYLOAD: &[u8] = b"manual step delivers ready data";
static STOP_BUSY: AtomicBool = AtomicBool::new(false);
static BUSY_STARTED: AtomicBool = AtomicBool::new(false);

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
    fn new(waiter: Waker) -> Result<Self, String> {
        let (done, ended) = channel();
        let thread = thread::Builder::new()
            .name("rt-manual-park-watchdog".to_owned())
            .spawn(move || {
                if ended.recv_timeout(WAIT).is_err() {
                    STOP_BUSY.store(true, Ordering::Release);
                    waiter.wake();
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

async fn receive() -> Result<bool, String> {
    let observed = std::net::UdpSocket::bind("127.0.0.1:0")
        .map_err(|error| format!("receiver bind: {error}"))?;
    observed
        .set_nonblocking(true)
        .map_err(|error| format!("nonblocking receiver: {error}"))?;
    // Both descriptors observe the same receive queue; peek does not consume the datagram.
    let socket = UdpSocket::adopt(
        observed
            .try_clone()
            .map_err(|error| format!("receiver duplicate: {error}"))?
            .into(),
    )
    .map_err(|error| format!("receiver adoption: {error}"))?;
    let peer =
        std::net::UdpSocket::bind("127.0.0.1:0").map_err(|error| format!("peer bind: {error}"))?;
    peer.set_nonblocking(true)
        .map_err(|error| format!("nonblocking peer: {error}"))?;
    let mut readiness = pin!(socket.readable());
    let waiter = poll_fn(|cx| {
        Poll::Ready(match readiness.as_mut().poll(cx) {
            Poll::Pending => Ok(cx.waker().clone()),
            Poll::Ready(result) => Err(format!("empty socket must first wait: {result:?}")),
        })
    })
    .await?;
    let _watchdog = Watchdog::new(waiter)?;
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
    let sent = peer
        .send_to(
            PAYLOAD,
            observed
                .local_addr()
                .map_err(|error| format!("receiver address: {error}"))?,
        )
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
    // Only the separate task keeps self-waking now. This waiter must receive driver readiness
    // while that task is still busy; a late watchdog wake is explicitly a failure.
    poll_fn(|cx| {
        if STOP_BUSY.load(Ordering::Acquire) {
            return Poll::Ready(Err(
                "step/park did not deliver readiness before cleanup".to_owned()
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
    Ok(before_stop)
}

#[test]
#[cfg_attr(miri, ignore)] // Native socket readiness opens kqueue, epoll or AFD.
fn a_park_skipped_for_ready_local_work_does_not_reset_the_readiness_age() {
    STOP_BUSY.store(false, Ordering::Release);
    BUSY_STARTED.store(false, Ordering::Release);
    let mut rt = LocalRuntime::new(&config()).unwrap();
    let (completed, received) = channel();
    rt.spawn(async move {
        let _ = completed.send(receive().await);
    })
    .expect("socket task admission");
    let started = Instant::now();
    let outcome = loop {
        // The cooperative sibling leaves local work ready. This public park
        // must return without sleeping, and cannot count as a driver retrieval.
        let _ = rt.step();
        match received.try_recv() {
            Ok(result) => break result,
            Err(TryRecvError::Disconnected) => break Err("socket task disappeared".to_owned()),
            Err(TryRecvError::Empty) => {}
        }
        if started.elapsed() >= WAIT {
            break Err("manual step/park exhausted the native fixture watchdog".to_owned());
        }
        // Check completion before this call: successful root/child cleanup may
        // leave no pending work at all, when an unbounded park would be valid.
        rt.park(None);
    };
    // Cancelling the runtime drops its socket task and joins that task's watchdog on failure.
    // Cleanup cannot turn an eventual post-stop datagram into a successful result.
    drop(rt);
    assert!(
        outcome.expect("public socket delivery"),
        "I/O must complete before the watchdog stops the yielding task"
    );
}
