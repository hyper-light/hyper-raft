//! SWIM between real processes over the sealed datagram plane on real UDP sockets: the usage
//! mantle, focal and slates make of it.
//!
//! The supervisor (`a_killed_member_is_declared_dead_by_every_survivor_and_no_live_one_is`)
//! starts `NODES` copies of this test binary as member processes (`member_process`, selected by
//! `HYPER_SWIM_NODE`). Each runs the detector each period over its own socket and prints its view
//! on stdout. The supervisor kills one with SIGKILL once the members have settled, then waits on
//! the fact it needs: every survivor reports the killed member dead and no live member suspected,
//! within a bound in periods derived from the detector's timing.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::disallowed_macros,
    clippy::cast_possible_truncation,
    clippy::print_stdout,
    clippy::cognitive_complexity,
    missing_docs
)]

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::UdpSocket;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use hyper_datagram::{AdmitAll, ExporterSecret, Plane, PlaneLimits, Role, SECRET_BYTES};
use hyper_swim::HostId;
use hyper_swim::codec::SwimMessage;
use hyper_swim::detector::{Detector, DetectorTiming};
use hyper_swim::membership::Liveness;

/// Members in the test cluster.
const NODES: u64 = 4;
/// The member the supervisor kills.
const VICTIM: u64 = NODES;
/// The protocol period. A test parameter: loopback round trips are well under a millisecond, so
/// any period of at least three of them satisfies node.md §3.5; this one leaves the processes idle
/// most of the time.
const PERIOD: Duration = Duration::from_millis(40);
/// The detector's timing for the test, each a test parameter of the shape the owner derives.
const TIMING: DetectorTiming = DetectorTiming {
    suspicion_periods: 3,
    // ⌈λ·ln(n+1)⌉ with λ = 3 and n = 4 (SWIM §4.4's infection-style dissemination).
    gossip_transmits: 5,
    health_max: 2,
    suspicion_min: 2,
    confirmations_expected: 2,
};
/// Gossip entries piggybacked per message.
const GOSSIP_PER_MESSAGE: usize = 8;
/// The longest a member may take, after the victim dies, to report it dead, in periods:
/// - a round to probe it (`NODES`);
/// - the widest suspicion window (`suspicion_periods × (health_max + 1)`);
/// - a gossip transmission to every member (`gossip_transmits × NODES`).
const DETECTION_BOUND_PERIODS: u64 = NODES
    + TIMING.suspicion_periods as u64 * (TIMING.health_max as u64 + 1)
    + TIMING.gossip_transmits as u64 * NODES;
/// Periods the supervisor lets the members settle before it kills the victim: every member
/// probed twice.
const SETTLE_PERIODS: u64 = 2 * NODES;
/// The most periods a member runs, and the supervisor waits, before the test fails: four times
/// everything the test needs, so only a member that stopped reporting runs out.
const RUN_PERIODS: u64 = 4 * (SETTLE_PERIODS + DETECTION_BOUND_PERIODS + NODES);

fn secret_between(a: u64, b: u64) -> ExporterSecret {
    // Stands for the QUIC exporter both ends of a connection compute: one secret per pair.
    let (low, high) = (a.min(b), a.max(b));
    let mut bytes = [0u8; SECRET_BYTES];
    bytes[..8].copy_from_slice(&low.to_le_bytes());
    bytes[8..16].copy_from_slice(&high.to_le_bytes());
    ExporterSecret::new(bytes)
}

fn liveness_letter(liveness: Liveness) -> char {
    match liveness {
        Liveness::Alive => 'A',
        Liveness::Suspect => 'S',
        Liveness::Dead => 'D',
    }
}

/// One member process: runs until it is killed.
#[test]
fn member_process() {
    let Ok(me) = std::env::var("HYPER_SWIM_NODE") else {
        return;
    };
    let me: u64 = me.parse().unwrap();
    let ports: Vec<u16> = std::env::var("HYPER_SWIM_PORTS")
        .unwrap()
        .split(',')
        .map(|port| port.parse().unwrap())
        .collect();
    let address = |id: u64| format!("127.0.0.1:{}", ports[(id - 1) as usize]);
    let socket = UdpSocket::bind(address(me)).unwrap();

    let limits = PlaneLimits {
        max_peers: NODES as usize,
        epochs_per_peer: 2,
        window_limit: 1_024,
    };
    let mut plane = Plane::new(me, limits).unwrap();
    let mut detector = Detector::new(HostId(me), TIMING);
    for peer in (1..=NODES).filter(|peer| *peer != me) {
        let role = if me < peer {
            Role::Initiator
        } else {
            Role::Acceptor
        };
        plane
            .install_epoch(peer, 1, &secret_between(me, peer), role)
            .unwrap();
        plane.set_path(peer, 1_200).unwrap();
        detector.join(HostId(peer));
    }

    // Ready, then wait for the supervisor's start: a member joins its peers only once they are up,
    // as a real member joins a peer once its transport connection to it exists.
    let mut stdout = std::io::stdout();
    writeln!(stdout, "ready {me}").unwrap();
    stdout.flush().unwrap();
    let mut start = String::new();
    std::io::stdin().read_line(&mut start).unwrap();

    let boot_nonce = me;
    let mut nonce = 0u64;
    let mut buffer = vec![0u8; 2_048];
    for period in 0..RUN_PERIODS {
        let deadline = Instant::now() + PERIOD;
        if let Some(ping) = detector.tick() {
            nonce += 1;
            let message = SwimMessage::Ping {
                from: HostId(me),
                nonce,
                boot_nonce,
                configuration_version: 0,
                gossip: detector.ping_gossip(ping.to, GOSSIP_PER_MESSAGE),
            };
            plane.queue(ping.to.0, &message.encode()).unwrap();
        }
        flush(&mut plane, &socket, &address);
        while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            if remaining.is_zero() {
                break;
            }
            socket.set_read_timeout(Some(remaining)).unwrap();
            let Ok((length, _)) = socket.recv_from(&mut buffer) else {
                break;
            };
            let Ok(opened) = plane.open(&mut buffer[..length], &AdmitAll) else {
                continue;
            };
            let messages: Vec<Vec<u8>> = opened.messages().map(<[u8]>::to_vec).collect();
            for bytes in messages {
                match SwimMessage::decode(&bytes).unwrap() {
                    SwimMessage::Ping {
                        from,
                        nonce,
                        gossip,
                        ..
                    } => {
                        detector.apply_gossip_from(from, &gossip);
                        let ack = SwimMessage::Ack {
                            from: HostId(me),
                            nonce,
                            boot_nonce,
                            configuration_version: 0,
                            standing: None,
                            gossip: detector.gossip(GOSSIP_PER_MESSAGE),
                            coordinate: detector.coordinate(),
                        };
                        let _ = plane.queue(from.0, &ack.encode());
                    }
                    SwimMessage::Ack { from, gossip, .. } => {
                        detector.apply_gossip_from(from, &gossip);
                        detector.on_ack(from);
                    }
                    _ => {}
                }
            }
            flush(&mut plane, &socket, &address);
        }
        let view: String = (1..=NODES)
            .filter(|peer| *peer != me)
            .map(|peer| {
                let state = detector.membership().state(HostId(peer)).unwrap();
                format!("{peer}{}", liveness_letter(state.liveness))
            })
            .collect::<Vec<_>>()
            .join(" ");
        writeln!(stdout, "{me} {period} {view}").unwrap();
        stdout.flush().unwrap();
    }
}

fn flush(plane: &mut Plane, socket: &UdpSocket, address: &dyn Fn(u64) -> String) {
    plane.flush(|peer, datagram: Result<&[u8], hyper_datagram::Refusal>| {
        socket.send_to(datagram.unwrap(), address(peer)).unwrap();
    });
}

/// Free loopback ports, one per member: bound, read and released.
fn free_ports() -> Vec<u16> {
    let sockets: Vec<UdpSocket> = (0..NODES)
        .map(|_| UdpSocket::bind("127.0.0.1:0").unwrap())
        .collect();
    sockets
        .iter()
        .map(|socket| socket.local_addr().unwrap().port())
        .collect()
}

#[test]
fn a_killed_member_is_declared_dead_by_every_survivor_and_no_live_one_is() {
    if std::env::var("HYPER_SWIM_NODE").is_ok() {
        return;
    }
    let ports = free_ports();
    let ports = ports
        .iter()
        .map(u16::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let mut members: BTreeMap<u64, Child> = (1..=NODES)
        .map(|id| {
            let child = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "member_process",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env("HYPER_SWIM_NODE", id.to_string())
                .env("HYPER_SWIM_PORTS", &ports)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();
            (id, child)
        })
        .collect();
    let (lines, receiver) = std::sync::mpsc::channel::<String>();
    for child in members.values_mut() {
        let stdout = child.stdout.take().unwrap();
        let lines = lines.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if lines.send(line).is_err() {
                    break;
                }
            }
        });
    }

    // Start every member together once all have bound their sockets.
    let mut ready = 0;
    while ready < NODES {
        let line = receiver
            .recv_timeout(PERIOD * 1_000)
            .expect("members never became ready");
        // libtest prints "test member_process ... " before the body runs, without a newline.
        if line.contains("ready ") {
            ready += 1;
        }
    }
    for child in members.values_mut() {
        writeln!(child.stdin.as_mut().unwrap(), "start").unwrap();
    }

    // Views as reported: member → (period, view of each peer).
    let mut views: BTreeMap<u64, (u64, BTreeMap<u64, char>)> = BTreeMap::new();
    let mut killed_at: Option<u64> = None;
    let mut detected: BTreeMap<u64, u64> = BTreeMap::new();
    let wait =
        PERIOD * u32::try_from(SETTLE_PERIODS + DETECTION_BOUND_PERIODS + NODES).unwrap() * 4;
    let started = Instant::now();
    while detected.len() < (NODES - 1) as usize {
        let line = receiver
            .recv_timeout(wait.saturating_sub(started.elapsed()))
            .expect("members stopped reporting");
        let mut fields = line.split(' ');
        let (Some(member), Some(period)) = (fields.next(), fields.next()) else {
            continue;
        };
        let (Ok(member), Ok(period)) = (member.parse::<u64>(), period.parse::<u64>()) else {
            continue;
        };
        let view: BTreeMap<u64, char> = fields
            .filter_map(|entry| {
                let (peer, state) = entry.split_at(entry.len() - 1);
                Some((peer.parse().ok()?, state.chars().next()?))
            })
            .collect();
        for (peer, state) in &view {
            if *peer != VICTIM {
                assert_eq!(
                    *state, 'A',
                    "member {member} saw live member {peer} as {state} in period {period}"
                );
            }
        }
        views.insert(member, (period, view.clone()));
        if killed_at.is_none() && period >= SETTLE_PERIODS && member != VICTIM {
            let mut victim = members.remove(&VICTIM).unwrap();
            victim.kill().unwrap();
            victim.wait().unwrap();
            killed_at = Some(period);
        }
        if let Some(killed) = killed_at
            && member != VICTIM
            && view.get(&VICTIM) == Some(&'D')
        {
            detected
                .entry(member)
                .or_insert(period.saturating_sub(killed));
        }
    }
    for child in members.values_mut() {
        child.kill().unwrap();
        child.wait().unwrap();
    }
    for (member, periods) in &detected {
        assert!(
            *periods <= DETECTION_BOUND_PERIODS,
            "member {member} took {periods} periods, past the bound of {DETECTION_BOUND_PERIODS}"
        );
    }
    println!(
        "detection, in periods after the kill: {detected:?} (bound {DETECTION_BOUND_PERIODS})"
    );
}
