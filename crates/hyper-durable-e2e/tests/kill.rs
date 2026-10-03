//! hyper-durable in real use. Each scenario starts a group of real processes
//! (`hyper-durable-node`), each a replica on hyper-log over its own fully flushed file and its own
//! UDP socket, and drives it as a client would. Members are killed with `SIGKILL` at named
//! durability points (`control::Point`: a write submitted, a write durable whose answer was not
//! taken, messages released, a change behind the commit fence, a change applied, an entry acted on
//! at start) and at random points, a flush is made to fail, a disk is stalled, and the
//! configuration changes. What is asserted is what a client and an operator can observe:
//! - every write a member answered is read back, linearizably, through whichever member leads;
//! - every member that is up applies the same history (the same digest at the same index);
//! - focal F17's two cases: a founder that removed its only peer, killed once it applied the
//!   removal and its peer stopped for good, elects itself alone; and a host that acted on a fence
//!   acts on it again from its own log before it hears from anyone, never below it;
//! - a member whose flush failed fences, exits, and rejoins on its log with nothing it answered
//!   lost;
//! - a member whose disk stalls is suspected by every other member, which elect without it;
//! - a member started again is reported restarted to every other member's core.
//!
//! The members' failure detectors are their own: each runs the node-pair liveness stream
//! (`hyper_liveness`, timing step L-3) and takes its words to its replica, and the test tells no
//! member what to believe. The test derives nothing. It waits on facts — a leader, an answer, a
//! line a member prints, a report — and goes on while the group moves (`docs/sim.md` §4.2): while
//! any member's term, commit, applied index, last index or restarts seen moves, or, while a member
//! still has a pair no margin judges, the heartbeats it has taken (the evidence its detectors are
//! built from). It gives up once a quiet period passes with nothing moved: the longest any member
//! states its detectors take to suspect a crash, its election's span and rounds and an ask's
//! rounds, by the members' own law; never less than the test's own retransmission timeout, past
//! which it could not tell a member that did not move from an answer it did not wait for. An ask
//! is an idempotent datagram, sent again at that timeout, RFC 6298's: one second before any
//! measurement and never less after (§2.1, §2.4; a loopback round trip is far below it).
//!
//! The scenarios run one after another in this one thread (`harness = false`), one group at a
//! time; the test reads each member's output on a thread of its own, one a process, at most three.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::disallowed_macros,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cognitive_complexity,
    missing_docs
)]

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, ErrorKind};
use std::net::{SocketAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, sync_channel};
use std::time::{Duration, Instant};

use hyper_durable_e2e::control::{self, Order, Point, Report};
use hyper_raft::proto::ConfChangeType;
use hyper_raft_e2e::run;
use hyper_raft_e2e::wire::{self, Control, Kind, Op, Outcome};

/// What a wait last saw of each member's progress, and until when it waits without seeing more.
struct Watch {
    seen: BTreeMap<u64, [u64; 7]>,
    until: Instant,
}

const NODE: &str = env!("CARGO_BIN_EXE_hyper-durable-node");
const TMP: &str = env!("CARGO_TARGET_TMPDIR");

/// RFC 6298 §2.1 and §2.4: the retransmission timeout before any round trip is measured, and the
/// least it is ever set to after, one second.
const RTO: Duration = Duration::from_secs(1);
/// The rounds an ask and its answer take beside an election: the ask, the broadcast that commits
/// it, and the answer.
const ANSWER_ROUNDS: u32 = 3;
/// The rounds an election takes past its delay: pre-vote, vote, and the new leader's first
/// append (`docs/timing.md` §2.3).
const ELECTION_ROUNDS: u32 = 3;
/// Keys a member holds at most, and asks it keeps waiting: far past what a scenario writes.
const MAX_KEYS: usize = 1 << 16;
const MAX_PENDING: usize = 64;
/// Writes a scenario makes before and after its fault: enough for the group to have a history to
/// lose, each one a commit the members must all apply.
const WRITES: usize = 4;

#[expect(
    clippy::disallowed_methods,
    reason = "a test removes the files it made in its own target directory"
)]
fn remove(path: &Path) {
    let _ = std::fs::remove_file(path);
}

struct Member {
    id: u64,
    child: Option<Child>,
    lines: Option<Receiver<String>>,
    address: SocketAddr,
    log: PathBuf,
}

struct Cluster {
    name: String,
    members: Vec<Member>,
    voters: Vec<u64>,
    /// What each member's latest report says its law takes: its stated detection, its election's
    /// span and rounds, and an ask's rounds.
    law: BTreeMap<u64, Duration>,
    /// Each member's latest report and when it came: what a failed wait prints.
    last: BTreeMap<u64, (Instant, Report)>,
    test: UdpSocket,
    datagram: usize,
    next_id: u64,
    buffer: Vec<u8>,
    /// Every write a member answered: key → value.
    acked: BTreeMap<Vec<u8>, Vec<u8>>,
    /// Members that printed `stopped`: they wait to be killed and answer nothing.
    stopped: Vec<u64>,
}

impl Drop for Cluster {
    fn drop(&mut self) {
        for member in &mut self.members {
            if let Some(mut child) = member.child.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
            remove(&member.log);
            remove(&run::path(&member.log));
        }
    }
}

/// Starts the member `id` of `voters` on its log; its port and the lines it prints after.
#[allow(
    clippy::disallowed_methods,
    reason = "real processes on the host's clock, threads and environment (CLAUDE.md §1a, end to end)"
)]
fn spawn(id: u64, voters: &[u64], log: &Path) -> (Child, u16, Receiver<String>) {
    let list: Vec<String> = voters.iter().map(u64::to_string).collect();
    let mut child = Command::new(NODE)
        .args(["--id", &id.to_string()])
        .args(["--voters", &list.join(",")])
        .args(["--listen", "127.0.0.1:0"])
        .args(["--log", log.to_str().unwrap()])
        .args(["--max-keys", &MAX_KEYS.to_string()])
        .args(["--max-pending", &MAX_PENDING.to_string()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("the member starts");
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    let port = line
        .trim()
        .strip_prefix("listening ")
        .and_then(|port| port.parse().ok())
        .unwrap_or_else(|| panic!("member {id} did not start: {line:?}"));
    let (lines, read) = sync_channel(4096);
    std::thread::spawn(move || {
        for line in stdout.lines() {
            let Ok(line) = line else { break };
            if lines.send(line).is_err() {
                break;
            }
        }
    });
    (child, port, read)
}

impl Cluster {
    fn start(name: &str, voters: u64) -> Self {
        let ids: Vec<u64> = (1..=voters).collect();
        let mut members = Vec::new();
        for &id in &ids {
            let log =
                PathBuf::from(TMP).join(format!("durable-{}-{name}-{id}.log", std::process::id()));
            remove(&log);
            remove(&run::path(&log));
            let (child, port, lines) = spawn(id, &ids, &log);
            members.push(Member {
                id,
                child: Some(child),
                lines: Some(lines),
                address: SocketAddr::from(([127, 0, 0, 1], port)),
                log,
            });
        }
        let test = UdpSocket::bind("127.0.0.1:0").unwrap();
        let datagram = wire::largest(&test).unwrap();
        let mut cluster = Self {
            name: name.to_owned(),
            members,
            voters: ids,
            law: BTreeMap::new(),
            last: BTreeMap::new(),
            test,
            datagram,
            next_id: 0,
            buffer: Vec::new(),
            acked: BTreeMap::new(),
            stopped: Vec::new(),
        };
        cluster.tell_peers();
        cluster
    }

    fn member(&mut self, id: u64) -> &mut Member {
        self.members.iter_mut().find(|m| m.id == id).unwrap()
    }

    fn up(&self) -> Vec<u64> {
        self.members
            .iter()
            .filter(|m| m.child.is_some())
            .map(|m| m.id)
            .collect()
    }

    /// Takes what a report says of the member's law: the longest its detectors state to suspect
    /// a crash, or, while a pair no margin judges takes heartbeats at a longer interval, that
    /// interval, the most a wait that goes on while those heartbeats move can see none; then its
    /// election's span and rounds, and an ask's rounds.
    fn heard_law(&mut self, id: u64, report: &Report) {
        let round = Duration::from_nanos(report.round_ns);
        let law = Duration::from_nanos(report.detection_ns.max(report.unjudged_interval_ns))
            + Duration::from_nanos(report.span_ns)
            + round * (ELECTION_ROUNDS + ANSWER_ROUNDS);
        self.law.insert(id, law);
    }

    /// Sends what is in the buffer to `id` and waits for its answer to `ask`, one retransmission
    /// timeout at most.
    #[allow(
        clippy::disallowed_methods,
        reason = "real processes on the host's clock, threads and environment (CLAUDE.md §1a, end to end)"
    )]
    fn exchange(&mut self, id: u64, ask: u64) -> Option<Vec<u8>> {
        let to = self.member(id).address;
        let sealed = wire::seal(&mut self.buffer, self.datagram);
        assert!(sealed);
        self.test.send_to(&self.buffer, to).ok()?;
        let deadline = Instant::now() + RTO;
        let mut received = vec![0u8; wire::MAX_DATAGRAM];
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            // Waited for by a peek, taken without waiting (`wire::arrives`). A reset is an
            // earlier send's, to a member gone (Windows reports it on the next receive).
            if !wire::arrives(&self.test, Some(left), &mut received).unwrap() {
                return None;
            }
            let length = match wire::take(&self.test, &mut received) {
                Ok((length, _)) => length,
                Err(error)
                    if matches!(
                        error.kind(),
                        ErrorKind::ConnectionReset | ErrorKind::WouldBlock
                    ) =>
                {
                    continue;
                }
                Err(error) => panic!("{}: the test's socket: {error}", self.name),
            };
            let Some((Kind::Response, body)) = wire::open(&received[..length]) else {
                continue;
            };
            let mut reader = wire::Reader::new(body);
            if reader.u64() == Some(ask) {
                return Some(body.to_vec());
            }
        }
    }

    fn ask(&mut self, id: u64, op: &Op<'_>) -> Option<Outcome> {
        self.next_id += 1;
        let ask = self.next_id;
        wire::put_request(&mut self.buffer, ask, op);
        let body = self.exchange(id, ask)?;
        wire::read_response(&body).map(|(_, outcome)| outcome)
    }

    fn order(&mut self, id: u64, order: Order) -> Option<Outcome> {
        self.next_id += 1;
        let ask = self.next_id;
        control::put_order(&mut self.buffer, ask, &order);
        let body = self.exchange_resent(id, ask)?;
        wire::read_response(&body).map(|(_, outcome)| outcome)
    }

    #[allow(
        clippy::disallowed_methods,
        reason = "real processes on the host's clock, threads and environment (CLAUDE.md §1a, end to end)"
    )]
    fn report(&mut self, id: u64) -> Option<Report> {
        self.next_id += 1;
        let ask = self.next_id;
        control::put_order(&mut self.buffer, ask, &Order::Report);
        let body = self.exchange(id, ask)?;
        let report = control::read_report(&body, hyper_raft::MAX_MEMBERS).map(|(_, r)| r)?;
        self.heard_law(id, &report);
        self.last.insert(id, (Instant::now(), report.clone()));
        Some(report)
    }

    /// What a failed wait saw: each member's latest report, how long ago it came, and the quiet
    /// period in force.
    #[allow(
        clippy::disallowed_methods,
        reason = "real processes on the host's clock, threads and environment (CLAUDE.md §1a, end to end)"
    )]
    fn state(&self) -> String {
        let mut out = format!("quiet {:?}", self.quiet());
        for (id, (at, r)) in &self.last {
            let s = &r.status;
            out.push_str(&format!(
                "\n  member {id}, {:?} ago: term {} leads {} commit {} applied {} last {}; \
                 suspected {:?} heard {:?} unjudged {} at up to {:?} taken {} restarts {}; \
                 detection {:?} span {:?} round {:?}",
                at.elapsed(),
                s.term,
                s.leads,
                s.commit,
                s.applied,
                s.last_index,
                r.suspected,
                r.heard,
                r.unjudged,
                Duration::from_nanos(r.unjudged_interval_ns),
                r.taken,
                r.restarts,
                Duration::from_nanos(r.detection_ns),
                Duration::from_nanos(r.span_ns),
                Duration::from_nanos(r.round_ns),
            ));
        }
        out
    }

    /// Tells every member up where the others listen, sent again each retransmission timeout until
    /// it answers, for as long as its process runs. A member just started has no group yet whose
    /// progress could bound the wait: it serves once its process is scheduled. A member whose
    /// process ended instead fails the wait.
    fn tell_peers(&mut self) {
        let peers: Vec<(u64, SocketAddr)> =
            self.members.iter().map(|m| (m.id, m.address)).collect();
        for id in self.up() {
            self.next_id += 1;
            let ask = self.next_id;
            wire::put_control(&mut self.buffer, ask, &Control::Peers(peers.clone()));
            loop {
                if self.exchange(id, ask).is_some() {
                    break;
                }
                let name = self.name.clone();
                let child = self.member(id).child.as_mut().unwrap();
                assert!(
                    child.try_wait().unwrap().is_none(),
                    "{name}: member {id} ended before it was told its peers"
                );
            }
        }
    }

    /// How long the group may go with nothing moving before a wait gives up: the longest any
    /// member's law takes, from its latest report, and never less than a retransmission timeout.
    /// A live group suspects a dead leader within its stated detection, elects or starts a new
    /// term within an election, and answers within an ask's rounds, so a quiet period in which
    /// nothing moves is a group that is stuck, not one that drew a split vote. A wait has no count
    /// of elections: it goes on while the group moves.
    fn quiet(&self) -> Duration {
        self.law.values().copied().max().unwrap_or(RTO).max(RTO)
    }

    /// A fresh watch over the group's progress.
    #[allow(
        clippy::disallowed_methods,
        reason = "real processes on the host's clock, threads and environment (CLAUDE.md §1a, end to end)"
    )]
    fn watch(&self) -> Watch {
        Watch {
            seen: BTreeMap::new(),
            until: Instant::now() + self.quiet(),
        }
    }

    /// Whether the group is still moving: asks every member up and not stopped for its report,
    /// and extends the watch by a quiet period whenever any member's term, commit, applied index,
    /// last index or restarts seen has moved since it last looked, or, while it has a pair no
    /// margin judges, the heartbeats it has taken.
    #[allow(
        clippy::disallowed_methods,
        reason = "real processes on the host's clock, threads and environment (CLAUDE.md §1a, end to end)"
    )]
    fn moving(&mut self, watch: &mut Watch) -> bool {
        let mut moved = false;
        for id in self.up() {
            if self.stopped.contains(&id) {
                continue;
            }
            if let Some(report) = self.report(id) {
                let judging = if report.unjudged > 0 { report.taken } else { 0 };
                let at = [
                    report.status.term,
                    report.status.commit,
                    report.status.applied,
                    report.status.last_index,
                    report.restarts,
                    report.unjudged,
                    judging,
                ];
                if watch.seen.insert(id, at) != Some(at) {
                    moved = true;
                }
            }
        }
        if moved {
            watch.until = Instant::now() + self.quiet();
        }
        Instant::now() < watch.until
    }

    /// Waits while the group moves until `fact` holds of the members' reports; whether it did.
    fn until(&mut self, fact: impl Fn(&BTreeMap<u64, Report>) -> bool) -> bool {
        let mut watch = self.watch();
        loop {
            let asked: Vec<u64> = self
                .up()
                .into_iter()
                .filter(|id| !self.stopped.contains(id))
                .collect();
            let reports: BTreeMap<u64, Report> = asked
                .into_iter()
                .filter_map(|id| self.report(id).map(|report| (id, report)))
                .collect();
            if fact(&reports) {
                return true;
            }
            if !self.moving(&mut watch) {
                return false;
            }
        }
    }

    /// A line `id` prints that starts with `prefix`, waited for while the group moves.
    fn line_while_moving(&mut self, id: u64, prefix: &str) -> Option<String> {
        let mut watch = self.watch();
        loop {
            let quiet = self.quiet();
            if let Some(line) = self.line(id, prefix, quiet) {
                return Some(line);
            }
            if !self.moving(&mut watch) {
                return None;
            }
        }
    }

    /// Sends what is in the buffer to `id` until it answers `ask`: an instruction is an
    /// idempotent datagram, which a loaded runner may drop or deliver late, so it is sent again
    /// each retransmission timeout, for as long as a quiet period.
    #[allow(
        clippy::disallowed_methods,
        reason = "real processes on the host's clock, threads and environment (CLAUDE.md §1a, end to end)"
    )]
    fn exchange_resent(&mut self, id: u64, ask: u64) -> Option<Vec<u8>> {
        let until = Instant::now() + self.quiet();
        loop {
            if let Some(body) = self.exchange(id, ask) {
                return Some(body);
            }
            if Instant::now() >= until {
                return None;
            }
        }
    }

    /// The member that leads, once one does, waited for while the group moves.
    fn leader(&mut self) -> Option<u64> {
        let mut watch = self.watch();
        while self.moving(&mut watch) {
            for id in self.up() {
                if self.stopped.contains(&id) {
                    continue;
                }
                if self.report(id).is_some_and(|r| r.status.leads) {
                    return Some(id);
                }
            }
        }
        None
    }

    /// Writes `key` = `value` through the leader until a member answers it, while the group
    /// moves; the index it was applied at.
    fn put(&mut self, key: &[u8], value: &[u8]) -> Option<u64> {
        let mut watch = self.watch();
        while self.moving(&mut watch) {
            let Some(leader) = self.leader() else {
                continue;
            };
            if let Some(Outcome::Put(index)) = self.ask(leader, &Op::Put { key, value }) {
                self.acked.insert(key.to_vec(), value.to_vec());
                return Some(index);
            }
        }
        None
    }

    /// Reads `key` linearizably through the leader.
    fn get(&mut self, key: &[u8]) -> Option<Option<Vec<u8>>> {
        let mut watch = self.watch();
        while self.moving(&mut watch) {
            let Some(leader) = self.leader() else {
                continue;
            };
            if let Some(Outcome::Value(value)) = self.ask(leader, &Op::Get { key }) {
                return Some(value);
            }
        }
        None
    }

    /// A line `id` prints that starts with `prefix`, within `wait`.
    #[allow(
        clippy::disallowed_methods,
        reason = "real processes on the host's clock, threads and environment (CLAUDE.md §1a, end to end)"
    )]
    fn line(&mut self, id: u64, prefix: &str, wait: Duration) -> Option<String> {
        let deadline = Instant::now() + wait;
        let lines = self.member(id).lines.as_ref()?;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match lines.recv_timeout(left) {
                Ok(line) if line.starts_with(prefix) => {
                    if line.starts_with("stopped") {
                        self.stopped.push(id);
                    }
                    return Some(line);
                }
                Ok(_) => {}
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => return None,
            }
        }
    }

    /// Kills `id` with `SIGKILL`. Nobody is told: the others' detectors find out.
    fn kill(&mut self, id: u64) {
        let member = self.member(id);
        if let Some(mut child) = member.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        member.lines = None;
        self.stopped.retain(|&stopped| stopped != id);
        self.law.remove(&id);
        self.last.remove(&id);
    }

    /// The exit status of a member that fenced and exited, once it printed `fenced` within
    /// `wait`.
    fn exited(&mut self, id: u64, wait: Duration) -> Option<i32> {
        self.line(id, "fenced", wait)?;
        let member = self.member(id);
        let status = member.child.take()?.wait().ok()?;
        member.lines = None;
        self.law.remove(&id);
        self.last.remove(&id);
        status.code()
    }

    /// Starts `id` again on its log, told where its peers listen when `tell`; then, while the
    /// group moves, every other member up that had heard its last run and shares a group with it,
    /// by both their configurations, reports the restart its stream saw
    /// (`hyper_liveness::Change::Restarted`). A member that never took a heartbeat of the last
    /// run cannot tell the new one from a first; and one whose configuration and the restarted
    /// one's do not both name the other keeps no stream to it: a peer removed by a change the
    /// restarted member holds in its log.
    fn restart(&mut self, id: u64, tell: bool) {
        let voters = self.voters.clone();
        let before: BTreeMap<u64, u64> = self
            .up()
            .into_iter()
            .filter_map(|other| self.report(other).map(|r| (other, r)))
            .filter(|(_, r)| r.heard.contains(&id))
            .map(|(other, r)| (other, r.restarts))
            .collect();
        let member = self.member(id);
        assert!(member.child.is_none());
        let (child, port, lines) = spawn(id, &voters, &member.log.clone());
        member.child = Some(child);
        member.lines = Some(lines);
        member.address = SocketAddr::from(([127, 0, 0, 1], port));
        if !tell {
            return;
        }
        self.tell_peers();
        let name = self.name.clone();
        let seen = self.until(|reports| {
            let Some(restarted) = reports.get(&id) else {
                return false;
            };
            before.iter().all(|(other, restarts)| {
                reports.get(other).is_none_or(|r| {
                    r.restarts > *restarts
                        || !r.voters.contains(&id)
                        || !restarted.voters.contains(other)
                })
            })
        });
        assert!(
            seen,
            "{name}: a member's stream never reported {id}'s restart; {}",
            self.state()
        );
    }

    /// Every write a member answered reads back as it was answered, and every member up applies
    /// the same history once a last write has reached them all.
    fn verify(&mut self) {
        let acked = self.acked.clone();
        for (key, value) in &acked {
            let read = self.get(key);
            assert_eq!(
                read,
                Some(Some(value.clone())),
                "{}: an answered write of {} did not read back",
                self.name,
                String::from_utf8_lossy(key)
            );
        }
        let index = self.put(b"last", b"write").expect("a last write");
        let up = self.up().len();
        let name = self.name.clone();
        let caught = self.until(|reports| {
            let caught: Vec<&Report> = reports
                .values()
                .filter(|r| r.status.applied >= index)
                .collect();
            if caught.len() < up {
                return false;
            }
            let first = &caught[0].status;
            assert!(
                caught
                    .iter()
                    .all(|r| r.status.applied != first.applied || r.status.digest == first.digest),
                "{name}: members applied different histories: {reports:?}"
            );
            true
        });
        assert!(
            caught,
            "{name}: members stopped moving before they caught up to {index}; {}",
            self.state()
        );
    }

    fn write_some(&mut self, prefix: &str, count: usize) {
        for i in 0..count {
            let key = format!("{prefix}/{i}");
            assert!(
                self.put(key.as_bytes(), format!("v{i}").as_bytes())
                    .is_some(),
                "{}: a write was never answered; {}",
                self.name,
                self.state()
            );
        }
    }
}

/// The most entries one pass of a member through a durability point can carry: a turn takes at
/// most this many datagrams (`node::receive_until`: the asks it keeps waiting, a message from each
/// voter, and its own wake), so with the test's writes one at a time a member passes each point at
/// least once for every so many writes it takes.
fn turn(voters: usize) -> usize {
    MAX_PENDING + voters + 1
}

/// Kills `target` (the leader or a follower) at `point`, the `count`-th time it passes it, while
/// the group takes writes; restarts it on its log; every answered write reads back and every
/// member applies the same history.
fn kill_at(point: Point, leader: bool, count: u64, name: &str) {
    let mut cluster = Cluster::start(name, 3);
    cluster.write_some("before", WRITES);
    let lead = cluster.leader().expect("a leader");
    let target = if leader {
        lead
    } else {
        cluster.up().into_iter().find(|&id| id != lead).unwrap()
    };
    assert_eq!(
        cluster.order(target, Order::Arm(point, count)),
        Some(Outcome::Done)
    );
    let most = usize::try_from(count).unwrap() * turn(3);
    let mut answered = 0;
    let mut written = 0;
    let stopped = loop {
        if let Some(line) = cluster.line(target, "stopped", Duration::ZERO) {
            break line;
        }
        let key = format!("during/{written}");
        written += 1;
        // The write may go to a target that stopped: unanswered, and asked again elsewhere.
        if cluster.put(key.as_bytes(), b"x").is_some() {
            answered += 1;
        }
        assert!(
            answered <= most,
            "{name}: member {target} never reached {}",
            point.name()
        );
    };
    assert_eq!(stopped, format!("stopped {}", point.name()));
    cluster.kill(target);
    cluster.write_some("while-down", WRITES);
    cluster.restart(target, true);
    cluster.write_some("after", WRITES);
    cluster.verify();
}

/// A member's disk stops completing flushes: its heartbeats stop with it (each needs a flush made
/// after the previous was due), every other member's detector suspects it, and they elect without
/// it if it led and take writes. Then it is killed and started again on its log.
fn stalled_disk(leader: bool) {
    let name = if leader {
        "stall-leader"
    } else {
        "stall-follower"
    };
    let mut cluster = Cluster::start(name, 3);
    cluster.write_some("before", WRITES);
    let lead = cluster.leader().expect("a leader");
    let target = if leader {
        lead
    } else {
        cluster.up().into_iter().find(|&id| id != lead).unwrap()
    };
    assert_eq!(
        cluster.order(target, Order::StallFlush),
        Some(Outcome::Done)
    );
    // The stalled member is asked nothing more: its answers would say what it holds, not what the
    // group does.
    cluster.stopped.push(target);
    let suspected = cluster.until(|reports| {
        reports
            .iter()
            .filter(|(id, _)| **id != target)
            .all(|(_, r)| r.suspected.contains(&target))
    });
    assert!(
        suspected,
        "{name}: the stalled member was not suspected by every other; {}",
        cluster.state()
    );
    cluster.write_some("while-stalled", WRITES);
    cluster.kill(target);
    cluster.restart(target, true);
    cluster.write_some("after", WRITES);
    cluster.verify();
}

/// focal F17 (`cli_network`): the founder of a group of two removes its only peer; the operator
/// stops the peer for good once the founder says it applied the change; the founder is killed
/// there and restarted alone, and must elect itself and take a write.
fn founder() {
    let mut cluster = Cluster::start("founder", 2);
    cluster.write_some("before", WRITES);
    let founder = cluster.leader().expect("a leader");
    let peer = cluster.up().into_iter().find(|&id| id != founder).unwrap();
    assert_eq!(
        cluster.order(founder, Order::Arm(Point::Changed, 1)),
        Some(Outcome::Done)
    );
    assert_eq!(
        cluster.order(founder, Order::Change(ConfChangeType::RemoveNode, peer)),
        Some(Outcome::Done)
    );
    let stopped = cluster.line_while_moving(founder, "stopped");
    assert_eq!(
        stopped.as_deref(),
        Some("stopped changed"),
        "the founder never applied the removal"
    );
    // The operator acts on the founder's word: the peer goes for good, then the founder dies.
    cluster.kill(peer);
    cluster.kill(founder);
    cluster.members.retain(|m| m.id != peer);
    cluster.restart(founder, true);
    let alone = cluster.until(|reports| {
        reports
            .get(&founder)
            .is_some_and(|r| r.status.leads && r.voters == vec![founder])
    });
    assert!(
        alone,
        "the founder stopped moving without electing itself after the kill; {}",
        cluster.state()
    );
    cluster.write_some("alone", WRITES);
    cluster.verify();
}

/// The same founder killed while the removal waits behind its commit fence: it applied nothing,
/// so the operator stops no one; restarted, the group finishes the removal.
#[allow(
    clippy::disallowed_methods,
    reason = "real processes on the host's clock, threads and environment (CLAUDE.md §1a, end to end)"
)]
fn founder_fenced() {
    let mut cluster = Cluster::start("founder-fenced", 2);
    cluster.write_some("before", WRITES);
    let founder = cluster.leader().expect("a leader");
    let peer = cluster.up().into_iter().find(|&id| id != founder).unwrap();
    assert_eq!(
        cluster.order(founder, Order::Arm(Point::Fenced, 1)),
        Some(Outcome::Done)
    );
    assert_eq!(
        cluster.order(founder, Order::Change(ConfChangeType::RemoveNode, peer)),
        Some(Outcome::Done)
    );
    let stopped = cluster.line_while_moving(founder, "stopped");
    assert_eq!(stopped.as_deref(), Some("stopped fenced"));
    cluster.kill(founder);
    cluster.restart(founder, true);
    let mut watch = cluster.watch();
    let mut asked = Instant::now();
    loop {
        if let Some(leader) = cluster.leader()
            && let Some(report) = cluster.report(leader)
            && report.voters == vec![founder]
        {
            break;
        }
        // A removal the kill lost is proposed again, once a quiet period has passed without it.
        if asked.elapsed() > cluster.quiet()
            && let Some(leader) = cluster.leader()
        {
            let _ = cluster.order(leader, Order::Change(ConfChangeType::RemoveNode, peer));
            asked = Instant::now();
        }
        assert!(
            cluster.moving(&mut watch),
            "the group stopped moving without finishing the removal after the kill; {}",
            cluster.state()
        );
    }
    cluster.kill(peer);
    cluster.members.retain(|m| m.id != peer);
    cluster.write_some("alone", WRITES);
    cluster.verify();
}

/// focal F17 (`cli_upgrade`): a member acts on a fence (an entry it acts on at its next start)
/// and is killed there; restarted and told of no peer, it acts on the fence again from its own
/// log before it hears from anyone.
fn fence_host(leader: bool) {
    let mut cluster = Cluster::start(
        if leader {
            "fence-leader"
        } else {
            "fence-follower"
        },
        3,
    );
    cluster.write_some("before", WRITES);
    let lead = cluster.leader().expect("a leader");
    let host = if leader {
        lead
    } else {
        cluster.up().into_iter().find(|&id| id != lead).unwrap()
    };
    assert_eq!(
        cluster.order(host, Order::Arm(Point::Acted, 1)),
        Some(Outcome::Done)
    );
    let _ = cluster.put(b"fence/upgrade", b"7");
    let acted = cluster
        .line_while_moving(host, "acted")
        .expect("the host never acted on the fence");
    let acted: u64 = acted.trim_start_matches("acted ").parse().unwrap();
    let stopped = cluster.line_while_moving(host, "stopped");
    assert_eq!(stopped.as_deref(), Some("stopped acted"));
    cluster.kill(host);
    // Restarted on its log and told of no one: whatever it reaches, it reaches alone.
    cluster.restart(host, false);
    let again = cluster
        .line_while_moving(host, "acted")
        .expect("the host reopened below the fence it acted on");
    let again: u64 = again.trim_start_matches("acted ").parse().unwrap();
    assert_eq!(again, acted, "the host acted on another entry at its start");
    cluster.tell_peers();
    cluster.verify();
}

/// A member's flush fails: it fences, exits, and is started again on its log; nothing answered
/// is lost.
fn failed_flush(leader: bool) {
    let mut cluster = Cluster::start(
        if leader {
            "flush-leader"
        } else {
            "flush-follower"
        },
        3,
    );
    cluster.write_some("before", WRITES);
    let lead = cluster.leader().expect("a leader");
    let target = if leader {
        lead
    } else {
        cluster.up().into_iter().find(|&id| id != lead).unwrap()
    };
    assert_eq!(cluster.order(target, Order::FailFlush), Some(Outcome::Done));
    let mut answered = 0;
    let mut written = 0;
    let status = loop {
        if let Some(status) = cluster.exited(target, Duration::ZERO) {
            break status;
        }
        written += 1;
        if cluster
            .put(format!("during/{written}").as_bytes(), b"x")
            .is_some()
        {
            answered += 1;
        }
        // The member flushes for every turn of writes it takes, its stream's own writes or its
        // replica's: the first that fails fences it.
        assert!(answered <= turn(3), "member {target} never fenced");
    };
    assert_eq!(status, 3, "the member did not exit fenced");
    cluster.restart(target, true);
    cluster.write_some("after", WRITES);
    cluster.verify();
}

/// Kills at random points: a seeded choice of member, point and count, many times.
fn random_kills(rounds: u64) {
    let mut state = 0x5eed_u64;
    let mut next = |n: u64| {
        state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        (z ^ (z >> 31)) % n
    };
    let points = [Point::Submitted, Point::Durable, Point::Released];
    for round in 0..rounds {
        let point = points[next(3) as usize];
        let leader = next(2) == 0;
        let count = 1 + next(8);
        kill_at(point, leader, count, &format!("random-{round}"));
    }
}

#[allow(
    clippy::disallowed_methods,
    reason = "real processes on the host's clock, threads and environment (CLAUDE.md §1a, end to end)"
)]
fn main() -> ExitCode {
    println!("elections by suspicion, on the members' own detectors (hyper-liveness)");
    let filter: Option<String> = std::env::args().skip(1).find(|a| !a.starts_with('-'));
    let runs = |name: &str| filter.as_deref().is_none_or(|f| name.contains(f));
    let started = Instant::now();
    for (point, name) in [
        (Point::Submitted, "submitted"),
        (Point::Durable, "durable"),
        (Point::Released, "released"),
    ] {
        for leader in [true, false] {
            let name = format!("kill-{name}-{}", if leader { "leader" } else { "follower" });
            if runs(&name) {
                let at = Instant::now();
                kill_at(point, leader, 1, &name);
                println!("{name}: ok in {:?}", at.elapsed());
            }
        }
    }
    if runs("stall") {
        for leader in [true, false] {
            let at = Instant::now();
            stalled_disk(leader);
            let name = if leader {
                "stall-leader"
            } else {
                "stall-follower"
            };
            println!("{name}: ok in {:?}", at.elapsed());
        }
    }
    if runs("founder") {
        founder();
        println!("founder: ok");
        founder_fenced();
        println!("founder-fenced: ok");
    }
    if runs("fence") {
        fence_host(false);
        fence_host(true);
        println!("fence: ok");
    }
    if runs("flush") {
        failed_flush(false);
        failed_flush(true);
        println!("flush: ok");
    }
    if runs("random") {
        let rounds = std::env::var("HYPER_DURABLE_E2E_ROUNDS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(6);
        random_kills(rounds);
        println!("random: {rounds} ok");
    }
    println!("all ok in {:?}", started.elapsed());
    ExitCode::SUCCESS
}
