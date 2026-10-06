//! Readiness waits inside one task's combinators (mantle's final review of hyper-rt, findings 1 and 2):
//! each wait is its own, and only a fire makes it ready.
//!
//! - Finding 1: two waits of one task on one handle and direction shared the table's node (keyed by the
//!   task's word); a race's losing wait, dropped, withdrew it, and the join's wait on the same socket
//!   never woke.
//! - Finding 2: a wait re-polled for any reason reported ready, so a timer that woke a biased race made
//!   the idle socket's wait win, and that wait's node was never withdrawn.

// Test harness code: a panic here is a failed test (CLAUDE.md §1).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::disallowed_macros,
    clippy::disallowed_methods,
    clippy::missing_panics_doc
)]

use hyper_rt::combine::{Either, join2, race2};
use hyper_rt::futures::{sleep, yield_now};
use hyper_rt::registry;
use hyper_rt::runtime::{LocalRuntime, RuntimeConfig};
use hyper_rt::udp::{Ipv4Addr, SocketAddr, UdpSocket};

/// Shape: the waits the shard holds at once; the leak test runs three times this many races.
const WAITS: usize = 16;

fn config() -> RuntimeConfig {
    RuntimeConfig {
        shards: 1,
        tasks_per_shard: 16,
        timers_per_shard: 16,
        interests_per_shard: WAITS,
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

/// Shape: a sleep that loses to nothing here: no datagram is sent until it has ended.
const TICK_NS: u64 = 1_000_000;
/// Shape: how long a wait that should have woken is given before the test fails rather than hangs.
const PATIENCE_NS: u64 = 10_000_000_000;

fn loopback() -> SocketAddr {
    SocketAddr::from((Ipv4Addr::LOCALHOST, 0))
}

fn waits_held() -> usize {
    registry::with_current(|ctx| ctx.waits_held()).unwrap()
}

/// Do: one task joins a read wait on a socket with a race of a second read wait on the same socket and a
/// sleep; the sleep wins the race, then a datagram is sent to the socket. Expect: the join's read wait
/// wakes, and no wait is held after.
#[test]
fn a_race_lost_on_a_handle_leaves_the_joins_wait_on_it() {
    let mut rt = LocalRuntime::new(&config()).unwrap();
    rt.block_on(async {
        let ours = UdpSocket::bind(loopback()).unwrap();
        let peer = UdpSocket::bind(loopback()).unwrap();
        let to = ours.local_addr().unwrap();
        let joined = join2(ours.readable(), async {
            let raced = race2(ours.readable(), sleep(TICK_NS)).await;
            assert!(
                matches!(raced, Either::Second(Ok(()))),
                "nothing was sent: the sleep wins the race, got {raced:?}"
            );
            peer.send_to(b"x", to).unwrap();
        });
        match race2(joined, sleep(PATIENCE_NS)).await {
            Either::First((read, ())) => read.unwrap(),
            Either::Second(_) => {
                panic!("the join's read wait never woke: the race's loser took it")
            }
        }
        yield_now().await;
        assert_eq!(waits_held(), 0, "every wait gave its slot back");
    })
    .unwrap();
}

/// Do: race a read wait on an idle socket against a sleep, three times as often as the shard has wait
/// slots. Expect: the sleep wins every race (the timer's wake does not make the unfired wait ready), and
/// every losing wait gives its slot back, so the bound is never reached.
#[test]
fn a_wait_woken_by_a_timer_stays_unready_and_leaves_when_it_loses() {
    let mut rt = LocalRuntime::new(&config()).unwrap();
    rt.block_on(async {
        let idle = UdpSocket::bind(loopback()).unwrap();
        for round in 0..WAITS * 3 {
            let raced = race2(idle.readable(), sleep(TICK_NS)).await;
            assert!(
                matches!(raced, Either::Second(Ok(()))),
                "round {round}: the idle socket's wait won ({raced:?})"
            );
            // The loser's withdrawal is applied by the loop after this poll: one yield, then its slot is back.
            yield_now().await;
            assert_eq!(waits_held(), 0, "round {round}: the loser's slot came back");
        }
    })
    .unwrap();
}

/// Windows, mantle's final review, finding 4: a dropped driver reclaims every AFD poll block without an
/// unbounded wait. Do: in each of several runtimes, lose a race of a read wait (its poll cancelled when the
/// last waiter leaves) and leave a task waiting on a second socket when the runtime ends (its poll still in
/// flight at the driver's drop). Expect: the drop's zero-timeout drain found every block, so none was left
/// to the kernel.
#[cfg(windows)]
#[test]
fn a_dropped_driver_leaves_no_afd_poll_to_the_kernel() {
    for _ in 0..8 {
        let mut rt = LocalRuntime::new(&config()).unwrap();
        rt.block_on(async {
            let idle = UdpSocket::bind(loopback()).unwrap();
            let raced = race2(idle.readable(), sleep(TICK_NS)).await;
            assert!(matches!(raced, Either::Second(Ok(()))));
            let waiting = UdpSocket::bind(loopback()).unwrap();
            hyper_rt::futures::spawn_detached(async move {
                let _ = waiting.readable().await;
            })
            .unwrap();
            yield_now().await;
        })
        .unwrap();
        drop(rt);
    }
    assert_eq!(hyper_rt::iocp::afd_blocks_left(), 0);
}
