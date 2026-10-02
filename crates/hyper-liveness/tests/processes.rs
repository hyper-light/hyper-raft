//! Node-pair liveness between real processes over real UDP sockets, each heartbeat proved by a
//! real flush of a real file: the usage mantle and focal make of it, through hyper-tokio's plane
//! socket and its kernel receive stamps.
//!
//! The supervisor (`a_stalled_disk_and_a_killed_node_are_suspected_and_no_live_one_is`) starts
//! `NODES` copies of this test binary as member processes (`member_process`, selected by
//! `HYPER_LIVENESS_NODE`). Each runs the crate as an owner does: it polls at the crate's wake, feeds
//! it what the plane socket received with the kernel's stamps and what its disk made durable,
//! queues the heartbeats it returns on the sealed plane, makes the liveness writes it asks for on
//! its own device thread (one thread, writing and flushing one file: `fdatasync` on Linux,
//! `F_FULLFSYNC` on macOS, `FlushFileBuffers` on Windows, through std), and charges each detector
//! the election the library's law gives over the round trips its streams measured. It reports what
//! the crate reports. The test times nothing of its own and derives no bound. The supervisor waits
//! on facts:
//! - every member's every pair configured; then it stalls one member's disk (its device thread
//!   stops completing flushes, as a disk that stops does) and waits for every other member to
//!   suspect it, each within the bound its detector stated, measured from the stalled member's
//!   last heartbeat's schedule on the host's monotonic clock, which every process reads alike;
//! - then it kills another member with SIGKILL (TerminateProcess on Windows) and waits for every
//!   survivor to suspect it, each within its stated bound the same way.
//!
//! Of live members it asserts what the configured detectors promise: Theorem 7 bounds the expected
//! number of suspicions of a live peer by the allowance `Σβ`, and a run refutes that only when the
//! 95 % lower limit of its count, summed over the live pairs, passes the summed allowance (the rule
//! of hyper-swim's cluster test and of the trace replay, `docs/timing.md` §2.6).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::disallowed_macros,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::print_stdout,
    clippy::cognitive_complexity,
    missing_docs
)]

use std::collections::BTreeMap;
use std::future::poll_fn;
use std::io::{BufRead, BufReader, Write as _};
use std::net::{SocketAddr, UdpSocket};
use std::pin::pin;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::task::Poll;
use std::time::Duration;

use hyper_datagram::{AdmitAll, ExporterSecret, Plane, PlaneLimits, Role, SECRET_BYTES};
use hyper_liveness::{Change, Liveness, Output, PeerId, Settings, Write, is_liveness};
use hyper_timing::{Ballot, Exposure, Trust, poisson95};
use hyper_tokio::{Clock, Io, PlaneSocket};

/// Members: one whose disk stalls, one killed, and two that watch both.
const NODES: u64 = 4;
/// The member whose disk the supervisor stalls.
const STALLED: u64 = 3;
/// The member the supervisor kills.
const KILLED: u64 = 4;
/// The plane's datagram on the path: QUIC's minimum, which every path carries (RFC 9000 §14.1).
const DATAGRAM: usize = 1_200;
/// The block a liveness write writes: 4 KiB, the page and the logical block of every device the
/// projects run on, so the write is one block and the flush is the device's.
const BLOCK: usize = 4_096;

fn secret_between(a: u64, b: u64) -> ExporterSecret {
    // Stands for the QUIC exporter both ends of a connection compute: one secret per pair.
    let (low, high) = (a.min(b), a.max(b));
    let mut bytes = [0u8; SECRET_BYTES];
    bytes[..8].copy_from_slice(&low.to_le_bytes());
    bytes[8..16].copy_from_slice(&high.to_le_bytes());
    ExporterSecret::new(bytes)
}

/// The member's disk: one thread writing and flushing one file, a request at a time.
enum Request {
    Flush,
    /// The disk stops: nothing it is asked completes again.
    Stall,
}

fn device(
    path: std::path::PathBuf,
    requests: Receiver<Request>,
    done: SyncSender<(u64, u64)>,
    wake: UdpSocket,
) {
    use std::io::{Seek, SeekFrom, Write};
    let clock = Clock::new().unwrap();
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .unwrap();
    let block = vec![0xa5u8; BLOCK];
    while let Ok(request) = requests.recv() {
        match request {
            Request::Flush => {
                let started = clock.now_ns();
                file.seek(SeekFrom::Start(0)).unwrap();
                file.write_all(&block).unwrap();
                file.sync_data().unwrap();
                if done.send((started, clock.now_ns())).is_err() {
                    return;
                }
                // A wake for the owner; lost only if the owner is gone.
                let _ = wake.send(&[0]);
            }
            Request::Stall => {
                // A disk that stopped: the thread holds its last request for ever.
                let (_keep, never) = sync_channel::<()>(0);
                let _ = never.recv();
                return;
            }
        }
    }
}

/// What the crate asks of the owner, gathered during one call.
struct Asked<'a> {
    plane: &'a mut Plane,
    flush: bool,
    changes: Vec<Change>,
}

impl Output for Asked<'_> {
    fn heartbeat(&mut self, peer: PeerId, message: &[u8]) {
        // A message the plane refuses is a lost heartbeat, which the detector measures.
        let _ = self.plane.queue(peer, message);
    }
    fn flush(&mut self) {
        self.flush = true;
    }
    fn change(&mut self, change: Change) {
        self.changes.push(change);
    }
}

/// One member process: runs until it is killed or its supervisor is gone.
#[test]
fn member_process() {
    let Ok(me) = std::env::var("HYPER_LIVENESS_NODE") else {
        return;
    };
    let me: u64 = me.parse().unwrap();
    let ports: Vec<u16> = std::env::var("HYPER_LIVENESS_PORTS")
        .unwrap()
        .split(',')
        .map(|port| port.parse().unwrap())
        .collect();
    let file = std::path::PathBuf::from(std::env::var("HYPER_LIVENESS_FILE").unwrap());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(member(me, ports, file));
}

async fn member(me: u64, ports: Vec<u16>, file: std::path::PathBuf) {
    let address = |id: u64| SocketAddr::from(([127, 0, 0, 1], ports[(id - 1) as usize]));
    let mut socket = PlaneSocket::bind(address(me), Io { batch: 64 }).unwrap();
    let clock = *socket.clock();
    let mut plane = Plane::new(
        me,
        PlaneLimits {
            max_peers: NODES as usize,
            epochs_per_peer: 2,
            window_limit: 1_024,
        },
    )
    .unwrap();
    let mut liveness = Liveness::new(Settings {
        local: me,
        boot: clock.now_ns() ^ u64::from(std::process::id()),
        max_peers: NODES as usize,
        history: Exposure::new(),
    })
    .unwrap();
    let peers: Vec<u64> = (1..=NODES).filter(|peer| *peer != me).collect();
    for &peer in &peers {
        let role = if me < peer {
            Role::Initiator
        } else {
            Role::Acceptor
        };
        plane
            .install_epoch(peer, 1, &secret_between(me, peer), role)
            .unwrap();
        plane.set_path(peer, DATAGRAM).unwrap();
        // One group of all the members.
        liveness.attach(peer).unwrap();
    }
    // The disk, and the socket its completions and the supervisor's commands wake the owner on.
    let wake = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let waker = UdpSocket::bind("127.0.0.1:0").unwrap();
    waker.connect(wake.local_addr().unwrap()).unwrap();
    let (requests, requested) = sync_channel::<Request>(1);
    let (done, completions) = sync_channel::<(u64, u64)>(1);
    std::thread::spawn(move || device(file, requested, done, waker));

    let mut stdout = std::io::stdout();
    if writeln!(stdout, "ready {me} {}", wake.local_addr().unwrap().port())
        .and_then(|()| stdout.flush())
        .is_err()
    {
        return;
    }
    let mut start = String::new();
    if !matches!(std::io::stdin().read_line(&mut start), Ok(read) if read > 0) {
        return;
    }

    let mut flushing = false;
    let mut reported_at = 0u64;
    let mut command = [0u8; 16];
    // The first poll: it asks for the flush that proves the first heartbeats.
    let mut first = Asked {
        plane: &mut plane,
        flush: false,
        changes: Vec::new(),
    };
    liveness.poll(clock.now_ns(), &mut first);
    if first.flush && requests.try_send(Request::Flush).is_ok() {
        flushing = true;
    }
    loop {
        // Wait for a datagram, a completion or a command, or the crate's wake.
        let deadline = liveness.wake().map(|at| {
            tokio::time::Instant::now() + Duration::from_nanos(at.saturating_sub(clock.now_ns()))
        });
        let mut stall = false;
        // What arrived, stamped by the kernel: fed before anything is judged at `now`.
        let mut inbox: Vec<(u64, u64, Vec<u8>)> = Vec::new();
        {
            let mut receive = pin!(socket.receive(&mut plane, &AdmitAll, |arrival, opened| {
                if let Ok(opened) = opened {
                    for message in opened.messages().filter(|m| is_liveness(m)) {
                        inbox.push((opened.sender, arrival.at_ns, message.to_vec()));
                    }
                }
            }));
            let mut woken = pin!(wake.recv(&mut command));
            let mut timer = pin!(async {
                match deadline {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending::<()>().await,
                }
            });
            poll_fn(|context| {
                if let Poll::Ready(result) = woken.as_mut().poll(context) {
                    stall = matches!(result, Ok(length) if length > 1);
                    return Poll::Ready(());
                }
                if receive.as_mut().poll(context).is_ready()
                    || timer.as_mut().poll(context).is_ready()
                {
                    return Poll::Ready(());
                }
                Poll::Pending
            })
            .await;
        }
        if stall {
            let _ = requests.try_send(Request::Stall);
            continue;
        }
        let mut asked = Asked {
            plane: &mut plane,
            flush: false,
            changes: Vec::new(),
        };
        socket
            .receive_ready(asked.plane, &AdmitAll, |arrival, opened| {
                if let Ok(opened) = opened {
                    for message in opened.messages().filter(|m| is_liveness(m)) {
                        inbox.push((opened.sender, arrival.at_ns, message.to_vec()));
                    }
                }
            })
            .unwrap();
        for (from, at, message) in &inbox {
            let _ = liveness.on_heartbeat(*from, message, *at, &mut asked);
        }
        while let Ok((started, durable)) = completions.try_recv() {
            flushing = false;
            liveness.on_durable(Write::Liveness, started, durable);
        }
        let now = clock.now_ns();
        liveness.poll(now, &mut asked);
        if asked.flush && !flushing && requests.try_send(Request::Flush).is_ok() {
            flushing = true;
        }
        let changes = std::mem::take(&mut asked.changes);
        socket.flush(&mut plane, |peer| Some(address(peer)), |_, _| {});
        elect(&mut liveness, &peers);
        if report(
            me,
            &liveness,
            &peers,
            &changes,
            now,
            &mut reported_at,
            &mut stdout,
        )
        .is_err()
        {
            return;
        }
    }
}

/// Charges each detector the election the library's law gives this member's group, over the round
/// trips its streams measured and the flush its disk takes.
fn elect(liveness: &mut Liveness, peers: &[u64]) {
    let (Some(granularity), Some(durable)) = (liveness.granularity(), liveness.flush_mean()) else {
        return;
    };
    let paths: Vec<_> = peers
        .iter()
        .filter_map(|peer| liveness.round_trip(*peer).copied())
        .collect();
    let Some(span) = Ballot::measure(paths.iter(), NODES as usize, durable, granularity)
        .and_then(|ballot| ballot.span(granularity))
    else {
        return;
    };
    for peer in peers {
        liveness.set_election(*peer, span.election).unwrap();
    }
}

/// A line for each suspicion as it happens, and a state line with each change and otherwise at
/// most every 100 ms of the host clock, which bounds the pipe's traffic; the supervisor waits on
/// what they say, not on the period.
fn report(
    me: u64,
    liveness: &Liveness,
    peers: &[u64],
    changes: &[Change],
    now: u64,
    reported_at: &mut u64,
    out: &mut impl std::io::Write,
) -> std::io::Result<()> {
    for change in changes {
        if let Change::Suspected(suspicion) = change {
            let last = suspicion.last;
            writeln!(
                out,
                "suspect {me} {} {} {} {} {}",
                suspicion.peer,
                suspicion.at_ns,
                last.map_or(0, |l| l.due_ns),
                last.map_or(0, |l| l.sent_ns),
                suspicion.detection.map_or(0, |d| d.as_nanos() as u64),
            )?;
        }
    }
    if changes.is_empty() && now < reported_at.saturating_add(100_000_000) {
        return out.flush();
    }
    *reported_at = now;
    let mut line = format!("state {me} {now}");
    for peer in peers {
        let report = liveness.report(*peer).unwrap_or_default();
        let trust = match liveness.trust(*peer) {
            Some(Trust::Trusted { .. }) => 'T',
            Some(Trust::Suspected) => 'S',
            _ => 'U',
        };
        line.push_str(&format!(
            " {peer}:{trust}:{}:{}:{}:{}",
            u8::from(report.configured),
            report.suspicions,
            report.allowance,
            report.taken,
        ));
    }
    writeln!(out, "{line}")?;
    out.flush()
}

/// The member processes, killed when the supervisor ends however it ends.
struct Members(BTreeMap<u64, Child>);

impl Drop for Members {
    fn drop(&mut self) {
        for child in self.0.values_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// What a member last reported of one peer.
#[derive(Clone, Copy, Debug, Default)]
struct Seen {
    trust: char,
    configured: bool,
    suspicions: u64,
    allowance: f64,
}

/// A suspicion a member reported.
#[derive(Clone, Copy, Debug)]
struct Suspected {
    at: u64,
    due: u64,
    sent: u64,
    detection: u64,
}

enum Line {
    State(u64, u64, BTreeMap<u64, Seen>),
    Suspect(u64, u64, Suspected),
}

fn parse(line: &str) -> Option<Line> {
    let mut fields = line.split(' ');
    match fields.next()? {
        "state" => {
            let member = fields.next()?.parse().ok()?;
            let now = fields.next()?.parse().ok()?;
            let mut peers = BTreeMap::new();
            for field in fields {
                let parts: Vec<&str> = field.split(':').collect();
                let [peer, trust, configured, suspicions, allowance, _taken] = parts[..] else {
                    return None;
                };
                peers.insert(
                    peer.parse().ok()?,
                    Seen {
                        trust: trust.chars().next()?,
                        configured: configured == "1",
                        suspicions: suspicions.parse().ok()?,
                        allowance: allowance.parse().ok()?,
                    },
                );
            }
            Some(Line::State(member, now, peers))
        }
        "suspect" => {
            let numbers: Vec<u64> = fields.map(|f| f.parse().ok()).collect::<Option<_>>()?;
            let [member, peer, at, due, sent, detection] = numbers[..] else {
                return None;
            };
            Some(Line::Suspect(
                member,
                peer,
                Suspected {
                    at,
                    due,
                    sent,
                    detection,
                },
            ))
        }
        _ => None,
    }
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

struct Supervisor {
    lines: std::sync::mpsc::Receiver<String>,
    /// Every member line echoed to stderr (`HYPER_LIVENESS_TRACE`), for a run to be read whole.
    trace: bool,
    latest: BTreeMap<u64, BTreeMap<u64, Seen>>,
    /// When each member's latest state line was written, on the host clock.
    stated: BTreeMap<u64, u64>,
    suspicions: Vec<(u64, u64, Suspected)>,
}

impl Supervisor {
    /// The next line any member reports, folded in.
    fn next(&mut self) {
        let line = self.lines.recv().expect("every member stopped reporting");
        if self.trace {
            eprintln!("{line}");
        }
        match parse(&line) {
            Some(Line::State(member, now, peers)) => {
                self.latest.insert(member, peers);
                self.stated.insert(member, now);
            }
            Some(Line::Suspect(member, peer, suspected)) => {
                self.suspicions.push((member, peer, suspected));
            }
            None => {}
        }
    }

    /// Waits until `fact` holds of what the members reported.
    fn until(&mut self, fact: impl Fn(&Self) -> bool) {
        while !fact(self) {
            self.next();
        }
    }

    /// Whether `member` stated, at or after `since` on the host clock, that it holds `peer`
    /// suspected: a fact about the run after `since`, whether the suspicion began before it (a
    /// live peer falsely suspected just before it stalled or died, never trusted again) or after.
    fn holds_suspected(&self, member: u64, peer: u64, since: u64) -> bool {
        self.stated.get(&member).is_some_and(|at| *at >= since)
            && self.latest[&member]
                .get(&peer)
                .is_some_and(|seen| seen.trust == 'S')
    }

    /// The suspicion `member` holds of `peer`: the latest it reported.
    fn suspicion(&self, member: u64, peer: u64) -> Suspected {
        self.suspicions
            .iter()
            .filter(|(m, p, _)| *m == member && *p == peer)
            .map(|(_, _, s)| *s)
            .max_by_key(|s| s.at)
            .expect("a member that holds a peer suspected reported the suspicion")
    }
}

#[test]
fn a_stalled_disk_and_a_killed_node_are_suspected_and_no_live_one_is() {
    if std::env::var("HYPER_LIVENESS_NODE").is_ok() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let ports = free_ports()
        .iter()
        .map(u16::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let mut members = Members(
        (1..=NODES)
            .map(|id| {
                let child = Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "member_process",
                        "--nocapture",
                        "--test-threads=1",
                    ])
                    .env("HYPER_LIVENESS_NODE", id.to_string())
                    .env("HYPER_LIVENESS_PORTS", &ports)
                    .env(
                        "HYPER_LIVENESS_FILE",
                        directory.path().join(format!("member-{id}.log")),
                    )
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .spawn()
                    .unwrap();
                (id, child)
            })
            .collect(),
    );
    let (sender, lines) = std::sync::mpsc::channel::<String>();
    for child in members.0.values_mut() {
        let stdout = child.stdout.take().unwrap();
        let sender = sender.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
    }
    drop(sender);
    // Every member ready: its sockets bound, and where its disk takes commands.
    let mut wakes = BTreeMap::new();
    while wakes.len() < NODES as usize {
        let line = lines.recv().expect("a member ended before it was ready");
        // libtest prints "test member_process ... " before the body runs, without a newline.
        if let Some((_, ready)) = line.split_once("ready ") {
            let fields: Vec<u64> = ready.split(' ').map(|f| f.parse().unwrap()).collect();
            wakes.insert(fields[0], fields[1]);
        }
    }
    for child in members.0.values_mut() {
        writeln!(child.stdin.as_mut().unwrap(), "start").unwrap();
    }
    let mut supervisor = Supervisor {
        lines,
        trace: std::env::var_os("HYPER_LIVENESS_TRACE").is_some(),
        latest: BTreeMap::new(),
        stated: BTreeMap::new(),
        suspicions: Vec::new(),
    };
    let clock = Clock::new().unwrap();

    // Every pair configured.
    supervisor.until(|s| {
        s.latest.len() == NODES as usize
            && s.latest.values().all(|peers| {
                peers.len() == NODES as usize - 1 && peers.values().all(|p| p.configured)
            })
    });

    // A disk that stops completing flushes.
    let stalled_at = clock.now_ns();
    UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .send_to(b"stall", ("127.0.0.1", wakes[&STALLED] as u16))
        .unwrap();
    let watchers: Vec<u64> = (1..=NODES).filter(|m| *m != STALLED).collect();
    supervisor.until(|s| {
        watchers
            .iter()
            .all(|m| s.holds_suspected(*m, STALLED, stalled_at))
    });
    for member in &watchers {
        let found = supervisor.suspicion(*member, STALLED);
        assert!(found.detection > 0, "member {member}: a bound is stated");
        assert!(
            found.at - found.due <= found.detection,
            "member {member} suspected the stalled disk {} ns after its last heartbeat was due, \
             past its stated bound of {} ns",
            found.at - found.due,
            found.detection
        );
        assert!(found.sent >= found.due);
    }

    // A node killed.
    let mut victim = members.0.remove(&KILLED).unwrap();
    let killed_at = clock.now_ns();
    victim.kill().unwrap();
    victim.wait().unwrap();
    let survivors: Vec<u64> = (1..=NODES).filter(|m| *m != KILLED).collect();
    supervisor.until(|s| {
        survivors
            .iter()
            .all(|m| s.holds_suspected(*m, KILLED, killed_at))
    });
    for member in &survivors {
        let found = supervisor.suspicion(*member, KILLED);
        assert!(
            found.at - found.due <= found.detection,
            "member {member} suspected the killed node {} ns after its last heartbeat was due, \
             past its stated bound of {} ns",
            found.at - found.due,
            found.detection
        );
    }
    // Live members: the pairs between the members that neither stalled nor died, both ways, as
    // each reports once it holds both of the others suspected.
    let live: Vec<u64> = (1..=NODES)
        .filter(|m| *m != STALLED && *m != KILLED)
        .collect();
    supervisor.until(|s| {
        live.iter().all(|m| {
            s.holds_suspected(*m, STALLED, killed_at) && s.holds_suspected(*m, KILLED, killed_at)
        })
    });
    drop(members);

    let (mut suspicions, mut allowance) = (0u64, 0.0f64);
    for member in &live {
        for peer in live.iter().filter(|p| *p != member) {
            // Whether a live peer is trusted at this moment is no promise; the count is.
            let seen = supervisor.latest[member][peer];
            suspicions += seen.suspicions;
            allowance += seen.allowance;
        }
    }
    println!(
        "stalled disk suspected after {:?}; killed node after {:?}; suspicions of live members \
         {suspicions} (Theorem 7 allows {allowance:.3})",
        watchers
            .iter()
            .map(|m| {
                let s = supervisor.suspicion(*m, STALLED);
                (
                    m,
                    Duration::from_nanos(s.at - s.due),
                    Duration::from_nanos(s.detection),
                )
            })
            .collect::<Vec<_>>(),
        survivors
            .iter()
            .map(|m| {
                let s = supervisor.suspicion(*m, KILLED);
                (
                    m,
                    Duration::from_nanos(s.at - s.due),
                    Duration::from_nanos(s.detection),
                )
            })
            .collect::<Vec<_>>(),
    );
    assert!(
        poisson95(suspicions).0 <= allowance,
        "{suspicions} suspicions of live members refute the {allowance} the configured detectors \
         allow"
    );
}
