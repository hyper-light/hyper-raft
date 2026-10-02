//! hyper-durable in real use. Each scenario starts a group of real processes
//! (`hyper-durable-node`), each a replica on hyper-log over its own fully flushed file and its own
//! UDP socket, and drives it as a client would. Members are killed with `SIGKILL` at named
//! durability points (`control::Point`: a write submitted, a write durable whose answer was not
//! taken, messages released, a change behind the commit fence, a change applied, an entry acted on
//! at start) and at random points, a flush is made to fail, and the configuration changes. What is
//! asserted is what a client and an operator can observe:
//! - every write a member answered is read back, linearizably, through whichever member leads;
//! - every member that is up applies the same history (the same digest at the same index);
//! - focal F17's two cases: a founder that removed its only peer, killed once it applied the
//!   removal and its peer stopped for good, elects itself alone; and a host that acted on a fence
//!   acts on it again from its own log before it hears from anyone, never below it;
//! - a member whose flush failed fences, exits, and rejoins on its log with nothing it answered
//!   lost.
//!
//! The scenarios run one after another in this one thread (`harness = false`), one group at a
//! time; the test reads each member's output on a thread of its own, one a process, at most three.
//! Every wait is on the fact it needs, bounded by a budget in ticks derived as hyper-raft-e2e
//! derives its own.
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
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, sync_channel};
use std::time::{Duration, Instant};

/// What a wait last saw of each member's progress, and until when it waits without seeing more.
struct Watch {
    seen: BTreeMap<u64, (u64, u64, u64, u64)>,
    until: Instant,
}

use hyper_durable_e2e::control::{self, Order, Point, Report};
use hyper_durable_e2e::node::{ELECTION_TICKS, HEARTBEAT_TICKS};
use hyper_raft::proto::ConfChangeType;
use hyper_raft_e2e::wire::{self, Control, Kind, Op, Outcome};

const NODE: &str = env!("CARGO_BIN_EXE_hyper-durable-node");
const TMP: &str = env!("CARGO_TARGET_TMPDIR");

/// The ticks one broadcast takes at most: one, as the tick is measured (`measure_tick`).
const BROADCAST_TICKS: u32 = 1;
/// The longest a live member takes to answer an ask: a leader cut off finds out by its quorum
/// check within two election timeouts, and an answer takes a tick to arrive (hyper-raft-e2e's).
const ANSWER_TICKS: u32 = 2 * ELECTION_TICKS + BROADCAST_TICKS;
/// The ticks one election takes at most: the longest randomized timeout and its two vote rounds
/// (pre-vote and vote).
const ELECTION_ROUND_TICKS: u32 = 2 * ELECTION_TICKS + 2 * BROADCAST_TICKS;
/// The share of a time's distribution its measured bound covers, and the confidence: the 95/95
/// one-sided tolerance limit (Wilks 1941), hyper-raft-e2e's.
const COVERAGE: f64 = 0.95;
const CONFIDENCE: f64 = 0.95;
/// The least tick: `--tick-ms` counts whole milliseconds.
const LEAST_TICK: Duration = Duration::from_millis(1);
/// Keys a member holds at most, and asks it keeps waiting: far past what a scenario writes.
const MAX_KEYS: usize = 1 << 16;
const MAX_PENDING: usize = 64;

fn tolerance_samples() -> usize {
    ((1.0 - CONFIDENCE).ln() / COVERAGE.ln()).ceil() as usize
}

fn bound(mut sample: impl FnMut()) -> Duration {
    let mut slowest = Duration::ZERO;
    for _ in 0..tolerance_samples() {
        let started = Instant::now();
        sample();
        slowest = slowest.max(started.elapsed());
    }
    slowest
}

#[expect(
    clippy::disallowed_methods,
    reason = "a test removes the files it made in its own target directory"
)]
fn remove(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// The tick, from what this machine's flushes, datagrams and timer cost: a broadcast (two
/// flushes and two datagrams of the most a message carries) and the timer's wake, at least a
/// millisecond (hyper-raft-e2e's `measure_tick`, whose reasons hold here).
fn measure_tick() -> Duration {
    let path = PathBuf::from(TMP).join(format!("durable-{}-probe", std::process::id()));
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .unwrap();
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    let block = vec![0x5au8; wire::largest(&socket).unwrap()];
    let flush = bound(|| {
        file.write_all(&block).unwrap();
        file.sync_data().unwrap();
    });
    drop(file);
    remove(&path);
    let to = socket.local_addr().unwrap();
    let mut received = vec![0u8; wire::MAX_DATAGRAM];
    let datagram = bound(|| {
        socket.send_to(&block, to).unwrap();
        socket.recv_from(&mut received).unwrap();
    });
    socket.set_read_timeout(Some(LEAST_TICK)).unwrap();
    let wake = bound(|| {
        let _ = socket.recv_from(&mut received);
    });
    let tick = ((flush + datagram) * 2).max(wake).max(LEAST_TICK);
    Duration::from_millis(tick.as_micros().div_ceil(1000).try_into().unwrap())
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
    tick: Duration,
    test: UdpSocket,
    datagram: usize,
    next_id: u64,
    buffer: Vec<u8>,
    /// Every write a member answered: key → value.
    acked: BTreeMap<Vec<u8>, Vec<u8>>,
}

impl Drop for Cluster {
    fn drop(&mut self) {
        for member in &mut self.members {
            if let Some(mut child) = member.child.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
            remove(&member.log);
        }
    }
}

/// Starts the member `id` of `voters` on its log; its port and the lines it prints after.
fn spawn(id: u64, voters: &[u64], log: &Path, tick: Duration) -> (Child, u16, Receiver<String>) {
    let list: Vec<String> = voters.iter().map(u64::to_string).collect();
    let mut child = Command::new(NODE)
        .args(["--id", &id.to_string()])
        .args(["--voters", &list.join(",")])
        .args(["--listen", "127.0.0.1:0"])
        .args(["--log", log.to_str().unwrap()])
        .args(["--tick-ms", &tick.as_millis().to_string()])
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
    fn start(name: &str, voters: u64, tick: Duration) -> Self {
        let ids: Vec<u64> = (1..=voters).collect();
        let mut members = Vec::new();
        for &id in &ids {
            let log =
                PathBuf::from(TMP).join(format!("durable-{}-{name}-{id}.log", std::process::id()));
            remove(&log);
            let (child, port, lines) = spawn(id, &ids, &log, tick);
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
            tick,
            test,
            datagram,
            next_id: 0,
            buffer: Vec::new(),
            acked: BTreeMap::new(),
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

    fn ticks(&self, ticks: u32) -> Duration {
        self.tick * ticks
    }

    /// Sends what is in the buffer to `id` and waits for its answer to `ask`, at most `wait`.
    fn exchange(&mut self, id: u64, ask: u64, wait: Duration) -> Option<Vec<u8>> {
        let to = self.member(id).address;
        let sealed = wire::seal(&mut self.buffer, self.datagram);
        assert!(sealed);
        self.test.send_to(&self.buffer, to).ok()?;
        let deadline = Instant::now() + wait;
        let mut received = vec![0u8; wire::MAX_DATAGRAM];
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            self.test.set_read_timeout(Some(left)).unwrap();
            let Ok((length, _)) = self.test.recv_from(&mut received) else {
                return None;
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
        let body = self.exchange(id, ask, self.ticks(ANSWER_TICKS))?;
        wire::read_response(&body).map(|(_, outcome)| outcome)
    }

    fn order(&mut self, id: u64, order: Order) -> Option<Outcome> {
        self.next_id += 1;
        let ask = self.next_id;
        control::put_order(&mut self.buffer, ask, &order);
        let body = self.exchange(id, ask, self.ticks(ANSWER_TICKS))?;
        wire::read_response(&body).map(|(_, outcome)| outcome)
    }

    fn report(&mut self, id: u64) -> Option<Report> {
        self.next_id += 1;
        let ask = self.next_id;
        control::put_order(&mut self.buffer, ask, &Order::Report);
        let body = self.exchange_resent(id, ask)?;
        control::read_report(&body, hyper_raft::MAX_MEMBERS).map(|(_, report)| report)
    }

    fn tell_peers(&mut self) {
        let peers: Vec<(u64, SocketAddr)> =
            self.members.iter().map(|m| (m.id, m.address)).collect();
        for id in self.up() {
            self.next_id += 1;
            let ask = self.next_id;
            wire::put_control(&mut self.buffer, ask, &Control::Peers(peers.clone()));
            assert!(
                self.exchange_resent(id, ask).is_some(),
                "{}: member {id} was not told its peers",
                self.name
            );
        }
    }

    /// How long the group may go with no member's term, commit, applied index or last index
    /// moving before a wait gives up: one election round and an answer, from the members' own
    /// settings. A live group elects or starts a new term within a round (Raft §5.2, under its
    /// randomized timeout), so a round in which nothing moves is a group that is stuck, not one
    /// that drew a split vote. A wait has no count of elections: it goes on while the group moves.
    fn quiet(&self) -> Duration {
        self.ticks(ELECTION_ROUND_TICKS + ANSWER_TICKS)
    }

    /// A fresh watch over the group's progress.
    fn watch(&self) -> Watch {
        Watch {
            seen: BTreeMap::new(),
            until: Instant::now() + self.quiet(),
        }
    }

    /// Whether the group is still moving: asks every member up for its report, and extends the
    /// watch by a quiet period whenever any member's term, commit, applied index or last index
    /// has moved since it last looked.
    fn moving(&mut self, watch: &mut Watch) -> bool {
        let mut moved = false;
        for id in self.up() {
            if let Some(report) = self.report(id) {
                let at = (
                    report.status.term,
                    report.status.commit,
                    report.status.applied,
                    report.status.last_index,
                );
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
    /// each answer's time, for as long as a quiet period.
    fn exchange_resent(&mut self, id: u64, ask: u64) -> Option<Vec<u8>> {
        let until = Instant::now() + self.quiet();
        loop {
            let answer = self.ticks(ANSWER_TICKS);
            if let Some(body) = self.exchange(id, ask, answer) {
                return Some(body);
            }
            if Instant::now() >= until {
                return None;
            }
        }
    }

    /// The member that leads, once one does, within the elections' budget.
    fn leader(&mut self) -> Option<u64> {
        let mut watch = self.watch();
        while self.moving(&mut watch) {
            for id in self.up() {
                if self.report(id).is_some_and(|r| r.status.leads) {
                    return Some(id);
                }
            }
        }
        None
    }

    /// Writes `key` = `value` through the leader until a member answers it, within the
    /// elections' budget; the index it was applied at.
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
    fn line(&mut self, id: u64, prefix: &str, wait: Duration) -> Option<String> {
        let deadline = Instant::now() + wait;
        let lines = self.member(id).lines.as_ref()?;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match lines.recv_timeout(left) {
                Ok(line) if line.starts_with(prefix) => return Some(line),
                Ok(_) => {}
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => return None,
            }
        }
    }

    fn kill(&mut self, id: u64) {
        let member = self.member(id);
        if let Some(mut child) = member.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        member.lines = None;
    }

    /// The exit status of a member that fenced and exited, once it printed `fenced` within
    /// `wait`.
    fn exited(&mut self, id: u64, wait: Duration) -> Option<i32> {
        self.line(id, "fenced", wait)?;
        let member = self.member(id);
        let status = member.child.take()?.wait().ok()?;
        member.lines = None;
        status.code()
    }

    fn restart(&mut self, id: u64, tell: bool) {
        let tick = self.tick;
        let voters = self.voters.clone();
        let member = self.member(id);
        assert!(member.child.is_none());
        let (child, port, lines) = spawn(id, &voters, &member.log.clone(), tick);
        member.child = Some(child);
        member.lines = Some(lines);
        member.address = SocketAddr::from(([127, 0, 0, 1], port));
        if tell {
            self.tell_peers();
        }
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
        let mut watch = self.watch();
        loop {
            let reports: Vec<Report> = self
                .up()
                .into_iter()
                .filter_map(|id| self.report(id))
                .collect();
            let caught: Vec<&Report> = reports
                .iter()
                .filter(|r| r.status.applied >= index)
                .collect();
            if caught.len() == self.up().len() {
                let first = &caught[0].status;
                let same = caught
                    .iter()
                    .all(|r| r.status.applied != first.applied || r.status.digest == first.digest);
                assert!(
                    same,
                    "{}: members applied different histories: {reports:?}",
                    self.name
                );
                return;
            }
            assert!(
                self.moving(&mut watch),
                "{}: members stopped moving before they caught up to {index}: {reports:?}",
                self.name
            );
        }
    }

    fn write_some(&mut self, prefix: &str, count: usize) {
        for i in 0..count {
            let key = format!("{prefix}/{i}");
            assert!(
                self.put(key.as_bytes(), format!("v{i}").as_bytes())
                    .is_some(),
                "{}: a write was never answered",
                self.name
            );
        }
    }
}

/// Kills `target` (the leader or a follower) at `point`, the `count`-th time it passes it, while
/// the group takes writes; restarts it on its log; every answered write reads back and every
/// member applies the same history.
fn kill_at(tick: Duration, point: Point, leader: bool, count: u64, name: &str) {
    let mut cluster = Cluster::start(name, 3, tick);
    cluster.write_some("before", tolerance_samples() / 4);
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
    let mut written = 0;
    let stopped = loop {
        if let Some(line) = cluster.line(target, "stopped", Duration::ZERO) {
            break line;
        }
        let key = format!("during/{written}");
        // The write may go to a target that stopped: unanswered, and asked again elsewhere.
        let _ = cluster.put(key.as_bytes(), b"x");
        written += 1;
        assert!(
            written < 10 * tolerance_samples(),
            "{name}: member {target} never reached {}",
            point.name()
        );
    };
    assert_eq!(stopped, format!("stopped {}", point.name()));
    cluster.kill(target);
    cluster.write_some("while-down", 4);
    cluster.restart(target, true);
    cluster.write_some("after", 4);
    cluster.verify();
}

/// focal F17 (`cli_network`): the founder of a group of two removes its only peer; the operator
/// stops the peer for good once the founder says it applied the change; the founder is killed
/// there and restarted alone, and must elect itself and take a write.
fn founder(tick: Duration) {
    let mut cluster = Cluster::start("founder", 2, tick);
    cluster.write_some("before", 4);
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
    cluster.restart(founder, true);
    let mut watch = cluster.watch();
    loop {
        if let Some(report) = cluster.report(founder)
            && report.status.leads
        {
            assert_eq!(report.voters, vec![founder]);
            break;
        }
        assert!(
            cluster.moving(&mut watch),
            "the founder stopped moving without electing itself after the kill"
        );
    }
    cluster.members.retain(|m| m.id != peer);
    cluster.write_some("alone", 4);
    cluster.verify();
}

/// The same founder killed while the removal waits behind its commit fence: it applied nothing,
/// so the operator stops no one; restarted, the group finishes the removal.
fn founder_fenced(tick: Duration) {
    let mut cluster = Cluster::start("founder-fenced", 2, tick);
    cluster.write_some("before", 4);
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
        // A removal the kill lost is proposed again.
        if asked.elapsed() > cluster.ticks(ANSWER_TICKS)
            && let Some(leader) = cluster.leader()
        {
            let _ = cluster.order(leader, Order::Change(ConfChangeType::RemoveNode, peer));
            asked = Instant::now();
        }
        assert!(
            cluster.moving(&mut watch),
            "the group stopped moving without finishing the removal after the kill"
        );
    }
    cluster.kill(peer);
    cluster.members.retain(|m| m.id != peer);
    cluster.write_some("alone", 4);
    cluster.verify();
}

/// focal F17 (`cli_upgrade`): a member acts on a fence (an entry it acts on at its next start)
/// and is killed there; restarted and told of no peer, it acts on the fence again from its own
/// log before it hears from anyone.
fn fence_host(tick: Duration, leader: bool) {
    let mut cluster = Cluster::start(
        if leader {
            "fence-leader"
        } else {
            "fence-follower"
        },
        3,
        tick,
    );
    cluster.write_some("before", 4);
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
        .line(host, "acted", cluster.ticks(ANSWER_TICKS))
        .expect("the host reopened below the fence it acted on");
    let again: u64 = again.trim_start_matches("acted ").parse().unwrap();
    assert_eq!(again, acted, "the host acted on another entry at its start");
    cluster.tell_peers();
    cluster.verify();
}

/// A member's flush fails: it fences, exits, and is started again on its log; nothing answered
/// is lost.
fn failed_flush(tick: Duration, leader: bool) {
    let mut cluster = Cluster::start(
        if leader {
            "flush-leader"
        } else {
            "flush-follower"
        },
        3,
        tick,
    );
    cluster.write_some("before", 4);
    let lead = cluster.leader().expect("a leader");
    let target = if leader {
        lead
    } else {
        cluster.up().into_iter().find(|&id| id != lead).unwrap()
    };
    assert_eq!(cluster.order(target, Order::FailFlush), Some(Outcome::Done));
    let mut written = 0;
    let status = loop {
        if let Some(status) = cluster.exited(target, Duration::ZERO) {
            break status;
        }
        let _ = cluster.put(format!("during/{written}").as_bytes(), b"x");
        written += 1;
        assert!(
            written < 10 * tolerance_samples(),
            "member {target} never fenced"
        );
    };
    assert_eq!(status, 3, "the member did not exit fenced");
    cluster.restart(target, true);
    cluster.write_some("after", 4);
    cluster.verify();
}

/// Kills at random points: a seeded choice of member, point and count, many times.
fn random_kills(tick: Duration, rounds: u64) {
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
        kill_at(tick, point, leader, count, &format!("random-{round}"));
    }
}

fn main() -> ExitCode {
    let tick = measure_tick();
    println!("tick {tick:?} (election {ELECTION_TICKS} ticks, heartbeat {HEARTBEAT_TICKS})");
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
                kill_at(tick, point, leader, 1, &name);
                println!("{name}: ok");
            }
        }
    }
    if runs("founder") {
        founder(tick);
        println!("founder: ok");
        founder_fenced(tick);
        println!("founder-fenced: ok");
    }
    if runs("fence") {
        fence_host(tick, false);
        fence_host(tick, true);
        println!("fence: ok");
    }
    if runs("flush") {
        failed_flush(tick, false);
        failed_flush(tick, true);
        println!("flush: ok");
    }
    if runs("random") {
        let rounds = std::env::var("HYPER_DURABLE_E2E_ROUNDS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(6);
        random_kills(tick, rounds);
        println!("random: {rounds} ok");
    }
    println!("all ok in {:?}", started.elapsed());
    ExitCode::SUCCESS
}
