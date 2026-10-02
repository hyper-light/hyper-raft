//! hyper-raft in real use. Each scenario starts a group of real processes
//! (`hyper-raft-node`), each a member on its own UDP socket with its own fsynced log, and
//! drives it as a client would, through the same datagrams a client sends. What is asserted is
//! what a client can observe:
//! - every write a member answered is read back, by a linearizable read through whichever
//!   member leads, after leaders are killed, members restart and members are cut off;
//! - a member cut off from the group never answers a read with a value the group has replaced,
//!   and its detectors and every other member's see the cut;
//! - every member that is up applies the same history (the same digest at the same index);
//! - a member started again is reported restarted to every other member's core.
//!
//! The members elect by suspicion on their own failure detectors: each runs the node-pair
//! liveness stream (`hyper_liveness`, timing step L-3) and takes its words to its core, with the
//! election law's timing derived from what the stream measured (`docs/timing.md` §2.9), and the
//! test tells no member what to believe. The test derives no time of its own. It waits on facts
//! — a leader, an answer, a report — and goes on while the group moves (`docs/sim.md` §4.2):
//! while any member's term, commit, applied index, last index or restarts seen moves, or, while a
//! member still has a pair no margin judges, the heartbeats it has taken (the evidence its
//! detectors are built from). It gives up once a quiet period passes with nothing moved: the
//! longest any member states its detectors take to suspect a crash (or, while a pair no margin
//! judges takes heartbeats at a longer interval, that interval), its election's span and
//! rounds and an ask's rounds, by the members' own law; never less than the test's own
//! retransmission timeout, RFC 6298's one second before any measurement and never less after
//! (§2.1, §2.4), past which it could not tell a member that did not move from an answer it did
//! not wait for. An ask waits that timeout for its answer.
//!
//! The scenarios run one after another in this one thread (`harness = false`), one group at a
//! time: at most five member processes at once.
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
    io::{BufRead, BufReader, Write},
    net::{SocketAddr, UdpSocket},
    path::{Path, PathBuf},
    process::{Child, Command, ExitCode, Stdio},
    time::{Duration, Instant},
};

use hyper_raft::proto::{self, Entry};
use hyper_raft_e2e::{
    node,
    stream::{self, Report},
    wire::{self, Control, Kind, Op, Outcome},
};

const NODE: &str = env!("CARGO_BIN_EXE_hyper-raft-node");
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

#[expect(
    clippy::disallowed_methods,
    reason = "a test removes the files it made in its own target directory"
)]
fn remove(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// The writes in a phase of scenario `name`: one more than one append carries to a member behind,
/// so that a member that missed a phase catches up over more than one append. An append carries
/// entries up to the datagram less a message's fixed bytes (`node::MESSAGE_ROOM`), counted at
/// their encoded bytes; the scenario's shortest write is its first, so this many of it is the
/// most an append carries.
fn phase(name: &str, datagram: usize) -> usize {
    let mut data = Vec::new();
    wire::put_command(
        &mut data,
        &wire::Command {
            origin: 1,
            sequence: 1,
            key: format!("{name}-0").as_bytes(),
            value: b"value-0",
        },
    );
    let entry = Entry {
        index: 1,
        term: 1,
        data,
        ..Entry::default()
    };
    let room = datagram.saturating_sub(node::MESSAGE_ROOM) as u64;
    (room / proto::encoded_bytes(&entry).max(1)) as usize + 1
}

/// What a member holds, from what its scenario writes: the keys, the asks it keeps waiting at
/// once, and the writes, which bound its log (one entry for each, and one for each term).
#[derive(Clone, Copy)]
struct Room {
    keys: usize,
    pending: usize,
    writes: usize,
}

struct Member {
    child: Option<Child>,
    address: SocketAddr,
    wal: PathBuf,
}

/// What a wait last saw of each member's progress, and until when it waits without seeing more.
struct Watch {
    seen: BTreeMap<u64, [u64; 7]>,
    until: Instant,
}

struct Cluster {
    name: &'static str,
    members: Vec<Member>,
    room: Room,
    /// What each member's latest report says its law takes: its stated detection, its election's
    /// span and rounds, and an ask's rounds.
    law: BTreeMap<u64, Duration>,
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

fn spawn(id: u64, voters: usize, listen: &str, wal: &Path, room: Room) -> (Child, u16) {
    let voters: Vec<String> = (1..=voters).map(|voter| voter.to_string()).collect();
    let mut child = Command::new(NODE)
        .args(["--id", &id.to_string()])
        .args(["--voters", &voters.join(",")])
        .args(["--listen", listen])
        .args(["--wal", wal.to_str().unwrap()])
        .args(["--max-keys", &room.keys.to_string()])
        .args(["--max-pending", &room.pending.to_string()])
        .args(["--max-writes", &room.writes.to_string()])
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
    /// Starts `voters` members for scenario `name`, each given `room`.
    fn start(name: &'static str, voters: usize, room: Room) -> Self {
        let mut members = Vec::new();
        for id in 1..=voters as u64 {
            let wal =
                PathBuf::from(TMP).join(format!("e2e-{}-{name}-{id}.wal", std::process::id()));
            remove(&wal);
            let (child, port) = spawn(id, voters, "127.0.0.1:0", &wal, room);
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
            room,
            law: BTreeMap::new(),
            test,
            datagram,
            next_id: 0,
            buffer: Vec::new(),
        };
        for id in cluster.up_members() {
            cluster.tell_peers(id);
        }
        cluster
    }
    fn voters(&self) -> usize {
        self.members.len()
    }
    fn up(&self, id: u64) -> bool {
        self.members[(id - 1) as usize].child.is_some()
    }
    fn up_members(&self) -> Vec<u64> {
        (1..=self.voters() as u64)
            .filter(|id| self.up(*id))
            .collect()
    }
    fn address(&self, id: u64) -> SocketAddr {
        self.members[(id - 1) as usize].address
    }

    /// Sends what is in the buffer to `id` and waits for its answer to `ask`, one retransmission
    /// timeout at most; the answer's body.
    #[allow(
        clippy::disallowed_methods,
        reason = "real processes on the host's clock, threads and environment (CLAUDE.md §1a, end to end)"
    )]
    fn exchange(&mut self, id: u64, ask: u64) -> Option<Vec<u8>> {
        assert!(wire::seal(&mut self.buffer, self.datagram));
        let to = self.address(id);
        self.test.send_to(&self.buffer, to).ok()?;
        let deadline = Instant::now() + RTO;
        let mut received = vec![0u8; wire::MAX_DATAGRAM];
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            // Waited for by a peek, taken without waiting (`wire::arrives`). A refusal from a
            // member that is down reads as an error on some platforms: no answer yet, and the
            // wait goes on to its timeout.
            if !wire::arrives(&self.test, Some(left), &mut received).unwrap() {
                return None;
            }
            let Ok((length, _)) = wire::take(&self.test, &mut received) else {
                continue;
            };
            let Some((Kind::Response, body)) = wire::open(&received[..length]) else {
                continue;
            };
            if wire::Reader::new(body).u64() == Some(ask) {
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
    fn report(&mut self, id: u64) -> Option<Report> {
        self.next_id += 1;
        let ask = self.next_id;
        stream::put_report_ask(&mut self.buffer, ask);
        let body = self.exchange(id, ask)?;
        let (_, report) = stream::read_report(&body, hyper_raft::MAX_MEMBERS)?;
        let round = Duration::from_nanos(report.round_ns);
        // The longest its detectors state to suspect a crash, or, while a pair no margin judges
        // takes heartbeats at a longer interval, that interval: the most a wait that goes on
        // while those heartbeats move can see none.
        let law = Duration::from_nanos(report.detection_ns.max(report.unjudged_interval_ns))
            + Duration::from_nanos(report.span_ns)
            + round * (ELECTION_ROUNDS + ANSWER_ROUNDS);
        self.law.insert(id, law);
        Some(report)
    }
    /// Sends an instruction to `id` until it answers, each retransmission timeout, for as long as
    /// its process runs: an instruction is an idempotent datagram, which a loaded machine may
    /// drop or deliver late, and a member just started has no group yet whose progress could
    /// bound the wait. A member whose process ended fails it.
    fn instruct(&mut self, id: u64, control: &Control) {
        self.next_id += 1;
        let ask = self.next_id;
        loop {
            wire::put_control(&mut self.buffer, ask, control);
            if let Some(body) = self.exchange(id, ask) {
                let outcome = wire::read_response(&body).map(|(_, outcome)| outcome);
                assert_eq!(
                    outcome,
                    Some(Outcome::Done),
                    "member {id} refused an instruction"
                );
                return;
            }
            let name = self.name;
            let child = self.members[(id - 1) as usize].child.as_mut().unwrap();
            assert!(
                child.try_wait().unwrap().is_none(),
                "{name}: member {id} ended before it took an instruction"
            );
        }
    }
    fn tell_peers(&mut self, id: u64) {
        let peers: Vec<(u64, SocketAddr)> = (1..=self.voters() as u64)
            .map(|peer| (peer, self.address(peer)))
            .collect();
        self.instruct(id, &Control::Peers(peers));
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
    /// Whether the group is still moving: asks every member up for its report, and extends the
    /// watch by a quiet period whenever any member's term, commit, applied index, last index or
    /// restarts seen has moved since it last looked, or, while it has a pair no margin judges,
    /// the heartbeats it has taken. The group is quiet only once a look that began after the
    /// quiet period ended saw nothing move: an ask whose answer was lost spends a retransmission
    /// timeout of the test's own, not of the group's.
    #[allow(
        clippy::disallowed_methods,
        reason = "real processes on the host's clock, threads and environment (CLAUDE.md §1a, end to end)"
    )]
    fn moving(&mut self, watch: &mut Watch) -> bool {
        let looked = Instant::now();
        let mut moved = false;
        let mut seen = Vec::new();
        for id in self.up_members() {
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
                seen.push(report);
            }
        }
        if moved {
            watch.until = Instant::now() + self.quiet();
        }
        let moving = looked < watch.until;
        if !moving {
            // What the group was when it was judged stuck, for the failure that follows.
            eprintln!(
                "{}: nothing moved for {:?}; the last look: {seen:?}",
                self.name,
                self.quiet()
            );
        }
        moving
    }
    /// The reports of the members in `among` that answer.
    fn reports(&mut self, among: &[u64]) -> BTreeMap<u64, Report> {
        among
            .iter()
            .filter_map(|id| self.report(*id).map(|report| (*id, report)))
            .collect()
    }
    /// Waits while the group moves until `fact` holds of the reports of the members in `among`;
    /// whether it did.
    fn until(&mut self, among: &[u64], fact: impl Fn(&BTreeMap<u64, Report>) -> bool) -> bool {
        let mut watch = self.watch();
        loop {
            let reports = self.reports(among);
            if fact(&reports) {
                return true;
            }
            if !self.moving(&mut watch) {
                return false;
            }
        }
    }
    /// The member that leads in the latest term any member leads in, once one does, waited for
    /// while the group moves. A member cut off may still believe it leads an older term.
    fn leader(&mut self) -> Option<u64> {
        let mut watch = self.watch();
        loop {
            let up = self.up_members();
            let leader = self
                .reports(&up)
                .into_iter()
                .filter(|(_, report)| report.status.leads)
                .max_by_key(|(_, report)| report.status.term)
                .map(|(id, _)| id);
            if leader.is_some() {
                return leader;
            }
            if !self.moving(&mut watch) {
                return None;
            }
        }
    }
    /// Waits while the group moves until the members in `among` agree on one leader of one term,
    /// which is among them, and says which.
    fn leader_among(&mut self, among: &[u64]) -> (u64, u64) {
        // What the reports agreed on when they did: asked again, a leader a detector's mistake
        // deposed since would be gone.
        let agreed = std::cell::Cell::new((0, 0));
        let count = among.len();
        let found = self.until(among, |reports| {
            let leaders: Vec<&Report> = reports.values().filter(|r| r.status.leads).collect();
            let [leader] = leaders.as_slice() else {
                return false;
            };
            let all = reports.len() == count
                && reports.values().all(|r| {
                    r.status.leader == leader.status.id && r.status.term == leader.status.term
                });
            if all {
                agreed.set((leader.status.id, leader.status.term));
            }
            all
        });
        assert!(
            found,
            "{}: the group stopped moving with no leader among {among:?}",
            self.name
        );
        agreed.get()
    }
    /// Kills member `id` with SIGKILL (TerminateProcess on Windows): no flush, no goodbye. Nobody
    /// is told: the others' detectors find out.
    fn kill(&mut self, id: u64) {
        let mut child = self.members[(id - 1) as usize]
            .child
            .take()
            .expect("a member that is up");
        child.kill().unwrap();
        child.wait().unwrap();
        self.law.remove(&id);
    }
    /// Starts member `id` again on its log, at the address it had; then, while the group moves,
    /// every other member up that had heard its last run reports the restart its stream saw
    /// (`hyper_liveness::Change::Restarted`). A member that never took a heartbeat of the last
    /// run cannot tell the new one from a first.
    fn restart(&mut self, id: u64) {
        let up = self.up_members();
        let before: BTreeMap<u64, u64> = self
            .reports(&up)
            .into_iter()
            .filter(|(_, r)| r.heard.contains(&id))
            .map(|(other, r)| (other, r.restarts))
            .collect();
        let member = &self.members[(id - 1) as usize];
        assert!(member.child.is_none(), "member {id} is up");
        let listen = member.address.to_string();
        let (child, port) = spawn(id, self.voters(), &listen, &member.wal.clone(), self.room);
        assert_eq!(port, self.members[(id - 1) as usize].address.port());
        self.members[(id - 1) as usize].child = Some(child);
        self.tell_peers(id);
        let others: Vec<u64> = before.keys().copied().collect();
        let seen = self.until(&others, |reports| {
            before
                .iter()
                .all(|(other, restarts)| reports.get(other).is_some_and(|r| r.restarts > *restarts))
        });
        assert!(
            seen,
            "{}: a member's stream never reported {id}'s restart",
            self.name
        );
    }
    fn isolate(&mut self, id: u64, cut: bool) {
        self.instruct(id, &Control::Isolate(cut));
    }
    /// Waits while the group moves until every member in `among` applied the same history
    /// through the same index, all it knows committed.
    fn converged(&mut self, among: &[u64]) -> u64 {
        let count = among.len();
        let applied = std::cell::Cell::new(0);
        let found = self.until(among, |reports| {
            let Some(first) = reports.values().next() else {
                return false;
            };
            let alike = reports.len() == count
                && first.status.applied > 0
                && reports.values().all(|r| {
                    r.status.applied == first.status.applied
                        && r.status.digest == first.status.digest
                        && r.status.applied == r.status.commit
                });
            if alike {
                applied.set(first.status.applied);
            }
            alike
        });
        if !found {
            let reports = self.reports(among);
            panic!(
                "{}: the members stopped moving before they converged: {reports:#?}",
                self.name
            );
        }
        applied.get()
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
    /// Whom to ask next: the leader `hint` names, if it is another member up; the member that
    /// leads the latest term otherwise, once one does.
    fn next(&mut self, cluster: &mut Cluster, hint: u64) {
        if hint != 0 && hint != self.leader && cluster.up(hint) {
            self.leader = hint;
        } else if let Some(leader) = cluster.leader() {
            self.leader = leader;
        }
    }
    /// Writes `key`; true once a member answered that it is committed, false once the group
    /// stopped moving without an answer.
    fn put(
        &mut self,
        cluster: &mut Cluster,
        history: &mut History,
        key: &[u8],
        value: &[u8],
    ) -> bool {
        let mut watch = cluster.watch();
        loop {
            let hint = match cluster.ask(self.leader, &Op::Put { key, value }) {
                Some(Outcome::Put(_)) => {
                    history.unknown.remove(key);
                    history.acked.insert(key.to_vec(), value.to_vec());
                    return true;
                }
                Some(Outcome::NotLeader(hint)) => hint,
                Some(Outcome::Busy) => self.leader,
                Some(other) => panic!("a write answered with {other:?}"),
                None => {
                    // No answer: the write may or may not be committed.
                    history.unknown.insert(key.to_vec(), value.to_vec());
                    0
                }
            };
            if !cluster.moving(&mut watch) {
                return false;
            }
            self.next(cluster, hint);
        }
    }
    /// Reads `key` linearizably through the leader.
    fn get(&mut self, cluster: &mut Cluster, key: &[u8]) -> Option<Vec<u8>> {
        let mut watch = cluster.watch();
        loop {
            let hint = match cluster.ask(self.leader, &Op::Get { key }) {
                Some(Outcome::Value(value)) => return value,
                Some(Outcome::NotLeader(hint)) => hint,
                Some(Outcome::Busy) => self.leader,
                Some(other) => panic!("a read answered with {other:?}"),
                None => 0,
            };
            assert!(
                cluster.moving(&mut watch),
                "{}: the group stopped moving with no read of {:?} answered",
                cluster.name,
                String::from_utf8_lossy(key)
            );
            self.next(cluster, hint);
        }
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
            "{}: the group stopped moving with a write of {key} unanswered",
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

/// The datagram the test's sockets carry at most, which every phase is sized by.
fn datagram() -> usize {
    wire::largest(&UdpSocket::bind("127.0.0.1:0").unwrap()).unwrap()
}

/// A group of `voters` commits a phase of writes, and every member applies it alike.
#[allow(
    clippy::disallowed_methods,
    reason = "real processes on the host's clock, threads and environment (CLAUDE.md §1a, end to end)"
)]
fn commits(voters: usize, name: &'static str) -> String {
    let writes = phase(name, datagram());
    let room = Room {
        keys: writes,
        pending: 1,
        writes,
    };
    let mut cluster = Cluster::start(name, voters, room);
    let all = cluster.up_members();
    let (leader, _) = cluster.leader_among(&all);
    let mut client = Client { leader };
    let mut history = History::default();
    let started = Instant::now();
    let slowest = write_range(&mut cluster, &mut client, &mut history, 0..writes);
    let elapsed = started.elapsed();
    let checked = verify(&mut cluster, &mut client, &history);
    let applied = cluster.converged(&all);
    format!(
        "{name}: {voters} members; {checked} writes answered and read back ({:.2} ms per write and read, {:.2} ms the slowest); all applied index {applied} alike",
        elapsed.as_secs_f64() * 1e3 / writes as f64,
        slowest.as_secs_f64() * 1e3,
    )
}

/// A client of its own for the writes in flight: it sends them at once without waiting, and reads
/// their answers only to leave out of its next sending the ones answered. Answers to that many at
/// once would fill the test's own socket and push out the reports it waits on.
struct Flight {
    socket: UdpSocket,
    /// The request each write was last sent under: id → the write's number.
    sent: BTreeMap<u64, usize>,
    /// The writes a member answered committed.
    answered: std::collections::BTreeSet<usize>,
}

impl Flight {
    fn new() -> Self {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.set_nonblocking(true).unwrap();
        Self {
            socket,
            sent: BTreeMap::new(),
            answered: std::collections::BTreeSet::new(),
        }
    }
    /// Sends the writes `keys` of scenario `name` not yet answered to `to`, at once.
    fn send(&mut self, cluster: &mut Cluster, name: &str, to: u64, keys: std::ops::Range<usize>) {
        self.take_answers();
        self.sent.clear();
        let address = cluster.address(to);
        for at in keys.filter(|at| !self.answered.contains(at)) {
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
            // A full socket refuses or drops: the write is sent again.
            let _ = self.socket.send_to(&cluster.buffer, address);
            self.sent.insert(cluster.next_id, at);
        }
    }
    /// Takes the answers that have come, without waiting.
    fn take_answers(&mut self) {
        let mut received = vec![0u8; wire::MAX_DATAGRAM];
        while let Ok((length, _)) = self.socket.recv_from(&mut received) {
            let Some((Kind::Response, body)) = wire::open(&received[..length]) else {
                continue;
            };
            if let Some((id, Outcome::Put(_))) = wire::read_response(body)
                && let Some(at) = self.sent.get(&id)
            {
                self.answered.insert(*at);
            }
        }
    }
}

/// The leader is killed while writes are in flight; the others elect, and nothing answered is
/// lost. The killed member comes back on its log and applies the same history.
fn leader_killed() -> String {
    let name = "leader-killed";
    let phase = phase(name, datagram());
    // A phase before, the writes in flight, and a phase after; the leader keeps the writes in
    // flight waiting, and the client's one.
    let room = Room {
        keys: 3 * phase,
        pending: phase + 1,
        writes: 3 * phase,
    };
    let mut cluster = Cluster::start(name, 3, room);
    let all = cluster.up_members();
    let (first, _) = cluster.leader_among(&all);
    let mut client = Client { leader: first };
    let mut history = History::default();
    write_range(&mut cluster, &mut client, &mut history, 0..phase);
    // The leader now, which the writes may have moved: the writes in flight go to it.
    let in_flight = phase;
    for at in phase..2 * phase {
        history.unknown.insert(
            format!("{name}-{at}").into_bytes(),
            format!("value-{at}").into_bytes(),
        );
    }
    // The writes in flight go to whoever leads, and those not answered are sent again while its
    // log holds fewer than every write made — a datagram is lost when a socket's buffer is full,
    // and the leadership may move — until a leader holds them all; a write a leader holds
    // already it does not propose again.
    let mut flight = Flight::new();
    let made = (phase + in_flight) as u64;
    let mut watch = cluster.watch();
    let (mut sent_to, mut seen) = (0, 0);
    let (old, old_term) = loop {
        let leader = cluster.leader().unwrap_or(sent_to);
        let report = cluster.report(leader).filter(|r| r.status.leads);
        if let Some(report) = &report
            && report.writes >= made
        {
            break (leader, report.status.term);
        }
        let holds = report.map_or(seen, |r| r.writes);
        if leader != sent_to || holds == seen {
            flight.send(&mut cluster, name, leader, phase..2 * phase);
            sent_to = leader;
        }
        seen = holds;
        if !cluster.moving(&mut watch) {
            let up = cluster.up_members();
            let reports = cluster.reports(&up);
            panic!(
                "{name}: the group stopped moving before a leader took the writes in flight ({} of {in_flight} answered): {reports:#?}",
                flight.answered.len()
            );
        }
    };
    // The writes in flight that were answered are committed.
    flight.take_answers();
    let answered = flight.answered.len();
    for at in &flight.answered {
        let key = format!("{name}-{at}").into_bytes();
        if let Some(value) = history.unknown.remove(&key) {
            history.acked.insert(key, value);
        }
    }
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
    let applied = cluster.converged(&all);
    format!(
        "{name}: leader {old} (term {old_term}) killed with the {in_flight} writes in flight in its log; {new} elected in term {new_term}; {checked} answered writes read back; {answered} of them answered before the kill, and {unknown_present} of those unanswered committed by the new leader; member {old} restarted on its log, its restart reported, and applied index {applied} alike"
    )
}

/// A follower is killed, the group goes on without it, and it comes back on its log and
/// catches up.
fn follower_restarts() -> String {
    let name = "follower-restarts";
    let phase = phase(name, datagram());
    // A phase with the follower, and a phase without it.
    let room = Room {
        keys: 2 * phase,
        pending: 1,
        writes: 2 * phase,
    };
    let mut cluster = Cluster::start(name, 3, room);
    let all = cluster.up_members();
    let (leader, _) = cluster.leader_among(&all);
    let follower = all.iter().copied().find(|id| *id != leader).unwrap();
    let mut client = Client { leader };
    let mut history = History::default();
    write_range(&mut cluster, &mut client, &mut history, 0..phase);
    cluster.kill(follower);
    write_range(&mut cluster, &mut client, &mut history, phase..2 * phase);
    let up = cluster.up_members();
    let commit = cluster
        .reports(&up)
        .values()
        .map(|r| r.status.commit)
        .max()
        .unwrap();
    cluster.restart(follower);
    let applied = cluster.converged(&all);
    assert!(applied >= commit);
    let checked = verify(&mut cluster, &mut client, &history);
    format!(
        "{name}: follower {follower} killed after {phase} writes, {phase} written without it, restarted on its log, its restart reported, caught up to index {applied} alike; {checked} writes read back"
    )
}

/// The leader of five is cut off by a drop filter in its own process, its heartbeats with its
/// Raft messages. It answers no read with what the others replace; it suspects every other and
/// every other suspects it; the others elect and go on; once the filter is lifted it follows and
/// applies the same history.
fn partition() -> String {
    let name = "partition";
    let phase = phase(name, datagram());
    // A phase before the cut, the moved key three times (before, to the member cut off, after),
    // and a phase after.
    let room = Room {
        keys: 2 * phase + 1,
        pending: 1,
        writes: 2 * phase + 3,
    };
    let mut cluster = Cluster::start(name, 5, room);
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
    // answers no read with a value and acknowledges no write; once its detectors leave it no
    // quorum it steps down and tells whoever waits that it does not lead.
    let early_read = cluster.ask(
        old,
        &Op::Get {
            key: key.as_bytes(),
        },
    );
    assert!(
        !matches!(early_read, Some(Outcome::Value(_))),
        "{name}: the member cut off answered a read: {early_read:?}"
    );
    let early_write = cluster.ask(
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
    // The cut is the detectors' to see: the member cut off suspects every other, and every other
    // suspects it.
    let seen = cluster.until(&all, |reports| {
        reports.len() == all.len()
            && reports.iter().all(|(id, r)| {
                if *id == old {
                    rest.iter().all(|peer| r.suspected.contains(peer))
                } else {
                    r.suspected.contains(&old)
                }
            })
    });
    assert!(seen, "{name}: the detectors did not see the cut");
    client.leader = new;
    assert!(client.put(&mut cluster, &mut history, key.as_bytes(), b"after"));
    // The write the cut-off member took was never committed: the group's value is "after".
    history.unknown.remove(key.as_bytes());
    write_range(&mut cluster, &mut client, &mut history, phase..2 * phase);
    // The member cut off may still believe it leads; a read it answers must not be stale.
    let stale = cluster.ask(
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
    let applied = cluster.converged(&all);
    let (leader, _) = cluster.leader_among(&all);
    let checked = verify(&mut cluster, &mut client, &history);
    format!(
        "{name}: leader {old} of 5 cut off; at once it answered a read with {early_read:?} and a write with {early_write:?}, and later the read of a replaced key with {stale:?}; it suspected all four and all four suspected it; {new} elected in term {new_term}; after the filter lifted, {leader} leads and all applied index {applied} alike; {checked} writes read back"
    )
}

/// Every member is killed at once and restarted on its log: every answered write survives.
fn all_killed() -> String {
    let name = "all-killed";
    let phase = phase(name, datagram());
    let room = Room {
        keys: phase,
        pending: 1,
        writes: phase,
    };
    let mut cluster = Cluster::start(name, 3, room);
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
    let applied = cluster.converged(&all);
    format!(
        "{name}: every member killed after {phase} answered writes and restarted on its log; {leader} leads in term {term}; {checked} writes read back; all applied index {applied} alike"
    )
}

/// A scenario: it says what it saw.
type Scenario = fn() -> String;

#[allow(
    clippy::disallowed_methods,
    reason = "real processes on the host's clock, threads and environment (CLAUDE.md §1a, end to end)"
)]
fn main() -> ExitCode {
    let mut out = std::io::stdout().lock();
    writeln!(
        out,
        "elections by suspicion, on the members' own detectors (hyper-liveness); a phase is {} writes on this datagram ({} bytes)",
        phase("commits-3", datagram()),
        datagram()
    )
    .unwrap();
    drop(out);
    let scenarios: [(&str, Scenario); 6] = [
        ("commits-3", || commits(3, "commits-3")),
        ("commits-5", || commits(5, "commits-5")),
        ("leader-killed", leader_killed),
        ("follower-restarts", follower_restarts),
        ("partition", partition),
        ("all-killed", all_killed),
    ];
    let filter: Vec<String> = std::env::args()
        .skip(1)
        .filter(|arg| !arg.starts_with('-'))
        .collect();
    let started = Instant::now();
    for (name, scenario) in scenarios {
        if !filter.is_empty() && !filter.iter().any(|wanted| name.contains(wanted.as_str())) {
            continue;
        }
        let at = Instant::now();
        let said = scenario();
        let mut out = std::io::stdout().lock();
        writeln!(out, "ok {said} [{:.1} s]", at.elapsed().as_secs_f64()).unwrap();
    }
    let mut out = std::io::stdout().lock();
    writeln!(out, "all ok in {:.1} s", started.elapsed().as_secs_f64()).unwrap();
    ExitCode::SUCCESS
}
