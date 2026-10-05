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
//! - a group whose survivors' devices hold their flushes while they elect goes on once they do;
//! - a member stopped mid-scenario fails the wait for what it cannot do, named, and once let go
//!   applies the same history;
//! - a member started again is reported restarted to every other member's core;
//! - every suspicion is the member's stream's: none was told while a heartbeat the kernel stamped
//!   before its point sat unread in the member's socket (each report's `unread`).
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
//! measurement and never less after (§2.1, §2.4; a loopback round trip is far below it). Quiet is
//! only time in which the test heard every member, by hyper-raft-e2e's rule
//! (`hyper_raft_e2e::quiet`): a look that did not hear a member decides nothing, and its answer
//! after counts as movement; the time the members report having a write of their logs out extends
//! the watch, for a group moves through a member only as its writes become durable; and a member
//! silent, or heard with its oldest write out, past the longest one write any member has reported
//! (or the hold the test ordered) and the quiet period, each counted less the timeout a look waits
//! for its answer, fails the wait, named. A fact holds only of members a look heard.
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
use hyper_raft_e2e::device::{self, Probe};
use hyper_raft_e2e::quiet::{self, Heard, Progress, Quiet, RTO, Stuck, Watch};
use hyper_raft_e2e::wire::{self, Control, Kind, Op, Outcome};
use hyper_raft_e2e::{run, stream};

const NODE: &str = env!("CARGO_BIN_EXE_hyper-durable-node");
const TMP: &str = env!("CARGO_TARGET_TMPDIR");

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
    /// Each member's law, the asks each left unanswered, the longest write any reported, the hold
    /// the test ordered, and what the looks have seen (`hyper_raft_e2e::quiet`).
    quiet: Quiet,
    /// Why the latest wait that gave up did.
    stuck: Option<Stuck>,
    /// The test's own file on the members' device, flushed before a member is judged stuck.
    probe: Probe,
    /// When the device was last measured.
    measured: Option<Instant>,
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
    /// Members ordered to fail a flush: each fences and exits, which ends its process as the test
    /// expects.
    fencing: Vec<u64>,
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
            quiet: Quiet::new(),
            stuck: None,
            probe: Probe::new(
                PathBuf::from(TMP).join(format!("durable-{}-{name}.probe", std::process::id())),
            ),
            measured: None,
            last: BTreeMap::new(),
            test,
            datagram,
            next_id: 0,
            buffer: Vec::new(),
            acked: BTreeMap::new(),
            stopped: Vec::new(),
            fencing: Vec::new(),
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

    /// Sends what is in the buffer to `id` and waits for its answer to `ask`, one retransmission
    /// timeout at most; an ask left unanswered counts toward the member's silence.
    #[allow(
        clippy::disallowed_methods,
        reason = "real processes on the host's clock, threads and environment (CLAUDE.md §1a, end to end)"
    )]
    fn exchange(&mut self, id: u64, ask: u64) -> Option<Vec<u8>> {
        let sent = Instant::now();
        let answer = self.answer(id, ask);
        self.quiet
            .asked(id, sent, answer.is_none().then(Instant::now));
        answer
    }

    /// Sends what is in the buffer to `id` and waits for its answer to `ask`, one retransmission
    /// timeout at most.
    #[allow(
        clippy::disallowed_methods,
        reason = "real processes on the host's clock, threads and environment (CLAUDE.md §1a, end to end)"
    )]
    fn answer(&mut self, id: u64, ask: u64) -> Option<Vec<u8>> {
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
            // Waited for by a peek, taken without waiting (`hyper_measure::wait::arrives`). A reset is an
            // earlier send's, to a member gone (Windows reports it on the next receive).
            if !hyper_measure::wait::arrives(&self.test, Some(left), &mut received).unwrap() {
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
        self.instruct(id, |buffer, ask| control::put_order(buffer, ask, &order))
    }

    /// Sends what `put` puts, an order answered as `wire::Outcome`, as [`Cluster::order`] sends
    /// one: hyper-raft-e2e's orders a member of either harness takes (`hyper_raft_e2e::stream`).
    fn instruct(&mut self, id: u64, put: impl Fn(&mut Vec<u8>, u64)) -> Option<Outcome> {
        self.next_id += 1;
        let ask = self.next_id;
        put(&mut self.buffer, ask);
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
        let report = control::read_report(&body, self.members.len()).map(|(_, r)| r)?;
        // Every suspicion is the stream's: none told while a heartbeat the kernel stamped before
        // its point sat unread in the member's socket.
        assert_eq!(
            report.unread, 0,
            "{}: member {id} suspected a peer whose heartbeat, stamped before the point, it had \
             not read: {report:?}",
            self.name
        );
        let law = quiet::law(
            report.detection_ns,
            report.unjudged_interval_ns,
            report.span_ns,
            report.round_ns,
        );
        self.quiet.reported(id, law, report.flush_most_ns);
        self.last.insert(id, (Instant::now(), report.clone()));
        Some(report)
    }

    /// What a failed wait saw: why the latest wait gave up, the quiet period in force, the
    /// longest write any member reported, what the looks have seen, and each member's latest
    /// report, how long ago it came.
    #[allow(
        clippy::disallowed_methods,
        reason = "real processes on the host's clock, threads and environment (CLAUDE.md §1a, end to end)"
    )]
    fn state(&self) -> String {
        let looks = self.quiet.seen();
        let ms = |d: Duration| d.as_secs_f64() * 1e3;
        let mut out = match &self.stuck {
            Some(stuck) => format!("{stuck}; "),
            None => String::new(),
        };
        out.push_str(&format!(
            "quiet {:?}; the members' longest write {:?}; looks that did not hear every member: \
             {}, the longest silence {:.1} ms against {:.1} ms excused; waits extended {:.1} ms \
             for members' writes, at most {:.1} ms at once",
            self.quiet(),
            self.quiet.write_most(),
            looks.unheard_looks,
            ms(looks.silence_most),
            ms(looks.excused_then),
            looks.extended_ns as f64 / 1e6,
            looks.extended_most_ns as f64 / 1e6,
        ));
        for (id, (at, r)) in &self.last {
            let s = &r.status;
            out.push_str(&format!(
                "\n  member {id}, {:?} ago: term {} leads {} commit {} applied {} last {}; \
                 suspected {:?} heard {:?} unjudged {} at up to {:?} taken {} restarts {}; \
                 detection {:?} span {:?} round {:?}; a write out {:?} all told, its oldest out \
                 {:?}, the longest write {:?}, the longest between two reads {:?}; stalled {} \
                 marked {} deadline {}; its core suspects {:?}, campaign {}",
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
                Duration::from_nanos(r.blocked_ns),
                Duration::from_nanos(r.writing_ns),
                Duration::from_nanos(r.flush_most_ns),
                Duration::from_nanos(r.turn_most_ns),
                r.stalled,
                r.marked,
                if r.deadline_ns == u64::MAX {
                    "none".to_string()
                } else {
                    format!("in {:?}", Duration::from_nanos(r.deadline_ns))
                },
                r.core_suspected,
                campaign(r.campaign, r.clock_ns),
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
    /// member's law takes (`quiet::law`), from its latest report, and never less than a
    /// retransmission timeout. A live group suspects a dead leader within its stated detection,
    /// elects or starts a new term within an election, and answers within an ask's rounds, so a
    /// quiet period in which nothing moves is a group that is stuck, not one that drew a split
    /// vote. A wait has no count of elections: it goes on while the group moves.
    fn quiet(&self) -> Duration {
        self.quiet.period()
    }

    /// A fresh watch over the group's progress; why an earlier wait gave up is forgotten.
    #[allow(
        clippy::disallowed_methods,
        reason = "real processes on the host's clock, threads and environment (CLAUDE.md §1a, end to end)"
    )]
    fn watch(&mut self) -> Watch {
        self.stuck = None;
        self.quiet.watch(Instant::now())
    }

    /// Whether the group is still moving: asks every member up and not stopped for its report, and
    /// judges the look by `hyper_raft_e2e::quiet`'s rule. The watch is extended by a quiet period
    /// whenever any member's term, commit, applied index, last index or restarts seen has moved
    /// since it last looked, or, while it has a pair no margin judges, the heartbeats it has taken;
    /// a look that did not hear a member decides nothing, and its answer after counts as
    /// movement; the time each member says it had a write of its log out since the watch last
    /// heard it extends the watch by the most any one had; and a member silent past the longest
    /// one write any member has reported (or the hold the test ordered) and the quiet period ends
    /// the wait, named in the state a failure prints.
    #[allow(
        clippy::disallowed_methods,
        reason = "real processes on the host's clock, threads and environment (CLAUDE.md §1a, end to end)"
    )]
    fn moving(&mut self, watch: &mut Watch) -> bool {
        let looked = Instant::now();
        let mut heard = Vec::new();
        let mut unheard = Vec::new();
        for id in self.up() {
            if self.stopped.contains(&id) {
                continue;
            }
            match self.report(id) {
                Some(report) => heard.push(Heard {
                    id,
                    progress: Progress::of(
                        &report.status,
                        report.restarts,
                        report.unjudged,
                        report.taken,
                    ),
                    blocked_ns: report.blocked_ns,
                    writing_ns: report.writing_ns,
                }),
                None if self.running(id) => unheard.push(id),
                None => {}
            }
        }
        match self
            .quiet
            .look(watch, looked, Instant::now(), &heard, &unheard)
        {
            Ok(()) => true,
            Err(stuck) => {
                match device::reconsider(
                    stuck,
                    &self.probe,
                    &mut self.measured,
                    &mut self.quiet,
                    Instant::now(),
                ) {
                    None => true,
                    Some(stuck) => {
                        self.stuck = Some(stuck);
                        false
                    }
                }
            }
        }
    }

    /// Whether member `id`'s process runs: one that did not answer is waited on only while it
    /// does. One ordered to fail a flush ends as the test expects, once it fences, and is gone from
    /// the looks; any other whose process ended fails the test.
    fn running(&mut self, id: u64) -> bool {
        let fencing = self.fencing.contains(&id);
        let ended = match self.member(id).child.as_mut() {
            Some(child) => child.try_wait().unwrap(),
            None => return false,
        };
        match ended {
            None => true,
            Some(_) if fencing => false,
            Some(status) => panic!(
                "{}: member {id} ended: {status}; {}",
                self.name,
                self.state()
            ),
        }
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

    /// The member that leads, once one does, waited for while the group moves: the members up
    /// are asked in turn, and the group is looked at only when none leads.
    fn leader(&mut self) -> Option<u64> {
        let mut watch = self.watch();
        loop {
            for id in self.up() {
                if self.stopped.contains(&id) {
                    continue;
                }
                if self.report(id).is_some_and(|r| r.status.leads) {
                    return Some(id);
                }
            }
            if !self.moving(&mut watch) {
                return None;
            }
        }
    }

    /// Writes `key` = `value` through the leader until a member answers it, while the group
    /// moves; the index it was applied at. The group is looked at only when no answer came.
    fn put(&mut self, key: &[u8], value: &[u8]) -> Option<u64> {
        let mut watch = self.watch();
        loop {
            let leader = self.leader()?;
            if let Some(Outcome::Put(index)) = self.ask(leader, &Op::Put { key, value }) {
                self.acked.insert(key.to_vec(), value.to_vec());
                return Some(index);
            }
            if !self.moving(&mut watch) {
                return None;
            }
        }
    }

    /// Reads `key` linearizably through the leader.
    fn get(&mut self, key: &[u8]) -> Option<Option<Vec<u8>>> {
        let mut watch = self.watch();
        loop {
            let leader = self.leader()?;
            if let Some(Outcome::Value(value)) = self.ask(leader, &Op::Get { key }) {
                return Some(value);
            }
            if !self.moving(&mut watch) {
                return None;
            }
        }
    }

    /// Orders member `id`'s device to answer no flush for `hold` from now
    /// (`hyper_raft_e2e::stream::put_stall`; the fault file holds it, `file::hold_flushes`).
    fn hold_flushes(&mut self, id: u64, hold: Duration) {
        let done = self.instruct(id, |buffer, ask| stream::put_stall(buffer, ask, hold));
        assert_eq!(
            done,
            Some(Outcome::Done),
            "{}: member {id} took no hold; {}",
            self.name,
            self.state()
        );
    }

    /// Stops member `id`'s process without ending it: it stays up and answers nothing, outside any
    /// write of its log, as a member deadlocked does. `SIGSTOP` on Unix.
    #[cfg(unix)]
    fn freeze(&mut self, id: u64) {
        self.signal(id, "-STOP");
    }

    /// Windows has no signal that stops a process: the member is ordered to hold its thread,
    /// outside any write of its log, until a byte comes on its standard input
    /// (`hyper_raft_e2e::parent`).
    #[cfg(windows)]
    fn freeze(&mut self, id: u64) {
        let done = self.instruct(id, stream::put_hold);
        assert_eq!(done, Some(Outcome::Done), "member {id} took no hold");
    }

    /// Lets member `id` go on after [`Cluster::freeze`], and waits, while its process runs, for its
    /// first answer: what it said before it stopped is no word of it since.
    fn thaw(&mut self, id: u64) {
        #[cfg(unix)]
        self.signal(id, "-CONT");
        #[cfg(windows)]
        {
            use std::io::Write;
            let stdin = self
                .member(id)
                .child
                .as_mut()
                .and_then(|child| child.stdin.as_mut())
                .expect("a member up, its standard input the test's");
            stdin.write_all(&[1]).unwrap();
            stdin.flush().unwrap();
        }
        while self.report(id).is_none() {
            assert!(self.running(id), "member {id} ended while stopped");
        }
    }

    /// Sends `signal` to member `id`'s process with the system's `kill`.
    #[cfg(unix)]
    fn signal(&mut self, id: u64, signal: &str) {
        let pid = self
            .member(id)
            .child
            .as_ref()
            .expect("a member that is up")
            .id();
        let status = Command::new("kill")
            .args([signal, &pid.to_string()])
            .status()
            .unwrap();
        assert!(status.success(), "kill {signal} {pid} failed: {status}");
    }

    /// What the scenario's looks saw, for the line it ends with: the looks that did not hear every
    /// member, the longest silence and what was excused then, how far waits were extended for the
    /// members' writes, and the longest write and time between two reads of a socket any member
    /// reported last.
    fn looks(&self) -> String {
        let looks = self.quiet.seen();
        let ms = |d: Duration| d.as_secs_f64() * 1e3;
        let turn_most = self
            .last
            .values()
            .map(|(_, r)| r.turn_most_ns)
            .max()
            .unwrap_or(0);
        format!(
            "looks that did not hear every member {}, the longest silence {:.1} ms against {:.1} ms \
             excused, waits extended {:.1} ms for members' writes (at most {:.1} ms at once), the \
             longest a write was out when heard {:.1} ms; the members' longest write {:.1} ms, \
             longest between two reads {:.1} ms",
            looks.unheard_looks,
            ms(looks.silence_most),
            ms(looks.excused_then),
            looks.extended_ns as f64 / 1e6,
            looks.extended_most_ns as f64 / 1e6,
            ms(looks.writing_most),
            ms(self.quiet.write_most()),
            turn_most as f64 / 1e6,
        )
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

    /// Takes `id` out of the cluster for good, as an operator removes a node: killed, and its log
    /// and run's record removed with it.
    fn forget(&mut self, id: u64) {
        self.kill(id);
        self.members.retain(|member| {
            if member.id == id {
                remove(&member.log);
                remove(&run::path(&member.log));
            }
            member.id != id
        });
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
        self.quiet.gone(id);
        self.last.remove(&id);
    }

    /// The exit status of a member that fenced and exited, once it printed `fenced` within
    /// `wait`.
    fn exited(&mut self, id: u64, wait: Duration) -> Option<i32> {
        self.line(id, "fenced", wait)?;
        let member = self.member(id);
        let status = member.child.take()?.wait().ok()?;
        member.lines = None;
        self.fencing.retain(|&fencing| fencing != id);
        self.quiet.gone(id);
        self.last.remove(&id);
        status.code()
    }

    /// Starts `id` again on its log, told where its peers listen when `tell`; then, while the
    /// group moves, every other member up that had heard its last run and shares a group with it,
    /// by both their configurations, reports the restart its stream saw
    /// (`hyper_liveness::Change::Restarted`). A member that never took a heartbeat of the last
    /// run cannot tell the new one from a first; and one whose configuration and the restarted
    /// one's do not both name the other keeps no stream to it: a peer removed by a change the
    /// restarted member holds in its log. A member that did not answer a look says nothing of the
    /// restart either way.
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
                reports.get(other).is_some_and(|r| {
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
fn kill_at(point: Point, leader: bool, count: u64, name: &str) -> String {
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
    cluster.looks()
}

/// A member's disk stops completing flushes: its heartbeats stop with it (each needs a flush made
/// after the previous was due), every other member's detector suspects it, and they elect without
/// it if it led and take writes. Then it is killed and started again on its log.
fn stalled_disk(leader: bool) -> String {
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
    // group does. Every other member must say it suspects it: one that did not answer a look says
    // nothing of it.
    cluster.stopped.push(target);
    let others: Vec<u64> = cluster
        .up()
        .into_iter()
        .filter(|&id| id != target)
        .collect();
    let suspected = cluster.until(|reports| {
        others.iter().all(|id| {
            reports
                .get(id)
                .is_some_and(|r| r.suspected.contains(&target))
        })
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
    cluster.looks()
}

/// The leader's disk stops for good and, as its survivors elect, their devices hold every flush for
/// longer than a wait can be quiet and look: the quiet period in force and two looks'
/// retransmission timeouts for each member. The survivors' heartbeats wait on their held writes,
/// and so does every vote of theirs: the test hears both and sees nothing move, which is not quiet,
/// for a member whose write is out moves nothing through itself (CI's ubuntu-24.04 runner once held
/// a stalled leader's survivors a second in term 4 with no leader, and a wait that called it quiet
/// gave up). Once the devices go on the survivors elect and take writes; the leader is killed and
/// restarted on its log, and every member applies the same history.
///
/// The hold comes once every pair is judged, as in a group that has run a while: a link younger
/// than its evidence, its heartbeats stopped by the hold, is judged only once its node's links have
/// evidence again (`docs/timing.md` §2.8), which on this machine kept the survivors trusting the
/// stalled leader 44 s.
fn stalled_devices() -> String {
    let name = "stalled-devices";
    let mut cluster = Cluster::start(name, 3);
    cluster.write_some("before", WRITES);
    let judged =
        cluster.until(|reports| reports.len() == 3 && reports.values().all(|r| r.unjudged == 0));
    assert!(
        judged,
        "{name}: the members' pairs were never all judged; {}",
        cluster.state()
    );
    let lead = cluster.leader().expect("a leader");
    let survivors: Vec<u64> = cluster.up().into_iter().filter(|&id| id != lead).collect();
    let hold = cluster.quiet() + RTO * 2 * cluster.voters.len() as u32;
    cluster.quiet.order_stall(hold);
    for id in &survivors {
        cluster.hold_flushes(*id, hold);
    }
    assert_eq!(cluster.order(lead, Order::StallFlush), Some(Outcome::Done));
    // The stalled leader is asked nothing more: its answers would say what it holds, not what the
    // group does.
    cluster.stopped.push(lead);
    cluster.write_some("while-held", WRITES);
    assert!(
        cluster.quiet.seen().extended_ns > 0,
        "{name}: no wait was extended for the members' writes through a hold of {hold:?}; {}",
        cluster.state()
    );
    cluster.kill(lead);
    cluster.restart(lead, true);
    cluster.write_some("after", WRITES);
    cluster.verify();
    format!("held {hold:?}; {}", cluster.looks())
}

/// A follower stopped mid-scenario, answering nothing outside any write of its log (`SIGSTOP`; on
/// Windows, which has no stop signal, it holds its thread until the test releases it): the wait for
/// a write it cannot apply fails, naming it, once its silence passes what the members' longest
/// write and the quiet period excuse, rather than waiting for good. Let go, it applies the same
/// history as the others.
fn member_stopped() -> String {
    let name = "member-stopped";
    let mut cluster = Cluster::start(name, 3);
    cluster.write_some("before", WRITES);
    let lead = cluster.leader().expect("a leader");
    let stopped = cluster.up().into_iter().find(|&id| id != lead).unwrap();
    cluster.freeze(stopped);
    // The leader and the other follower commit it; the member stopped cannot apply it.
    let Some(index) = cluster.put(b"after", b"stopped") else {
        panic!(
            "{name}: the two members up took no write; {}",
            cluster.state()
        );
    };
    let voters = cluster.voters.len();
    let applied = cluster.until(|reports| {
        reports.len() == voters && reports.values().all(|r| r.status.applied >= index)
    });
    assert!(
        !applied,
        "{name}: member {stopped}, stopped, applied what it cannot have"
    );
    let Some(Stuck::Silent {
        member,
        silence,
        excuse,
    }) = cluster.stuck.take()
    else {
        panic!(
            "{name}: the wait ended without naming a member silent; {}",
            cluster.state()
        );
    };
    assert_eq!(
        member, stopped,
        "{name}: the wait named member {member}, not member {stopped}, which was stopped"
    );
    cluster.thaw(stopped);
    cluster.verify();
    format!(
        "member {stopped} stopped; the wait for it failed after {silence:?} of its silence against \
         {excuse:?} excused, naming it; let go, it applied the same history; {}",
        cluster.looks()
    )
}

/// focal F17 (`cli_network`): the founder of a group of two removes its only peer; the operator
/// stops the peer for good once the founder says it applied the change; the founder is killed
/// there and restarted alone, and must elect itself and take a write.
fn founder() -> String {
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
    cluster.forget(peer);
    cluster.kill(founder);
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
    cluster.looks()
}

/// The same founder killed while the removal waits behind its commit fence: it applied nothing,
/// so the operator stops no one; restarted, the group finishes the removal.
#[allow(
    clippy::disallowed_methods,
    reason = "real processes on the host's clock, threads and environment (CLAUDE.md §1a, end to end)"
)]
fn founder_fenced() -> String {
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
    cluster.forget(peer);
    cluster.write_some("alone", WRITES);
    cluster.verify();
    cluster.looks()
}

/// focal F17 (`cli_upgrade`): a member acts on a fence (an entry it acts on at its next start)
/// and is killed there; restarted and told of no peer, it acts on the fence again from its own
/// log before it hears from anyone.
fn fence_host(leader: bool) -> String {
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
    cluster.looks()
}

/// A member's flush fails: it fences, exits, and is started again on its log; nothing answered
/// is lost.
fn failed_flush(leader: bool) -> String {
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
    cluster.fencing.push(target);
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
    cluster.looks()
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

/// A scenario: it says what its looks saw.
type Scenario = fn() -> String;

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
                let looks = kill_at(point, leader, 1, &name);
                println!("{name}: ok in {:?}; {looks}", at.elapsed());
            }
        }
    }
    let scenarios: [(&str, Scenario); 10] = [
        ("stall-leader", || stalled_disk(true)),
        ("stall-follower", || stalled_disk(false)),
        ("stalled-devices", stalled_devices),
        ("member-stopped", member_stopped),
        ("founder", founder),
        ("founder-fenced", founder_fenced),
        ("fence-follower", || fence_host(false)),
        ("fence-leader", || fence_host(true)),
        ("flush-follower", || failed_flush(false)),
        ("flush-leader", || failed_flush(true)),
    ];
    for (name, scenario) in scenarios {
        if runs(name) {
            let at = Instant::now();
            let looks = scenario();
            println!("{name}: ok in {:?}; {looks}", at.elapsed());
        }
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

/// A member's campaign as its report states it (`hyper_raft::CampaignState`): when it is due from
/// the report's clock, and what holds it.
fn campaign(state: Option<hyper_raft::CampaignState>, clock_ns: u64) -> String {
    let Some(state) = state else {
        return "none".to_string();
    };
    let due = match state.due {
        Some(at) if at >= clock_ns => format!("due in {:?}", Duration::from_nanos(at - clock_ns)),
        Some(at) => format!("due {:?} ago", Duration::from_nanos(clock_ns - at)),
        None => "untimed".to_string(),
    };
    format!(
        "armed {} {due}, led {}, held {}, trusted quorum {}, may campaign {}, may lead {}, \
         promotable {}",
        state.armed,
        state.led,
        state.held,
        state.trusted_quorum,
        state.may_campaign,
        state.may_lead,
        state.promotable,
    )
}
