//! hyper-raft in real use. Each scenario starts a group of real processes
//! (`hyper-raft-node`), each a member on its own UDP socket with its own fsynced log, and
//! drives it as a client would, through the same datagrams a client sends. What is asserted is
//! what a client can observe:
//! - every write a member answered is read back, by a linearizable read through whichever
//!   member leads, after leaders are killed, members restart and members are cut off;
//! - a member cut off from the group never answers a read with a value the group has replaced;
//! - every member that is up applies the same history (the same digest at the same index).
//!
//! The scenarios run one after another in this one thread (`harness = false`), one group at a
//! time: at most five member processes at once, each of one thread.
//!
//! Every wait is on the fact it needs (a member's answer), bounded by a budget derived in ticks:
//! a request waits for its answer as long as a live member can take to give one
//! (`ANSWER_TICKS`), and one write or read is retried through as many elections as the run needs
//! for every election it causes to succeed at its confidence (`Cluster::elections`).
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

use std::{
    collections::BTreeMap,
    fs::OpenOptions,
    io::{BufRead, BufReader, ErrorKind, Write},
    net::{SocketAddr, UdpSocket},
    path::{Path, PathBuf},
    process::{Child, Command, ExitCode, Stdio},
    time::{Duration, Instant},
};

use hyper_raft_e2e::{
    node,
    wire::{self, Control, Kind, Op, Outcome, Status},
};

const NODE: &str = env!("CARGO_BIN_EXE_hyper-raft-node");
const TMP: &str = env!("CARGO_TARGET_TMPDIR");

/// Ticks in a member's election timeout, at least, and between a leader's heartbeats: the node's
/// own. Its randomized timeout is drawn from `[ELECTION_TICKS, 2 · ELECTION_TICKS)`
/// (`set_randomized_election_timeout`).
const ELECTION_TICKS: u32 = node::ELECTION_TICKS;
const HEARTBEAT_TICKS: u32 = node::HEARTBEAT_TICKS;
/// The broadcasts an election takes once a candidate's timer fires: its pre-vote round and its
/// vote round (the node runs with `pre_vote`), each within a tick.
const VOTE_ROUNDS: u32 = 2 * BROADCAST_TICKS;
/// The ticks one broadcast takes at most: one, as the tick is measured (`measure_tick`).
const BROADCAST_TICKS: u32 = 1;
/// The longest a live member takes to answer an ask. One that is not leading answers at once; a
/// leader answers a write once it commits and a read once a quorum confirms it, within a
/// broadcast; a leader cut off from its quorum finds out by its quorum check, which looks back
/// over the last election timeout every election timeout, so within two of them, and then
/// answers everyone it kept waiting that it does not lead (`lead_or_let_go`). Each answer takes
/// a tick to arrive. An ask unanswered for this long went to a member that is down or cut off.
const ANSWER_TICKS: u32 = 2 * ELECTION_TICKS + BROADCAST_TICKS;
/// The ticks one election takes at most: the longest randomized timeout and its vote rounds.
const ELECTION_ROUND_TICKS: u32 = 2 * ELECTION_TICKS + VOTE_ROUNDS;
/// The elections a full run causes on purpose beyond each scenario's first: the leader killed,
/// the leader cut off, and every member killed at once. Restarts cause none: a member back on
/// its log pre-votes, and a group with a leader refuses it.
const DISRUPTIONS: u32 = 3;
/// The share of a time's distribution its measured bound covers, and the confidence it does: the
/// 95/95 one-sided tolerance limit (Wilks 1941; the criterion USNRC Regulatory Guide 1.157 holds
/// best-estimate analyses to). The slowest of n samples bounds the share with that confidence
/// once 1 − COVERAGE^n ≥ CONFIDENCE, so n is derived, not chosen (`tolerance_samples`: 59).
const COVERAGE: f64 = 0.95;
const CONFIDENCE: f64 = 0.95;
/// The least tick a member is given: `--tick-ms` counts whole milliseconds.
const LEAST_TICK: Duration = Duration::from_millis(1);

/// What every scenario of a run shares: the tick, and how many elections the run causes, which
/// sets how many each wait must allow for.
#[derive(Clone, Copy)]
struct Run {
    tick: Duration,
    caused: u32,
}

/// The writes in a phase of a scenario: enough that the slowest of them bounds the 95th
/// percentile of a write's time with 95% confidence (`tolerance_samples`), which `commits`
/// reports. Every phase writes as many, and the writes a leader holds unanswered when it dies are
/// as many again.
fn workload() -> usize {
    tolerance_samples()
}

#[expect(
    clippy::disallowed_methods,
    reason = "a test removes the files it made in its own target directory"
)]
fn remove(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// The samples whose slowest is the 95/95 upper bound of a time (Wilks 1941).
fn tolerance_samples() -> usize {
    ((1.0 - CONFIDENCE).ln() / COVERAGE.ln()).ceil() as usize
}

/// The 95/95 upper bound of `sample`'s time.
#[allow(
    clippy::disallowed_methods,
    reason = "real processes on the host's clock, threads and environment (CLAUDE.md §1a, end to end)"
)]
fn bound(mut sample: impl FnMut()) -> Duration {
    let mut slowest = Duration::ZERO;
    for _ in 0..tolerance_samples() {
        let started = Instant::now();
        sample();
        slowest = slowest.max(started.elapsed());
    }
    slowest
}

/// The tick, from what this machine's flushes, datagrams and timer cost.
///
/// - The broadcast time — what one replication takes — is the leader's flush, a datagram to the
///   follower, the follower's flush and a datagram back, in series, each of the most a message
///   carries: the member flushes before it
///   acts on a `Ready`. Raft needs the election timeout an order of magnitude above it (Ongaro
///   and Ousterhout 2014, §5.6), and the election timeout is `ELECTION_TICKS` ticks, so a tick
///   is at least one broadcast time.
/// - A member ticks on a socket timeout, which the OS ends on its own timer, not when asked
///   (Windows on its clock interrupt, 15.6 ms unless a process asks for finer: Microsoft,
///   `timeBeginPeriod`). The leader's heartbeat goes out on a tick, so a tick finer than the
///   timer keeps is a heartbeat interval the leader cannot keep: a tick is at least the bound of
///   a wait asked for `LEAST_TICK`.
fn measure_tick() -> Duration {
    let path = PathBuf::from(TMP).join(format!("e2e-{}-probe", std::process::id()));
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .unwrap();
    // The most one message carries, and so the most one flush appends for it: the member caps a
    // message's entries at what a datagram holds.
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
    let broadcast = (flush + datagram) * 2;
    let tick = broadcast.max(wake).max(LEAST_TICK);
    // Whole milliseconds, rounded up: `--tick-ms`'s unit.
    Duration::from_millis(tick.as_micros().div_ceil(1000).try_into().unwrap())
}

struct Member {
    child: Option<Child>,
    address: SocketAddr,
    wal: PathBuf,
}

struct Cluster {
    name: &'static str,
    members: Vec<Member>,
    tick: Duration,
    /// The elections one wait allows for (`Cluster::elections`).
    elections: u32,
    /// What each member is told it holds: the keys the scenario writes, the asks it keeps waiting
    /// and the entries its log holds.
    room: Room,
    test: UdpSocket,
    /// The most bytes the test's socket sends in one datagram ([`wire::largest`]).
    datagram: usize,
    next_id: u64,
    buffer: Vec<u8>,
}

impl Drop for Cluster {
    fn drop(&mut self) {
        for member in &mut self.members {
            if let Some(mut child) = member.child.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
            remove(&member.wal);
        }
    }
}

/// What a member holds, from what its scenario writes.
#[derive(Clone, Copy)]
struct Room {
    keys: usize,
    pending: usize,
    entries: usize,
}

impl Room {
    /// Room for `writes` writes made at `elections` elections a wait.
    /// - Keys: no more than the writes.
    /// - Asks kept waiting: the writes a leader holds unanswered when it dies (`workload`), and
    ///   the client's own one at a time.
    /// - Entries: each write proposes once for each try that waited out an answer, at most
    ///   `budget / ANSWER_TICKS` and the one answered; and each election adds its leader's empty
    ///   entry.
    fn for_writes(writes: usize, elections: u32, caused: u32) -> Self {
        let tries = Cluster::budget_ticks(elections).div_ceil(ANSWER_TICKS) as usize + 1;
        Self {
            keys: writes,
            pending: workload() + 1,
            entries: writes * tries + (elections * caused) as usize,
        }
    }
}

fn spawn(
    id: u64,
    voters: usize,
    listen: &str,
    wal: &Path,
    tick: Duration,
    room: Room,
) -> (Child, u16) {
    let voters: Vec<String> = (1..=voters).map(|voter| voter.to_string()).collect();
    let mut child = Command::new(NODE)
        .args(["--id", &id.to_string()])
        .args(["--voters", &voters.join(",")])
        .args(["--listen", listen])
        .args(["--wal", wal.to_str().unwrap()])
        .args(["--tick-ms", &tick.as_millis().to_string()])
        .args(["--max-keys", &room.keys.to_string()])
        .args(["--max-pending", &room.pending.to_string()])
        .args(["--max-entries", &room.entries.to_string()])
        // The member serves until this pipe closes: when the test ends, however it ends.
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("the member starts");
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let port = line
        .trim()
        .strip_prefix("listening ")
        .and_then(|port| port.parse().ok())
        .unwrap_or_else(|| panic!("member {id} did not start: {line:?}"));
    (child, port)
}

impl Cluster {
    /// Starts `voters` members for scenario `name`, which makes `writes` writes.
    fn start(name: &'static str, voters: usize, run: Run, writes: usize) -> Self {
        let elections = Self::elections(voters, run.caused);
        let room = Room::for_writes(writes, elections, run.caused);
        let tick = run.tick;
        let mut members = Vec::new();
        for id in 1..=voters as u64 {
            let wal =
                PathBuf::from(TMP).join(format!("e2e-{}-{name}-{id}.wal", std::process::id()));
            remove(&wal);
            let (child, port) = spawn(id, voters, "127.0.0.1:0", &wal, tick, room);
            members.push(Member {
                child: Some(child),
                address: SocketAddr::from(([127, 0, 0, 1], port)),
                wal,
            });
        }
        let test = UdpSocket::bind("127.0.0.1:0").unwrap();
        let datagram = wire::largest(&test).unwrap();
        let mut cluster = Self {
            name,
            members,
            tick,
            elections,
            room,
            test,
            datagram,
            next_id: 0,
            buffer: Vec::new(),
        };
        cluster.tell_peers();
        cluster
    }
    fn voters(&self) -> usize {
        self.members.len()
    }
    fn up(&self, id: u64) -> bool {
        self.members[(id - 1) as usize].child.is_some()
    }
    fn address(&self, id: u64) -> SocketAddr {
        self.members[(id - 1) as usize].address
    }
    /// Sends `buffer` to member `id` and waits for the answer to `request`.
    #[allow(
        clippy::disallowed_methods,
        reason = "real processes on the host's clock, threads and environment (CLAUDE.md §1a, end to end)"
    )]
    fn ask(&mut self, id: u64, request: u64) -> Option<Outcome> {
        if !wire::seal(&mut self.buffer, self.datagram) {
            panic!("a request too long for a datagram");
        }
        let address = self.address(id);
        self.test.send_to(&self.buffer, address).unwrap();
        let until = Instant::now() + self.tick * ANSWER_TICKS;
        let mut received = vec![0u8; wire::MAX_DATAGRAM];
        loop {
            let wait = until.saturating_duration_since(Instant::now());
            if wait.is_zero() {
                return None;
            }
            // Waited for by a peek, taken without waiting (`wire::arrives`).
            if !wire::arrives(&self.test, Some(wait), &mut received).unwrap() {
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
                Err(error) => panic!("the test's socket: {error}"),
            };
            let Some((Kind::Response, body)) = wire::open(&received[..length]) else {
                continue;
            };
            if let Some((id, outcome)) = wire::read_response(body)
                && id == request
            {
                return Some(outcome);
            }
        }
    }
    fn request(&mut self, id: u64, op: &Op<'_>) -> Option<Outcome> {
        self.next_id += 1;
        let request = self.next_id;
        wire::put_request(&mut self.buffer, request, op);
        self.ask(id, request)
    }
    /// Sends an instruction until the member answers it. An instruction is a datagram, which a
    /// member descheduled on a slow runner may answer late or a full socket may drop, so it is
    /// sent again, under the same request, for as long as a write is given; setting peers or
    /// isolation twice changes nothing, so the instruction is idempotent.
    #[allow(
        clippy::disallowed_methods,
        reason = "real processes on the host's clock, threads and environment (CLAUDE.md §1a, end to end)"
    )]
    fn control(&mut self, id: u64, control: &Control) {
        self.next_id += 1;
        let request = self.next_id;
        let until = Instant::now() + self.budget();
        loop {
            wire::put_control(&mut self.buffer, request, control);
            if let Some(outcome) = self.ask(id, request) {
                assert_eq!(outcome, Outcome::Done, "member {id} refused an instruction");
                return;
            }
            assert!(
                Instant::now() < until,
                "member {id} took no instruction within {:?}",
                self.budget()
            );
        }
    }
    fn tell_peers(&mut self) {
        let peers: Vec<(u64, SocketAddr)> = (1..=self.voters() as u64)
            .map(|id| (id, self.address(id)))
            .collect();
        for id in 1..=self.voters() as u64 {
            if self.up(id) {
                self.control(id, &Control::Peers(peers.clone()));
            }
        }
    }
    fn status(&mut self, id: u64) -> Option<Status> {
        match self.request(id, &Op::Status)? {
            Outcome::Status(status) => Some(status),
            other => panic!("member {id} answered a status with {other:?}"),
        }
    }
    /// Kills member `id` with SIGKILL (TerminateProcess on Windows): no flush, no goodbye.
    fn kill(&mut self, id: u64) {
        let mut child = self.members[(id - 1) as usize]
            .child
            .take()
            .expect("a member that is up");
        child.kill().unwrap();
        child.wait().unwrap();
    }
    /// Starts member `id` again on its log, at the address it had.
    fn restart(&mut self, id: u64) {
        let member = &self.members[(id - 1) as usize];
        assert!(member.child.is_none(), "member {id} is up");
        let listen = member.address.to_string();
        let (child, port) = spawn(
            id,
            self.voters(),
            &listen,
            &member.wal.clone(),
            self.tick,
            self.room,
        );
        assert_eq!(port, self.members[(id - 1) as usize].address.port());
        self.members[(id - 1) as usize].child = Some(child);
        self.tell_peers();
    }
    fn isolate(&mut self, id: u64, cut: bool) {
        self.control(id, &Control::Isolate(cut));
    }
    /// Waits until the members in `among` agree on one leader of one term, which is among
    /// them, and says which.
    #[allow(
        clippy::disallowed_methods,
        reason = "real processes on the host's clock, threads and environment (CLAUDE.md §1a, end to end)"
    )]
    fn leader_among(&mut self, among: &[u64]) -> (u64, u64) {
        let until = Instant::now() + self.budget();
        while Instant::now() < until {
            let mut seen: Vec<Status> = Vec::new();
            for id in among {
                if let Some(status) = self.status(*id) {
                    seen.push(status);
                }
            }
            let leaders: Vec<&Status> = seen.iter().filter(|status| status.leads).collect();
            if let [leader] = leaders.as_slice()
                && seen.len() == among.len()
                && seen
                    .iter()
                    .all(|status| status.leader == leader.id && status.term == leader.term)
            {
                return (leader.id, leader.term);
            }
            // No agreement yet: the members are electing. A leader elected makes itself known
            // within a heartbeat.
            self.wait_ticks(HEARTBEAT_TICKS);
        }
        panic!(
            "{}: no leader among {among:?} within {:?}",
            self.name,
            self.budget()
        );
    }
    /// Lets `ticks` pass. The test's socket is asked nothing, so the wait is its timeout; an
    /// answer that comes late to a request already given up on is dropped, and the wait goes on
    /// to its end.
    #[allow(
        clippy::disallowed_methods,
        reason = "real processes on the host's clock, threads and environment (CLAUDE.md §1a, end to end)"
    )]
    fn wait_ticks(&mut self, ticks: u32) {
        let until = Instant::now() + self.tick * ticks;
        let mut sink = vec![0u8; wire::MAX_DATAGRAM];
        loop {
            let wait = until.saturating_duration_since(Instant::now());
            if wait.is_zero() {
                return;
            }
            self.test.set_read_timeout(Some(wait)).unwrap();
            let _ = self.test.recv_from(&mut sink);
        }
    }
    /// The chance one election of a group of `voters` elects: the earliest timer of those that
    /// campaign fires `VOTE_ROUNDS` ticks or more before the next, so its vote rounds end before
    /// another campaigns. Each of `voters` timers is drawn uniformly from the `ELECTION_TICKS`
    /// ticks of `[ELECTION_TICKS, 2 · ELECTION_TICKS)`; the chance that a given one is drawn at
    /// tick `k` and every other at `k + VOTE_ROUNDS` or later, summed over the `voters` that may
    /// be earliest and the ticks it may be drawn at. Every voter campaigning is the least chance:
    /// a group that lost its leader has one fewer.
    fn election_chance(voters: usize) -> f64 {
        let slots = f64::from(ELECTION_TICKS);
        let voters = voters as f64;
        (0..ELECTION_TICKS)
            .map(|at| {
                let later = f64::from(ELECTION_TICKS.saturating_sub(at + VOTE_ROUNDS));
                voters / slots * (later / slots).powf(voters - 1.0)
            })
            .sum()
    }
    /// The elections one wait allows for: enough that each of the `caused` elections of the run
    /// elects within them with the run's confidence shared among them (Bonferroni), so the whole
    /// run fails for want of an election with at most `1 − CONFIDENCE`.
    fn elections(voters: usize, caused: u32) -> u32 {
        let miss = (1.0 - CONFIDENCE) / f64::from(caused);
        (miss.ln() / (1.0 - Self::election_chance(voters)).ln()).ceil() as u32
    }
    /// The ticks one write or read may take to be answered: its elections, and the answer.
    fn budget_ticks(elections: u32) -> u32 {
        elections * ELECTION_ROUND_TICKS + ANSWER_TICKS
    }
    fn budget(&self) -> Duration {
        self.tick * Self::budget_ticks(self.elections)
    }
    fn up_members(&self) -> Vec<u64> {
        (1..=self.voters() as u64)
            .filter(|id| self.up(*id))
            .collect()
    }
    /// Waits until every member in `among` applied the same history through the same index.
    /// A member behind catches up at least an entry a broadcast, so the wait is the elections'
    /// budget and a tick for each entry the group's log holds.
    #[allow(
        clippy::disallowed_methods,
        reason = "real processes on the host's clock, threads and environment (CLAUDE.md §1a, end to end)"
    )]
    fn converged(&mut self, among: &[u64]) -> Status {
        let entries = among
            .iter()
            .filter_map(|id| self.status(*id))
            .map(|status| status.last_index)
            .max()
            .unwrap_or(0);
        let until = Instant::now() + self.budget() + self.tick * u32::try_from(entries).unwrap();
        while Instant::now() < until {
            let seen: Vec<Status> = among.iter().filter_map(|id| self.status(*id)).collect();
            if seen.len() == among.len()
                && seen.iter().all(|status| {
                    status.applied == seen[0].applied
                        && status.digest == seen[0].digest
                        && status.applied == status.commit
                })
                && seen[0].applied > 0
            {
                return seen[0];
            }
            self.wait_ticks(HEARTBEAT_TICKS);
        }
        let seen: Vec<Option<Status>> = among.iter().map(|id| self.status(*id)).collect();
        panic!("{}: the members did not converge: {seen:#?}", self.name);
    }
}

/// What the client knows of the writes it made: the ones answered, which must be there, and
/// the ones whose answer never came, which may or may not be.
#[derive(Default)]
struct History {
    acked: BTreeMap<Vec<u8>, Vec<u8>>,
    unknown: BTreeMap<Vec<u8>, Vec<u8>>,
}

struct Client {
    leader: u64,
}

impl Client {
    fn next(&self, cluster: &Cluster, from: u64) -> u64 {
        let voters = cluster.voters() as u64;
        let mut id = from;
        for _ in 0..voters {
            id = id % voters + 1;
            if cluster.up(id) {
                return id;
            }
        }
        from
    }
    /// Whom to ask after `from` answered that it does not lead and named `hint`. A member that
    /// names no other leader is in an election, or its leader is gone: the client lets a
    /// heartbeat interval pass, within which a leader elected makes itself known, before it asks
    /// the next. Without the pause, two members that disagree on who leads during an election
    /// (one names the other, which names no one) answer at once, over and over, and the client
    /// spends its budget before the election ends.
    fn target(&self, hint: u64, cluster: &mut Cluster, from: u64) -> u64 {
        if hint != 0 && hint != from && cluster.up(hint) {
            return hint;
        }
        cluster.wait_ticks(HEARTBEAT_TICKS);
        self.next(cluster, from)
    }
    /// Writes `key`; true once a member answered that it is committed.
    #[allow(
        clippy::disallowed_methods,
        reason = "real processes on the host's clock, threads and environment (CLAUDE.md §1a, end to end)"
    )]
    fn put(
        &mut self,
        cluster: &mut Cluster,
        history: &mut History,
        key: &[u8],
        value: &[u8],
    ) -> bool {
        let mut target = self.leader;
        let until = Instant::now() + cluster.budget();
        while Instant::now() < until {
            match cluster.request(target, &Op::Put { key, value }) {
                Some(Outcome::Put(_)) => {
                    self.leader = target;
                    history.unknown.remove(key);
                    history.acked.insert(key.to_vec(), value.to_vec());
                    return true;
                }
                Some(Outcome::NotLeader(hint)) => target = self.target(hint, cluster, target),
                // Its asks are answered within a broadcast, which makes room.
                Some(Outcome::Busy) => cluster.wait_ticks(BROADCAST_TICKS),
                Some(other) => panic!("a write answered with {other:?}"),
                None => {
                    // No answer: the write may or may not be committed.
                    history.unknown.insert(key.to_vec(), value.to_vec());
                    target = self.next(cluster, target);
                }
            }
        }
        false
    }
    /// Reads `key` linearizably through the leader.
    #[allow(
        clippy::disallowed_methods,
        reason = "real processes on the host's clock, threads and environment (CLAUDE.md §1a, end to end)"
    )]
    fn get(&mut self, cluster: &mut Cluster, key: &[u8]) -> Option<Vec<u8>> {
        let mut target = self.leader;
        let until = Instant::now() + cluster.budget();
        while Instant::now() < until {
            match cluster.request(target, &Op::Get { key }) {
                Some(Outcome::Value(value)) => {
                    self.leader = target;
                    return value;
                }
                Some(Outcome::NotLeader(hint)) => target = self.target(hint, cluster, target),
                // Its asks are answered within a broadcast, which makes room.
                Some(Outcome::Busy) => cluster.wait_ticks(BROADCAST_TICKS),
                Some(other) => panic!("a read answered with {other:?}"),
                None => target = self.next(cluster, target),
            }
        }
        panic!(
            "{}: no read of {:?} was answered",
            cluster.name,
            String::from_utf8_lossy(key)
        );
    }
}

/// Every write answered reads back as written, and every unanswered one reads back as written
/// or as never written.
fn verify(cluster: &mut Cluster, client: &mut Client, history: &History) -> usize {
    for (key, value) in &history.acked {
        let read = client.get(cluster, key);
        assert_eq!(
            read.as_deref(),
            Some(value.as_slice()),
            "{}: an answered write of {} was lost",
            cluster.name,
            String::from_utf8_lossy(key)
        );
    }
    for (key, value) in &history.unknown {
        let read = client.get(cluster, key);
        assert!(
            read.is_none() || read.as_deref() == Some(value.as_slice()),
            "{}: {} reads as a value never written",
            cluster.name,
            String::from_utf8_lossy(key)
        );
    }
    history.acked.len()
}

/// Writes and reads back keys `keys`; says the slowest write and read.
#[allow(
    clippy::disallowed_methods,
    reason = "real processes on the host's clock, threads and environment (CLAUDE.md §1a, end to end)"
)]
fn write_range(
    cluster: &mut Cluster,
    client: &mut Client,
    history: &mut History,
    keys: std::ops::Range<usize>,
) -> Duration {
    let mut slowest = Duration::ZERO;
    for at in keys {
        let started = Instant::now();
        let key = format!("{}-{at}", cluster.name);
        let value = format!("value-{at}");
        assert!(
            client.put(cluster, history, key.as_bytes(), value.as_bytes()),
            "{}: a write of {key} was never answered",
            cluster.name
        );
        // Read what was just answered: linearizability asks that it be there at once.
        let read = client.get(cluster, key.as_bytes());
        assert_eq!(
            read.as_deref(),
            Some(value.as_bytes()),
            "{}: {key} did not read back",
            cluster.name
        );
        slowest = slowest.max(started.elapsed());
    }
    slowest
}

/// A group of `voters` commits a workload, and every member applies it alike.
#[allow(
    clippy::disallowed_methods,
    reason = "real processes on the host's clock, threads and environment (CLAUDE.md §1a, end to end)"
)]
fn commits(voters: usize, run: Run, name: &'static str) -> String {
    let writes = workload();
    let mut cluster = Cluster::start(name, voters, run, writes);
    let all = cluster.up_members();
    let (leader, _) = cluster.leader_among(&all);
    let mut client = Client { leader };
    let mut history = History::default();
    let started = Instant::now();
    let slowest = write_range(&mut cluster, &mut client, &mut history, 0..writes);
    let elapsed = started.elapsed();
    let checked = verify(&mut cluster, &mut client, &history);
    let status = cluster.converged(&all);
    format!(
        "{name}: {voters} members; {checked} writes answered and read back ({:.2} ms per write and read, {:.2} ms the 95/95 bound); all applied index {} alike",
        elapsed.as_secs_f64() * 1e3 / writes as f64,
        slowest.as_secs_f64() * 1e3,
        status.applied
    )
}

/// The leader is killed while writes are in flight; the others elect, and nothing answered is
/// lost. The killed member comes back on its log and applies the same history.
#[allow(
    clippy::disallowed_methods,
    reason = "real processes on the host's clock, threads and environment (CLAUDE.md §1a, end to end)"
)]
fn leader_killed(run: Run) -> String {
    let name = "leader-killed";
    let phase = workload();
    // A phase before, the writes in flight, and a phase after.
    let mut cluster = Cluster::start(name, 3, run, 3 * phase);
    let all = cluster.up_members();
    let (first, _) = cluster.leader_among(&all);
    let mut client = Client { leader: first };
    let mut history = History::default();
    write_range(&mut cluster, &mut client, &mut history, 0..phase);
    // The leader now, which the writes may have moved: the writes in flight go to it.
    let (old, old_term) = cluster.leader_among(&all);
    // Writes the leader has taken into its log, and has not answered, when it dies.
    let before = cluster.status(old).unwrap().last_index;
    for at in phase..2 * phase {
        let key = format!("{name}-{at}");
        let value = format!("value-{at}");
        cluster.next_id += 1;
        wire::put_request(
            &mut cluster.buffer,
            cluster.next_id,
            &Op::Put {
                key: key.as_bytes(),
                value: value.as_bytes(),
            },
        );
        assert!(wire::seal(&mut cluster.buffer, cluster.datagram));
        let address = cluster.address(old);
        cluster.test.send_to(&cluster.buffer, address).unwrap();
        history.unknown.insert(key.into_bytes(), value.into_bytes());
    }
    // The member takes datagrams in the order they arrive, so the first status answered after
    // them sees them; one lost is asked again, within the budget.
    let in_flight = phase as u64;
    let until = Instant::now() + cluster.budget();
    let mut appended = before;
    while appended < before + in_flight && Instant::now() < until {
        if let Some(status) = cluster.status(old) {
            appended = status.last_index;
        }
    }
    assert!(
        appended >= before + in_flight,
        "{name}: the leader never took the writes in flight"
    );
    cluster.kill(old);
    let rest: Vec<u64> = cluster.up_members();
    let (new, new_term) = cluster.leader_among(&rest);
    assert_ne!(new, old);
    assert!(new_term > old_term);
    client.leader = new;
    write_range(
        &mut cluster,
        &mut client,
        &mut history,
        2 * phase..3 * phase,
    );
    let checked = verify(&mut cluster, &mut client, &history);
    let unknown_present = history
        .unknown
        .keys()
        .filter(|key| client.get(&mut cluster, key).is_some())
        .count();
    cluster.restart(old);
    let status = cluster.converged(&all);
    format!(
        "{name}: leader {old} (term {old_term}) killed with {phase} writes in its log unanswered; {new} elected in term {new_term}; {checked} answered writes read back; {unknown_present} of those {phase} were committed by the new leader; member {old} restarted on its log and applied index {} alike",
        status.applied
    )
}

/// A follower is killed, the group goes on without it, and it comes back on its log and
/// catches up.
fn follower_restarts(run: Run) -> String {
    let name = "follower-restarts";
    let phase = workload();
    // A phase with the follower, and a phase without it.
    let mut cluster = Cluster::start(name, 3, run, 2 * phase);
    let all = cluster.up_members();
    let (leader, _) = cluster.leader_among(&all);
    let follower = all.iter().copied().find(|id| *id != leader).unwrap();
    let mut client = Client { leader };
    let mut history = History::default();
    write_range(&mut cluster, &mut client, &mut history, 0..phase);
    cluster.kill(follower);
    write_range(&mut cluster, &mut client, &mut history, phase..2 * phase);
    let before = cluster.status(leader).unwrap();
    cluster.restart(follower);
    let status = cluster.converged(&all);
    assert!(status.applied >= before.commit);
    let checked = verify(&mut cluster, &mut client, &history);
    format!(
        "{name}: follower {follower} killed after {phase} writes, {phase} written without it, restarted on its log, caught up to index {} alike; {checked} writes read back",
        status.applied
    )
}

/// The leader of five is cut off by a drop filter in its own process. It answers no read with
/// what the others replace; the others elect and go on; once the filter is lifted it follows
/// and applies the same history.
fn partition(run: Run) -> String {
    let name = "partition";
    let phase = workload();
    // A phase before the cut, the moved key three times (before, to the member cut off, after),
    // and a phase after.
    let mut cluster = Cluster::start(name, 5, run, 2 * phase + 3);
    let all = cluster.up_members();
    let (first, _) = cluster.leader_among(&all);
    let mut client = Client { leader: first };
    let mut history = History::default();
    write_range(&mut cluster, &mut client, &mut history, 0..phase);
    let key = format!("{name}-moved");
    assert!(client.put(&mut cluster, &mut history, key.as_bytes(), b"before"));
    // The leader now, which the writes may have moved.
    let (old, old_term) = cluster.leader_among(&all);
    client.leader = old;
    cluster.isolate(old, true);
    // At once, while it still believes it leads: it can confirm nothing with a quorum, so it
    // answers no read with a value and acknowledges no write; once its check of the quorum
    // fails it steps down and tells whoever waits that it does not lead.
    let early_read = cluster.request(
        old,
        &Op::Get {
            key: key.as_bytes(),
        },
    );
    assert!(
        !matches!(early_read, Some(Outcome::Value(_))),
        "{name}: the member cut off answered a read: {early_read:?}"
    );
    let early_write = cluster.request(
        old,
        &Op::Put {
            key: key.as_bytes(),
            value: b"cut-off",
        },
    );
    assert!(
        !matches!(early_write, Some(Outcome::Put(_))),
        "{name}: the member cut off acknowledged a write: {early_write:?}"
    );
    history
        .unknown
        .insert(key.clone().into_bytes(), b"cut-off".to_vec());
    let rest: Vec<u64> = all.iter().copied().filter(|id| *id != old).collect();
    let (new, new_term) = cluster.leader_among(&rest);
    assert!(new_term > old_term);
    client.leader = new;
    assert!(client.put(&mut cluster, &mut history, key.as_bytes(), b"after"));
    // The write the cut-off member took was never committed: the group's value is "after".
    history.unknown.remove(key.as_bytes());
    write_range(&mut cluster, &mut client, &mut history, phase..2 * phase);
    // The member cut off may still believe it leads; a read it answers must not be stale.
    let stale = cluster.request(
        old,
        &Op::Get {
            key: key.as_bytes(),
        },
    );
    assert!(
        !matches!(stale, Some(Outcome::Value(_))),
        "{name}: the member cut off answered a read: {stale:?}"
    );
    cluster.isolate(old, false);
    let status = cluster.converged(&all);
    let (leader, _) = cluster.leader_among(&all);
    let checked = verify(&mut cluster, &mut client, &history);
    format!(
        "{name}: leader {old} of 5 cut off; at once it answered a read with {early_read:?} and a write with {early_write:?}, and later the read of a replaced key with {stale:?}; {new} elected in term {new_term}; after the filter lifted, {leader} leads and all applied index {} alike; {checked} writes read back",
        status.applied
    )
}

/// Every member is killed at once and restarted on its log: every answered write survives.
fn all_killed(run: Run) -> String {
    let name = "all-killed";
    let phase = workload();
    let mut cluster = Cluster::start(name, 3, run, phase);
    let all = cluster.up_members();
    let (leader, _) = cluster.leader_among(&all);
    let mut client = Client { leader };
    let mut history = History::default();
    write_range(&mut cluster, &mut client, &mut history, 0..phase);
    for id in &all {
        cluster.kill(*id);
    }
    for id in &all {
        cluster.restart(*id);
    }
    let (leader, term) = cluster.leader_among(&all);
    client.leader = leader;
    let checked = verify(&mut cluster, &mut client, &history);
    let status = cluster.converged(&all);
    format!(
        "{name}: every member killed after {phase} answered writes and restarted on its log; {leader} leads in term {term}; {checked} writes read back; all applied index {} alike",
        status.applied
    )
}

/// A scenario: run at a tick, it says what it saw.
type Scenario = fn(Run) -> String;

#[allow(
    clippy::disallowed_methods,
    reason = "real processes on the host's clock, threads and environment (CLAUDE.md §1a, end to end)"
)]
fn main() -> ExitCode {
    let tick = measure_tick();
    let mut out = std::io::stdout().lock();
    writeln!(
        out,
        "tick {} ms (the 95/95 bounds of a broadcast and a timed wait, {} samples each)",
        tick.as_millis(),
        tolerance_samples()
    )
    .unwrap();
    drop(out);
    let scenarios: [(&str, Scenario); 6] = [
        ("commits-3", |run| commits(3, run, "commits-3")),
        ("commits-5", |run| commits(5, run, "commits-5")),
        ("leader-killed", leader_killed),
        ("follower-restarts", follower_restarts),
        ("partition", partition),
        ("all-killed", all_killed),
    ];
    let filter: Vec<String> = std::env::args()
        .skip(1)
        .filter(|arg| !arg.starts_with('-'))
        .collect();
    // Each scenario's first election, and the disruptions; a run of fewer causes fewer.
    let run = Run {
        tick,
        caused: scenarios.len() as u32 + DISRUPTIONS,
    };
    for (name, scenario) in scenarios {
        if !filter.is_empty() && !filter.iter().any(|wanted| name.contains(wanted.as_str())) {
            continue;
        }
        let started = Instant::now();
        let said = scenario(run);
        let mut out = std::io::stdout().lock();
        writeln!(out, "ok {said} [{:.1} s]", started.elapsed().as_secs_f64()).unwrap();
    }
    ExitCode::SUCCESS
}
