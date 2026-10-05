//! One member of an `n`-log group as a process: a `hyper_multilog::MultiLog` over `n` fsynced logs,
//! driven over a UDP socket, with a key-value store as its application, applied in the layer's
//! merged order. One thread does everything, as `hyper_raft_e2e::node`'s member does: it waits on
//! the socket until its logs' earliest deadline or its liveness stream's wake, takes what arrived,
//! polls the stream, and drives every log's `Ready`s, then the merge, then the barriers it owes.
//!
//! What is the core's member's here is that member's, carried over (`hyper_raft_e2e::node`):
//! elections by suspicion on one node-pair liveness stream, which every log shares
//! (`docs/timing.md` §2.8: a stream a node pair, whatever the groups they share); heartbeats proven
//! by a durable write of a log; the group's timing from what the stream measured, given to every
//! log; a write answered once applied, a read by ReadIndex once applied through its index.
//!
//! What the layer adds (`docs/multilog.md` §9):
//! - a key's writes go to the log its key routes to: the FNV-1a hash of its bytes (an owner's hash
//!   of byte keys, §2.1; FNV, Fowler, Noll and Vo, IETF `draft-eastlake-fnv`), a key beginning
//!   `*` is a global write, to log 0; a member that does not lead that log answers with its leader;
//! - a read asks the key's log, and is answered once the merge has consumed that log through the
//!   read's index (§8);
//! - each Raft datagram carries its log's number after its sender's, in the datagram the
//!   harness's checksum covers;
//! - the committed entries each log gives are handed over, the merge applied, the barriers owed
//!   proposed, after every drive;
//! - what the member reports is summed over its logs, and its digest is of the store, which every
//!   member reaches alike whatever interleaving of the logs it applied (§4.2).
use std::{
    collections::BTreeMap,
    io::{self, ErrorKind},
    net::{SocketAddr, UdpSocket},
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use hyper_liveness::{Change, Liveness, PeerId, Settings as LiveSettings, Write as LiveWrite};
use hyper_multilog::{Applied, Flow, Limits as LayerLimits, MultiLog, Point, Route, entry};
use hyper_raft::{
    Config, Elections, Limits, StateRole, Stated,
    proto::{ConfState, Entry, Message},
    wire::Record,
};
use hyper_raft_e2e::{
    stream::{self, Asked, Report},
    wal::{Wal, WalError},
    wire::{self, Command, Control, Kind, Op, Outcome, Status},
};
use hyper_timing::{Exposure, Trust};
use hyper_tokio::{Stamped, Taken};

/// The bytes of a member's Raft datagram besides an append's entries: the core's member's
/// (`hyper_raft_e2e::node::MESSAGE_ROOM`), and the log's number.
pub const MESSAGE_ROOM: usize = hyper_raft_e2e::node::MESSAGE_ROOM + 8;
/// FNV-1a's 64-bit offset basis (Fowler, Noll and Vo; IETF `draft-eastlake-fnv`, §2).
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
/// FNV-1a's 64-bit prime (as [`FNV_OFFSET`]).
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
/// The first byte of a key whose writes are global.
const GLOBAL_KEY: u8 = b'*';

/// Why the member stopped.
#[derive(Debug)]
pub enum MemberError {
    /// A log refused.
    Wal(WalError),
    /// The layer, or a log's member, found its own state no longer adds up.
    Layer(hyper_multilog::Error),
    /// The socket refused.
    Io(std::io::Error),
    /// The liveness stream refused.
    Liveness(hyper_liveness::Refusal),
}

impl std::fmt::Display for MemberError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Wal(error) => write!(f, "{error}"),
            Self::Layer(error) => write!(f, "the layer stopped: {error}"),
            Self::Io(error) => write!(f, "the socket: {error}"),
            Self::Liveness(error) => write!(f, "the liveness stream: {error}"),
        }
    }
}

impl std::error::Error for MemberError {}

impl From<WalError> for MemberError {
    fn from(error: WalError) -> Self {
        Self::Wal(error)
    }
}
impl From<std::io::Error> for MemberError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

/// A refusal changed nothing and is the asker's to hear; only a fatal error stops the member.
fn heard<T>(outcome: hyper_multilog::Result<T>) -> Result<Option<T>, MemberError> {
    match outcome {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.is_fatal() => Err(MemberError::Layer(error)),
        Err(_) => Ok(None),
    }
}

/// A log's member's answer, as the layer's.
fn core<T>(log: usize, outcome: hyper_raft::Result<T>) -> Result<Option<T>, MemberError> {
    heard(outcome.map_err(|error| hyper_multilog::Error::of(log, error)))
}

/// The route of `key`: a global write for a key beginning `*`, else the FNV-1a hash of its bytes.
pub fn route_of(key: &[u8]) -> Route {
    if key.first() == Some(&GLOBAL_KEY) {
        return Route::Global;
    }
    let hash = key.iter().fold(FNV_OFFSET, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME)
    });
    Route::Key(hash)
}

/// How a member runs, as its command line says.
#[derive(Clone, Debug)]
pub struct Settings {
    /// This member.
    pub id: u64,
    /// Every voter, this member included.
    pub voters: Vec<u64>,
    /// The logs the group's log is divided into.
    pub logs: usize,
    /// The most keys the store holds.
    pub max_keys: usize,
    /// The most writes and reads one member waits to answer.
    pub max_pending: usize,
    /// The most writes the scenario makes, which bounds each log.
    pub max_writes: usize,
    /// The most of them that are global, which bounds each log's barriers ([`per_term`]).
    pub max_globals: usize,
}

/// The entries a log adds each term besides the writes: its leader's empty entry, and a barrier
/// for each global at most. A log's leader appends barriers naming strictly later globals within
/// its term: its own only past what it covers, a forwarded one only past it, and either raises
/// what it covers (`docs/multilog.md` §3.1, `MultiLog::barriers` and `MultiLog::step`).
pub fn per_term(settings: &Settings) -> usize {
    settings.max_globals.saturating_add(1)
}

/// A client waiting for its answer.
#[derive(Clone, Copy, Debug)]
struct Asker {
    address: SocketAddr,
    id: u64,
}

/// Where answers go out: the socket, and the buffer a datagram is built in.
struct Reply<'a> {
    socket: &'a UdpSocket,
    sending: &'a mut Vec<u8>,
    datagram: usize,
}

impl Reply<'_> {
    fn send(&self, to: SocketAddr) -> Result<(), MemberError> {
        match self.socket.send_to(self.sending, to) {
            Ok(_) => Ok(()),
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::ConnectionRefused
                        | ErrorKind::ConnectionReset
                        | ErrorKind::WouldBlock
                ) =>
            {
                Ok(())
            }
            Err(error) => Err(error.into()),
        }
    }
    fn respond(&mut self, to: SocketAddr, id: u64, outcome: &Outcome) -> Result<(), MemberError> {
        wire::put_response(self.sending, id, outcome);
        if wire::seal(self.sending, self.datagram) {
            self.send(to)?;
        }
        Ok(())
    }
}

/// The application: the store, the commands applied, and the askers this member answers.
struct App {
    max_keys: usize,
    store: BTreeMap<Vec<u8>, Vec<u8>>,
    applied: u64,
    /// The writes this member waits to answer: key → the value asked, who asked, and its log.
    writes: BTreeMap<Vec<u8>, (Vec<u8>, Asker, usize)>,
}

impl App {
    /// Applies one command the merge gives, answering its asker where it waits here: a write
    /// waits for the command writing its value to its key in its log, whichever member proposed
    /// it (a client that asks again of a new leader waits for the entry the old one appended).
    fn apply(
        &mut self,
        log: usize,
        index: u64,
        data: &[u8],
        reply: &mut Reply<'_>,
    ) -> Result<(), MemberError> {
        self.applied = self.applied.saturating_add(1);
        let Some(command) = wire::read_command(data) else {
            return Ok(());
        };
        let room = self.store.len() < self.max_keys || self.store.contains_key(command.key);
        let outcome = if room {
            self.store
                .insert(command.key.to_vec(), command.value.to_vec());
            Outcome::Put(index)
        } else {
            Outcome::Busy
        };
        let waited = self
            .writes
            .get(command.key)
            .is_some_and(|(value, _, at)| value.as_slice() == command.value && *at == log);
        if waited && let Some((_, asker, _)) = self.writes.remove(command.key) {
            reply.respond(asker.address, asker.id, &outcome)?;
        }
        Ok(())
    }

    /// A digest of the store, whatever order it was written in: every member that applied the same
    /// commands holds the same (`docs/multilog.md` §4.2).
    fn digest(&self) -> u64 {
        self.store
            .iter()
            .flat_map(|(key, value)| key.iter().chain([&0u8]).chain(value).chain([&1u8]))
            .fold(FNV_OFFSET, |hash, byte| {
                (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME)
            })
    }
}

/// A member, its socket, its logs, its liveness stream and who waits on it.
pub struct Member {
    multi: MultiLog<Wal>,
    socket: UdpSocket,
    settings: Settings,
    peers: Vec<(u64, SocketAddr)>,
    isolated: bool,
    app: App,
    next_sequence: u64,
    /// Reads asked: sequence → the asker, the key and its log.
    reads: BTreeMap<u64, (Asker, Vec<u8>, usize)>,
    /// Reads confirmed at an index of a log: (log, index, sequence).
    confirmed: Vec<(usize, u64, u64)>,
    /// For each log, the term this member leads it in, as it last looked.
    leading: Vec<Option<u64>>,
    received: Vec<u8>,
    sending: Vec<u8>,
    datagram: usize,
    command: Vec<u8>,
    stamped: Stamped,
    liveness: Liveness,
    attached: Vec<PeerId>,
    asked: Asked,
    restarts: u64,
    told: Vec<(PeerId, u64)>,
    unread: u64,
    blocked: u64,
    flush_most: u64,
    turn_most: u64,
    read_ns: u64,
}

/// The configuration each log's member runs with, from what the member states (as
/// `hyper_raft_e2e::node`'s): a message its datagram's room, the voters, a queue of the scenario's
/// writes a datagram each.
fn config(settings: &Settings, datagram: usize) -> Result<Config, MemberError> {
    let room = datagram.saturating_sub(MESSAGE_ROOM);
    let limits = Limits::derive(Stated {
        message: room.saturating_add(hyper_raft::wire::MESSAGE_RECORD_FIXED_BYTES),
        members: settings.voters.len(),
        memory: settings.max_writes.saturating_mul(datagram),
        depth: 1,
    })
    .map_err(|error| MemberError::Layer(hyper_multilog::Error::of(0, error)))?;
    Ok(Config {
        elections: Elections::Suspicion,
        max_size_per_msg: u64::try_from(room).unwrap_or(u64::MAX),
        check_quorum: true,
        pre_vote: true,
        seed: settings.id,
        ..Config::new(settings.id, limits)
    })
}

impl Member {
    /// The member `settings` names, on `socket`, its logs `wals` (one a log), in its run `run`.
    pub fn open(
        settings: Settings,
        run: u64,
        socket: UdpSocket,
        wals: Vec<Wal>,
    ) -> Result<Self, MemberError> {
        let datagram = wire::largest(&socket)?;
        let stamped =
            Stamped::new(&socket).map_err(|error| MemberError::Io(io::Error::other(error)))?;
        let boot = ConfState {
            voters: settings.voters.clone(),
            ..ConfState::default()
        };
        let point = Point::origin(vec![boot; settings.logs]).map_err(MemberError::Layer)?;
        // Every log holds at most the scenario's writes and a barrier a global among them.
        let limits = LayerLimits {
            unmerged: u64::try_from(settings.max_writes).unwrap_or(u64::MAX),
        };
        let multi = MultiLog::open(&config(&settings, datagram)?, wals, &point, limits)
            .map_err(MemberError::Layer)?;
        let liveness = Liveness::new(LiveSettings {
            local: settings.id,
            run,
            max_peers: settings.voters.len(),
            history: Exposure::new(),
            resolution: stamped.clock().resolution(),
        })
        .map_err(MemberError::Liveness)?;
        Ok(Self {
            multi,
            socket,
            peers: Vec::new(),
            isolated: false,
            app: App {
                max_keys: settings.max_keys,
                store: BTreeMap::new(),
                applied: 0,
                writes: BTreeMap::new(),
            },
            next_sequence: 0,
            reads: BTreeMap::new(),
            confirmed: Vec::new(),
            leading: vec![None; settings.logs],
            received: vec![0; wire::MAX_DATAGRAM],
            sending: Vec::with_capacity(datagram),
            datagram,
            command: Vec::new(),
            stamped,
            liveness,
            attached: Vec::new(),
            asked: Asked::default(),
            restarts: 0,
            told: Vec::new(),
            unread: 0,
            blocked: 0,
            flush_most: 0,
            turn_most: 0,
            read_ns: 0,
            settings,
        })
    }

    fn now(&self) -> u64 {
        self.stamped.clock().now_ns()
    }

    /// Runs until `stop` is set, which it reads once a turn, or until the member fails.
    pub fn run(&mut self, stop: &AtomicBool) -> Result<(), MemberError> {
        self.pairs()?;
        self.drive()?;
        self.read_ns = self.now();
        while !stop.load(Ordering::Acquire) {
            if let Some(clock) = self.drain(self.turn())? {
                self.live(clock)?;
            }
            let wake = self.liveness.wake();
            let until = [self.deadline(), wake].into_iter().flatten().min();
            if let Some(clock) = self.receive_until(until)? {
                self.live(clock)?;
            }
            self.measure()?;
            self.drive()?;
        }
        Ok(())
    }

    /// The earliest deadline of the member's logs.
    fn deadline(&self) -> Option<u64> {
        (0..self.settings.logs)
            .filter_map(|log| self.multi.node(log).and_then(hyper_raft::RawNode::deadline))
            .min()
    }

    /// Each log's member, in turn.
    fn each(
        &mut self,
        mut call: impl FnMut(&mut hyper_raft::RawNode<Wal>) -> hyper_raft::Result<()>,
    ) -> Result<(), MemberError> {
        for log in 0..self.settings.logs {
            if let Some(node) = self.multi.node_mut(log) {
                core(log, call(node))?;
            }
        }
        Ok(())
    }

    /// Keeps the stream told which peers the group has: log 0's configuration's other members, the
    /// configuration every log holds.
    fn pairs(&mut self) -> Result<(), MemberError> {
        let id = self.settings.id;
        let mut now: Vec<PeerId> = self
            .multi
            .node(0)
            .map(|node| {
                node.raft
                    .configuration()
                    .members()
                    .filter(|member| *member != id)
                    .collect()
            })
            .unwrap_or_default();
        now.sort_unstable();
        now.dedup();
        for peer in now.clone() {
            if self.attached.binary_search(&peer).is_err() {
                self.liveness.attach(peer).map_err(MemberError::Liveness)?;
                let suspected = self.liveness.trust(peer) == Some(Trust::Suspected);
                self.each(|node| {
                    if suspected {
                        node.suspect(peer)
                    } else {
                        node.trust(peer)
                    }
                })?;
            }
        }
        for peer in &self.attached {
            if now.binary_search(peer).is_err() {
                self.liveness.detach(*peer).map_err(MemberError::Liveness)?;
            }
        }
        self.told
            .retain(|(peer, _)| now.binary_search(peer).is_ok());
        self.attached = now;
        Ok(())
    }

    /// The group's timing from what the stream measured, given to every log.
    fn measure(&mut self) -> Result<(), MemberError> {
        let voters = self.settings.voters.clone();
        let Some((timing, span)) = stream::timing(&self.liveness, self.settings.id, &voters) else {
            return Ok(());
        };
        self.each(|node| {
            if node.raft.timing() == Some(timing) {
                Ok(())
            } else {
                node.set_timing(timing)
            }
        })?;
        for peer in &self.attached {
            let _ = self.liveness.set_election(*peer, span.election);
        }
        Ok(())
    }

    fn live(&mut self, clock: u64) -> Result<(), MemberError> {
        self.liveness.poll(clock, &mut self.asked);
        if self.act_on_liveness()?
            && let Some(clock) = self.drain(self.turn())?
        {
            self.liveness.poll(clock, &mut self.asked);
            self.act_on_liveness()?;
        }
        Ok(())
    }

    fn act_on_liveness(&mut self) -> Result<bool, MemberError> {
        let flushed = std::mem::take(&mut self.asked.flush);
        if flushed {
            let started = self.now();
            if let Some(node) = self.multi.node_mut(0) {
                node.store_mut().prove()?;
            }
            let durable = self.now();
            self.wrote(started, durable);
            self.liveness
                .on_durable(LiveWrite::Liveness, started, durable);
        }
        let heartbeats = std::mem::take(&mut self.asked.heartbeats);
        for (peer, message) in &heartbeats {
            self.send_heartbeat(*peer, message)?;
        }
        self.asked.heartbeats = heartbeats;
        self.asked.heartbeats.clear();
        let changes = std::mem::take(&mut self.asked.changes);
        for change in &changes {
            self.believe(change)?;
        }
        self.asked.changes = changes;
        self.asked.changes.clear();
        Ok(flushed)
    }

    fn send_heartbeat(&mut self, peer: PeerId, message: &[u8]) -> Result<(), MemberError> {
        if self.isolated {
            return Ok(());
        }
        let Some(address) = self.address_of(peer) else {
            return Ok(());
        };
        stream::put_heartbeat(&mut self.sending, self.settings.id, message);
        if wire::seal(&mut self.sending, self.datagram) {
            self.reply().send(address)?;
        }
        Ok(())
    }

    fn address_of(&self, peer: u64) -> Option<SocketAddr> {
        self.peers
            .iter()
            .find(|(id, _)| *id == peer)
            .map(|(_, address)| *address)
    }

    fn wrote(&mut self, started: u64, durable: u64) {
        let took = durable.saturating_sub(started);
        self.blocked = self.blocked.saturating_add(took);
        self.flush_most = self.flush_most.max(took);
    }

    /// Takes a change the stream reported to every log's member.
    fn believe(&mut self, change: &Change) -> Result<(), MemberError> {
        let peer = change.peer();
        match change {
            Change::Suspected(suspicion) => {
                self.told.retain(|(told, _)| *told != peer);
                self.told.push((peer, suspicion.at_ns));
                self.each(|node| node.suspect(peer))
            }
            Change::Trusted { .. } => self.each(|node| node.trust(peer)),
            Change::Restarted { .. } => {
                self.restarts = self.restarts.saturating_add(1);
                self.each(|node| node.restarted(peer))
            }
        }
    }

    /// As `hyper_raft_e2e::node::Node::receive_until`.
    pub fn receive_until(&mut self, until: Option<u64>) -> Result<Option<u64>, MemberError> {
        let turn = self.turn();
        let mut most = turn;
        let wake = self.liveness.wake();
        let began = self.now();
        let wait = until.map(|at| Duration::from_nanos(at.saturating_sub(began)));
        if wait.is_none_or(|wait| !wait.is_zero()) {
            most = turn.saturating_add(1);
            self.turn_most = self.turn_most.max(began.saturating_sub(self.read_ns));
            let came = hyper_measure::wait::arrives(&self.socket, wait, &mut self.received)?;
            let woke = self.now();
            self.read_ns = woke;
            if let Some(at) = wake
                && began < at
                && woke >= at
            {
                self.liveness.on_wait(at, woke);
            }
            if !came {
                most = turn;
            }
        }
        self.drain(most)
    }

    /// The datagrams a turn takes: as many as the member has askers to answer and, from each
    /// voter, a Raft message for each log and a heartbeat.
    fn turn(&self) -> usize {
        let per_voter = self.settings.logs.saturating_add(1);
        self.settings
            .max_pending
            .saturating_add(self.settings.voters.len().saturating_mul(per_voter))
    }

    fn drain(&mut self, most: usize) -> Result<Option<u64>, MemberError> {
        let clock = self.now();
        self.turn_most = self.turn_most.max(clock.saturating_sub(self.read_ns));
        self.socket.set_nonblocking(true)?;
        let mut emptied = false;
        let mut outcome = Ok(());
        for _ in 0..most {
            match self.receive_one() {
                Ok(true) => {}
                Ok(false) => {
                    emptied = true;
                    break;
                }
                Err(error) => {
                    outcome = Err(error);
                    break;
                }
            }
        }
        self.socket.set_nonblocking(false)?;
        self.read_ns = self.now();
        outcome.map(|()| emptied.then_some(clock))
    }

    fn receive_one(&mut self) -> Result<bool, MemberError> {
        let (length, arrival) = match self.stamped.receive(&self.socket, &mut self.received) {
            Ok(Some(Taken::Datagram(length, arrival))) => (length, arrival),
            Ok(Some(Taken::Unreadable)) => return Ok(true),
            Ok(None) => return Ok(false),
            Err(error) if error.kind() == ErrorKind::ConnectionReset => return Ok(true),
            Err(error) => return Err(error.into()),
        };
        let from = arrival.from;
        let datagram = std::mem::take(&mut self.received);
        let outcome = match datagram.get(..length).and_then(wire::open) {
            Some((Kind::Raft, body)) => self.hear_peer(body),
            Some((Kind::Request, body)) => self.hear_client(body, from),
            Some((Kind::Control, body)) => self.hear_test(body, from, arrival.at_ns),
            Some((Kind::Response, _)) | None => Ok(()),
        };
        self.received = datagram;
        outcome.map(|()| true)
    }

    fn hear_peer(&mut self, body: &[u8]) -> Result<(), MemberError> {
        if self.isolated {
            return Ok(());
        }
        let mut reader = wire::Reader::new(body);
        let (Some(from), Some(log)) = (reader.u64(), reader.u64()) else {
            return Ok(());
        };
        let Ok(log) = usize::try_from(log) else {
            return Ok(());
        };
        let Ok(message) = Message::decode(reader.rest()) else {
            return Ok(());
        };
        if message.from != from || message.to != self.settings.id {
            return Ok(());
        }
        heard(self.multi.step(log, message)).map(drop)
    }

    fn reply(&mut self) -> Reply<'_> {
        Reply {
            socket: &self.socket,
            sending: &mut self.sending,
            datagram: self.datagram,
        }
    }
    fn respond(&mut self, to: SocketAddr, id: u64, outcome: &Outcome) -> Result<(), MemberError> {
        self.reply().respond(to, id, outcome)
    }

    fn hear_client(&mut self, body: &[u8], from: SocketAddr) -> Result<(), MemberError> {
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

    /// What the member is, summed over its logs: log 0's term, role and leader; the commits given
    /// to apply, the indexes the merge consumed, the logs' last indexes, each a sum; the store's
    /// digest. Its merge has consumed all it was given where the two sums agree.
    fn status(&self) -> Status {
        let mut status = Status {
            id: self.settings.id,
            digest: self.app.digest(),
            ..Status::default()
        };
        if let Some(raft) = self.multi.node(0).map(|node| &node.raft) {
            status.term = raft.term();
            status.leads = raft.state() == StateRole::Leader;
            status.leader = raft.leader_id();
        }
        for log in 0..self.settings.logs {
            let Some(node) = self.multi.node(log) else {
                continue;
            };
            status.commit = status.commit.saturating_add(node.given_to_apply());
            status.applied = status
                .applied
                .saturating_add(self.multi.merged_through(log).unwrap_or(0));
            status.last_index = status
                .last_index
                .saturating_add(node.raft.log().last_index().unwrap_or(0));
        }
        status
    }

    /// What the member says of itself, its law and its detectors (as the core's member's).
    fn report(&self) -> Report {
        let nanos = |d: Duration| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX);
        let timing = self.multi.node(0).and_then(|node| node.raft.timing());
        let pairs = self
            .attached
            .iter()
            .filter_map(|peer| self.liveness.report(*peer));
        let peers = |keep: &dyn Fn(PeerId) -> bool| -> Vec<u64> {
            self.attached
                .iter()
                .copied()
                .filter(|peer| keep(*peer))
                .collect()
        };
        Report {
            status: self.status(),
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
            writes: self.writes_held(),
            restarts: self.restarts,
            blocked_ns: self.blocked,
            flush_most_ns: self.flush_most,
            turn_most_ns: self.turn_most,
            waiting: u64::try_from(self.app.writes.len().saturating_add(self.reads.len()))
                .unwrap_or(u64::MAX),
            stray: self.stray(),
            unread: self.unread,
            suspected: peers(&|peer| self.liveness.trust(peer) == Some(Trust::Suspected)),
            heard: peers(&|peer| {
                self.liveness
                    .report(peer)
                    .is_some_and(|pair| pair.taken > 0)
            }),
            // The first log's core, whose timing the report states too
            core_suspected: self
                .multi
                .node(0)
                .map_or_else(Vec::new, |node| node.raft.suspected().to_vec()),
            clock_ns: self.now(),
            campaign: self
                .multi
                .node(0)
                .and_then(|node| node.raft.campaign_state()),
        }
    }

    /// The writes the member's logs hold.
    fn writes_held(&self) -> u64 {
        let held = (0..self.settings.logs)
            .filter_map(|log| self.multi.node(log))
            .map(|node| {
                node.store()
                    .entries()
                    .iter()
                    .filter(|entry| wire::read_command(&entry_command(entry)).is_some())
                    .count()
            })
            .sum::<usize>();
        u64::try_from(held).unwrap_or(u64::MAX)
    }

    /// The writes this member waits to answer that no merge will answer: each is to have an entry
    /// in its log past what the merge consumed there that writes the value asked, while it leads
    /// the term it took the asker in.
    fn stray(&self) -> u64 {
        let stray = self
            .app
            .writes
            .iter()
            .filter(|(key, (value, _, log))| !self.awaited(*log, key, value))
            .count();
        u64::try_from(stray).unwrap_or(u64::MAX)
    }

    fn awaited(&self, log: usize, key: &[u8], value: &[u8]) -> bool {
        let Some(node) = self.multi.node(log) else {
            return false;
        };
        let raft = &node.raft;
        let leads = raft.state() == StateRole::Leader
            && self.leading.get(log).copied().flatten() == Some(raft.term());
        let merged = self.multi.merged_through(log).unwrap_or(0);
        let last = raft.log().last_index().unwrap_or(0);
        leads
            && merged < last
            && raft
                .log()
                .any_entry(merged.saturating_add(1), last.saturating_add(1), |entry| {
                    wire::read_command(&entry_command(entry))
                        .is_some_and(|command| command.key == key && command.value == value)
                })
                .unwrap_or(false)
    }

    /// Whether this member leads `log`; the refusal, naming that log's leader, sent when not.
    fn leads(&mut self, log: usize, asker: Asker) -> Result<bool, MemberError> {
        self.lead_or_let_go()?;
        if self.leading.get(log).copied().flatten().is_some() {
            return Ok(true);
        }
        let leader = self.multi.node(log).map_or(0, |node| node.raft.leader_id());
        self.respond(asker.address, asker.id, &Outcome::NotLeader(leader))?;
        Ok(false)
    }

    fn room(&mut self, asker: Asker, waiting: usize) -> Result<bool, MemberError> {
        if waiting < self.settings.max_pending {
            return Ok(true);
        }
        self.respond(asker.address, asker.id, &Outcome::Busy)?;
        Ok(false)
    }

    fn sequence(&mut self) -> u64 {
        self.next_sequence = self.next_sequence.wrapping_add(1);
        self.next_sequence
    }

    /// The index of the entry of `log`'s durable log that writes `value` to `key`, if one does.
    fn held(&self, log: usize, key: &[u8], value: &[u8]) -> Option<u64> {
        self.multi
            .node(log)?
            .store()
            .entries()
            .iter()
            .find_map(|entry| {
                wire::read_command(&entry_command(entry))
                    .filter(|command| command.key == key && command.value == value)
                    .map(|_| entry.index)
            })
    }

    fn write(&mut self, asker: Asker, key: &[u8], value: &[u8]) -> Result<(), MemberError> {
        let route = route_of(key);
        let log = self.multi.route(route);
        if !self.leads(log, asker)? {
            return Ok(());
        }
        if let Some((waited, at, _)) = self.app.writes.get_mut(key)
            && waited.as_slice() == value
        {
            *at = asker;
            return Ok(());
        }
        if !self.room(asker, self.app.writes.len())? {
            return Ok(());
        }
        let merged = self.multi.merged_through(log).unwrap_or(0);
        match self.held(log, key, value) {
            Some(index) if index <= merged => {
                let outcome = if self.app.store.get(key).is_some_and(|held| held == value) {
                    Outcome::Put(index)
                } else {
                    Outcome::Busy
                };
                self.respond(asker.address, asker.id, &outcome)
            }
            Some(_) => {
                self.app
                    .writes
                    .insert(key.to_vec(), (value.to_vec(), asker, log));
                Ok(())
            }
            None => self.propose(asker, route, log, key, value),
        }
    }

    fn propose(
        &mut self,
        asker: Asker,
        route: Route,
        log: usize,
        key: &[u8],
        value: &[u8],
    ) -> Result<(), MemberError> {
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
        // The layer's room reserved, as an owner that knows the layer makes a command.
        let mut data = Vec::with_capacity(self.command.len().saturating_add(entry::SUFFIX_BYTES));
        data.extend_from_slice(&self.command);
        match heard(self.multi.propose(route, data))? {
            Some(_) => {
                self.app
                    .writes
                    .insert(key.to_vec(), (value.to_vec(), asker, log));
                Ok(())
            }
            None => self.respond(asker.address, asker.id, &Outcome::Busy),
        }
    }

    fn read(&mut self, asker: Asker, key: &[u8]) -> Result<(), MemberError> {
        let log = self.multi.route(route_of(key));
        if !self.leads(log, asker)? || !self.room(asker, self.reads.len())? {
            return Ok(());
        }
        let sequence = self.sequence();
        let asked = match self.multi.node_mut(log) {
            Some(node) => core(log, node.read_index(sequence.to_le_bytes().to_vec()))?,
            None => None,
        };
        match asked {
            Some(()) => {
                self.reads.insert(sequence, (asker, key.to_vec(), log));
                Ok(())
            }
            None => self.respond(asker.address, asker.id, &Outcome::Busy),
        }
    }

    fn hear_test(&mut self, body: &[u8], from: SocketAddr, at_ns: u64) -> Result<(), MemberError> {
        if let Some((peer, message)) = stream::read_heartbeat(body) {
            return self.heartbeat(peer, message, at_ns);
        }
        if let Some(id) = stream::read_report_ask(body) {
            self.lead_or_let_go()?;
            let report = self.report();
            stream::put_report(&mut self.sending, id, &report);
            if wire::seal(&mut self.sending, self.datagram) {
                self.reply().send(from)?;
            }
            return Ok(());
        }
        if let Some(id) = stream::read_account_ask(body) {
            let voters = self.settings.voters.clone();
            let election = stream::timing(&self.liveness, self.settings.id, &voters)
                .map(|(_, span)| span.election);
            let account = stream::account(&self.liveness, &self.attached, election);
            stream::put_account(&mut self.sending, id, &account);
            if wire::seal(&mut self.sending, self.datagram) {
                self.reply().send(from)?;
            }
            return Ok(());
        }
        let Some((id, control)) = wire::read_control(body, self.settings.voters.len()) else {
            return Ok(());
        };
        match control {
            Control::Peers(peers) => self.peers = peers,
            Control::Isolate(cut) => self.isolated = cut,
        }
        self.respond(from, id, &Outcome::Done)
    }

    fn heartbeat(&mut self, peer: u64, message: &[u8], at_ns: u64) -> Result<(), MemberError> {
        if self.isolated {
            return Ok(());
        }
        let taken = self
            .liveness
            .on_heartbeat(peer, message, at_ns, &mut self.asked);
        if taken.is_ok()
            && let Some(index) = self.told.iter().position(|(told, _)| *told == peer)
        {
            let (_, point) = self.told.swap_remove(index);
            if at_ns < point {
                self.unread = self.unread.saturating_add(1);
            }
        }
        self.act_on_liveness().map(drop)
    }

    fn send_raft(&mut self, log: usize, messages: Vec<Message>) -> Result<(), MemberError> {
        if self.isolated {
            return Ok(());
        }
        let tag = u64::try_from(log).unwrap_or(u64::MAX);
        for mut message in messages {
            let Some(address) = self.address_of(message.to) else {
                continue;
            };
            self.multi.stamp(log, &mut message);
            wire::begin(&mut self.sending, Kind::Raft);
            wire::put_u64(&mut self.sending, self.settings.id);
            wire::put_u64(&mut self.sending, tag);
            message.encode(&mut self.sending);
            if !wire::seal(&mut self.sending, self.datagram) {
                continue;
            }
            self.reply().send(address)?;
        }
        Ok(())
    }

    /// Wakes every log at the member's clock and acts on each `Ready` they have; then the merge
    /// and the barriers owed, which may make more; until no log has one.
    fn drive(&mut self) -> Result<(), MemberError> {
        loop {
            let now = self.now();
            self.each(|node| node.wake(now).map(drop))?;
            let mut any = false;
            for log in 0..self.settings.logs {
                while self
                    .multi
                    .node(log)
                    .is_some_and(hyper_raft::RawNode::has_ready)
                {
                    self.ready(log)?;
                    any = true;
                }
            }
            self.merge()?;
            if !any && !self.has_ready() {
                return Ok(());
            }
        }
    }

    fn has_ready(&self) -> bool {
        (0..self.settings.logs).any(|log| {
            self.multi
                .node(log)
                .is_some_and(hyper_raft::RawNode::has_ready)
        })
    }

    /// One `Ready` of `log`, as the core's member takes one (in place, written and flushed, a
    /// leader's messages at once and a follower's once durable), its committed entries handed to
    /// the layer once it is advanced.
    fn ready(&mut self, log: usize) -> Result<(), MemberError> {
        let Some(node) = self.multi.node_mut(log) else {
            return Ok(());
        };
        let Some(mut ready) = core(log, node.ready_in_place())? else {
            return Ok(());
        };
        let messages = ready.take_messages();
        self.send_raft(log, messages)?;
        let started = self.now();
        let Some(node) = self.multi.node_mut(log) else {
            return Ok(());
        };
        let persist = node.to_persist();
        let hard = ready.hard_state();
        let wrote = !persist.entries.is_empty() || hard.is_some();
        persist.store.write(persist.entries, hard)?;
        if wrote {
            let durable = self.now();
            self.wrote(started, durable);
            self.liveness.on_durable(LiveWrite::Log, started, durable);
        }
        for read in ready.take_read_states() {
            self.confirm(log, read.index, &read.request_ctx);
        }
        let first_range = ready.committed_range();
        let persisted = ready.take_persisted_messages();
        self.send_raft(log, persisted)?;
        let Some(node) = self.multi.node_mut(log) else {
            return Ok(());
        };
        let Some(mut light) = core(
            log,
            node.advance_append_keeping(ready, |wal, kept| wal.keep(kept.entries)),
        )?
        else {
            return Ok(());
        };
        node.store_mut().damage()?;
        if let Some(commit) = light.commit_index() {
            node.store_mut().set_commit(commit);
        }
        let more = light.take_messages();
        let second_range = light.committed_range();
        self.send_raft(log, more)?;
        for (first, last) in [first_range, second_range].into_iter().flatten() {
            self.hand_over(log, first, last)?;
        }
        self.lead_or_let_go()
    }

    /// Hands `[first, last]` of `log`, read where its log holds it, to the layer.
    fn hand_over(&mut self, log: usize, first: u64, last: u64) -> Result<(), MemberError> {
        let entries: Vec<Entry> = match self.multi.node(log) {
            Some(node) => node.store().held(first, last)?.to_vec(),
            None => return Ok(()),
        };
        heard(self.multi.hand_over(log, &entries)).map(drop)
    }

    /// The merge applies what the logs allow, answering the writes it applies; the reads it has
    /// passed the index of are answered; the barriers owed are proposed.
    fn merge(&mut self) -> Result<(), MemberError> {
        let Self {
            multi,
            app,
            socket,
            sending,
            datagram,
            ..
        } = self;
        let mut reply = Reply {
            socket,
            sending,
            datagram: *datagram,
        };
        let mut failed = None;
        let applied = multi.apply(u64::MAX, &mut |applied| {
            if let Applied::Command(command) = applied
                && let Err(error) = app.apply(command.log, command.index, command.data, &mut reply)
            {
                failed = Some(error);
                return Flow::Stop;
            }
            Flow::Continue
        });
        if let Some(error) = failed {
            return Err(error);
        }
        heard(applied)?;
        heard(self.multi.barriers())?;
        self.answer_reads()
    }

    fn confirm(&mut self, log: usize, index: u64, context: &[u8]) {
        let Ok(sequence) = <[u8; 8]>::try_from(context).map(u64::from_le_bytes) else {
            return;
        };
        if self.reads.contains_key(&sequence) && self.confirmed.len() < self.settings.max_pending {
            self.confirmed.push((log, index, sequence));
        }
    }

    /// A member that stopped leading a log in the term it took that log's askers in answers every
    /// one of them: they ask the new leader.
    fn lead_or_let_go(&mut self) -> Result<(), MemberError> {
        for log in 0..self.settings.logs {
            let (leading, leader) = match self.multi.node(log) {
                Some(node) => (
                    (node.raft.state() == StateRole::Leader).then(|| node.raft.term()),
                    node.raft.leader_id(),
                ),
                None => continue,
            };
            let was = self.leading.get(log).copied().flatten();
            if was.is_some() && was != leading {
                self.let_go(log, leader)?;
            }
            if let Some(held) = self.leading.get_mut(log) {
                *held = leading;
            }
        }
        Ok(())
    }

    fn let_go(&mut self, log: usize, leader: u64) -> Result<(), MemberError> {
        let mut askers = Vec::new();
        self.app.writes.retain(|_, (_, asker, at)| {
            if *at == log {
                askers.push(*asker);
                return false;
            }
            true
        });
        self.reads.retain(|_, (asker, _, at)| {
            if *at == log {
                askers.push(*asker);
                return false;
            }
            true
        });
        self.confirmed.retain(|(at, _, _)| *at != log);
        for asker in askers {
            self.respond(asker.address, asker.id, &Outcome::NotLeader(leader))?;
        }
        Ok(())
    }

    fn answer_reads(&mut self) -> Result<(), MemberError> {
        let mut at = 0;
        while let Some((log, index, sequence)) = self.confirmed.get(at).copied() {
            if index > self.multi.merged_through(log).unwrap_or(0) {
                at = at.saturating_add(1);
                continue;
            }
            self.confirmed.swap_remove(at);
            if let Some((asker, key, _)) = self.reads.remove(&sequence) {
                let value = self.app.store.get(&key).cloned();
                self.respond(asker.address, asker.id, &Outcome::Value(value))?;
            }
        }
        Ok(())
    }
}

/// The owner's command an entry holds, the layer's tag and key read off; empty for an entry that
/// holds none.
fn entry_command(entry: &Entry) -> Vec<u8> {
    match entry::read(entry) {
        entry::Stated::Global(command) | entry::Stated::Keyed { command, .. } => command.to_vec(),
        _ => Vec::new(),
    }
}
