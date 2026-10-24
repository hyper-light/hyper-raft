//! DIAG (hunt branch only): what Windows does with a UDP socket after an ICMP port unreachable, step by
//! step, printed. No assertion decides anything here; every wait is bounded so nothing hangs.
#![cfg(windows)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::disallowed_macros,
    clippy::disallowed_methods,
    clippy::missing_panics_doc,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::too_many_lines,
    clippy::cognitive_complexity,
    unsafe_code
)]

use std::time::Instant;

use hyper_rt::combine::{Either, join2, race2};
use hyper_rt::futures::{sleep, yield_now};
use hyper_rt::runtime::{LocalRuntime, RuntimeConfig};
use hyper_rt::udp::{Batched, Io, Ipv4Addr, Queued, Ring, Sent, SocketAddr, UdpSocket};

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
        latency_objective_ns: None,
    }
}

const BOUND_NS: u64 = 1_500_000_000;

fn lo() -> SocketAddr {
    SocketAddr::from((Ipv4Addr::LOCALHOST, 0))
}

/// A port no socket holds: the probe and target sockets are bound first so neither can be given it.
fn closed_port() -> SocketAddr {
    UdpSocket::bind(lo()).unwrap().local_addr().unwrap()
}

fn recv_line(tag: &str, socket: &UdpSocket) -> Option<usize> {
    let mut buf = [0u8; 2048];
    let before = socket.astray_reports();
    let fionread = socket.queued_bytes();
    let got = socket.try_recv_from(&mut buf);
    println!(
        "DIAG {tag}: fionread-before {fionread:?} try_recv {got:?} astray {} -> {}",
        before,
        socket.astray_reports()
    );
    got.ok().flatten().map(|(n, _)| n)
}

/// Waits for `socket` readable or the bound; prints which and how long.
async fn readable_bounded(tag: &str, socket: &UdpSocket) -> bool {
    let start = Instant::now();
    let fired = match race2(socket.readable(), sleep(BOUND_NS)).await {
        Either::First(outcome) => {
            println!("DIAG {tag}: readable fired {outcome:?} after {:?}", start.elapsed());
            true
        }
        Either::Second(_) => {
            println!("DIAG {tag}: readable NOT fired in {:?}", start.elapsed());
            false
        }
    };
    println!("DIAG {tag}: fionread now {:?}", socket.queued_bytes());
    fired
}

/// Polls FIONREAD until it reaches `at_least` or the bound.
async fn fionread_at_least(tag: &str, socket: &UdpSocket, at_least: usize) {
    let start = Instant::now();
    loop {
        let now = socket.queued_bytes().unwrap_or(0);
        if now >= at_least {
            println!("DIAG {tag}: fionread {now} after {:?}", start.elapsed());
            return;
        }
        if start.elapsed().as_nanos() as u64 > BOUND_NS {
            println!("DIAG {tag}: fionread stuck at {now} after {:?}", start.elapsed());
            return;
        }
        let _ = sleep(1_000_000).await;
    }
}

#[test]
fn d1_report_alone() {
    let mut rt = LocalRuntime::new(&config()).unwrap();
    rt.block_on(async {
        let a = UdpSocket::bind(lo()).unwrap();
        let closed = closed_port();
        println!("DIAG d1: a {:?} closed {closed:?}", a.local_addr());
        println!("DIAG d1: send {:?}", a.try_send_to(b"astray", closed));
        readable_bounded("d1 after send to closed", &a).await;
        recv_line("d1 first recv", &a);
        recv_line("d1 second recv", &a);
        readable_bounded("d1 readable after the report taken", &a).await;
    })
    .unwrap();
}

#[test]
fn d2_report_then_data_plain() {
    let mut rt = LocalRuntime::new(&config()).unwrap();
    rt.block_on(async {
        let a = UdpSocket::bind(lo()).unwrap();
        let b = UdpSocket::bind(lo()).unwrap();
        let closed = closed_port();
        println!("DIAG d2: send astray {:?}", a.try_send_to(b"astray", closed));
        readable_bounded("d2 after send to closed", &a).await;
        let to = a.local_addr().unwrap();
        println!("DIAG d2: b send {:?}", b.try_send_to(b"data-one", to));
        println!("DIAG d2: b send {:?}", b.try_send_to(b"data-two", to));
        fionread_at_least("d2 data queued", &a, 16).await;
        for round in 0..5 {
            if recv_line(&format!("d2 recv {round}"), &a).is_none() && round >= 2 {
                break;
            }
        }
        readable_bounded("d2 readable at the end", &a).await;
        recv_line("d2 recv after readable", &a);
    })
    .unwrap();
}

#[test]
fn d3_data_then_report_plain() {
    let mut rt = LocalRuntime::new(&config()).unwrap();
    rt.block_on(async {
        let a = UdpSocket::bind(lo()).unwrap();
        let b = UdpSocket::bind(lo()).unwrap();
        let closed = closed_port();
        let to = a.local_addr().unwrap();
        println!("DIAG d3: b send {:?}", b.try_send_to(b"data-one", to));
        fionread_at_least("d3 data queued", &a, 8).await;
        println!("DIAG d3: send astray {:?}", a.try_send_to(b"astray", closed));
        let _ = sleep(300_000_000).await;
        println!("DIAG d3: fionread after 300 ms {:?}", a.queued_bytes());
        for round in 0..5 {
            if recv_line(&format!("d3 recv {round}"), &a).is_none() && round >= 2 {
                break;
            }
        }
        readable_bounded("d3 readable at the end", &a).await;
    })
    .unwrap();
}

#[test]
fn d4_report_then_data_batched_queued() {
    let mut rt = LocalRuntime::new(&config()).unwrap();
    rt.block_on(async {
        let mut l = Batched::bind_lent(lo(), true).unwrap();
        let mut ring = Ring::new(Io { batch: 64 }).unwrap();
        let b = UdpSocket::bind(lo()).unwrap();
        let closed = closed_port();
        l.queue(closed, b"astray");
        println!("DIAG d4: send {:?} stats {:?}", l.send(), l.stats());
        readable_bounded("d4 after send to closed", l.socket()).await;
        let to = l.local_addr().unwrap();
        for _ in 0..3 {
            let _ = b.try_send_to(b"data-xyz", to);
        }
        fionread_at_least("d4 data queued", l.socket(), 24).await;
        let mut total = 0usize;
        for round in 0..6 {
            let began = hyper_rt::futures::now_ns();
            let mut got = 0usize;
            let out = l.receive_queued(&mut ring, began, 128, |_, _| got += 1);
            total += got;
            println!(
                "DIAG d4 receive_queued {round}: {out:?} delivered {got} total {total} fionread {:?} stats {:?}",
                l.socket().queued_bytes(),
                l.stats()
            );
            match out {
                Ok((_, Queued::Emptied)) => {
                    if total >= 3 {
                        break;
                    }
                    if !readable_bounded("d4 readable after Emptied", l.socket()).await {
                        break;
                    }
                }
                Ok((_, Queued::More)) => {}
                Err(_) => break,
            }
        }
        println!("DIAG d4: total delivered {total} of 3");
    })
    .unwrap();
}

#[test]
fn d5_report_taken_then_readable_armed_then_data() {
    let mut rt = LocalRuntime::new(&config()).unwrap();
    rt.block_on(async {
        let a = UdpSocket::bind(lo()).unwrap();
        let b = UdpSocket::bind(lo()).unwrap();
        let closed = closed_port();
        println!("DIAG d5: send astray {:?}", a.try_send_to(b"astray", closed));
        readable_bounded("d5 after send to closed", &a).await;
        recv_line("d5 recv takes the report?", &a);
        let to = a.local_addr().unwrap();
        let start = Instant::now();
        let outcome = race2(
            join2(a.readable(), async {
                yield_now().await;
                b.try_send_to(b"data-one", to)
            }),
            sleep(BOUND_NS),
        )
        .await;
        println!(
            "DIAG d5: armed readable then data: {outcome:?} after {:?}",
            start.elapsed()
        );
        recv_line("d5 recv after", &a);
    })
    .unwrap();
}

#[test]
fn d6_readable_armed_then_report() {
    let mut rt = LocalRuntime::new(&config()).unwrap();
    rt.block_on(async {
        let a = UdpSocket::bind(lo()).unwrap();
        let closed = closed_port();
        let start = Instant::now();
        let outcome = race2(
            join2(a.readable(), async {
                yield_now().await;
                a.try_send_to(b"astray", closed)
            }),
            sleep(BOUND_NS),
        )
        .await;
        println!(
            "DIAG d6: armed readable then report: {outcome:?} after {:?}",
            start.elapsed()
        );
        recv_line("d6 recv", &a);
        recv_line("d6 recv again", &a);
    })
    .unwrap();
}

#[test]
fn d7_send_after_report() {
    let mut rt = LocalRuntime::new(&config()).unwrap();
    rt.block_on(async {
        let a = UdpSocket::bind(lo()).unwrap();
        let b = UdpSocket::bind(lo()).unwrap();
        let closed = closed_port();
        println!("DIAG d7: send astray {:?}", a.try_send_to(b"astray", closed));
        readable_bounded("d7 after send to closed", &a).await;
        let before = a.astray_reports();
        let sent = a.try_send_to(b"to-b", b.local_addr().unwrap());
        println!(
            "DIAG d7: a send to b {sent:?} astray {before} -> {}",
            a.astray_reports()
        );
        let sent = a.try_send_to(b"to-b-2", b.local_addr().unwrap());
        println!("DIAG d7: a send to b again {sent:?} astray {}", a.astray_reports());
        fionread_at_least("d7 b queued", &b, 4).await;
        recv_line("d7 b recv", &b);
        recv_line("d7 a recv (report left?)", &a);
        recv_line("d7 a recv again", &a);
    })
    .unwrap();
}

/// The two IOCTLs off on `socket` (`WSAIoctl`, a BOOL FALSE in), each one's return printed.
fn reports_off(socket: &UdpSocket) {
    use windows_sys::Win32::Networking::WinSock::{
        SIO_UDP_CONNRESET, SIO_UDP_NETRESET, SOCKET, WSAGetLastError, WSAIoctl,
    };
    let raw = socket.readiness_handle();
    let handle = SOCKET::try_from(u32::from_ne_bytes(raw.to_ne_bytes())).unwrap();
    for (name, code) in [("CONNRESET", SIO_UDP_CONNRESET), ("NETRESET", SIO_UDP_NETRESET)] {
        let off: u32 = 0;
        let mut returned: u32 = 0;
        // SAFETY: diag; the input is one live u32, no output, no overlapped.
        let rc = unsafe {
            WSAIoctl(
                handle,
                code,
                (&raw const off).cast(),
                4,
                core::ptr::null_mut(),
                0,
                &raw mut returned,
                core::ptr::null_mut(),
                None,
            )
        };
        // SAFETY: a pure read of the thread's last error.
        let error = unsafe { WSAGetLastError() };
        println!("DIAG reports_off {name}: rc {rc} error {error} returned {returned}");
    }
}

#[test]
fn d8_reports_off() {
    let mut rt = LocalRuntime::new(&config()).unwrap();
    rt.block_on(async {
        let a = UdpSocket::bind(lo()).unwrap();
        let b = UdpSocket::bind(lo()).unwrap();
        reports_off(&a);
        let closed = closed_port();
        println!("DIAG d8: send astray {:?}", a.try_send_to(b"astray", closed));
        readable_bounded("d8 after send to closed (reports off)", &a).await;
        recv_line("d8 recv with nothing sent to it", &a);
        let to = a.local_addr().unwrap();
        println!("DIAG d8: b send {:?}", b.try_send_to(b"data-one", to));
        fionread_at_least("d8 data queued", &a, 8).await;
        recv_line("d8 recv 0", &a);
        recv_line("d8 recv 1", &a);
        let sent = a.try_send_to(b"astray-2", closed);
        println!("DIAG d8: second astray send {sent:?}");
        let _ = sleep(300_000_000).await;
        let sent = a.try_send_to(b"to-b", b.local_addr().unwrap());
        println!("DIAG d8: send after second astray {sent:?} astray {}", a.astray_reports());
        recv_line("d8 recv after second astray", &a);
    })
    .unwrap();
}

#[test]
fn d9_report_then_data_batched_receive() {
    let mut rt = LocalRuntime::new(&config()).unwrap();
    rt.block_on(async {
        let mut l = Batched::bind(lo(), Io { batch: 8 }, true).unwrap();
        let b = UdpSocket::bind(lo()).unwrap();
        let closed = closed_port();
        l.queue(closed, b"astray");
        println!("DIAG d9: send {:?}", l.send());
        readable_bounded("d9 after send to closed", l.socket()).await;
        let to = l.local_addr().unwrap();
        for _ in 0..3 {
            let _ = b.try_send_to(b"data-xyz", to);
        }
        fionread_at_least("d9 data queued", l.socket(), 24).await;
        let mut got = 0usize;
        let out = l.receive(|_, _| got += 1);
        println!("DIAG d9 receive: {out:?} delivered {got} stats {:?}", l.stats());
        let out = l.receive(|_, _| got += 1);
        println!("DIAG d9 receive again: {out:?} delivered {got} stats {:?}", l.stats());
    })
    .unwrap();
}

/// The driver's own pattern, many times: report pending first, then a burst, through receive_queued with
/// readiness awaited only on Emptied. Counts rounds whose datagrams were not all taken before readiness
/// went quiet.
#[test]
fn d10_driver_pattern_rounds() {
    let mut rt = LocalRuntime::new(&config()).unwrap();
    rt.block_on(async {
        let mut l = Batched::bind_lent(lo(), true).unwrap();
        let mut ring = Ring::new(Io { batch: 64 }).unwrap();
        let b = UdpSocket::bind(lo()).unwrap();
        let to = l.local_addr().unwrap();
        let mut stuck = 0usize;
        for round in 0..20 {
            let closed = closed_port();
            l.queue(closed, b"astray");
            let _ = l.send();
            // No wait for the report: the burst races it, as a live listener's traffic does.
            for _ in 0..3 {
                let _ = b.try_send_to(b"data-xyz", to);
            }
            let mut total = 0usize;
            let mut quiet = false;
            for _ in 0..12 {
                let began = hyper_rt::futures::now_ns();
                let mut got = 0usize;
                let out = l.receive_queued(&mut ring, began, 128, |_, _| got += 1);
                total += got;
                match out {
                    Ok((_, Queued::Emptied)) => {
                        if total >= 3 {
                            break;
                        }
                        if !readable_bounded(&format!("d10 round {round} total {total}"), l.socket())
                            .await
                        {
                            quiet = true;
                            break;
                        }
                    }
                    Ok((_, Queued::More)) => {}
                    Err(error) => {
                        println!("DIAG d10 round {round}: error {error:?}");
                        break;
                    }
                }
            }
            if quiet || total < 3 {
                stuck += 1;
                println!(
                    "DIAG d10 round {round}: STUCK total {total} fionread {:?} stats {:?}",
                    l.socket().queued_bytes(),
                    l.stats()
                );
                // Drain whatever is left so the next round starts clean.
                for _ in 0..8 {
                    let _ = l.receive_queued(&mut ring, hyper_rt::futures::now_ns(), 128, |_, _| {});
                }
            }
        }
        println!("DIAG d10: stuck rounds {stuck} of 20, stats {:?}", l.stats());
        let _ = Sent::Drained;
    })
    .unwrap();
}

/// The report taken first, data already queued behind it, and only then the readable wait armed:
/// does the wait fire at once (AFD's poll completing for what is queued)?
#[test]
fn d11_report_taken_then_readable_armed_with_data_queued() {
    let mut rt = LocalRuntime::new(&config()).unwrap();
    rt.block_on(async {
        let a = UdpSocket::bind(lo()).unwrap();
        let b = UdpSocket::bind(lo()).unwrap();
        let closed = closed_port();
        println!("DIAG d11: send astray {:?}", a.try_send_to(b"astray", closed));
        readable_bounded("d11 after send to closed", &a).await;
        let to = a.local_addr().unwrap();
        println!("DIAG d11: b send {:?}", b.try_send_to(b"data-one", to));
        fionread_at_least("d11 data queued", &a, 8).await;
        recv_line("d11 recv takes the report", &a);
        readable_bounded("d11 readable armed with data queued", &a).await;
        recv_line("d11 recv after", &a);
    })
    .unwrap();
}

/// The async receive (`UdpSocket::recv_from`) with a report and then data queued: does it return the
/// data with no further datagram?
#[test]
fn d12_recv_from_future_report_then_data() {
    let mut rt = LocalRuntime::new(&config()).unwrap();
    rt.block_on(async {
        let a = UdpSocket::bind(lo()).unwrap();
        let b = UdpSocket::bind(lo()).unwrap();
        let closed = closed_port();
        println!("DIAG d12: send astray {:?}", a.try_send_to(b"astray", closed));
        readable_bounded("d12 after send to closed", &a).await;
        let to = a.local_addr().unwrap();
        println!("DIAG d12: b send {:?}", b.try_send_to(b"data-one", to));
        fionread_at_least("d12 data queued", &a, 8).await;
        let mut buf = [0u8; 64];
        let start = Instant::now();
        let got = race2(a.recv_from(&mut buf), sleep(BOUND_NS)).await;
        println!(
            "DIAG d12: recv_from {got:?} after {:?} astray {}",
            start.elapsed(),
            a.astray_reports()
        );
    })
    .unwrap();
}

/// A report that comes while the receive future already waits: the future is woken by the report,
/// takes it, waits again, and the data sent after is delivered.
#[test]
fn d13_recv_from_waiting_when_the_report_comes() {
    let mut rt = LocalRuntime::new(&config()).unwrap();
    rt.block_on(async {
        let a = UdpSocket::bind(lo()).unwrap();
        let b = UdpSocket::bind(lo()).unwrap();
        let closed = closed_port();
        let to = a.local_addr().unwrap();
        let mut buf = [0u8; 64];
        let start = Instant::now();
        let got = race2(
            join2(a.recv_from(&mut buf), async {
                yield_now().await;
                let astray = a.try_send_to(b"astray", closed);
                // Wait until the report has been taken by the waiting receive.
                let mut spins = 0u32;
                while a.astray_reports() == 0 && spins < 1_500 {
                    let _ = sleep(1_000_000).await;
                    spins += 1;
                }
                let data = b.try_send_to(b"data-one", to);
                (astray, spins, data)
            }),
            sleep(3 * BOUND_NS),
        )
        .await;
        println!(
            "DIAG d13: {got:?} after {:?} astray {}",
            start.elapsed(),
            a.astray_reports()
        );
    })
    .unwrap();
}
