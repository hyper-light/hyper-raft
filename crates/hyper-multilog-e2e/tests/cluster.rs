//! hyper-multilog in real use. Each scenario starts a group of real processes
//! (`hyper-multilog-node`), each a member of every log of the group on its own UDP socket, each
//! log on its own fsynced file, and drives it as a client would. What is asserted is what a client
//! can observe, as the core's scenarios assert it (`hyper-raft-e2e/tests/cluster.rs`):
//! - every write a member answered is read back, by a linearizable read through whichever member
//!   leads the key's log, after a member is killed and started again on its logs, and after a
//!   member is cut off and let back;
//! - a member cut off answers no read with a value and acknowledges no write;
//! - every member that is up reaches the same state having merged all its logs gave it (the same
//!   store's digest, every log's commit merged), whatever interleaving of the logs it applied in.
//!
//! The writes alternate a keyed write and a global one (a key beginning `*`): every keyed write in
//! a log other than log 0 then waits at a barrier naming the global before it, and every global
//! waits for every other log's barrier, so each scenario takes the layer's whole merge path. A
//! client knows the group's routing (`hyper_multilog::Route::log` of `member::route_of`) and
//! keeps a leader for each log, moved by a member's answer naming the log's leader, and, where a
//! member knows none, to the next member up.
//!
//! The members elect by suspicion on their own failure detectors, one node-pair stream a pair
//! shared by every log; the waits are the core harness's, on facts, by its quiet rule
//! (`hyper_raft_e2e::quiet`). A member's report states log 0's term and leader, and its commit,
//! applied and last indexes summed over its logs (`hyper_multilog_e2e::member`).
//!
//! The scenarios run one after another in this one thread (`harness = false`), one group at a
//! time: at most three member processes at once.
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

use hyper_measure::usage::{self, Usage, UsageError};
use hyper_multilog_e2e::member::{self, route_of};
use hyper_raft::proto::{self, Entry};
use hyper_raft_e2e::{
    quiet::{self, Heard, Progress, Quiet, RTO, Stuck, Watch},
    run,
    stream::{self, Report},
    wire::{self, Control, Kind, Op, Outcome},
};

const NODE: &str = env!("CARGO_BIN_EXE_hyper-multilog-node");
const TMP: &str = env!("CARGO_TARGET_TMPDIR");

#[expect(
    clippy::disallowed_methods,
    reason = "a test removes the files it made in its own target directory"
)]
fn remove(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// The writes in a phase of scenario `name`: one more than one append carries to a member behind,
/// so that a member that missed a phase catches up over more than one append. An append carries
/// entries up to the datagram less a message's fixed bytes (`member::MESSAGE_ROOM`), counted at
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
    let room = datagram.saturating_sub(member::MESSAGE_ROOM) as u64;
    (room / proto::encoded_bytes(&entry).max(1)) as usize + 1
}

/// What a member holds, from what its scenario writes: its logs, the keys, the asks it keeps
/// waiting at once, and the writes and how many of them are global, which bound each log (one
/// entry for each write, and for each term one and a barrier a global, `member::per_term`).
#[derive(Clone, Copy)]
struct Room {
    logs: usize,
    keys: usize,
    pending: usize,
    writes: usize,
    globals: usize,
}

struct Member {
    child: Option<Child>,
    address: SocketAddr,
    wal: PathBuf,
}

struct Cluster {
    name: &'static str,
    members: Vec<Member>,
    room: Room,
    /// Each member's law, the asks each left unanswered, the longest write any reported, the stall
    /// the test ordered, and what the looks have seen.
    quiet: Quiet,
    /// Why the latest wait that gave up did.
    stuck: Option<Stuck>,
    test: UdpSocket,
    /// The most bytes the test's socket sends in one datagram ([`wire::largest`]).
    datagram: usize,
    next_id: u64,
    buffer: Vec<u8>,
    /// Each answered write's and read's nanoseconds, as the client waited for it.
    puts: Vec<u64>,
    gets: Vec<u64>,
    /// The bytes of key and value the answered writes stored.
    stored: u64,
}

impl Drop for Cluster {
    fn drop(&mut self) {
        for member in &mut self.members {
            if let Some(mut child) = member.child.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
            remove_logs(&member.wal, self.room.logs);
        }
    }
}

/// Log `log`'s file of the member whose logs are at `wal` (`hyper-multilog-node`).
fn log_path(wal: &Path, log: usize) -> PathBuf {
    let mut path = wal.as_os_str().to_owned();
    path.push(format!(".{log}"));
    PathBuf::from(path)
}

/// Removes the member's logs and its run record.
fn remove_logs(wal: &Path, logs: usize) {
    for log in 0..logs {
        remove(&log_path(wal, log));
    }
    remove(&run::path(wal));
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
        .args(["--max-globals", &room.globals.to_string()])
        .args(["--logs", &room.logs.to_string()])
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
    #[allow(
        clippy::disallowed_methods,
        reason = "real processes on the host's clock, threads and environment (CLAUDE.md §1a, end to end)"
    )]
    fn start(name: &'static str, voters: usize, room: Room) -> Self {
        let mut members = Vec::new();
        for id in 1..=voters as u64 {
            let wal = PathBuf::from(TMP).join(format!(
                "multilog-e2e-{}-{name}-{id}.wal",
                std::process::id()
            ));
            remove_logs(&wal, room.logs);
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
            quiet: Quiet::new(),
            stuck: None,
            test,
            datagram,
            next_id: 0,
            buffer: Vec::new(),
            puts: Vec::new(),
            gets: Vec::new(),
            stored: 0,
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
            // Waited for by a peek, taken without waiting (`hyper_measure::wait::arrives`). A refusal from a
            // member that is down reads as an error on some platforms: no answer yet, and the
            // wait goes on to its timeout.
            if !hyper_measure::wait::arrives(&self.test, Some(left), &mut received).unwrap() {
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
    #[allow(
        clippy::disallowed_methods,
        reason = "real processes on the host's clock, threads and environment (CLAUDE.md §1a, end to end)"
    )]
    fn report(&mut self, id: u64) -> Option<Report> {
        self.next_id += 1;
        let ask = self.next_id;
        stream::put_report_ask(&mut self.buffer, ask);
        let body = self.exchange(id, ask)?;
        let (_, report) = stream::read_report(&body, self.members.len())?;
        // Every write a member keeps waiting is one an apply will answer.
        assert_eq!(
            report.stray, 0,
            "{}: member {id} keeps writes no apply will answer: {report:?}",
            self.name
        );
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
        Some(report)
    }
    /// Sends an instruction to `id` until it answers, each retransmission timeout, for as long as
    /// its process runs: an instruction is an idempotent datagram, which a loaded machine may
    /// drop or deliver late, and a member just started has no group yet whose progress could
    /// bound the wait. A member whose process ended fails it.
    fn instruct(&mut self, id: u64, control: &Control) {
        self.order(id, |buffer, ask| wire::put_control(buffer, ask, control));
    }
    /// Sends what `put` puts, an instruction answered `Done`, as [`Cluster::instruct`] sends one.
    fn order(&mut self, id: u64, put: impl Fn(&mut Vec<u8>, u64)) {
        self.next_id += 1;
        let ask = self.next_id;
        loop {
            put(&mut self.buffer, ask);
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

    #[allow(
        clippy::disallowed_methods,
        reason = "real processes on the host's clock, threads and environment (CLAUDE.md §1a, end to end)"
    )]
    fn watch(&self) -> Watch {
        self.quiet.watch(Instant::now())
    }
    /// Whether the group is still moving: asks every member up for its report, and judges the look
    /// by `hyper_raft_e2e::quiet`'s rule. The watch is extended by a quiet period whenever any
    /// member's term, commit, applied index, last index or restarts seen has moved since it last
    /// looked, or, while it has a pair no margin judges, the heartbeats it has taken.
    ///
    /// Quiet is time in which the test saw the group and nothing moved. A look that began before
    /// the quiet period ended counts as movement unseen: an ask whose answer was lost spends a
    /// retransmission timeout of the test's own, not of the group's. A member's one thread
    /// answers nothing and moves nothing while it is in a write of its log, so neither is quiet:
    /// a look in which a member up did not answer decides nothing (its answer after counts as
    /// movement: it can act again), and the time the members say they spent in their logs' writes
    /// since this watch last heard them extends the watch by the most any one of them spent. A
    /// device that stalls stalls every member on it at once, and the test hears no one. A member's
    /// silence is excused only as far as the members' own measures go: the longest one write of a
    /// log any member has reported (or the stall the test ordered), and the quiet period. A member
    /// silent past that answers nothing whatever holds it, and the wait fails, naming it.
    #[allow(
        clippy::disallowed_methods,
        reason = "real processes on the host's clock, threads and environment (CLAUDE.md §1a, end to end)"
    )]
    fn moving(&mut self, watch: &mut Watch) -> bool {
        let looked = Instant::now();
        let mut seen = Vec::new();
        let mut heard = Vec::new();
        let mut unheard = Vec::new();
        for id in self.up_members() {
            let Some(report) = self.report(id) else {
                unheard.push(id);
                continue;
            };
            heard.push(Heard {
                id,
                progress: Progress::of(
                    &report.status,
                    report.restarts,
                    report.unjudged,
                    report.taken,
                ),
                blocked_ns: report.blocked_ns,
                // Its one thread makes its writes, and answers only between them.
                writing_ns: 0,
            });
            seen.push(report);
        }
        for id in &unheard {
            self.running(*id);
        }
        let Err(stuck) = self
            .quiet
            .look(watch, looked, Instant::now(), &heard, &unheard)
        else {
            return true;
        };
        // What the group was when it was judged stuck, and what it says a look later, for the
        // failure that follows.
        let up = self.up_members();
        let after = self.reports(&up);
        eprintln!(
            "{}: {stuck}; the last look: {seen:?}; a look after: {after:?}",
            self.name
        );
        self.stuck = Some(stuck);
        false
    }
    /// Fails the test if member `id`'s process has ended: one that did not answer is waited on
    /// only while it runs.
    fn running(&mut self, id: u64) {
        let name = self.name;
        if let Some(child) = self.members[(id - 1) as usize].child.as_mut()
            && let Some(status) = child.try_wait().unwrap()
        {
            panic!("{name}: member {id} ended: {status}");
        }
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
    /// What every member up says its detectors measured and were configured to, a line a pair:
    /// the heartbeats it sent the peer and the slots it skipped, what the configurator was fed (the
    /// arrivals' unseen share, their mean lateness and its deviation), the detector in force (`η`,
    /// `α`, the mistake recurrence it promises, the unavailability it was chosen for), and the
    /// pair's suspicions against the allowance its detectors promised for a peer alive throughout.
    fn detectors(&mut self) -> String {
        let ms = |ns: u64| ns as f64 / 1e6;
        let looks = self.quiet.seen();
        let mut out = format!(
            "\n  looks that did not hear every member: {}, the longest silence {:.1} ms against \
             {:.1} ms excused; waits extended {:.1} ms for members in their logs' writes, at most \
             {:.1} ms at once",
            looks.unheard_looks,
            looks.silence_most.as_secs_f64() * 1e3,
            looks.excused_then.as_secs_f64() * 1e3,
            ms(looks.extended_ns),
            ms(looks.extended_most_ns)
        );
        for id in self.up_members() {
            self.next_id += 1;
            let ask = self.next_id;
            stream::put_account_ask(&mut self.buffer, ask);
            let Some((_, account)) = self
                .exchange(id, ask)
                .and_then(|body| stream::read_account(&body, self.members.len()))
            else {
                out.push_str(&format!("\n  member {id}: no account"));
                continue;
            };
            let (flush_most, turn_most) = self
                .report(id)
                .map_or((0, 0), |r| (r.flush_most_ns, r.turn_most_ns));
            out.push_str(&format!(
                "\n  member {id}: G {:.3} ms, E[flush] {:.3} ms, T_E {:.1} ms; longest flush {:.1} ms, \
                 longest between two reads {:.1} ms",
                ms(account.granularity_ns),
                ms(account.flush_ns),
                ms(account.election_ns),
                ms(flush_most),
                ms(turn_most)
            ));
            for pair in &account.pairs {
                let own = if pair.configured {
                    "own"
                } else if pair.judged {
                    "pool"
                } else {
                    "unjudged"
                };
                out.push_str(&format!(
                    "\n    {id}->{}: {own}, {} configurations, {} sent, {} skipped, {} taken, {} refused unproven; \
                     fed unseen {:.4}, lateness {:.3} ms, sd {:.3} ms; eta {:.3} ms, alpha {:.3} ms, \
                     recurrence {:.1} ms, U {:.2e}; {} suspicions, allowance {:.2}",
                    pair.peer,
                    pair.configurations,
                    pair.sent,
                    pair.skipped,
                    pair.taken,
                    pair.unproven,
                    pair.unseen,
                    ms(pair.lateness_ns),
                    ms(pair.deviation_ns),
                    ms(pair.interval_ns),
                    ms(pair.margin_ns),
                    ms(pair.recurrence_ns),
                    pair.unavailability,
                    pair.suspicions,
                    pair.allowance
                ));
            }
        }
        out
    }
    /// Every member up's account from its operating system (`hyper_measure::usage`); none on an
    /// operating system it does not read.
    fn accounts(&self) -> Option<Vec<Usage>> {
        let mut accounts = Vec::new();
        for child in self
            .members
            .iter()
            .filter_map(|member| member.child.as_ref())
        {
            match usage::of(child.id()) {
                Ok(account) => accounts.push(account),
                Err(UsageError::Unsupported) => return None,
                Err(error) => panic!("the OS refused the member's account: {error}"),
            }
        }
        Some(accounts)
    }
    /// The answered writes' and reads' tails, in milliseconds, each quantile with its 95%
    /// interval (`docs/tails.md` §3.3).
    fn tails(&self) -> String {
        let line = |name: &str, samples: &[u64]| {
            let mut sorted = samples.to_vec();
            sorted.sort_unstable();
            format!(
                "{name} ({} samples): p50 {}, p99 {}, p99.9 {}, max {:.2}",
                sorted.len(),
                cell(&sorted, 0.5),
                cell(&sorted, 0.99),
                cell(&sorted, 0.999),
                sorted.last().copied().unwrap_or(0) as f64 / 1e6
            )
        };
        format!(
            "\n  {}\n  {}",
            line("writes", &self.puts),
            line("reads", &self.gets)
        )
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
        self.quiet.gone(id);
    }
    /// Starts member `id` again on its log, at a port the system gives it, and tells every member
    /// up where it listens now: a port freed by a process killed is the system's to give to
    /// whoever binds next (another member's startup took one in a run, and the restart could not
    /// bind). Then, while the group moves, every other member up that had heard its last run
    /// reports the restart its stream saw (`hyper_liveness::Change::Restarted`). A member that
    /// never took a heartbeat of the last run cannot tell the new one from a first.
    #[allow(
        clippy::disallowed_methods,
        reason = "real processes on the host's clock, threads and environment (CLAUDE.md §1a, end to end)"
    )]
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
        let (child, port) = spawn(
            id,
            self.voters(),
            "127.0.0.1:0",
            &member.wal.clone(),
            self.room,
        );
        let member = &mut self.members[(id - 1) as usize];
        member.child = Some(child);
        member.address = SocketAddr::from(([127, 0, 0, 1], port));
        for up in self.up_members() {
            self.tell_peers(up);
        }
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

/// A client: whom it asks for each log.
struct Client {
    leaders: Vec<u64>,
}

impl Client {
    /// A client asking `leader` for every log at first.
    fn new(cluster: &Cluster, leader: u64) -> Self {
        Self {
            leaders: vec![leader; cluster.room.logs],
        }
    }
    /// The log `key` routes to.
    fn log(cluster: &Cluster, key: &[u8]) -> usize {
        route_of(key).log(cluster.room.logs)
    }
    /// Whom to ask next for `log`: the leader `hint` names, if it is another member up; the next
    /// member up after the one asked otherwise (a member that knows no leader of a log names
    /// none, and only log 0's leader is in a report).
    fn next(&mut self, cluster: &Cluster, log: usize, hint: u64) {
        let asked = self.leaders[log];
        if hint != 0 && hint != asked && cluster.up(hint) {
            self.leaders[log] = hint;
            return;
        }
        let up = cluster.up_members();
        let after = up.iter().copied().find(|id| *id > asked);
        if let Some(next) = after.or_else(|| up.first().copied()) {
            self.leaders[log] = next;
        }
    }
    /// Writes `key`; true once a member answered that it is applied, false once the group stopped
    /// moving without an answer.
    fn put(
        &mut self,
        cluster: &mut Cluster,
        history: &mut History,
        key: &[u8],
        value: &[u8],
    ) -> bool {
        let log = Self::log(cluster, key);
        let mut watch = cluster.watch();
        loop {
            let hint = match cluster.ask(self.leaders[log], &Op::Put { key, value }) {
                Some(Outcome::Put(_)) => {
                    history.unknown.remove(key);
                    history.acked.insert(key.to_vec(), value.to_vec());
                    return true;
                }
                Some(Outcome::NotLeader(hint)) => hint,
                Some(Outcome::Busy) => self.leaders[log],
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
            self.next(cluster, log, hint);
        }
    }
    /// Reads `key` linearizably through its log's leader.
    fn get(&mut self, cluster: &mut Cluster, key: &[u8]) -> Option<Vec<u8>> {
        let log = Self::log(cluster, key);
        let mut watch = cluster.watch();
        loop {
            let hint = match cluster.ask(self.leaders[log], &Op::Get { key }) {
                Some(Outcome::Value(value)) => return value,
                Some(Outcome::NotLeader(hint)) => hint,
                Some(Outcome::Busy) => self.leaders[log],
                Some(other) => panic!("a read answered with {other:?}"),
                None => 0,
            };
            assert!(
                cluster.moving(&mut watch),
                "{}: the group stopped moving with no read of {:?} answered",
                cluster.name,
                String::from_utf8_lossy(key)
            );
            self.next(cluster, log, hint);
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

/// The 1-based ranks of the order statistics bracketing the `q`-quantile of `n` samples with at
/// least 95% coverage (David and Nagaraja, *Order Statistics*): the count below the quantile is
/// Binomial(`n`, `q`). `None` above when the upper rank would pass the largest sample.
fn ranks(n: usize, q: f64) -> (usize, Option<usize>) {
    let tail = 0.025;
    let (lq, lp) = (q.ln(), (1.0 - q).ln());
    let mut ln_pmf = n as f64 * lp;
    let mut cdf = Vec::with_capacity(n + 1);
    let mut sum = 0.0;
    for k in 0..=n {
        sum += ln_pmf.exp();
        cdf.push(f64::min(sum, 1.0));
        if k < n {
            ln_pmf += ((n - k) as f64).ln() - ((k + 1) as f64).ln() + lq - lp;
        }
    }
    let below = |rank: usize| if rank == 0 { 0.0 } else { cdf[rank - 1] };
    let mut lower = 1;
    while lower < n && below(lower + 1) <= tail {
        lower += 1;
    }
    (lower, (lower..=n).find(|rank| below(*rank) >= 1.0 - tail))
}

/// `q`'s estimate and interval among `sorted` nanoseconds, in milliseconds.
fn cell(sorted: &[u64], q: f64) -> String {
    let n = sorted.len();
    if n == 0 {
        return "none".into();
    }
    let at = ((n as f64 * q).ceil() as usize).clamp(1, n);
    let (lower, upper) = ranks(n, q);
    let ms = |ns: u64| ns as f64 / 1e6;
    match upper {
        Some(upper) => format!(
            "{:.2} [{:.2}–{:.2}]",
            ms(sorted[at - 1]),
            ms(sorted[lower - 1]),
            ms(sorted[upper - 1])
        ),
        None => format!(
            "{:.2} [{:.2}–unresolved]",
            ms(sorted[at - 1]),
            ms(sorted[lower - 1])
        ),
    }
}

/// The samples a p99.9 needs for its 95% interval to close above (`docs/tails.md` §3.3): the
/// least `n` with `0.999^n <= 0.025`, so that the upper order statistic lies within the samples.
const SAMPLES_P999: usize = 3_688;

/// The window an idle group's account is read over: a wakeup a second is resolved to a tenth of
/// one, the resolution its report states. A measured interval, not a wait for a fact.
const IDLE: Duration = Duration::from_secs(10);

/// A group of three over `logs` logs takes the writes a p99.9 needs, closed loop as one client
/// makes them (a write and its read before the next, `docs/tails.md` §3.1 labels it), on the
/// machine as it is loaded; the tails of what the client waited, and the members' accounts per
/// write: CPU, instructions, cycles, energy, device bytes written per byte stored. Then the
/// group sits idle for [`IDLE`]: its wakeups, CPU and energy a second, and its footprint.
#[allow(
    clippy::disallowed_methods,
    reason = "real processes on the host's clock, threads and environment (CLAUDE.md §1a, end to end)"
)]
fn tails(logs: usize, name: &'static str) -> String {
    let writes = SAMPLES_P999;
    let mut cluster = Cluster::start(name, 3, room(logs, writes));
    let all = cluster.up_members();
    let (leader, _) = cluster.leader_among(&all);
    let mut client = Client::new(&cluster, leader);
    let mut history = History::default();
    let before = cluster.accounts();
    let started = Instant::now();
    write_range(&mut cluster, &mut client, &mut history, 0..writes);
    let elapsed = started.elapsed();
    let after = cluster.accounts();
    let applied = cluster.converged(&all);
    let tails = cluster.tails();
    let head = format!(
        "{name}: 3 members, {logs} logs, {writes} writes ({} global) in {:.1} s; all merged {applied} entries alike{tails}",
        globals(writes),
        elapsed.as_secs_f64(),
    );
    let (Some(before), Some(after)) = (before, after) else {
        return format!("{head}\n  the members' accounts: unmeasured on this operating system");
    };
    let summed = |after: &[Usage], before: &[Usage]| {
        after
            .iter()
            .zip(before)
            .map(|(after, before)| after.since(before))
            .fold(Usage::default(), |sum, member| sum.plus(&member))
    };
    let spent = summed(&after, &before);
    let idle_from = cluster.accounts().expect("accounts read a moment ago");
    std::thread::sleep(IDLE);
    let idle = summed(
        &cluster.accounts().expect("accounts read a moment ago"),
        &idle_from,
    );
    let per = |total: Option<u64>, by: f64| {
        total.map_or("unmeasured".to_string(), |t| {
            format!("{:.3}", t as f64 / by)
        })
    };
    let seconds = IDLE.as_secs_f64();
    let members = all.len() as f64;
    format!(
        "{head}\n  per write, the three members together: CPU {:.0} us (user {:.0}, system {:.0}), instructions {}, cycles {}, energy {} nJ, device bytes written {} per byte stored; the members' peak footprint {:.1} MiB\n  idle {} s, per member: {} wakeups a second, CPU {:.3} ms a second, energy {} mJ a second, footprint {:.1} MiB",
        spent.cpu_ns() as f64 / writes as f64 / 1e3,
        spent.user_ns as f64 / writes as f64 / 1e3,
        spent.system_ns as f64 / writes as f64 / 1e3,
        per(spent.instructions, writes as f64),
        per(spent.cycles, writes as f64),
        per(spent.energy_nj, writes as f64),
        per(spent.disk_written, cluster.stored.max(1) as f64),
        after
            .iter()
            .filter_map(|u| u.peak_footprint)
            .max()
            .unwrap_or(0) as f64
            / (1u64 << 20) as f64,
        IDLE.as_secs(),
        per(idle.wakeups, seconds * members),
        idle.cpu_ns() as f64 / seconds / members / 1e6,
        per(idle.energy_nj, seconds * members * 1e6),
        idle.footprint.unwrap_or(0) as f64 / members / (1u64 << 20) as f64,
    )
}

/// The key of write `at` of a scenario: odd writes are global.
fn key(name: &str, at: usize) -> String {
    if at % 2 == 1 {
        format!("*{name}-{at}")
    } else {
        format!("{name}-{at}")
    }
}

/// The global writes among writes `0..writes`.
fn globals(writes: usize) -> usize {
    writes / 2
}

/// Writes and reads back writes `writes`; says the slowest write and read.
#[allow(
    clippy::disallowed_methods,
    reason = "real processes on the host's clock, threads and environment (CLAUDE.md §1a, end to end)"
)]
fn write_range(
    cluster: &mut Cluster,
    client: &mut Client,
    history: &mut History,
    writes: std::ops::Range<usize>,
) -> Duration {
    let mut slowest = Duration::ZERO;
    for at in writes {
        let started = Instant::now();
        let key = key(cluster.name, at);
        let value = format!("value-{at}");
        assert!(
            client.put(cluster, history, key.as_bytes(), value.as_bytes()),
            "{}: the group stopped moving with a write of {key} unanswered",
            cluster.name
        );
        let written = Instant::now();
        cluster
            .puts
            .push(written.duration_since(started).as_nanos() as u64);
        cluster.stored += (key.len() + value.len()) as u64;
        // Read what was just answered: linearizability asks that it be there at once.
        let read = client.get(cluster, key.as_bytes());
        cluster.gets.push(written.elapsed().as_nanos() as u64);
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

/// The room of a scenario of `writes` writes over `logs` logs.
fn room(logs: usize, writes: usize) -> Room {
    Room {
        logs,
        keys: writes,
        pending: 1,
        writes,
        globals: globals(writes),
    }
}

/// A group of three over `logs` logs commits a phase of writes, and every member reaches the same
/// state. One log is the layer's one code path at `n = 1`.
#[allow(
    clippy::disallowed_methods,
    reason = "real processes on the host's clock, threads and environment (CLAUDE.md §1a, end to end)"
)]
fn commits(logs: usize, name: &'static str) -> String {
    let writes = phase(name, datagram());
    let mut cluster = Cluster::start(name, 3, room(logs, writes));
    let all = cluster.up_members();
    let (leader, _) = cluster.leader_among(&all);
    let mut client = Client::new(&cluster, leader);
    let mut history = History::default();
    let started = Instant::now();
    let slowest = write_range(&mut cluster, &mut client, &mut history, 0..writes);
    let elapsed = started.elapsed();
    let checked = verify(&mut cluster, &mut client, &history);
    let applied = cluster.converged(&all);
    let detectors = cluster.detectors() + &cluster.tails();
    format!(
        "{name}: 3 members, {logs} logs; {checked} writes ({} global) answered and read back ({:.2} ms per write and read, {:.2} ms the slowest); all merged {applied} entries alike{detectors}",
        globals(writes),
        elapsed.as_secs_f64() * 1e3 / writes as f64,
        slowest.as_secs_f64() * 1e3,
    )
}

/// Log 0's leader is killed; the others elect in every log it led and go on, and nothing
/// answered is lost. It comes back on its logs, its restart is reported, and it merges the same
/// history.
fn member_killed() -> String {
    let name = "member-killed";
    let logs = 3;
    let phase = phase(name, datagram());
    // A phase with the member, and a phase without it.
    let mut cluster = Cluster::start(name, 3, room(logs, 2 * phase));
    let all = cluster.up_members();
    let (leader, _) = cluster.leader_among(&all);
    let mut client = Client::new(&cluster, leader);
    let mut history = History::default();
    write_range(&mut cluster, &mut client, &mut history, 0..phase);
    // The leader now, which the writes may have moved.
    let (old, old_term) = cluster.leader_among(&all);
    cluster.kill(old);
    let rest: Vec<u64> = cluster.up_members();
    let (new, new_term) = cluster.leader_among(&rest);
    assert!(new_term > old_term);
    write_range(&mut cluster, &mut client, &mut history, phase..2 * phase);
    let commit = cluster
        .reports(&rest)
        .values()
        .map(|r| r.status.commit)
        .max()
        .unwrap();
    cluster.restart(old);
    let applied = cluster.converged(&all);
    assert!(applied >= commit);
    let checked = verify(&mut cluster, &mut client, &history);
    let detectors = cluster.detectors() + &cluster.tails();
    format!(
        "{name}: {logs} logs; log 0's leader {old} killed after {phase} writes, {new} elected in term {new_term}, {phase} written without it; restarted on its logs, its restart reported, all merged {applied} entries alike; {checked} writes read back{detectors}"
    )
}

/// Log 0's leader is cut off by a drop filter in its own process, its heartbeats with its Raft
/// messages of every log. It answers no read with a value and acknowledges no write; the others
/// elect and go on; once the filter is lifted it follows and merges the same history.
fn partition() -> String {
    let name = "partition";
    let logs = 3;
    let phase = phase(name, datagram());
    // A phase before the cut, the moved key twice (to the member cut off, and after), and a phase
    // after; the moved key is global, in log 0.
    let writes = 2 * phase + 2;
    let mut cluster = Cluster::start(
        name,
        3,
        Room {
            globals: globals(2 * phase) + 2,
            ..room(logs, writes)
        },
    );
    let all = cluster.up_members();
    let (first, _) = cluster.leader_among(&all);
    let mut client = Client::new(&cluster, first);
    let mut history = History::default();
    write_range(&mut cluster, &mut client, &mut history, 0..phase);
    let key = format!("*{name}-moved");
    let (old, old_term) = cluster.leader_among(&all);
    cluster.isolate(old, true);
    // At once, while it still believes it leads log 0: it can confirm nothing with a quorum.
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
    client.leaders = vec![new; logs];
    assert!(client.put(&mut cluster, &mut history, key.as_bytes(), b"after"));
    // The write the cut-off member took was never committed: the group's value is "after".
    history.unknown.remove(key.as_bytes());
    write_range(&mut cluster, &mut client, &mut history, phase..2 * phase);
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
    let checked = verify(&mut cluster, &mut client, &history);
    let detectors = cluster.detectors() + &cluster.tails();
    format!(
        "{name}: {logs} logs; log 0's leader {old} cut off; it answered a read with {early_read:?} and a write with {early_write:?}, and later a read of the replaced key with {stale:?}; {new} elected in term {new_term}; after the filter lifted all merged {applied} entries alike; {checked} writes read back{detectors}"
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
        ("commits-1", || commits(1, "commits-1")),
        ("commits-3", || commits(3, "commits-3")),
        ("member-killed", member_killed),
        ("partition", partition),
        ("tails-1", || tails(1, "tails-1")),
        ("tails-3", || tails(3, "tails-3")),
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
