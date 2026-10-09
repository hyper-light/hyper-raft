//! Both busy and idle run_until_idle paths must deliver registered native readiness.
//! Exact datagram presence, not a delay or yield count, establishes that the I/O can complete.

// Test harness failures and serialization, as in the existing lifecycle fixtures.
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

/// One case's facts: the watchdog's expiry (or its guard's drop) stops the case, and the busy
/// task's start is seen. Each case has its own, so the cases run in parallel with no lock.
struct Flags {
    stop: AtomicBool,
    started: AtomicBool,
}

impl Flags {
    const fn new() -> Self {
        Self {
            stop: AtomicBool::new(false),
            started: AtomicBool::new(false),
        }
    }
}

static BUSY: Flags = Flags::new();
static QUIET: Flags = Flags::new();
static LATER: Flags = Flags::new();
// The existing channel/cross-thread fixture watchdog, used only to fail and clean up.
const WAIT: Duration = Duration::from_secs(5);
const PAYLOAD: &[u8] = b"owned loop readiness";

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
    flags: &'static Flags,
    done: Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl Watchdog {
    fn new(root: Waker, flags: &'static Flags) -> Result<Self, String> {
        let (done, ended) = channel();
        let thread = thread::Builder::new()
            .name("rt-owned-loop-watchdog".to_owned())
            .spawn(move || {
                if ended.recv_timeout(WAIT).is_err() {
                    flags.stop.store(true, Ordering::Release);
                    root.wake();
                }
            })
            .map_err(|error| format!("watchdog start: {error}"))?;
        Ok(Self {
            flags,
            done,
            thread: Some(thread),
        })
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        self.flags.stop.store(true, Ordering::Release);
        let _ = self.done.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

async fn receive(busy: bool, flags: &'static Flags) -> Result<bool, String> {
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
    let peer =
        std::net::UdpSocket::bind("127.0.0.1:0").map_err(|error| format!("peer bind: {error}"))?;
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
    let _watchdog = Watchdog::new(root, flags)?;
    if busy {
        hyper_rt::futures::spawn_detached(async move {
            flags.started.store(true, Ordering::Release);
            while !flags.stop.load(Ordering::Acquire) {
                hyper_rt::futures::yield_now().await;
            }
        })
        .map_err(|error| format!("busy task admission: {error}"))?;
        while !flags.started.load(Ordering::Acquire) {
            if flags.stop.load(Ordering::Acquire) {
                return Err("busy task did not start inside the watchdog".to_owned());
            }
            hyper_rt::futures::yield_now().await;
        }
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
        if flags.stop.load(Ordering::Acquire) {
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
    // Kernel data is now present. This root no longer self-wakes. Neither the watchdog's
    // wake nor readiness delivered only after stopping the busy task is successful I/O.
    poll_fn(|cx| {
        if flags.stop.load(Ordering::Acquire) {
            return Poll::Ready(Err(
                "registered ready data starved until watchdog cleanup".to_owned()
            ));
        }
        readiness
            .as_mut()
            .poll(cx)
            .map(|result| result.map_err(|error| format!("readiness: {error}")))
    })
    .await?;
    let before_stop = !flags.stop.load(Ordering::Acquire);
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

fn run_until_idle(busy: bool, flags: &'static Flags) {
    let mut rt = LocalRuntime::new(&config()).unwrap();
    let (done, received) = channel();
    rt.spawn(async move {
        let _ = done.send(receive(busy, flags).await);
    })
    .unwrap();
    rt.run_until_idle();
    let outcome = received.try_recv();
    // Early-return and watchdog failures also drop all task futures and join the watchdog
    // through its guard before a verdict. No socket or runtime task is left detached.
    drop(rt);
    assert_eq!(
        outcome,
        Ok(Ok(true)),
        "run_until_idle must deliver the already-present datagram before returning/cleanup"
    );
}

#[test]
#[cfg_attr(miri, ignore)] // Native socket readiness opens kqueue, epoll or AFD.
fn run_until_idle_delivers_readiness_while_another_task_keeps_yielding() {
    run_until_idle(true, &BUSY);
}

#[test]
#[cfg_attr(miri, ignore)] // Native socket readiness opens kqueue, epoll or AFD.
fn run_until_idle_does_not_skip_a_ready_socket_when_no_task_is_runnable() {
    run_until_idle(false, &QUIET);
}

#[test]
#[cfg_attr(miri, ignore)] // Native socket readiness opens kqueue, epoll or AFD.
fn idle_external_waits_return_intact_and_complete_on_a_later_run() {
    let flags = &LATER;
    let observed = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    observed.set_nonblocking(true).unwrap();
    let address = observed.local_addr().unwrap();
    let owned = observed.try_clone().unwrap().into();
    let peer = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    peer.set_nonblocking(true).unwrap();
    let mut rt = LocalRuntime::new(&config()).unwrap();
    let (armed, pending) = channel();
    let (done, received) = channel();
    rt.spawn(async move {
        let outcome = async {
            let socket =
                UdpSocket::adopt(owned).map_err(|error| format!("receiver adoption: {error}"))?;
            let mut readiness = pin!(socket.readable());
            let root = poll_fn(|cx| {
                Poll::Ready(match readiness.as_mut().poll(cx) {
                    Poll::Pending => Ok(cx.waker().clone()),
                    Poll::Ready(result) => Err(format!("empty socket must first wait: {result:?}")),
                })
            })
            .await?;
            let _watchdog = Watchdog::new(root, flags)?;
            let _ = armed.send(());
            poll_fn(|cx| {
                if flags.stop.load(Ordering::Acquire) {
                    return Poll::Ready(Err("idle external wait did not return".to_owned()));
                }
                readiness
                    .as_mut()
                    .poll(cx)
                    .map(|result| result.map_err(|error| format!("readiness: {error}")))
            })
            .await?;
            let before_stop = !flags.stop.load(Ordering::Acquire);
            let mut bytes = [0; PAYLOAD.len()];
            let delivered = socket
                .try_recv_from(&mut bytes)
                .map_err(|error| format!("ready receive: {error}"))?
                .ok_or("ready socket had no datagram")?;
            if bytes.get(..delivered.0) != Some(PAYLOAD) {
                return Err("the readiness delivery changed the datagram".to_owned());
            }
            Ok::<_, String>(before_stop)
        }
        .await;
        let _ = done.send(outcome);
    })
    .unwrap();
    let (sender, mut receiver) = hyper_rt::sync::channel::<u32>(1).unwrap();
    let (channel_done, channel_received) = channel();
    rt.spawn(async move {
        let _ = channel_done.send(receiver.recv().await);
    })
    .unwrap();
    rt.run_until_idle();
    let first = (
        pending.try_recv(),
        received.try_recv(),
        channel_received.try_recv(),
        flags.stop.load(Ordering::Acquire),
    );
    // On a broken infinite idle park the watchdog returns failure, not data. Dropping the
    // runtime before the assertion closes the sockets/tasks and joins that watchdog.
    if first
        != (
            Ok(()),
            Err(std::sync::mpsc::TryRecvError::Empty),
            Err(std::sync::mpsc::TryRecvError::Empty),
            false,
        )
    {
        drop(rt);
        panic!("idle external waits must remain pending without blocking: {first:?}");
    }
    assert_eq!(peer.send_to(PAYLOAD, address).unwrap(), PAYLOAD.len());
    let mut peeked = [0; PAYLOAD.len()];
    let present = loop {
        if flags.stop.load(Ordering::Acquire) {
            break Err("loopback data did not arrive inside the watchdog".to_owned());
        }
        match observed.peek_from(&mut peeked) {
            Ok((n, _)) if peeked.get(..n) == Some(PAYLOAD) => break Ok(()),
            Ok(other) => break Err(format!("loopback peek changed the datagram: {other:?}")),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => thread::yield_now(),
            Err(error) => break Err(format!("loopback peek: {error}")),
        }
    };
    if let Err(error) = present {
        drop(rt);
        panic!("{error}");
    }
    sender.try_send(9).unwrap();
    rt.run_until_idle();
    let outcomes = (received.try_recv(), channel_received.try_recv());
    drop(rt);
    assert_eq!(outcomes, (Ok(Ok(true)), Ok(Ok(9))));
}
