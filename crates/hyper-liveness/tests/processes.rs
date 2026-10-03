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
//! the crate reports, its disk's state and its flushes. The test times nothing of its own and
//! derives no bound. The supervisor waits on facts, each for as long as the members move toward
//! it: a quiet period derived from what they state (the longest of their judged pairs' `η + α`
//! and their unjudged pairs' intervals, past their longest flush, their wakes' lateness and their
//! reporting period) that passes with nothing moving fails the wait with every member's last
//! state, as hyper-durable-e2e's waits do; a wait for a disk on one flush is the disk's, and goes on:
//! - every member's every pair configured; then it stalls one member's disk (its device thread
//!   stops completing flushes, as a disk that stops does) and waits for every other member to
//!   suspect it, each within the bound its detector stated, measured from the stalled member's
//!   last heartbeat's schedule on the host's monotonic clock, which every process reads alike;
//! - then it kills another member with SIGKILL (TerminateProcess on Windows) and waits for every
//!   survivor to suspect it, each within its stated bound the same way.
//!
//! A second supervisor (`a_node_killed_in_its_first_heartbeats_is_suspected_once_a_sibling_has_its_evidence`)
//! starts three members and kills one with SIGKILL as soon as it says it has heard every peer and
//! sent to each, before any of its links could have its own evidence: each survivor, left one live
//! link, suspects it no later than its first poll once the link's freshness point has passed and
//! its live link has configured (`docs/timing.md` §3, item 10).
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
use hyper_timing::{Ballot, Exposure, Trust, WINDOW_LIMIT, poisson95};
use hyper_tokio::{Clock, Io, PlaneSocket};

/// Members: one whose disk stalls, one killed, and two that watch both.
const NODES: u64 = 4;
/// The member whose disk the supervisor stalls.
const STALLED: u64 = 3;
/// The member the supervisor kills.
const KILLED: u64 = 4;
/// The plane's datagram on the path: QUIC's minimum, which every path carries (RFC 9000 §14.1).
const DATAGRAM: usize = 1_200;

fn secret_between(a: u64, b: u64) -> ExporterSecret {
    // Stands for the QUIC exporter both ends of a connection compute: one secret per pair.
    let (low, high) = (a.min(b), a.max(b));
    let mut bytes = [0u8; SECRET_BYTES];
    bytes[..8].copy_from_slice(&low.to_le_bytes());
    bytes[8..16].copy_from_slice(&high.to_le_bytes());
    ExporterSecret::new(bytes)
}

/// What the device thread sends the owner's wake socket: a flush completed, or the disk stopped.
/// The supervisor's commands are longer.
const COMPLETED: u8 = 0;
const STOPPED: u8 = 1;

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
    // Read access too: Windows answers a query of the file's volume on a handle that may read.
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    // A liveness write is one block of the size the system reports for the file, so the device
    // takes it whole and the flush is of that block alone.
    let block = vec![0xa5u8; hyper_block::file::preferred_block(&file, &path).unwrap()];
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
                let _ = wake.send(&[COMPLETED]);
            }
            Request::Stall => {
                // A disk that stopped: the thread says so, then holds its last request for ever.
                let _ = wake.send(&[STOPPED]);
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
#[allow(
    clippy::disallowed_methods,
    reason = "real processes on the host's clock, threads and environment (CLAUDE.md §1a, end to end)"
)]
fn member_process() {
    let Ok(me) = std::env::var("HYPER_LIVENESS_NODE") else {
        return;
    };
    let me: u64 = me.parse().unwrap();
    let nodes: u64 = std::env::var("HYPER_LIVENESS_NODES")
        .unwrap()
        .parse()
        .unwrap();
    let file = std::path::PathBuf::from(std::env::var("HYPER_LIVENESS_FILE").unwrap());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(member(me, nodes, file));
}

#[allow(
    clippy::disallowed_methods,
    reason = "real processes on the host's clock, threads and environment (CLAUDE.md §1a, end to end)"
)]
async fn member(me: u64, nodes: u64, file: std::path::PathBuf) {
    // Its own port, the system's choice: a port picked for it and released could be taken in
    // between, by another group's member as two supervisors start at once.
    let mut socket =
        PlaneSocket::bind(SocketAddr::from(([127, 0, 0, 1], 0)), Io { batch: 64 }).unwrap();
    let port = socket.local_addr().unwrap().port();
    let clock = *socket.clock();
    let mut plane = Plane::new(
        me,
        PlaneLimits {
            max_peers: nodes as usize,
            epochs_per_peer: 2,
            window_limit: 1_024,
        },
    )
    .unwrap();
    let mut liveness = Liveness::new(Settings {
        local: me,
        run: raise_run(&file),
        max_peers: nodes as usize,
        history: Exposure::new(),
    })
    .unwrap();
    let peers: Vec<u64> = (1..=nodes).filter(|peer| *peer != me).collect();
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
    // Room for the most requests ever outstanding: the one flush the owner keeps in flight, and
    // the stall. With room for one, a stall that came while a flush waited for the device thread
    // was refused and lost, and the member's disk never stopped.
    let (requests, requested) = sync_channel::<Request>(2);
    let (done, completions) = sync_channel::<(u64, u64)>(1);
    std::thread::spawn(move || device(file, requested, done, waker));

    let mut stdout = std::io::stdout();
    if writeln!(
        stdout,
        "ready {me} {} {port}",
        wake.local_addr().unwrap().port()
    )
    .and_then(|()| stdout.flush())
    .is_err()
    {
        return;
    }
    // Told to start with every member's port.
    let mut start = String::new();
    if !matches!(std::io::stdin().read_line(&mut start), Ok(read) if read > 0) {
        return;
    }
    let ports: Vec<u16> = start
        .trim()
        .trim_start_matches("start ")
        .split(',')
        .map(|port| port.parse().unwrap())
        .collect();
    let address = |id: u64| SocketAddr::from(([127, 0, 0, 1], ports[(id - 1) as usize]));

    // When the flush in flight was asked for, on the host clock.
    let mut flight: Option<u64> = None;
    let mut disk = Disk::Running;
    let mut flush_most = 0u64;
    let mut reported_at = 0u64;
    let mut heard_all = false;
    let mut command = [0u8; 16];
    // The first poll: it asks for the flush that proves the first heartbeats.
    let mut first = Asked {
        plane: &mut plane,
        flush: false,
        changes: Vec::new(),
    };
    let now = clock.now_ns();
    liveness.poll(now, &mut first);
    if first.flush && requests.try_send(Request::Flush).is_ok() {
        flight = Some(now);
    }
    // Its first state, before it waits: its first flush in flight, which proves its first
    // heartbeats and on which a slow disk holds everything after.
    let member = Member {
        me,
        disk,
        flight,
        flush_most,
    };
    if report(
        &member,
        &liveness,
        &peers,
        &[],
        now,
        &mut reported_at,
        &mut stdout,
    )
    .is_err()
    {
        return;
    }
    loop {
        // Wait for a datagram, a completion or a command, or the crate's wake.
        let deadline = liveness.wake().map(|at| {
            tokio::time::Instant::now() + Duration::from_nanos(at.saturating_sub(clock.now_ns()))
        });
        let mut woke_with = None;
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
                    woke_with = result.ok();
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
        let stall = match woke_with {
            Some(1) if command[0] == STOPPED => {
                disk = Disk::Stopped;
                false
            }
            Some(length) => length > 1,
            None => false,
        };
        if stall {
            disk = if requests.try_send(Request::Stall).is_ok() {
                Disk::Asked
            } else {
                Disk::Refused
            };
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
            flight = None;
            flush_most = flush_most.max(durable.saturating_sub(started));
            liveness.on_durable(Write::Liveness, started, durable);
        }
        let now = clock.now_ns();
        liveness.poll(now, &mut asked);
        if asked.flush && flight.is_none() && requests.try_send(Request::Flush).is_ok() {
            flight = Some(now);
        }
        let changes = std::mem::take(&mut asked.changes);
        socket.flush(&mut plane, |peer| Some(address(peer)), |_, _| {});
        elect(&mut liveness, &peers);
        let member = Member {
            me,
            disk,
            flight,
            flush_most,
        };
        if report(
            &member,
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
        // Once it has heard every peer and sent to each: its links' first heartbeats.
        if !heard_all
            && peers.iter().all(|peer| {
                liveness
                    .report(*peer)
                    .is_some_and(|r| r.taken > 0 && r.sent > 0)
            })
        {
            heard_all = true;
            if writeln!(stdout, "heard {me}")
                .and_then(|()| stdout.flush())
                .is_err()
            {
                return;
            }
        }
    }
}

/// The member's run: the count kept beside its file raised by one (one where there is none), a
/// record kept whole and durable before the stream sends anything under it, as an owner keeps it
/// (`hyper_liveness::Settings::run`; hyper-durable-e2e's `run`). A member started once on a fresh
/// directory is in its first.
fn raise_run(file: &std::path::Path) -> u64 {
    let mut name = file.as_os_str().to_owned();
    name.push(".run");
    let path = std::path::PathBuf::from(name);
    let previous = hyper_block::record::read(&path, 8)
        .unwrap()
        .map_or(0, |count| u64::from_le_bytes(count.try_into().unwrap()));
    let run = previous + 1;
    hyper_block::record::write(&path, &run.to_le_bytes()).unwrap();
    run
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
    let Some(span) = Ballot::measure(paths.iter(), peers.len() + 1, durable, granularity)
        .and_then(|ballot| ballot.span(granularity))
    else {
        return;
    };
    for peer in peers {
        liveness.set_election(*peer, span.election).unwrap();
    }
}

/// What the member's disk is doing, as its owner knows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Disk {
    Running,
    /// Told to stop, the stop queued for the device thread behind any flush in flight.
    Asked,
    /// Told to stop, and the device thread's queue full: the stop was not taken, which its room
    /// for every request outstanding rules out.
    Refused,
    /// The device thread said it stopped.
    Stopped,
}

impl Disk {
    fn letter(self) -> char {
        match self {
            Self::Running => 'R',
            Self::Asked => 'A',
            Self::Refused => 'X',
            Self::Stopped => 'S',
        }
    }
}

/// The member's own state beside its stream's, for its report.
struct Member {
    me: u64,
    disk: Disk,
    /// When the flush in flight was asked for, on the host clock.
    flight: Option<u64>,
    /// The longest flush its disk has taken, nanoseconds.
    flush_most: u64,
}

/// A line for each suspicion as it happens, and a state line with each change and otherwise once
/// the member's shortest interval has passed since the last (its floor before any pair has one),
/// the soonest its evidence can move again; the supervisor waits on what they say.
fn report(
    member: &Member,
    liveness: &Liveness,
    peers: &[u64],
    changes: &[Change],
    now: u64,
    reported_at: &mut u64,
    out: &mut impl std::io::Write,
) -> std::io::Result<()> {
    let nanos = |d: Duration| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX);
    let me = member.me;
    for change in changes {
        if let Change::Suspected(suspicion) = change {
            let last = suspicion.last;
            writeln!(
                out,
                "suspect {me} {} {} {} {} {} {} {}",
                suspicion.peer,
                suspicion.at_ns,
                last.map_or(0, |l| l.due_ns),
                last.map_or(0, |l| l.sent_ns),
                suspicion.detection.map_or(0, nanos),
                suspicion.noticed_ns,
                nanos(liveness.latest_wake(suspicion.noticed_ns)),
            )?;
        }
    }
    let period = peers
        .iter()
        .filter_map(|peer| liveness.report(*peer).and_then(|r| r.interval))
        .min()
        .or_else(|| liveness.floor())
        .map_or(0, nanos);
    if changes.is_empty() && now < reported_at.saturating_add(period) {
        return out.flush();
    }
    *reported_at = now;
    let mut line = format!(
        "state {me} {now} {} {} {} {} {}",
        member.disk.letter(),
        member.flight.unwrap_or(0),
        liveness.floor().map_or(0, nanos),
        member.flush_most,
        nanos(liveness.latest_wake(now)),
    );
    for peer in peers {
        let report = liveness.report(*peer).unwrap_or_default();
        let (trust, until) = match liveness.trust(*peer) {
            Some(Trust::Trusted { until_ns }) => ('T', until_ns),
            Some(Trust::Suspected) => ('S', 0),
            _ => ('U', 0),
        };
        line.push_str(&format!(
            " {peer}:{trust}:{}:{}:{}:{}:{}:{}:{}:{}:{until}",
            u8::from(report.configured),
            u8::from(report.judged),
            report.suspicions,
            report.allowance,
            report.taken,
            report.sent,
            report.interval.map_or(0, nanos),
            report.freshness.map_or(0, nanos),
        ));
    }
    writeln!(out, "{line}")?;
    out.flush()
}

/// The member processes, killed when the supervisor ends however it ends.
struct Members(BTreeMap<u64, Child>);

impl Members {
    /// Every member killed and reaped: a dropped `Child` leaves its process running.
    fn stop(&mut self) {
        for child in self.0.values_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.0.clear();
    }
}

impl Drop for Members {
    fn drop(&mut self) {
        self.stop();
    }
}

/// What a member last stated of one peer.
#[derive(Clone, Copy, Debug, Default)]
struct Seen {
    trust: char,
    configured: bool,
    judged: bool,
    suspicions: u64,
    allowance: f64,
    taken: u64,
    sent: u64,
    /// The interval the peer's heartbeats come at, and `η + α` while a margin judges, nanoseconds.
    interval: u64,
    freshness: u64,
    /// The freshness point the member trusts the peer to, while it does.
    until: u64,
}

/// A member's latest state line.
#[derive(Clone, Debug, Default)]
struct Stated {
    /// When the member wrote it, on the host clock.
    at: u64,
    /// Its disk (`Disk::letter`), and when the flush in flight was asked for (zero for none), on
    /// the host clock.
    disk: char,
    flight: u64,
    /// Its floor `E[flush] + G`, its longest flush, and the latest its wakes came past what they
    /// asked, nanoseconds.
    floor: u64,
    flush_most: u64,
    late: u64,
    peers: BTreeMap<u64, Seen>,
}

/// A suspicion a member reported.
#[derive(Clone, Copy, Debug)]
struct Suspected {
    at: u64,
    due: u64,
    sent: u64,
    detection: u64,
    /// When the member's poll noticed it.
    noticed: u64,
    /// The latest the member had woken past a wake it asked, when it noticed.
    late: u64,
}

enum Line {
    State(u64, Stated),
    Suspect(u64, u64, Suspected),
    Heard(u64),
}

fn parse(line: &str) -> Option<Line> {
    let mut fields = line.split(' ');
    match fields.next()? {
        "state" => {
            let member = fields.next()?.parse().ok()?;
            let mut stated = Stated {
                at: fields.next()?.parse().ok()?,
                disk: fields.next()?.chars().next()?,
                flight: fields.next()?.parse().ok()?,
                floor: fields.next()?.parse().ok()?,
                flush_most: fields.next()?.parse().ok()?,
                late: fields.next()?.parse().ok()?,
                peers: BTreeMap::new(),
            };
            for field in fields {
                let parts: Vec<&str> = field.split(':').collect();
                let [
                    peer,
                    trust,
                    configured,
                    judged,
                    suspicions,
                    allowance,
                    taken,
                    sent,
                    interval,
                    freshness,
                    until,
                ] = parts[..]
                else {
                    return None;
                };
                stated.peers.insert(
                    peer.parse().ok()?,
                    Seen {
                        trust: trust.chars().next()?,
                        configured: configured == "1",
                        judged: judged == "1",
                        suspicions: suspicions.parse().ok()?,
                        allowance: allowance.parse().ok()?,
                        taken: taken.parse().ok()?,
                        sent: sent.parse().ok()?,
                        interval: interval.parse().ok()?,
                        freshness: freshness.parse().ok()?,
                        until: until.parse().ok()?,
                    },
                );
            }
            Some(Line::State(member, stated))
        }
        "suspect" => {
            let numbers: Vec<u64> = fields.map(|f| f.parse().ok()).collect::<Option<_>>()?;
            let [member, peer, at, due, sent, detection, noticed, late] = numbers[..] else {
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
                    noticed,
                    late,
                },
            ))
        }
        "heard" => Some(Line::Heard(fields.next()?.parse().ok()?)),
        _ => None,
    }
}

/// RFC 6298 §2.1 and §2.4: the retransmission timeout before any round trip is measured, and the
/// least it is ever set to after, one second: the quiet period before any member has stated a law
/// (hyper-durable-e2e's waits take it so).
const RTO: Duration = Duration::from_secs(1);

struct Supervisor {
    /// The member processes still running, killed when the supervisor ends however it ends.
    members: Members,
    lines: std::sync::mpsc::Receiver<String>,
    /// The host's monotonic clock, which every member's lines are stated on.
    clock: Clock,
    /// Every member line echoed to stderr (`HYPER_LIVENESS_TRACE`), for a run to be read whole.
    trace: bool,
    latest: BTreeMap<u64, Stated>,
    suspicions: Vec<(u64, u64, Suspected)>,
    /// The members that said they heard every peer and sent to each.
    heard: Vec<u64>,
    /// The first state line in which each member stated each pair configured: `(member, peer)` to
    /// its time on the host clock, which is no earlier than the configuration.
    configured_since: BTreeMap<(u64, u64), u64>,
}

impl Supervisor {
    /// The next line any member reports within `left`, folded in; nothing, past it.
    fn next(&mut self, left: Duration, what: &str) {
        let line = match self.lines.recv_timeout(left) {
            Ok(line) => line,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => return,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                panic!("{what}: every member stopped reporting\n{}", self.dump())
            }
        };
        if self.trace {
            eprintln!("{line}");
        }
        match parse(&line) {
            Some(Line::State(member, stated)) => {
                for (peer, seen) in &stated.peers {
                    if seen.configured {
                        self.configured_since
                            .entry((member, *peer))
                            .or_insert(stated.at);
                    }
                }
                self.latest.insert(member, stated);
            }
            Some(Line::Suspect(member, peer, suspected)) => {
                self.suspicions.push((member, peer, suspected));
            }
            Some(Line::Heard(member)) => self.heard.push(member),
            None => {}
        }
    }

    /// How long the members may go with nothing moving before a wait gives up: the longest any
    /// member's law lets its evidence go still, from its latest state. A judged pair suspects a
    /// peer gone silent within `η + α` of its last heartbeat's expected arrival, an unjudged one
    /// takes a heartbeat each interval while its peer lives; a heartbeat waits for the flush that
    /// proves it, the longest the member's disk has taken; a poll comes up to the latest its wakes
    /// were late; and a state is stated at least once a reporting period, the member's shortest
    /// interval. Never less than a retransmission timeout, the wait before any member states one.
    fn quiet(&self) -> Duration {
        let law = self
            .latest
            .values()
            .map(|stated| {
                let evidence = stated
                    .peers
                    .values()
                    .map(|seen| {
                        if seen.judged {
                            seen.freshness
                        } else {
                            seen.interval
                        }
                    })
                    .max()
                    .unwrap_or(0);
                let period = stated
                    .peers
                    .values()
                    .map(|seen| seen.interval)
                    .filter(|interval| *interval > 0)
                    .min()
                    .unwrap_or(stated.floor);
                evidence
                    .saturating_add(stated.flush_most)
                    .saturating_add(stated.late)
                    .saturating_add(period)
            })
            .max()
            .unwrap_or(0);
        Duration::from_nanos(law).max(RTO)
    }

    /// What moves the members toward a wait's fact: each peer's trust, judgement, configuration
    /// and suspicions, the heartbeats taken on pairs not yet configured (the evidence they gather
    /// toward it), each disk, and the suspicions and arrivals reported. A configured pair's
    /// heartbeats are not progress: a peer that stops sending is then suspected within the law.
    fn signature(&self) -> Vec<u64> {
        let mut out = Vec::new();
        for (member, stated) in &self.latest {
            out.extend([*member, u64::from(u32::from(stated.disk))]);
            for (peer, seen) in &stated.peers {
                out.extend([
                    *peer,
                    u64::from(u32::from(seen.trust)),
                    u64::from(seen.judged),
                    u64::from(seen.configured),
                    seen.suspicions,
                    if seen.configured { 0 } else { seen.taken },
                ]);
            }
        }
        out.extend([self.suspicions.len() as u64, self.heard.len() as u64]);
        out
    }

    /// A pair that has taken more heartbeats unconfigured than any window holds
    /// (`hyper_timing::WINDOW_LIMIT`): a link whose correlation no window of it resolves, which
    /// the crate's moves to the interval its evidence needs exist to prevent.
    fn unresolved(&self) -> Option<(u64, u64, u64)> {
        self.latest.iter().find_map(|(member, stated)| {
            stated.peers.iter().find_map(|(peer, seen)| {
                (!seen.configured && seen.taken > WINDOW_LIMIT)
                    .then_some((*member, *peer, seen.taken))
            })
        })
    }

    /// Waits until `fact` holds of what the members stated, while they move toward it. Fails
    /// with every member's last state once a quiet period passes with nothing moving, or once a
    /// pair takes more heartbeats unconfigured than any window holds. A quiet period through which
    /// a member whose disk runs waited on one flush is that disk's, not the members' law: the
    /// slowest part of a member's law is its flush, which it measures only once the flush
    /// completes. One through which a member has stated nothing yet is its scheduler's, while its
    /// process runs. The wait goes on through either, saying on stderr what it waits for.
    fn until(&mut self, what: &str, fact: impl Fn(&Self) -> bool) {
        let mut seen = self.signature();
        let mut moved_at = self.clock.now_ns();
        while !fact(self) {
            let quiet = u64::try_from(self.quiet().as_nanos()).unwrap_or(u64::MAX);
            let now = self.clock.now_ns();
            let left = moved_at.saturating_add(quiet).saturating_sub(now);
            if left == 0 {
                // A member that has stated nothing yet has no law to bound the wait: it states
                // once its process is scheduled, and one whose process ended fails the wait.
                let silent: Vec<u64> = self
                    .members
                    .0
                    .keys()
                    .copied()
                    .filter(|id| !self.latest.contains_key(id))
                    .collect();
                for id in &silent {
                    let ended = self
                        .members
                        .0
                        .get_mut(id)
                        .and_then(|child| child.try_wait().unwrap());
                    if let Some(status) = ended {
                        panic!(
                            "{what}: member {id} ended before it stated anything: {status}\n{}",
                            self.dump()
                        );
                    }
                }
                let on_disk = self.latest.values().any(|stated| {
                    matches!(stated.disk, 'R' | 'X')
                        && stated.flight != 0
                        && stated.flight <= moved_at
                });
                assert!(
                    on_disk || !silent.is_empty(),
                    "{what}: nothing moved for {:?}\n{}",
                    self.quiet(),
                    self.dump()
                );
                if silent.is_empty() {
                    eprintln!("{what}: waiting on a flush\n{}", self.dump());
                } else {
                    eprintln!("{what}: waiting for members {silent:?} to state anything");
                }
                moved_at = now;
                continue;
            }
            self.next(Duration::from_nanos(left), what);
            if let Some((member, peer, taken)) = self.unresolved() {
                panic!(
                    "{what}: member {member} took {taken} heartbeats from {peer} unconfigured, \
                     more than any window holds\n{}",
                    self.dump()
                );
            }
            let now = self.signature();
            if now != seen {
                seen = now;
                moved_at = self.clock.now_ns();
            }
        }
    }

    /// Every member's last state and the suspicions reported, for a wait that failed.
    fn dump(&self) -> String {
        let now = self.clock.now_ns();
        let ms = |ns: u64| ns as f64 / 1e6;
        let mut out = format!("quiet period {:?}", self.quiet());
        for (member, stated) in &self.latest {
            out.push_str(&format!(
                "\n  member {member}, stated {:.1} ms ago: disk {} flush in flight {} floor \
                 {:.3} ms longest flush {:.3} ms wakes up to {:.3} ms late",
                ms(now.saturating_sub(stated.at)),
                stated.disk,
                if stated.flight == 0 {
                    "none".to_owned()
                } else {
                    format!("for {:.1} ms", ms(now.saturating_sub(stated.flight)))
                },
                ms(stated.floor),
                ms(stated.flush_most),
                ms(stated.late),
            ));
            for (peer, seen) in &stated.peers {
                out.push_str(&format!(
                    "\n    peer {peer}: trust {} judged {} configured {} taken {} sent {} \
                     interval {:.3} ms freshness {:.3} ms trusted until {} suspicions {} \
                     allowance {:.3}",
                    seen.trust,
                    seen.judged,
                    seen.configured,
                    seen.taken,
                    seen.sent,
                    ms(seen.interval),
                    ms(seen.freshness),
                    if seen.until == 0 {
                        "-".to_owned()
                    } else {
                        format!("{:+.1} ms of the line", ms(seen.until) - ms(stated.at))
                    },
                    seen.suspicions,
                    seen.allowance,
                ));
            }
        }
        for (member, peer, suspected) in &self.suspicions {
            out.push_str(&format!(
                "\n  suspicion by {member} of {peer}: {:.1} ms ago, {:.3} ms past the last \
                 heartbeat's schedule, bound {:.3} ms",
                ms(now.saturating_sub(suspected.at)),
                ms(suspected.at.saturating_sub(suspected.due)),
                ms(suspected.detection),
            ));
        }
        out
    }

    /// Whether `member` stated, at or after `since` on the host clock, that it holds `peer`
    /// suspected: a fact about the run after `since`, whether the suspicion began before it (a
    /// live peer falsely suspected just before it stalled or died, never trusted again) or after.
    fn holds_suspected(&self, member: u64, peer: u64, since: u64) -> bool {
        self.latest.get(&member).is_some_and(|stated| {
            stated.at >= since
                && stated
                    .peers
                    .get(&peer)
                    .is_some_and(|seen| seen.trust == 'S')
        })
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

/// A group of `nodes` member processes, started: each ready, its disk's command port known, and
/// told to begin. The directory holds their files.
struct Group {
    _directory: tempfile::TempDir,
    supervisor: Supervisor,
    /// Where each member's disk takes commands.
    wakes: BTreeMap<u64, u64>,
}

#[allow(
    clippy::disallowed_methods,
    reason = "real processes on the host's clock, threads and environment (CLAUDE.md §1a, end to end)"
)]
fn start(nodes: u64) -> Group {
    let directory = tempfile::tempdir().unwrap();
    let mut members = Members(
        (1..=nodes)
            .map(|id| {
                let child = Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "member_process",
                        "--nocapture",
                        "--test-threads=1",
                    ])
                    .env("HYPER_LIVENESS_NODE", id.to_string())
                    .env("HYPER_LIVENESS_NODES", nodes.to_string())
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
    // Every member ready: its sockets bound, where its disk takes commands and where it listens.
    // A member just started has no law yet to bound the wait: it is ready once its process is
    // scheduled, and one whose process ended fails it, looked at every retransmission timeout.
    let mut wakes = BTreeMap::new();
    let mut ports = BTreeMap::new();
    while wakes.len() < nodes as usize {
        let line = match lines.recv_timeout(RTO) {
            Ok(line) => line,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                for (id, child) in &mut members.0 {
                    if let Some(status) = child.try_wait().unwrap() {
                        panic!("member {id} ended before it was ready: {status}");
                    }
                }
                continue;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                panic!("every member ended before it was ready")
            }
        };
        // libtest prints "test member_process ... " before the body runs, without a newline.
        if let Some((_, ready)) = line.split_once("ready ") {
            let fields: Vec<u64> = ready.split(' ').map(|f| f.parse().unwrap()).collect();
            wakes.insert(fields[0], fields[1]);
            ports.insert(fields[0], fields[2]);
        }
    }
    let ports = ports
        .values()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join(",");
    for child in members.0.values_mut() {
        writeln!(child.stdin.as_mut().unwrap(), "start {ports}").unwrap();
    }
    Group {
        _directory: directory,
        supervisor: Supervisor {
            members,
            lines,
            clock: Clock::new().unwrap(),
            trace: std::env::var_os("HYPER_LIVENESS_TRACE").is_some(),
            latest: BTreeMap::new(),
            suspicions: Vec::new(),
            heard: Vec::new(),
            configured_since: BTreeMap::new(),
        },
        wakes,
    }
}

#[test]
#[allow(
    clippy::disallowed_methods,
    reason = "real processes on the host's clock, threads and environment (CLAUDE.md §1a, end to end)"
)]
fn a_stalled_disk_and_a_killed_node_are_suspected_and_no_live_one_is() {
    if std::env::var("HYPER_LIVENESS_NODE").is_ok() {
        return;
    }
    let Group {
        _directory,
        mut supervisor,
        wakes,
    } = start(NODES);
    let clock = Clock::new().unwrap();

    // Every pair configured.
    supervisor.until("every pair configured", |s| {
        s.latest.len() == NODES as usize
            && s.latest.values().all(|stated| {
                stated.peers.len() == NODES as usize - 1
                    && stated.peers.values().all(|p| p.configured)
            })
    });

    // A disk that stops completing flushes.
    let stalled_at = clock.now_ns();
    UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .send_to(b"stall", ("127.0.0.1", wakes[&STALLED] as u16))
        .unwrap();
    let watchers: Vec<u64> = (1..=NODES).filter(|m| *m != STALLED).collect();
    supervisor.until("every other member suspects the stalled disk", |s| {
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
    let mut victim = supervisor.members.0.remove(&KILLED).unwrap();
    let killed_at = clock.now_ns();
    victim.kill().unwrap();
    victim.wait().unwrap();
    let survivors: Vec<u64> = (1..=NODES).filter(|m| *m != KILLED).collect();
    supervisor.until("every survivor suspects the killed member", |s| {
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
    supervisor.until("each live member states both suspected", |s| {
        live.iter().all(|m| {
            s.holds_suspected(*m, STALLED, killed_at) && s.holds_suspected(*m, KILLED, killed_at)
        })
    });
    supervisor.members.stop();

    let (mut suspicions, mut allowance) = (0u64, 0.0f64);
    for member in &live {
        for peer in live.iter().filter(|p| *p != member) {
            // Whether a live peer is trusted at this moment is no promise; the count is.
            let seen = supervisor.latest[member].peers[peer];
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

/// Members of the group whose victim dies young: the fewest that elect without one of them, so each
/// survivor has one live link, whose configuration lengthens its interval and with it the rate its
/// node's pool is fed at (`docs/timing.md` §3, item 10).
const YOUNG_NODES: u64 = 3;

/// A member killed in its links' first heartbeats, once it has heard every peer and sent to each,
/// before any pair could have its own evidence (a configuration needs an Allan level of seven
/// windows, 56 heartbeats at the least): every survivor suspects it, within the bound it states
/// where it states one, and no later than the first poll once both its freshness point has passed
/// and a pair of its own has configured, the evidence the young link is judged by
/// (`docs/timing.md` §2.8, "Judged before its own evidence").
#[test]
#[allow(
    clippy::disallowed_methods,
    reason = "real processes on the host's clock, threads and environment (CLAUDE.md §1a, end to end)"
)]
fn a_node_killed_in_its_first_heartbeats_is_suspected_once_a_sibling_has_its_evidence() {
    if std::env::var("HYPER_LIVENESS_NODE").is_ok() {
        return;
    }
    let victim = YOUNG_NODES;
    let Group {
        _directory,
        mut supervisor,
        ..
    } = start(YOUNG_NODES);
    let clock = Clock::new().unwrap();
    supervisor.until("the victim heard every peer", |s| s.heard.contains(&victim));
    let mut child = supervisor.members.0.remove(&victim).unwrap();
    let killed_at = clock.now_ns();
    child.kill().unwrap();
    child.wait().unwrap();
    let survivors: Vec<u64> = (1..YOUNG_NODES).collect();
    supervisor.until("every survivor suspects the young victim", |s| {
        survivors
            .iter()
            .all(|m| s.holds_suspected(*m, victim, killed_at))
    });
    supervisor.members.stop();
    let mut noticed = Vec::new();
    for member in &survivors {
        let found = supervisor.suspicion(*member, victim);
        if found.due > 0 && found.detection > 0 {
            assert!(
                found.at - found.due <= found.detection,
                "member {member} suspected the young victim {} ns after its last heartbeat was \
                 due, past its stated bound of {} ns",
                found.at - found.due,
                found.detection
            );
        }
        let sibling = survivors.iter().copied().find(|m| m != member).unwrap();
        let evidence = supervisor
            .configured_since
            .get(&(*member, sibling))
            .copied()
            .unwrap_or(u64::MAX);
        assert!(
            found.noticed <= (found.at + found.late).max(evidence),
            "member {member} noticed the young victim's death {:?} after the kill: its freshness \
             point {:?} after it, its wakes up to {:?} late, its sibling configured {:?} after it",
            Duration::from_nanos(found.noticed.saturating_sub(killed_at)),
            Duration::from_nanos(found.at.saturating_sub(killed_at)),
            Duration::from_nanos(found.late),
            Duration::from_nanos(evidence.saturating_sub(killed_at)),
        );
        noticed.push((
            member,
            Duration::from_nanos(found.noticed.saturating_sub(killed_at)),
        ));
    }
    println!("a node killed in its first heartbeats was noticed dead after {noticed:?}");
}
