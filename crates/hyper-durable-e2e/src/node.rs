//! One member as a process: a hyper-durable `Replica` over hyper-log on a real file, a UDP socket,
//! and the key-value store of [`crate::machine`]. One thread does the member's work: it waits on
//! its socket until the replica's deadline or the end of its period, steps what arrived, and drives
//! the replica, which submits its writes with the member's waker and returns. The log's answer
//! wakes the waker, which a relay thread turns into a datagram to the member's own socket, so the
//! one wait the member makes covers both. The log runs its own two threads. A process runs these
//! four, whatever it holds.
//!
//! The replica elects by suspicion (timing step L-2, `docs/timing.md` §2.8) and takes no ticks.
//! What its detectors believe of its peers the test tells it (`control::Order::Suspect`, `Trust`,
//! `Restarted`): the test kills the members, so it knows, and stands for L-3's node-pair stream
//! until L-4. Its group's timing is hyper-timing's law over what the member measures: each period
//! it probes each peer and times the answer (`ExchangeRtt`), its timed waits give the granularity
//! (`Lateness`), and its replica's vote writes the mean flush (`Flushes`); the ballot and its span
//! are derived again whenever a measurement moves, and a group with no measured quorum of paths
//! draws no delay and so does not campaign (`docs/timing.md` §3, item 10).
//!
//! A write is answered once it is applied, so an answered write is committed; a read once a
//! quorum confirmed the leader and the member applied through the index it was confirmed at.
//! Where the test armed a point (`control::Point`), the member stops there, prints `stopped
//! <point>`, and waits to be killed.
use std::collections::BTreeMap;
use std::io::{ErrorKind, Write as _};
use std::net::{SocketAddr, UdpSocket};
use std::path::Path;
use std::sync::mpsc::Receiver;
use std::task::Waker;
use std::time::{Duration, Instant};

use hyper_durable::{
    Cause, GroupStore, Output, Replica, ReplicaError, Settings as Shell, Unbounded,
};
use hyper_log::{Config as LogConfig, Log, Waits};
use hyper_raft::proto::{ConfChangeSingle, ConfChangeTransition, ConfChangeV2, ConfState, Message};
use hyper_raft::wire::Record;
use hyper_raft::{Config, StateRole, Timing};
use hyper_raft_e2e::wire::{self, Command, Control, Kind, Op, Outcome, Status};
use hyper_timing::{Ballot, ExchangeRtt, Lateness};

use crate::control::{self, Order, Point, Report};
use crate::file::{self, FaultFile};
use crate::machine::{Applied, Kv};

/// The group every member's log holds: one group a log.
pub const GROUP: u128 = 1;
/// The log's id.
const LOG_ID: u128 = 0x0068_7970_6572_2d64_7572_6162_6c65;

/// The bytes of a member's datagram besides an append's entries: the datagram's header and the
/// sender's id, and the message record's header, fixed fields and checksum (hyper-raft-e2e's).
const MESSAGE_ROOM: usize = wire::HEADER
    + 8
    + hyper_raft::wire::HEADER_BYTES
    + hyper_raft::wire::MESSAGE_FIXED_BYTES
    + hyper_raft::wire::CHECKSUM_BYTES;

/// The log's settings: segments of 64 blocks, sixty-four of them (16 MiB), one group, and the
/// writer's measured waits (`Waits::Measured`, a node's).
pub fn log_config() -> LogConfig {
    LogConfig {
        segment_bytes: 64 * 4096,
        max_segments: 64,
        max_groups: 1,
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
    /// The next period is past what the clock counts.
    Clock,
}

impl std::fmt::Display for NodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Fenced(cause) => write!(f, "fenced: {cause}"),
            Self::Log(e) => write!(f, "the log: {e}"),
            Self::Open(e) => write!(f, "open: {e}"),
            Self::Io(e) => write!(f, "the socket: {e}"),
            Self::Clock => write!(f, "the next period is past what the clock counts"),
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
    /// The owner's period: a commit no write stated for a period is written, and each peer is
    /// probed once a period.
    pub period: Duration,
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
    /// The member's clock's origin: probes are stamped in nanoseconds since.
    epoch: Instant,
    /// The round trips measured to each peer, in the order of `peers`.
    paths: Vec<(u64, ExchangeRtt)>,
    /// How late the member's timed waits end: the granularity `G`.
    lateness: Lateness,
    /// The timing last given to the replica.
    timing: Option<Timing>,
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
    /// The member `settings` names on `socket`, opened on `log`, woken by `waker`.
    pub fn open(
        settings: Settings,
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
            quiet: settings.period,
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
            paths: Vec::new(),
            lateness: Lateness::new(),
            timing: None,
            settings,
        })
    }

    /// Nanoseconds on the member's clock.
    fn nanos(&self, at: Instant) -> u64 {
        u64::try_from(at.saturating_duration_since(self.epoch).as_nanos()).unwrap_or(u64::MAX)
    }

    /// Probes every peer, stamped now: each answers at once, and the answer times the path.
    fn probe(&mut self) -> Result<(), NodeError> {
        if self.isolated {
            return Ok(());
        }
        let stamp = self.nanos(Instant::now());
        for at in 0..self.peers.len() {
            let Some(&(peer, address)) = self.peers.get(at) else {
                continue;
            };
            if peer == self.settings.id {
                continue;
            }
            control::put_order(&mut self.sending, 0, &Order::Probe(stamp));
            if wire::seal(&mut self.sending, self.datagram) {
                self.send(address)?;
            }
        }
        Ok(())
    }

    /// The group's timing from what the member measured: the ballot over its paths to the other
    /// voters, the span it chooses, given to the replica when it moved. None before a quorum's
    /// paths and a wait are measured.
    fn measure(&mut self) -> Result<(), NodeError> {
        let Some(granularity) = self.lateness.granularity().filter(|g| !g.is_zero()) else {
            return Ok(());
        };
        let voters = &self.replica.configuration().voters;
        let paths = self
            .paths
            .iter()
            .filter(|(peer, _)| voters.contains(peer))
            .map(|(_, path)| path);
        let durable = self.replica.flushes().mean().unwrap_or(Duration::ZERO);
        let Some(ballot) = Ballot::measure(paths, voters.len(), durable, granularity) else {
            return Ok(());
        };
        let Some(span) = ballot.span(granularity) else {
            return Ok(());
        };
        let timing = Timing::of(&ballot, &span);
        if self.timing != Some(timing) {
            self.timing = Some(timing);
            heard(self.replica.set_timing(timing))?;
        }
        Ok(())
    }

    /// The log, which outlives the replica's handle on it.
    pub fn log(&self) -> &Log<FaultFile> {
        &self.log
    }

    /// Runs until `parent` says the test is gone, or until the member stops at an armed point
    /// (`Ok(Some(point))`), or fails.
    pub fn run(&mut self, parent: &Receiver<()>) -> Result<Option<Point>, NodeError> {
        let period = self.settings.period;
        let mut next_period = Instant::now();
        // The member drives once before it waits: a reopened member replays its log alone.
        if let Some(point) = self.drive()? {
            return Ok(Some(point));
        }
        while parent.try_recv().is_err() {
            let now = Instant::now();
            if now >= next_period {
                self.probe()?;
                next_period = now.checked_add(period).ok_or(NodeError::Clock)?;
            }
            // Woken at the replica's deadline, or at the period's end, whichever is first.
            let until = self
                .replica
                .deadline()
                .and_then(|at| self.epoch.checked_add(Duration::from_nanos(at)))
                .map_or(next_period, |deadline| deadline.min(next_period));
            if !self.receive_until(until)? {
                let woke = Instant::now();
                let (asked, ended) = (self.nanos(until), self.nanos(woke));
                // A fold that is full keeps its mean: the wait is one of more than it counts.
                let _ = self.lateness.on_wait(asked, ended);
            }
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

    /// Waits for a datagram until `until`, then takes what else has arrived, at most a turn's
    /// worth (hyper-raft-e2e's `receive_until`). False when the wait timed out with nothing.
    fn receive_until(&mut self, until: Instant) -> Result<bool, NodeError> {
        let wait = until.saturating_duration_since(Instant::now());
        if !wait.is_zero() {
            self.socket.set_read_timeout(Some(wait))?;
            if !self.receive_one()? {
                return Ok(false);
            }
        }
        self.socket.set_nonblocking(true)?;
        let turn = self
            .settings
            .max_pending
            .saturating_add(self.settings.voters.len())
            .saturating_add(1);
        let mut outcome = Ok(());
        for _ in 0..turn {
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
        outcome.map(|()| true)
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
        if let Some((id, order)) = control::read_order(body) {
            return self.obey(id, order, from);
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
                let report = Report {
                    status: self.status(),
                    durable_commit: self.replica.durable_commit(),
                    known: self.replica.configuration_known(),
                    voters: self.replica.configuration().voters.clone(),
                    span_ns: self.timing.map_or(0, |t| nanos(t.span)),
                    round_ns: self.timing.map_or(0, |t| nanos(t.round)),
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
            Order::Suspect(member) => {
                heard(self.replica.suspect(member))?;
                self.respond(from, id, &Outcome::Done)
            }
            Order::Trust(member) => {
                heard(self.replica.trust(member))?;
                self.respond(from, id, &Outcome::Done)
            }
            Order::Restarted(member) => {
                heard(self.replica.restarted(member))?;
                self.respond(from, id, &Outcome::Done)
            }
            Order::Probe(stamp) => {
                if self.isolated {
                    return Ok(());
                }
                control::put_order(&mut self.sending, id, &Order::Echo(stamp));
                if wire::seal(&mut self.sending, self.datagram) {
                    self.send(from)?;
                }
                Ok(())
            }
            Order::Echo(stamp) => {
                let Some(peer) = self
                    .peers
                    .iter()
                    .find(|(_, address)| *address == from)
                    .map(|(peer, _)| *peer)
                else {
                    return Ok(());
                };
                let took = self.nanos(Instant::now()).saturating_sub(stamp);
                match self.paths.iter_mut().find(|(at, _)| *at == peer) {
                    Some((_, path)) => path.on_sample(took),
                    None => {
                        let mut path = ExchangeRtt::new();
                        path.on_sample(took);
                        // One a peer, and the peers are bounded by `MAX_MEMBERS` (`hear_test`).
                        self.paths.push((peer, path));
                    }
                }
                Ok(())
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

    /// Drives the replica until it has nothing more to do now, stopping at an armed point.
    fn drive(&mut self) -> Result<Option<Point>, NodeError> {
        loop {
            let out_before = self.replica.in_flight();
            let configuration = self.replica.configuration().clone();
            self.out.clear();
            let driven =
                match self
                    .replica
                    .drive(self.nanos(Instant::now()), &self.waker, &mut self.out)
                {
                    Ok(driven) => driven,
                    Err(ReplicaError::Fenced(cause)) => return Err(NodeError::Fenced(cause)),
                    Err(_) => return Ok(None),
                };
            let submitted = self.replica.in_flight() > out_before;
            let messages = std::mem::take(&mut self.out.messages);
            let released = !messages.is_empty();
            self.send_raft(messages)?;
            self.answer()?;
            let acted = self.replica.machine_mut().take_acted();
            self.say_acted(&acted)?;
            let changed = self.replica.configuration() != &configuration;
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
