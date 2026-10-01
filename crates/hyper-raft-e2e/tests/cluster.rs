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
//! Every wait is on the fact it needs (a member's answer), bounded by a stated budget in ticks:
//! a request waits for its answer through the longest election timeout, and one write or read
//! is retried through `WAIT_ELECTIONS` elections before it fails.
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

use hyper_raft_e2e::wire::{self, Control, Kind, Op, Outcome, Status};

const NODE: &str = env!("CARGO_BIN_EXE_hyper-raft-node");
const TMP: &str = env!("CARGO_TARGET_TMPDIR");

/// Ticks in a member's election timeout, at least (focal's shell's `election_tick`, which the
/// node runs with); its randomized timeout is below twice this.
const ELECTION_TICKS: u32 = 10;
/// Ticks between a leader's heartbeats (focal's shell's `heartbeat_tick`, which the node runs
/// with): within this a member hears of a leader elected.
const HEARTBEAT_TICKS: u32 = 2;
/// How long one request waits for its answer: two of the longest election timeouts, so a
/// request to a member that is electing is answered once the election is over.
const ANSWER_TICKS: u32 = 4 * ELECTION_TICKS;
/// How many statuses a wait for a member to have taken what it was sent asks for. The member
/// takes datagrams in the order they arrive, so the first status after them sees them.
const ATTEMPTS: usize = 64;
/// How many election timeouts a wait for a leader, for members to agree, or for one write or
/// read to be answered through the elections it meets, may take.
const WAIT_ELECTIONS: u32 = 40;
/// The most writes and reads one scenario makes, and more: `commits` makes 600 (200 writes,
/// each read back at once and again in the check at its end), and the others fewer.
const MAX_OPERATIONS: u32 = 1024;
/// The keys a member's store holds, the asks it keeps waiting, and the entries its log holds:
/// above what any scenario writes, so that none is refused for room.
const MAX_KEYS: usize = 4096;
const MAX_PENDING: usize = 64;
const MAX_ENTRIES: usize = 1 << 16;
/// The share of a time's distribution its measured bound covers, and the confidence it does: the
/// 95/95 one-sided tolerance limit (Wilks 1941; the criterion USNRC Regulatory Guide 1.157 holds
/// best-estimate analyses to). The slowest of n samples bounds the share with that confidence
/// once 1 − COVERAGE^n ≥ CONFIDENCE, so n is derived, not chosen (`tolerance_samples`: 59).
const COVERAGE: f64 = 0.95;
const CONFIDENCE: f64 = 0.95;
/// The least tick a member is given: `--tick-ms` counts whole milliseconds.
const LEAST_TICK: Duration = Duration::from_millis(1);

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
    deadline: Duration,
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

fn spawn(
    id: u64,
    voters: usize,
    listen: &str,
    wal: &Path,
    tick: Duration,
    deadline: Duration,
) -> (Child, u16) {
    let voters: Vec<String> = (1..=voters).map(|voter| voter.to_string()).collect();
    let mut child = Command::new(NODE)
        .args(["--id", &id.to_string()])
        .args(["--voters", &voters.join(",")])
        .args(["--listen", listen])
        .args(["--wal", wal.to_str().unwrap()])
        .args(["--tick-ms", &tick.as_millis().to_string()])
        .args(["--deadline-ms", &deadline.as_millis().to_string()])
        .args(["--max-keys", &MAX_KEYS.to_string()])
        .args(["--max-pending", &MAX_PENDING.to_string()])
        .args(["--max-entries", &MAX_ENTRIES.to_string()])
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
    fn start(name: &'static str, voters: usize, tick: Duration) -> Self {
        // Every scenario's members end by themselves after it would have failed: each of its
        // operations fails once it outlasts its budget, so none outlives them all.
        let deadline = Self::budget(tick) * MAX_OPERATIONS;
        let mut members = Vec::new();
        for id in 1..=voters as u64 {
            let wal =
                PathBuf::from(TMP).join(format!("e2e-{}-{name}-{id}.wal", std::process::id()));
            remove(&wal);
            let (child, port) = spawn(id, voters, "127.0.0.1:0", &wal, tick, deadline);
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
            deadline,
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
            self.test.set_read_timeout(Some(wait)).unwrap();
            let length = match self.test.recv_from(&mut received) {
                Ok((length, _)) => length,
                Err(error)
                    if matches!(
                        error.kind(),
                        ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::ConnectionReset
                    ) =>
                {
                    if error.kind() == ErrorKind::ConnectionReset {
                        continue;
                    }
                    return None;
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
    fn control(&mut self, id: u64, control: &Control) {
        self.next_id += 1;
        let request = self.next_id;
        let until = Instant::now() + Self::budget(self.tick);
        loop {
            wire::put_control(&mut self.buffer, request, control);
            if let Some(outcome) = self.ask(id, request) {
                assert_eq!(outcome, Outcome::Done, "member {id} refused an instruction");
                return;
            }
            assert!(
                Instant::now() < until,
                "member {id} took no instruction within {:?}",
                Self::budget(self.tick)
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
            self.deadline,
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
    fn leader_among(&mut self, among: &[u64]) -> (u64, u64) {
        for _ in 0..WAIT_ELECTIONS * 2 {
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
            // No agreement yet: the members are electing. The next round of statuses waits
            // for them, through the time each answer takes.
            self.wait_ticks(ELECTION_TICKS);
        }
        panic!("{}: no leader among {among:?}", self.name);
    }
    /// Lets `ticks` pass. The test's socket is asked nothing, so the wait is its timeout; an
    /// answer that comes late to a request already given up on is dropped, and the wait goes on
    /// to its end.
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
    /// How long one write or read may take to be answered: `WAIT_ELECTIONS` elections, each
    /// within twice the election timeout.
    fn budget(tick: Duration) -> Duration {
        tick * (2 * ELECTION_TICKS * WAIT_ELECTIONS)
    }
    fn up_members(&self) -> Vec<u64> {
        (1..=self.voters() as u64)
            .filter(|id| self.up(*id))
            .collect()
    }
    /// Waits until every member in `among` applied the same history through the same index.
    fn converged(&mut self, among: &[u64]) -> Status {
        for _ in 0..WAIT_ELECTIONS * 2 {
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
            self.wait_ticks(ELECTION_TICKS);
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
    fn put(
        &mut self,
        cluster: &mut Cluster,
        history: &mut History,
        key: &[u8],
        value: &[u8],
    ) -> bool {
        let mut target = self.leader;
        let until = Instant::now() + Cluster::budget(cluster.tick);
        while Instant::now() < until {
            match cluster.request(target, &Op::Put { key, value }) {
                Some(Outcome::Put(_)) => {
                    self.leader = target;
                    history.unknown.remove(key);
                    history.acked.insert(key.to_vec(), value.to_vec());
                    return true;
                }
                Some(Outcome::NotLeader(hint)) => target = self.target(hint, cluster, target),
                Some(Outcome::Busy) => cluster.wait_ticks(1),
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
    fn get(&mut self, cluster: &mut Cluster, key: &[u8]) -> Option<Vec<u8>> {
        let mut target = self.leader;
        let until = Instant::now() + Cluster::budget(cluster.tick);
        while Instant::now() < until {
            match cluster.request(target, &Op::Get { key }) {
                Some(Outcome::Value(value)) => {
                    self.leader = target;
                    return value;
                }
                Some(Outcome::NotLeader(hint)) => target = self.target(hint, cluster, target),
                Some(Outcome::Busy) => cluster.wait_ticks(1),
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

fn write_range(
    cluster: &mut Cluster,
    client: &mut Client,
    history: &mut History,
    keys: std::ops::Range<usize>,
) {
    for at in keys {
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
    }
}

/// A group of `voters` commits a workload, and every member applies it alike.
fn commits(voters: usize, tick: Duration, name: &'static str) -> String {
    let mut cluster = Cluster::start(name, voters, tick);
    let all = cluster.up_members();
    let (leader, _) = cluster.leader_among(&all);
    let mut client = Client { leader };
    let mut history = History::default();
    let started = Instant::now();
    write_range(&mut cluster, &mut client, &mut history, 0..200);
    let elapsed = started.elapsed();
    let checked = verify(&mut cluster, &mut client, &history);
    let status = cluster.converged(&all);
    format!(
        "{name}: {voters} members; {checked} writes answered and read back ({:.2} ms per write and read); all applied index {} alike",
        elapsed.as_secs_f64() * 1e3 / 200.0,
        status.applied
    )
}

/// The leader is killed while writes are in flight; the others elect, and nothing answered is
/// lost. The killed member comes back on its log and applies the same history.
fn leader_killed(tick: Duration) -> String {
    let name = "leader-killed";
    let mut cluster = Cluster::start(name, 3, tick);
    let all = cluster.up_members();
    let (first, _) = cluster.leader_among(&all);
    let mut client = Client { leader: first };
    let mut history = History::default();
    write_range(&mut cluster, &mut client, &mut history, 0..50);
    // The leader now, which the writes may have moved: the writes in flight go to it.
    let (old, old_term) = cluster.leader_among(&all);
    // Writes the leader has taken into its log, and has not answered, when it dies.
    let before = cluster.status(old).unwrap().last_index;
    for at in 50..55 {
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
    let mut appended = before;
    for _ in 0..ATTEMPTS {
        appended = cluster.status(old).unwrap().last_index;
        if appended >= before + 5 {
            break;
        }
    }
    assert!(
        appended >= before + 5,
        "{name}: the leader never took the writes in flight"
    );
    cluster.kill(old);
    let rest: Vec<u64> = cluster.up_members();
    let (new, new_term) = cluster.leader_among(&rest);
    assert_ne!(new, old);
    assert!(new_term > old_term);
    client.leader = new;
    write_range(&mut cluster, &mut client, &mut history, 55..105);
    let checked = verify(&mut cluster, &mut client, &history);
    let unknown_present = history
        .unknown
        .keys()
        .filter(|key| client.get(&mut cluster, key).is_some())
        .count();
    cluster.restart(old);
    let status = cluster.converged(&all);
    format!(
        "{name}: leader {old} (term {old_term}) killed with 5 writes in its log unanswered; {new} elected in term {new_term}; {checked} answered writes read back; {unknown_present} of those 5 were committed by the new leader; member {old} restarted on its log and applied index {} alike",
        status.applied
    )
}

/// A follower is killed, the group goes on without it, and it comes back on its log and
/// catches up.
fn follower_restarts(tick: Duration) -> String {
    let name = "follower-restarts";
    let mut cluster = Cluster::start(name, 3, tick);
    let all = cluster.up_members();
    let (leader, _) = cluster.leader_among(&all);
    let follower = all.iter().copied().find(|id| *id != leader).unwrap();
    let mut client = Client { leader };
    let mut history = History::default();
    write_range(&mut cluster, &mut client, &mut history, 0..30);
    cluster.kill(follower);
    write_range(&mut cluster, &mut client, &mut history, 30..130);
    let before = cluster.status(leader).unwrap();
    cluster.restart(follower);
    let status = cluster.converged(&all);
    assert!(status.applied >= before.commit);
    let checked = verify(&mut cluster, &mut client, &history);
    format!(
        "{name}: follower {follower} killed after 30 writes, 100 written without it, restarted on its log, caught up to index {} alike; {checked} writes read back",
        status.applied
    )
}

/// The leader of five is cut off by a drop filter in its own process. It answers no read with
/// what the others replace; the others elect and go on; once the filter is lifted it follows
/// and applies the same history.
fn partition(tick: Duration) -> String {
    let name = "partition";
    let mut cluster = Cluster::start(name, 5, tick);
    let all = cluster.up_members();
    let (first, _) = cluster.leader_among(&all);
    let mut client = Client { leader: first };
    let mut history = History::default();
    write_range(&mut cluster, &mut client, &mut history, 0..20);
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
    write_range(&mut cluster, &mut client, &mut history, 20..60);
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
fn all_killed(tick: Duration) -> String {
    let name = "all-killed";
    let mut cluster = Cluster::start(name, 3, tick);
    let all = cluster.up_members();
    let (leader, _) = cluster.leader_among(&all);
    let mut client = Client { leader };
    let mut history = History::default();
    write_range(&mut cluster, &mut client, &mut history, 0..50);
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
        "{name}: every member killed after 50 answered writes and restarted on its log; {leader} leads in term {term}; {checked} writes read back; all applied index {} alike",
        status.applied
    )
}

/// A scenario: run at a tick, it says what it saw.
type Scenario = fn(Duration) -> String;

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
        ("commits-3", |tick| commits(3, tick, "commits-3")),
        ("commits-5", |tick| commits(5, tick, "commits-5")),
        ("leader-killed", leader_killed),
        ("follower-restarts", follower_restarts),
        ("partition", partition),
        ("all-killed", all_killed),
    ];
    let filter: Vec<String> = std::env::args()
        .skip(1)
        .filter(|arg| !arg.starts_with('-'))
        .collect();
    for (name, scenario) in scenarios {
        if !filter.is_empty() && !filter.iter().any(|wanted| name.contains(wanted.as_str())) {
            continue;
        }
        let started = Instant::now();
        let said = scenario(tick);
        let mut out = std::io::stdout().lock();
        writeln!(out, "ok {said} [{:.1} s]", started.elapsed().as_secs_f64()).unwrap();
    }
    ExitCode::SUCCESS
}
