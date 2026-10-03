//! One member as a process: a hyper-durable `Replica` over hyper-log on a real file, a UDP socket,
//! the node-pair liveness stream (`hyper_liveness`), and the key-value store of [`crate::machine`].
//! One thread does the member's work: it waits on its socket until the replica's deadline or the
//! stream's wake, whichever is first, takes what arrived, polls the stream, and drives the replica,
//! which submits its writes with the member's waker and returns. The log's answer wakes the waker,
//! which a relay thread turns into a datagram to the member's own socket, so the one wait the
//! member makes covers both. The log runs its own two threads. A process runs these four and the
//! one that watches for its test to go, whatever it holds.
//!
//! The replica elects by suspicion (timing step L-2, `docs/timing.md` §2.9) and takes no ticks.
//! What its detectors believe of its peers is the node-pair stream's word (L-3, §2.8), wired as
//! hyper-durable's `Owner` wires it for an owner of one replica: the pairs attached from the
//! replica's configuration, each change taken to the replica (`suspect`, `trust`, `restarted`),
//! the group's timing derived by hyper-timing's law over what the stream measured (its echoed round
//! trips, its granularity, the mean flush: `Replica::measure`) and each pair charged the group's
//! expected election, and every durable write of the replica handed to the stream as its flush
//! proof. A heartbeat leaves only once the member's log made a write durable after the previous was
//! due: where the group wrote none, the member makes one on the same log, an empty update of a group
//! of the stream's own ([`LIVENESS_GROUP`]), so a disk that stops stops the heartbeats with it.
//! Heartbeats travel as hyper-raft-e2e's members' do ([`stream::put_heartbeat`]), stamped when the
//! member reads them, as hyper-tokio stamps a datagram where the kernel cannot (`docs/timing.md`
//! §3, item 5): the read delay counts as the sender's.
//!
//! A write is answered once it is applied, so an answered write is committed; a read once a
//! quorum confirmed the leader and the member applied through the index it was confirmed at.
//! Where the test armed a point (`control::Point`), the member stops there, prints `stopped
//! <point>`, and waits to be killed.
//!
//! The member reports what the test's waits read of it (`hyper_raft_e2e::quiet`): the time it has
//! had a write of its log out, how long its oldest write still out has been, the longest one write
//! took, and the longest it went between two reads of its socket. Its thread does no write of its
//! own, but its group moves through it only as its writes become durable, and a device a machine's
//! processes share holds them all at once.
use std::collections::{BTreeMap, VecDeque};
use std::io::{ErrorKind, Write as _};
use std::net::{SocketAddr, UdpSocket};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::Waker;
use std::time::{Duration, Instant};

use hyper_liveness::{
    Change, Liveness, Output as LiveOutput, PeerId, Settings as LiveSettings, Write as LiveWrite,
};
use hyper_log::{Class, Pending, Update};
use hyper_timing::{Exposure, Trust};

use hyper_durable::{
    Cause, GroupStore, Output, Replica, ReplicaError, Settings as Shell, Unbounded,
};
use hyper_log::{Config as LogConfig, Log, Waits};
use hyper_raft::proto::{ConfChangeSingle, ConfChangeTransition, ConfChangeV2, ConfState, Message};
use hyper_raft::wire::Record;
use hyper_raft::{Config, StateRole};
use hyper_raft_e2e::run::RunError;
use hyper_raft_e2e::stream;
use hyper_raft_e2e::wire::{self, Command, Control, Kind, Op, Outcome, Status};

use crate::control::{self, Order, Point, Report};
use crate::file::{self, FaultFile};
use crate::machine::{Applied, Kv};

/// The group every member's log holds: one group a log.
pub const GROUP: u128 = 1;
/// The group the node-pair stream's own writes go to, on the same log and device as the replica's,
/// where no write of the replica's came in time to prove a heartbeat: a group of no records, so
/// its writes hold nothing a reopened member reads.
pub const LIVENESS_GROUP: u128 = 2;
/// The log's id.
const LOG_ID: u128 = 0x0068_7970_6572_2d64_7572_6162_6c65;

/// The bytes of a member's datagram besides an append's entries: the datagram's header and the
/// sender's id, and the message record's header, fixed fields and checksum (hyper-raft-e2e's).
const MESSAGE_ROOM: usize = wire::HEADER
    + 8
    + hyper_raft::wire::HEADER_BYTES
    + hyper_raft::wire::MESSAGE_FIXED_BYTES
    + hyper_raft::wire::CHECKSUM_BYTES;

/// The log's settings: segments of 64 blocks, sixty-four of them (16 MiB), the replica's group and
/// the liveness stream's ([`LIVENESS_GROUP`]), and the writer's measured waits (`Waits::Measured`, a
/// node's).
pub fn log_config() -> LogConfig {
    LogConfig {
        segment_bytes: 64 * 4096,
        max_segments: 64,
        max_groups: 2,
        group_entries: 1 << 16,
        group_bytes: 1 << 24,
        group_cache: 1 << 20,
        queue_submissions: 16,
        waits: Waits::Measured,
    }
}

/// Why the member stopped.
#[derive(Debug)]
pub enum NodeError {
    /// The replica was fenced: a write failed or the replica's state no longer adds up.
    Fenced(Cause),
    /// The log refused.
    Log(hyper_log::LogError),
    /// The replica would not open.
    Open(hyper_durable::OpenError),
    /// The socket refused.
    Io(std::io::Error),
    /// The liveness stream refused: a peer it cannot keep.
    Liveness(hyper_liveness::Refusal),
    /// The member has no run: its record could not be read or raised (`hyper_raft_e2e::run`).
    Run(RunError),
}

impl std::fmt::Display for NodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Fenced(cause) => write!(f, "fenced: {cause}"),
            Self::Log(e) => write!(f, "the log: {e}"),
            Self::Open(e) => write!(f, "open: {e}"),
            Self::Io(e) => write!(f, "the socket: {e}"),
            Self::Liveness(e) => write!(f, "the liveness stream: {e}"),
            Self::Run(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for NodeError {}

impl From<std::io::Error> for NodeError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// How a member runs, as its command line says.
#[derive(Clone, Debug)]
pub struct Settings {
    /// This member.
    pub id: u64,
    /// The voters a new group is founded with.
    pub voters: Vec<u64>,
    /// The most keys the store holds.
    pub max_keys: usize,
    /// The most writes and reads one member waits to answer.
    pub max_pending: usize,
}

#[derive(Clone, Copy, Debug)]
struct Asker {
    address: SocketAddr,
    id: u64,
}

type Member = Replica<GroupStore<FaultFile>, Kv, Unbounded>;

/// A member, its socket, and who waits on it.
pub struct Node {
    // Dropped before the log, whose owner its handle reaches.
    replica: Member,
    log: Log<FaultFile>,
    socket: UdpSocket,
    waker: Waker,
    settings: Settings,
    peers: Vec<(u64, SocketAddr)>,
    isolated: bool,
    armed: Option<(Point, u64)>,
    woken: bool,
    writes: BTreeMap<u64, Asker>,
    reads: BTreeMap<u64, (Asker, Vec<u8>)>,
    next_sequence: u64,
    leading: bool,
    out: Output<Applied>,
    received: Vec<u8>,
    sending: Vec<u8>,
    datagram: usize,
    command: Vec<u8>,
    /// The member's clock's origin: its times are nanoseconds since.
    epoch: Instant,
    /// The node-pair liveness stream.
    liveness: Liveness,
    /// The peers the stream was told the group shares, in order.
    attached: Vec<PeerId>,
    /// What the stream asked of the member during one call.
    asked: Asked,
    /// The stream's write out, and when it was submitted.
    liveness_write: Option<(Pending, u64)>,
    /// A write of the stream's failed: the log is fenced, and no heartbeat is proved again.
    liveness_failed: bool,
    /// The restarts of its peers the stream reported.
    restarts: u64,
    /// Since when the member has had a write of its log out, the replica's or the stream's, while
    /// it has one.
    writing_since: Option<u64>,
    /// When each of the replica's writes still out was submitted, oldest first: they become
    /// durable in the order made (`hyper_durable::Replica`), so the first is the oldest. At most
    /// the replica's writes out.
    outs: VecDeque<u64>,
    /// The time it has had a write of its log out, all told, the one out not counted: time its
    /// group's progress through it waited on its device, whatever its thread did meanwhile.
    blocked: u64,
    /// The longest one write of its log took, from its submission to the answer taken.
    flush_most: u64,
    /// The longest it went between two reads of its socket: the longest it could not answer.
    turn_most: u64,
}

/// What the liveness stream asks of the member: heartbeats to send, a write to make, changes.
#[derive(Default)]
struct Asked {
    heartbeats: Vec<(PeerId, Vec<u8>)>,
    flush: bool,
    changes: Vec<Change>,
}

impl LiveOutput for Asked {
    fn heartbeat(&mut self, peer: PeerId, message: &[u8]) {
        self.heartbeats.push((peer, message.to_vec()));
    }
    fn flush(&mut self) {
        self.flush = true;
    }
    fn change(&mut self, change: Change) {
        self.changes.push(change);
    }
}

/// Opens the log at `path`, or creates it when the file is new.
pub fn open_log(path: &Path) -> Result<Log<FaultFile>, NodeError> {
    let fresh = !path.exists();
    let file =
        FaultFile::open(path, fresh).map_err(|e| NodeError::Log(hyper_log::LogError::Disk(e)))?;
    if fresh {
        Log::create(file, log_config(), LOG_ID).map_err(NodeError::Log)
    } else {
        Log::open(file, log_config(), LOG_ID)
            .map(|(log, _)| log)
            .map_err(NodeError::Log)
    }
}

impl Node {
    /// The member `settings` names on `socket`, opened on `log`, woken by `waker`, in its run
    /// `run` (`hyper_raft_e2e::run::raise`).
    #[allow(
        clippy::disallowed_methods,
        reason = "real processes on the host's clock, threads and environment (CLAUDE.md §1a, end to end)"
    )]
    pub fn open(
        settings: Settings,
        run: u64,
        socket: UdpSocket,
        log: Log<FaultFile>,
        waker: Waker,
    ) -> Result<Self, NodeError> {
        let datagram = wire::largest(&socket)?;
        let max_size_per_msg =
            u64::try_from(datagram.saturating_sub(MESSAGE_ROOM)).unwrap_or(u64::MAX);
        let shell = Shell {
            core: Config {
                max_size_per_msg,
                check_quorum: true,
                pre_vote: true,
                seed: settings.id,
                ..Config::new(settings.id)
            },
            // An owner woken by events has no period: the commit is written alone at the first
            // moment no write is out, the soonest a member that stops can reopen with what it
            // applied, at one write a lull (`docs/durable.md` §4.1).
            quiet: Duration::ZERO,
        };
        let store = GroupStore::claim(&log, GROUP).map_err(|e| match e {
            hyper_durable::ClaimError::Log(e) => NodeError::Log(e),
            hyper_durable::ClaimError::Damaged => {
                NodeError::Log(hyper_log::LogError::Damaged("the group"))
            }
        })?;
        let configuration = ConfState {
            voters: settings.voters.clone(),
            ..ConfState::default()
        };
        let machine = Kv::new(configuration, settings.max_keys);
        let replica = Replica::open(&shell, store, machine, Unbounded).map_err(NodeError::Open)?;
        let liveness = Liveness::new(LiveSettings {
            local: settings.id,
            run,
            max_peers: hyper_raft::MAX_MEMBERS,
            history: Exposure::new(),
        })
        .map_err(NodeError::Liveness)?;
        Ok(Self {
            replica,
            log,
            socket,
            waker,
            peers: Vec::new(),
            isolated: false,
            armed: None,
            woken: false,
            writes: BTreeMap::new(),
            reads: BTreeMap::new(),
            next_sequence: 0,
            leading: false,
            out: Output::default(),
            received: vec![0; wire::MAX_DATAGRAM],
            sending: Vec::with_capacity(datagram),
            datagram,
            command: Vec::new(),
            epoch: Instant::now(),
            liveness,
            attached: Vec::new(),
            asked: Asked::default(),
            liveness_write: None,
            liveness_failed: false,
            restarts: 0,
            writing_since: None,
            outs: VecDeque::new(),
            blocked: 0,
            flush_most: 0,
            turn_most: 0,
            settings,
        })
    }

    /// Notes whether a write of the member's log is out, the replica's or the stream's: the time
    /// one is, all told, is the time the member reports spending in its writes. Read at each of
    /// its turns, as they go.
    fn writing(&mut self) {
        let out = self.replica.in_flight() > 0 || self.liveness_write.is_some();
        let now = self.now();
        match (out, self.writing_since) {
            (true, None) => self.writing_since = Some(now),
            (false, Some(since)) => {
                self.blocked = self.blocked.saturating_add(now.saturating_sub(since));
                self.writing_since = None;
            }
            _ => {}
        }
    }

    /// The time the member has had a write of its log out, all told, the one out counted to now.
    fn blocked_now(&self) -> u64 {
        let out = self
            .writing_since
            .map_or(0, |since| self.now().saturating_sub(since));
        self.blocked.saturating_add(out)
    }

    /// How long the member's oldest write still out, the replica's or the stream's, has been out;
    /// zero when none is.
    fn writing_now(&self) -> u64 {
        let replica = self.outs.front().copied();
        let stream = self.liveness_write.as_ref().map(|(_, started)| *started);
        [replica, stream]
            .into_iter()
            .flatten()
            .min()
            .map_or(0, |oldest| self.now().saturating_sub(oldest))
    }

    /// The writes the replica has made, all told: each is a write out until the log answers it.
    fn made(&self) -> u64 {
        let w = self.replica.writes();
        [w.readies, w.empty, w.fenced, w.quiet, w.starts]
            .into_iter()
            .fold(0u64, u64::saturating_add)
    }

    /// Takes a drive at `now` into the writes out: the writes it made, submitted at `now`, and those
    /// the log answered, the oldest, each one write's time from its submission to its answer taken.
    /// `made_before` and `out_before` are what the replica had made and had out before the drive.
    /// The oldest writes out whose submission the member did not see (made as the replica opened)
    /// are answered first, and their time is not known.
    fn track(&mut self, now: u64, made_before: u64, out_before: usize) {
        let made = usize::try_from(self.made().saturating_sub(made_before)).unwrap_or(0);
        let unseen = out_before.saturating_sub(self.outs.len());
        let answered = out_before
            .saturating_add(made)
            .saturating_sub(self.replica.in_flight());
        self.outs.extend(std::iter::repeat_n(now, made));
        for _ in 0..answered.saturating_sub(unseen) {
            let Some(submitted) = self.outs.pop_front() else {
                break;
            };
            self.flush_most = self.flush_most.max(now.saturating_sub(submitted));
        }
    }

    /// Nanoseconds on the member's clock, now.
    #[allow(
        clippy::disallowed_methods,
        reason = "real processes on the host's clock, threads and environment (CLAUDE.md §1a, end to end)"
    )]
    fn now(&self) -> u64 {
        u64::try_from(
            Instant::now()
                .saturating_duration_since(self.epoch)
                .as_nanos(),
        )
        .unwrap_or(u64::MAX)
    }

    /// The group's timing from what the stream measured, given to the replica when it moved, and
    /// each pair charged the group's expected election (hyper-durable's `Owner::measure` for one
    /// replica): none before a quorum's paths and the granularity are measured.
    fn measure(&mut self) -> Result<(), NodeError> {
        let Some(span) = heard(self.replica.measure(&self.liveness))?.flatten() else {
            return Ok(());
        };
        for peer in &self.attached {
            // Every attached peer has its pair.
            let _ = self.liveness.set_election(*peer, span.election);
        }
        Ok(())
    }

    /// Keeps the stream told which peers the group has: its configuration's other members,
    /// attached as they join and detached as they leave (hyper-durable's `Owner::pairs`).
    fn pairs(&mut self) -> Result<(), NodeError> {
        let mut now: Vec<PeerId> = self.replica.peers().collect();
        now.sort_unstable();
        now.dedup();
        for peer in &now {
            if self.attached.binary_search(peer).is_err() {
                self.liveness.attach(*peer).map_err(NodeError::Liveness)?;
                // What the stream believes of it now (hyper-durable's `Owner::pairs`).
                let suspected = self.liveness.trust(*peer) == Some(Trust::Suspected);
                heard(if suspected {
                    self.replica.suspect(*peer)
                } else {
                    self.replica.trust(*peer)
                })?;
            }
        }
        for peer in &self.attached {
            if now.binary_search(peer).is_err() {
                self.liveness.detach(*peer).map_err(NodeError::Liveness)?;
            }
        }
        self.attached = now;
        Ok(())
    }

    /// Polls the stream at the member's clock: what its write made durable first, then what is
    /// due; sends the heartbeats it gives, makes the write it asks for, and takes its changes to
    /// the replica.
    fn live(&mut self) -> Result<(), NodeError> {
        if let Some((pending, started)) = &self.liveness_write
            && let Some(answer) = pending.poll()
        {
            let started = *started;
            self.liveness_write = None;
            match answer {
                Ok(()) => {
                    let now = self.now();
                    self.flush_most = self.flush_most.max(now.saturating_sub(started));
                    self.liveness.on_durable(LiveWrite::Liveness, started, now);
                }
                // The device failed: the log is fenced, and the replica fences at its next
                // write. No heartbeat is proved again, so its peers suspect it.
                Err(_) => self.liveness_failed = true,
            }
        }
        let now = self.now();
        self.liveness.poll(now, &mut self.asked);
        self.act_on_liveness()?;
        self.writing();
        Ok(())
    }

    /// Carries out what the stream asked during the last call into it.
    fn act_on_liveness(&mut self) -> Result<(), NodeError> {
        if std::mem::take(&mut self.asked.flush)
            && self.liveness_write.is_none()
            && !self.liveness_failed
        {
            let started = self.now();
            match self.log.submit_waking(
                LIVENESS_GROUP,
                Class::Latency,
                Update::default(),
                self.waker.clone(),
            ) {
                Ok(pending) => self.liveness_write = Some((pending, started)),
                Err(_) => self.liveness_failed = true,
            }
        }
        let heartbeats = std::mem::take(&mut self.asked.heartbeats);
        for (peer, message) in &heartbeats {
            if self.isolated {
                break;
            }
            let Some(address) = self
                .peers
                .iter()
                .find(|(id, _)| id == peer)
                .map(|(_, address)| *address)
            else {
                // A peer whose address the member was not told: the heartbeat is lost to it.
                continue;
            };
            stream::put_heartbeat(&mut self.sending, self.settings.id, message);
            if wire::seal(&mut self.sending, self.datagram) {
                self.send(address)?;
            }
        }
        self.asked.heartbeats = heartbeats;
        self.asked.heartbeats.clear();
        let changes = std::mem::take(&mut self.asked.changes);
        for change in &changes {
            let peer = change.peer();
            match change {
                Change::Suspected(_) => heard(self.replica.suspect(peer))?,
                Change::Trusted { .. } => heard(self.replica.trust(peer))?,
                Change::Restarted { .. } => {
                    self.restarts = self.restarts.saturating_add(1);
                    heard(self.replica.restarted(peer))?
                }
            };
        }
        self.asked.changes = changes;
        self.asked.changes.clear();
        Ok(())
    }

    /// The log, which outlives the replica's handle on it.
    pub fn log(&self) -> &Log<FaultFile> {
        &self.log
    }

    /// Runs until `stop` is set, which it reads once a turn (the test is gone), or until the
    /// member stops at an armed point (`Ok(Some(point))`), or fails.
    pub fn run(&mut self, stop: &AtomicBool) -> Result<Option<Point>, NodeError> {
        // The member drives once before it waits: a reopened member replays its log alone.
        self.pairs()?;
        if let Some(point) = self.drive()? {
            return Ok(Some(point));
        }
        let mut read = self.now();
        while !stop.load(Ordering::Acquire) {
            self.live()?;
            self.turn_most = self.turn_most.max(self.now().saturating_sub(read));
            // Woken at the replica's deadline or the stream's, whichever is first; by a datagram
            // otherwise, the test's going among them.
            let until = [self.replica.deadline(), self.liveness.wake()]
                .into_iter()
                .flatten()
                .min();
            self.receive_until(until)?;
            read = self.now();
            self.live()?;
            if std::mem::take(&mut self.woken) && self.stops_at(Point::Durable) {
                return Ok(Some(Point::Durable));
            }
            self.measure()?;
            if let Some(point) = self.drive()? {
                return Ok(Some(point));
            }
        }
        Ok(None)
    }

    /// Whether the member passes an armed `point` for the last time it was armed for.
    fn stops_at(&mut self, point: Point) -> bool {
        match &mut self.armed {
            Some((armed, count)) if *armed == point => {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    self.armed = None;
                    return true;
                }
                false
            }
            _ => false,
        }
    }

    /// Waits for a datagram until `until` on the member's clock, or for one however long when
    /// nothing is due, then takes it and what else has arrived, at most a turn's worth
    /// (hyper-raft-e2e's `receive_until`). The wait is a peek, and every datagram is taken without
    /// waiting: a receive that waits can lose what arrives as it times out (`wire::arrives`).
    fn receive_until(&mut self, until: Option<u64>) -> Result<(), NodeError> {
        let turn = self
            .settings
            .max_pending
            .saturating_add(self.settings.voters.len())
            .saturating_add(1);
        let mut most = turn;
        let wait = until.map(|at| Duration::from_nanos(at.saturating_sub(self.now())));
        if wait.is_none_or(|wait| !wait.is_zero()) {
            most = turn.saturating_add(1);
            if !wire::arrives(&self.socket, wait, &mut self.received)? {
                return Ok(());
            }
        }
        self.socket.set_nonblocking(true)?;
        let mut outcome = Ok(());
        for _ in 0..most {
            match self.receive_one() {
                Ok(true) => {}
                Ok(false) => break,
                Err(error) => {
                    outcome = Err(error);
                    break;
                }
            }
        }
        self.socket.set_nonblocking(false)?;
        outcome
    }

    fn receive_one(&mut self) -> Result<bool, NodeError> {
        let (length, from) = match self.socket.recv_from(&mut self.received) {
            Ok(received) => received,
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                return Ok(false);
            }
            Err(e) if e.kind() == ErrorKind::ConnectionReset => return Ok(true),
            Err(e) => return Err(e.into()),
        };
        let datagram = std::mem::take(&mut self.received);
        let outcome = match datagram.get(..length).and_then(wire::open) {
            Some((Kind::Raft, body)) => self.hear_peer(body),
            Some((Kind::Request, body)) => self.hear_client(body, from),
            Some((Kind::Control, body)) => self.hear_test(body, from),
            Some((Kind::Response, _)) | None => Ok(()),
        };
        self.received = datagram;
        outcome.map(|()| true)
    }

    fn hear_peer(&mut self, body: &[u8]) -> Result<(), NodeError> {
        if self.isolated {
            return Ok(());
        }
        let mut reader = wire::Reader::new(body);
        let Some(from) = reader.u64() else {
            return Ok(());
        };
        let Ok(message) = Message::decode(reader.rest()) else {
            return Ok(());
        };
        if message.from != from || message.to != self.settings.id {
            return Ok(());
        }
        heard(self.replica.step(message)).map(drop)
    }

    fn send(&self, to: SocketAddr) -> Result<(), NodeError> {
        match self.socket.send_to(&self.sending, to) {
            Ok(_) => Ok(()),
            Err(e)
                if matches!(
                    e.kind(),
                    ErrorKind::ConnectionRefused
                        | ErrorKind::ConnectionReset
                        | ErrorKind::WouldBlock
                ) =>
            {
                Ok(())
            }
            Err(e) => Err(e.into()),
        }
    }

    fn respond(&mut self, to: SocketAddr, id: u64, outcome: &Outcome) -> Result<(), NodeError> {
        wire::put_response(&mut self.sending, id, outcome);
        if wire::seal(&mut self.sending, self.datagram) {
            self.send(to)?;
        }
        Ok(())
    }

    fn hear_client(&mut self, body: &[u8], from: SocketAddr) -> Result<(), NodeError> {
        let Some((id, op)) = wire::read_request(body) else {
            return Ok(());
        };
        let asker = Asker { address: from, id };
        match op {
            Op::Status => {
                let status = self.status();
                self.respond(from, id, &Outcome::Status(status))
            }
            Op::Put { key, value } => self.write(asker, key, value),
            Op::Get { key } => self.read(asker, key),
        }
    }

    fn status(&self) -> Status {
        let raft = &self.replica.core().raft;
        Status {
            id: self.settings.id,
            term: raft.term(),
            leads: raft.state() == StateRole::Leader,
            leader: raft.leader_id(),
            commit: raft.log().committed(),
            applied: self.replica.applied().index,
            last_index: raft.log().last_index().unwrap_or(0),
            digest: self.replica.machine().digest(),
        }
    }

    fn admits(&mut self, asker: Asker, waiting: usize) -> Result<bool, NodeError> {
        if !self.replica.is_leader() {
            let leader = self.replica.leader();
            self.respond(asker.address, asker.id, &Outcome::NotLeader(leader))?;
            return Ok(false);
        }
        if waiting >= self.settings.max_pending {
            self.respond(asker.address, asker.id, &Outcome::Busy)?;
            return Ok(false);
        }
        Ok(true)
    }

    fn sequence(&mut self) -> u64 {
        self.next_sequence = self.next_sequence.wrapping_add(1);
        self.next_sequence
    }

    fn write(&mut self, asker: Asker, key: &[u8], value: &[u8]) -> Result<(), NodeError> {
        if !self.admits(asker, self.writes.len())? {
            return Ok(());
        }
        let sequence = self.sequence();
        wire::put_command(
            &mut self.command,
            &Command {
                origin: self.settings.id,
                sequence,
                key,
                value,
            },
        );
        let data = self.command.clone();
        match heard(self.replica.propose(Vec::new(), data))? {
            Some(()) => {
                self.writes.insert(sequence, asker);
                Ok(())
            }
            None => self.respond(asker.address, asker.id, &Outcome::Busy),
        }
    }

    fn read(&mut self, asker: Asker, key: &[u8]) -> Result<(), NodeError> {
        if !self.admits(asker, self.reads.len())? {
            return Ok(());
        }
        if !self.replica.core().raft.commit_to_current_term() {
            return self.respond(asker.address, asker.id, &Outcome::Busy);
        }
        let sequence = self.sequence();
        match heard(self.replica.read(sequence.to_le_bytes().to_vec()))? {
            Some(()) => {
                self.reads.insert(sequence, (asker, key.to_vec()));
                Ok(())
            }
            None => self.respond(asker.address, asker.id, &Outcome::Busy),
        }
    }

    fn hear_test(&mut self, body: &[u8], from: SocketAddr) -> Result<(), NodeError> {
        if let Some((peer, message)) = stream::read_heartbeat(body) {
            if self.isolated {
                return Ok(());
            }
            // Stamped as it is read; a refusal is the stream's to make (a stale or unproven
            // heartbeat, a peer that shares no group), and the message is dropped.
            let now = self.now();
            let _ = self
                .liveness
                .on_heartbeat(peer, message, now, &mut self.asked);
            return self.act_on_liveness();
        }
        if let Some((id, order)) = control::read_order(body) {
            return self.obey(id, order, from);
        }
        if let Some((id, hold)) = stream::read_stall(body) {
            file::hold_flushes(hold);
            return self.respond(from, id, &Outcome::Done);
        }
        if let Some(id) = stream::read_hold(body) {
            self.respond(from, id, &Outcome::Done)?;
            hyper_raft_e2e::parent::hold_until_released();
            return Ok(());
        }
        let Some((id, control)) = wire::read_control(body, hyper_raft::MAX_MEMBERS) else {
            return Ok(());
        };
        match control {
            Control::Peers(peers) => self.peers = peers,
            Control::Isolate(cut) => self.isolated = cut,
        }
        self.respond(from, id, &Outcome::Done)
    }

    #[allow(
        clippy::disallowed_methods,
        reason = "real processes on the host's clock, threads and environment (CLAUDE.md §1a, end to end)"
    )]
    fn obey(&mut self, id: u64, order: Order, from: SocketAddr) -> Result<(), NodeError> {
        match order {
            Order::Wake => {
                self.woken = true;
                Ok(())
            }
            Order::Arm(point, count) => {
                self.armed = Some((point, count.max(1)));
                self.respond(from, id, &Outcome::Done)
            }
            Order::FailFlush => {
                file::fail_next_flush();
                self.respond(from, id, &Outcome::Done)
            }
            Order::Report => {
                let nanos = |d: Duration| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX);
                let timing = self.replica.core().raft.timing();
                let pairs = self
                    .attached
                    .iter()
                    .filter_map(|peer| self.liveness.report(*peer));
                let report = Report {
                    status: self.status(),
                    durable_commit: self.replica.durable_commit(),
                    known: self.replica.configuration_known(),
                    voters: self.replica.configuration().voters.clone(),
                    span_ns: timing.map_or(0, |t| nanos(t.span)),
                    round_ns: timing.map_or(0, |t| nanos(t.round)),
                    detection_ns: pairs
                        .clone()
                        .filter_map(|pair| pair.freshness)
                        .map(nanos)
                        .max()
                        .unwrap_or(0),
                    taken: pairs.clone().map(|pair| pair.taken).sum(),
                    unjudged: u64::try_from(pairs.clone().filter(|pair| !pair.judged).count())
                        .unwrap_or(u64::MAX),
                    unjudged_interval_ns: pairs
                        .filter(|pair| !pair.judged)
                        .filter_map(|pair| pair.interval)
                        .map(nanos)
                        .max()
                        .unwrap_or(0),
                    restarts: self.restarts,
                    blocked_ns: self.blocked_now(),
                    writing_ns: self.writing_now(),
                    flush_most_ns: self.flush_most,
                    turn_most_ns: self.turn_most,
                    suspected: self
                        .attached
                        .iter()
                        .copied()
                        .filter(|peer| self.liveness.trust(*peer) == Some(Trust::Suspected))
                        .collect(),
                    heard: self
                        .attached
                        .iter()
                        .copied()
                        .filter(|peer| {
                            self.liveness
                                .report(*peer)
                                .is_some_and(|pair| pair.taken > 0)
                        })
                        .collect(),
                };
                control::put_report(&mut self.sending, id, &report);
                if wire::seal(&mut self.sending, self.datagram) {
                    self.send(from)?;
                }
                Ok(())
            }
            Order::Change(kind, member) => {
                let change = ConfChangeV2 {
                    transition: ConfChangeTransition::Auto,
                    changes: vec![ConfChangeSingle {
                        change_type: kind,
                        node_id: member,
                    }],
                    context: Vec::new(),
                };
                let outcome = if !self.replica.is_leader() {
                    Outcome::NotLeader(self.replica.leader())
                } else {
                    match heard(self.replica.change(Vec::new(), &change))? {
                        Some(()) => Outcome::Done,
                        None => Outcome::Busy,
                    }
                };
                self.respond(from, id, &outcome)
            }
            Order::StallFlush => {
                file::stall_flushes();
                self.respond(from, id, &Outcome::Done)
            }
        }
    }

    fn send_raft(&mut self, messages: Vec<Message>) -> Result<(), NodeError> {
        if self.isolated {
            return Ok(());
        }
        for message in messages {
            let Some(address) = self
                .peers
                .iter()
                .find(|(peer, _)| *peer == message.to)
                .map(|(_, address)| *address)
            else {
                continue;
            };
            wire::begin(&mut self.sending, Kind::Raft);
            wire::put_u64(&mut self.sending, self.settings.id);
            message.encode(&mut self.sending);
            if !wire::seal(&mut self.sending, self.datagram) {
                continue;
            }
            self.send(address)?;
        }
        Ok(())
    }

    /// Drives the replica until it has nothing more to do now, stopping at an armed point. Each
    /// write the replica made durable is a flush the stream's heartbeats may prove.
    fn drive(&mut self) -> Result<Option<Point>, NodeError> {
        loop {
            let out_before = self.replica.in_flight();
            let made_before = self.made();
            let configuration = self.replica.configuration().clone();
            self.out.clear();
            let now = self.now();
            let driven = match self.replica.drive(now, &self.waker, &mut self.out) {
                Ok(driven) => driven,
                Err(ReplicaError::Fenced(cause)) => return Err(NodeError::Fenced(cause)),
                Err(_) => return Ok(None),
            };
            if let Some((started, durable)) = driven.flushed {
                self.liveness.on_durable(LiveWrite::Log, started, durable);
            }
            self.track(now, made_before, out_before);
            self.writing();
            let submitted = self.replica.in_flight() > out_before;
            let messages = std::mem::take(&mut self.out.messages);
            let released = !messages.is_empty();
            self.send_raft(messages)?;
            self.answer()?;
            let acted = self.replica.machine_mut().take_acted();
            self.say_acted(&acted)?;
            let changed = self.replica.configuration() != &configuration;
            if changed {
                self.pairs()?;
            }
            for (point, passed) in [
                (Point::Submitted, submitted),
                (Point::Released, released),
                (Point::Fenced, self.replica.behind_fence().is_some()),
                (Point::Changed, changed),
                (Point::Acted, !acted.is_empty()),
            ] {
                if passed && self.stops_at(point) {
                    return Ok(Some(point));
                }
            }
            self.lead_or_let_go()?;
            if !driven.more {
                return Ok(None);
            }
        }
    }

    /// Prints every entry acted on at start: what the host acted on, which the test holds a
    /// restart to.
    fn say_acted(&self, acted: &[u64]) -> Result<(), NodeError> {
        if acted.is_empty() {
            return Ok(());
        }
        let mut stdout = std::io::stdout().lock();
        for index in acted {
            writeln!(stdout, "acted {index}")?;
        }
        stdout.flush()?;
        Ok(())
    }

    /// Answers the writes this member proposed that were applied, and the reads now applied
    /// through their index.
    fn answer(&mut self) -> Result<(), NodeError> {
        let answers = std::mem::take(&mut self.out.answers);
        for applied in &answers {
            if applied.origin != self.settings.id {
                continue;
            }
            if let Some(asker) = self.writes.remove(&applied.sequence) {
                let outcome = if applied.taken {
                    Outcome::Put(applied.index)
                } else {
                    Outcome::Busy
                };
                self.respond(asker.address, asker.id, &outcome)?;
            }
        }
        self.out.answers = answers;
        let reads = std::mem::take(&mut self.out.reads);
        for (context, _) in &reads {
            let Ok(sequence) = <[u8; 8]>::try_from(context.as_slice()).map(u64::from_le_bytes)
            else {
                continue;
            };
            if let Some((asker, key)) = self.reads.remove(&sequence) {
                let value = self.replica.machine().get(&key).cloned();
                self.respond(asker.address, asker.id, &Outcome::Value(value))?;
            }
        }
        self.out.reads = reads;
        Ok(())
    }

    /// A member that stopped leading answers everyone it kept waiting: they ask the new leader.
    fn lead_or_let_go(&mut self) -> Result<(), NodeError> {
        let leading = self.replica.is_leader();
        if self.leading && !leading {
            let leader = self.replica.leader();
            let writes = std::mem::take(&mut self.writes);
            let reads = std::mem::take(&mut self.reads);
            for asker in writes
                .into_values()
                .chain(reads.into_values().map(|(asker, _)| asker))
            {
                self.respond(asker.address, asker.id, &Outcome::NotLeader(leader))?;
            }
        }
        self.leading = leading;
        Ok(())
    }
}

/// A refusal is the asker's to hear; a fence stops the member.
fn heard<T>(outcome: Result<T, ReplicaError>) -> Result<Option<T>, NodeError> {
    match outcome {
        Ok(value) => Ok(Some(value)),
        Err(ReplicaError::Fenced(cause)) => Err(NodeError::Fenced(cause)),
        Err(_) => Ok(None),
    }
}
