//! One member as a process: hyper-raft driven over a UDP socket and a [`Wal`], with a key-value
//! store as its application. One thread does everything: it waits on the socket until its core's
//! deadline or its liveness stream's wake, whichever is first, takes what arrived, polls the
//! stream, and drives the member's `Ready`s.
//!
//! The member elects by suspicion (timing step L-2, `docs/timing.md` §2.9) and takes no ticks.
//! What its detectors believe of its peers is its node-pair liveness stream's word (L-3, §2.8),
//! wired as hyper-durable's owner wires it for one group and as hyper-durable-e2e's members are:
//! the pairs attached from the configuration, each change taken to the core (`suspect`, `trust`,
//! `restarted`), the group's timing derived by hyper-timing's law over what the stream measured
//! ([`stream::timing`]) and each pair charged the group's expected election, and every durable
//! write of the member's log handed to the stream as its flush proof. A heartbeat leaves only once
//! the log made a write durable after the previous was due: where the group wrote none, the member
//! writes its hard state again ([`Wal::prove`]), on the same file, so a disk that stops stops the
//! heartbeats with it. Heartbeats travel as the test's control datagrams
//! ([`stream::put_heartbeat`]), stamped when the member reads them, as hyper-tokio stamps a
//! datagram where the kernel cannot (`docs/timing.md` §3, item 5): the read delay counts as the
//! sender's. A member cut off ([`Control::Isolate`]) drops its heartbeats in and out with its Raft
//! messages, so its detectors and its peers' see the cut.
//!
//! A write is answered once it is committed and applied, so an answered write is on a majority
//! of the members' disks; a read is answered by ReadIndex, once the leader has confirmed with a
//! quorum that it still leads and has applied through the index it was given, so a read never
//! returns what a newer leader has replaced. A leader proposes no write its log already holds: a
//! client asks again when an answer does not come, and the write it asks again is answered at the
//! entry that holds it, so the log holds each write once ([`Wal`]'s bound).
use std::{
    collections::BTreeMap,
    io::ErrorKind,
    net::{SocketAddr, UdpSocket},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

use hyper_liveness::{Change, Liveness, PeerId, Settings as LiveSettings, Write as LiveWrite};
use hyper_raft::{
    Config, Elections, RawNode, StateRole,
    proto::{Entry, Message},
    wire::Record,
};
use hyper_timing::{Exposure, Trust};

use crate::{
    stream::{self, Asked, Report},
    wal::{Wal, WalError},
    wire::{self, Command, Control, Kind, Op, Outcome, Status},
};

/// The bytes of a member's datagram besides an append's entries: the datagram's header and the
/// sender's id ([`wire`]), and the message record's header, fixed fields and checksum
/// (`docs/raft.md` §3.1). The core counts an entry at its encoded bytes
/// ([`hyper_raft::proto::encoded_bytes`]), so entries bounded to the datagram less this fill it.
pub const MESSAGE_ROOM: usize = wire::HEADER
    + 8
    + hyper_raft::wire::HEADER_BYTES
    + hyper_raft::wire::MESSAGE_FIXED_BYTES
    + hyper_raft::wire::CHECKSUM_BYTES;

/// Why the member stopped.
#[derive(Debug)]
pub enum NodeError {
    /// Its log refused.
    Wal(WalError),
    /// The core found its own state no longer adds up.
    Raft(hyper_raft::Error),
    /// The socket refused.
    Io(std::io::Error),
    /// The liveness stream refused: a peer it cannot keep.
    Liveness(hyper_liveness::Refusal),
    /// The member has no run: its record could not be read or raised (`crate::run`).
    Run(crate::run::RunError),
}

impl std::fmt::Display for NodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Wal(error) => write!(f, "{error}"),
            Self::Raft(error) => write!(f, "the core stopped: {error}"),
            Self::Io(error) => write!(f, "the socket: {error}"),
            Self::Liveness(error) => write!(f, "the liveness stream: {error}"),
            Self::Run(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for NodeError {}

impl From<WalError> for NodeError {
    fn from(error: WalError) -> Self {
        Self::Wal(error)
    }
}
impl From<std::io::Error> for NodeError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

/// Holds the member's thread for `hold`, as the test ordered (`stream::put_hold`): it reads
/// nothing and answers nothing meanwhile, as a member deadlocked outside a write of its log.
#[allow(
    clippy::disallowed_methods,
    reason = "the member holds its thread for a hang the test ordered, as a deadlocked member holds it"
)]
fn hold_thread(hold: Duration) {
    std::thread::sleep(hold);
}

/// A refusal changed nothing and is the asker's to hear; only a fatal error stops the member.
fn heard<T>(outcome: hyper_raft::Result<T>) -> Result<Option<T>, NodeError> {
    match outcome {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.is_fatal() => Err(NodeError::Raft(error)),
        Err(_) => Ok(None),
    }
}

/// How a member runs, as its command line says.
#[derive(Clone, Debug)]
pub struct Settings {
    /// This member.
    pub id: u64,
    /// Every voter, this member included.
    pub voters: Vec<u64>,
    /// The most keys the store holds; a write of a new key past it is refused, alike on every
    /// member.
    pub max_keys: usize,
    /// The most writes and reads one member waits to answer.
    pub max_pending: usize,
    /// The most writes the scenario makes, which bounds the log ([`Wal::open`]).
    pub max_writes: usize,
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
    fn send(&self, to: SocketAddr) -> Result<(), NodeError> {
        match self.socket.send_to(self.sending, to) {
            Ok(_) => Ok(()),
            // A peer that is down refuses; Raft sends again.
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
    fn respond(&mut self, to: SocketAddr, id: u64, outcome: &Outcome) -> Result<(), NodeError> {
        wire::put_response(self.sending, id, outcome);
        if wire::seal(self.sending, self.datagram) {
            self.send(to)?;
        }
        Ok(())
    }
}

/// The application: the key-value store, what it applied, and the writes it answers once
/// applied.
struct App {
    max_keys: usize,
    store: BTreeMap<Vec<u8>, Vec<u8>>,
    applied: u64,
    digest: u64,
    /// The writes this member waits to answer: key → the value asked, and who asked.
    writes: BTreeMap<Vec<u8>, (Vec<u8>, Asker)>,
}

impl App {
    /// Applies `entries`, read where the log holds them, answering the writes this member
    /// waits on.
    fn apply(&mut self, entries: &[Entry], reply: &mut Reply<'_>) -> Result<(), NodeError> {
        for entry in entries {
            self.applied = entry.index;
            let mut digest = self.digest ^ 0xcbf2_9ce4_8422_2325;
            for byte in entry.index.to_le_bytes().iter().chain(&entry.data) {
                digest = (digest ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3);
            }
            self.digest = digest;
            let Some(command) = wire::read_command(&entry.data) else {
                continue;
            };
            let room = self.store.len() < self.max_keys || self.store.contains_key(command.key);
            let outcome = if room {
                self.store
                    .insert(command.key.to_vec(), command.value.to_vec());
                Outcome::Put(entry.index)
            } else {
                Outcome::Busy
            };
            let waited = self
                .writes
                .get(command.key)
                .is_some_and(|(value, _)| value.as_slice() == command.value);
            if waited && let Some((_, asker)) = self.writes.remove(command.key) {
                reply.respond(asker.address, asker.id, &outcome)?;
            }
        }
        Ok(())
    }
}

/// A member, its socket, its store, its liveness stream and who waits on it.
pub struct Node {
    raw: RawNode<Wal>,
    socket: UdpSocket,
    settings: Settings,
    peers: Vec<(u64, SocketAddr)>,
    isolated: bool,
    app: App,
    next_sequence: u64,
    reads: BTreeMap<u64, (Asker, Vec<u8>)>,
    /// Reads confirmed at an index, waiting for it to be applied.
    confirmed: Vec<(u64, u64)>,
    /// The term this member leads in, as it last looked; the askers it keeps waiting were taken
    /// in it.
    leading: Option<u64>,
    received: Vec<u8>,
    sending: Vec<u8>,
    /// The most bytes the socket sends in one datagram ([`wire::largest`]).
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
    /// The restarts of its peers the stream reported.
    restarts: u64,
    /// The time its thread has spent in the writes of its log, all told: time it could neither
    /// read its socket nor move its group.
    blocked: u64,
    /// The longest one write of its log took to be durable.
    flush_most: u64,
    /// The longest it went between two reads of its socket: how long it could not answer.
    turn_most: u64,
}

impl Node {
    /// The member `settings` names, on `socket`, opened on `wal`, in its run `run`
    /// (`crate::run::raise`).
    #[allow(
        clippy::disallowed_methods,
        reason = "real processes on the host's clock, threads and environment (CLAUDE.md §1a, end to end)"
    )]
    pub fn open(
        settings: Settings,
        run: u64,
        socket: UdpSocket,
        wal: Wal,
    ) -> Result<Self, NodeError> {
        let datagram = wire::largest(&socket)?;
        let max_size_per_msg =
            u64::try_from(datagram.saturating_sub(MESSAGE_ROOM)).unwrap_or(u64::MAX);
        let config = Config {
            elections: Elections::Suspicion,
            max_size_per_msg,
            check_quorum: true,
            pre_vote: true,
            seed: settings.id,
            ..Config::new(settings.id)
        };
        let raw = heard(RawNode::new(&config, wal))?.ok_or(NodeError::Raft(
            hyper_raft::Error::Settings("the member would not open"),
        ))?;
        let liveness = Liveness::new(LiveSettings {
            local: settings.id,
            run,
            max_peers: hyper_raft::MAX_MEMBERS,
            history: Exposure::new(),
        })
        .map_err(NodeError::Liveness)?;
        Ok(Self {
            raw,
            socket,
            peers: Vec::new(),
            isolated: false,
            app: App {
                max_keys: settings.max_keys,
                store: BTreeMap::new(),
                applied: 0,
                digest: 0,
                writes: BTreeMap::new(),
            },
            next_sequence: 0,
            reads: BTreeMap::new(),
            confirmed: Vec::new(),
            leading: None,
            received: vec![0; wire::MAX_DATAGRAM],
            sending: Vec::with_capacity(datagram),
            datagram,
            command: Vec::new(),
            epoch: Instant::now(),
            liveness,
            attached: Vec::new(),
            asked: Asked::default(),
            restarts: 0,
            blocked: 0,
            flush_most: 0,
            turn_most: 0,
            settings,
        })
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

    /// Runs until `stop` is set, which it reads once a turn, or until the member fails.
    pub fn run(&mut self, stop: &AtomicBool) -> Result<(), NodeError> {
        self.pairs()?;
        // The member drives once before it waits: a reopened member replays its log alone.
        self.drive()?;
        let mut read = self.now();
        while !stop.load(Ordering::Acquire) {
            self.live()?;
            self.turn_most = self.turn_most.max(self.now().saturating_sub(read));
            // Woken at the core's deadline or the stream's, whichever is first; by a datagram
            // otherwise, the parent's going among them.
            let until = [self.raw.deadline(), self.liveness.wake()]
                .into_iter()
                .flatten()
                .min();
            self.receive_until(until)?;
            read = self.now();
            self.live()?;
            self.measure()?;
            self.drive()?;
        }
        Ok(())
    }

    /// Keeps the stream told which peers the group has: its configuration's other members
    /// (hyper-durable's `Owner::pairs`). A peer attached is told to the core as the stream
    /// believes it then: a change the stream reported while the pair was not attached never
    /// reached the core.
    fn pairs(&mut self) -> Result<(), NodeError> {
        let id = self.settings.id;
        let mut now: Vec<PeerId> = self
            .raw
            .raft
            .configuration()
            .members()
            .filter(|member| *member != id)
            .collect();
        now.sort_unstable();
        now.dedup();
        for peer in &now {
            if self.attached.binary_search(peer).is_err() {
                self.liveness.attach(*peer).map_err(NodeError::Liveness)?;
                let told = if self.liveness.trust(*peer) == Some(Trust::Suspected) {
                    self.raw.suspect(*peer)
                } else {
                    self.raw.trust(*peer)
                };
                heard(told)?;
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

    /// The group's timing from what the stream measured, given to the core when it moved, and
    /// each pair charged the group's expected election (hyper-durable's `Owner::measure` for one
    /// group).
    fn measure(&mut self) -> Result<(), NodeError> {
        let voters = self.raw.raft.configuration().voters().to_vec();
        let Some((timing, span)) = stream::timing(&self.liveness, self.settings.id, &voters) else {
            return Ok(());
        };
        if self.raw.raft.timing() != Some(timing) {
            heard(self.raw.set_timing(timing))?;
        }
        for peer in &self.attached {
            // Every attached peer has its pair.
            let _ = self.liveness.set_election(*peer, span.election);
        }
        Ok(())
    }

    /// Polls the stream at the member's clock and does what it asks; once more after a write it
    /// asked for, which proves the heartbeats that waited on it.
    fn live(&mut self) -> Result<(), NodeError> {
        let now = self.now();
        self.liveness.poll(now, &mut self.asked);
        if self.act_on_liveness()? {
            let now = self.now();
            self.liveness.poll(now, &mut self.asked);
            self.act_on_liveness()?;
        }
        Ok(())
    }

    /// Carries out what the stream asked during the last call into it; whether it made the
    /// write the stream asked for.
    fn act_on_liveness(&mut self) -> Result<bool, NodeError> {
        let flushed = std::mem::take(&mut self.asked.flush);
        if flushed {
            let started = self.now();
            self.raw.store_mut().prove()?;
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

    fn send_heartbeat(&mut self, peer: PeerId, message: &[u8]) -> Result<(), NodeError> {
        if self.isolated {
            return Ok(());
        }
        let Some(address) = self
            .peers
            .iter()
            .find(|(id, _)| *id == peer)
            .map(|(_, address)| *address)
        else {
            // A peer whose address the member was not told: the heartbeat is lost to it.
            return Ok(());
        };
        stream::put_heartbeat(&mut self.sending, self.settings.id, message);
        if wire::seal(&mut self.sending, self.datagram) {
            self.reply().send(address)?;
        }
        Ok(())
    }

    /// A write of its log took from `started` to `durable`.
    fn wrote(&mut self, started: u64, durable: u64) {
        let took = durable.saturating_sub(started);
        self.blocked = self.blocked.saturating_add(took);
        self.flush_most = self.flush_most.max(took);
    }

    /// Takes a change the stream reported to the core.
    fn believe(&mut self, change: &Change) -> Result<(), NodeError> {
        let peer = change.peer();
        let told = match change {
            Change::Suspected(_) => self.raw.suspect(peer),
            Change::Trusted { .. } => self.raw.trust(peer),
            Change::Restarted { .. } => {
                self.restarts = self.restarts.saturating_add(1);
                self.raw.restarted(peer)
            }
        };
        heard(told).map(drop)
    }

    /// Waits for a datagram until `until` on the member's clock, or for one however long when
    /// nothing is due, then takes what else has arrived without waiting, at most what one turn
    /// of the loop takes before it drives the member again.
    ///
    /// A member whose deadline is already due waits for nothing, but it still takes what has
    /// arrived: its peers' answers and heartbeats are what its commits and its detectors are made
    /// of. A member that skipped its socket when behind (as one on a loaded machine is, at every
    /// turn) acted deaf: on ticks, as leader it stepped down by its quorum check with its
    /// followers' answers waiting unread in its socket.
    pub fn receive_until(&mut self, until: Option<u64>) -> Result<(), NodeError> {
        // A turn takes as many datagrams as the member has askers to answer and, from each
        // voter, a Raft message and a heartbeat, so that one busy peer never holds the others'
        // back past a turn; and the one it waited for, when it waited.
        let turn = self
            .settings
            .max_pending
            .saturating_add(self.settings.voters.len().saturating_mul(2));
        let mut most = turn;
        let wait = until.map(|at| Duration::from_nanos(at.saturating_sub(self.now())));
        if wait.is_none_or(|wait| !wait.is_zero()) {
            most = turn.saturating_add(1);
            if !wire::arrives(&self.socket, wait, &mut self.received)? {
                return Ok(());
            }
        }
        // The datagram waited for is taken with the rest, none of them waited on: a receive that
        // waits can lose what arrives as it times out (`wire::arrives`).
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

    /// One datagram, if one is there; false when none is.
    fn receive_one(&mut self) -> Result<bool, NodeError> {
        let (length, from) = match self.socket.recv_from(&mut self.received) {
            Ok(received) => received,
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                return Ok(false);
            }
            // A datagram this member sent was refused by a peer that is gone; nothing to read.
            Err(error) if error.kind() == ErrorKind::ConnectionReset => return Ok(true),
            Err(error) => return Err(error.into()),
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
        heard(self.raw.step(message)).map(|_| ())
    }

    fn reply(&mut self) -> Reply<'_> {
        Reply {
            socket: &self.socket,
            sending: &mut self.sending,
            datagram: self.datagram,
        }
    }
    fn respond(&mut self, to: SocketAddr, id: u64, outcome: &Outcome) -> Result<(), NodeError> {
        self.reply().respond(to, id, outcome)
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
        let raft = &self.raw.raft;
        Status {
            id: self.settings.id,
            term: raft.term(),
            leads: raft.state() == StateRole::Leader,
            leader: raft.leader_id(),
            commit: raft.log().committed(),
            applied: self.app.applied,
            last_index: raft.log().last_index().unwrap_or(0),
            digest: self.app.digest,
        }
    }

    /// What the member says of itself, its law and its detectors.
    fn report(&self) -> Report {
        let nanos = |d: Duration| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX);
        let timing = self.raw.raft.timing();
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
            writes: u64::try_from(
                self.raw
                    .store()
                    .entries()
                    .iter()
                    .filter(|entry| !entry.data.is_empty())
                    .count(),
            )
            .unwrap_or(u64::MAX),
            restarts: self.restarts,
            blocked_ns: self.blocked,
            flush_most_ns: self.flush_most,
            turn_most_ns: self.turn_most,
            waiting: u64::try_from(self.app.writes.len().saturating_add(self.reads.len()))
                .unwrap_or(u64::MAX),
            stray: self.stray(),
            suspected: peers(&|peer| self.liveness.trust(peer) == Some(Trust::Suspected)),
            heard: peers(&|peer| {
                self.liveness
                    .report(peer)
                    .is_some_and(|pair| pair.taken > 0)
            }),
        }
    }

    /// The writes this member waits to answer that no apply will answer: every one is to have an
    /// entry in its log above what it applied that writes the value asked, while it leads the
    /// term it took the asker in (`lead_or_let_go` lets them go when that term ends). A write
    /// kept otherwise waits for good, and a client asking it again is told nothing.
    fn stray(&self) -> u64 {
        let raft = &self.raw.raft;
        let leads = raft.state() == StateRole::Leader && self.leading == Some(raft.term());
        let applied = self.app.applied;
        let last = raft.log().last_index().unwrap_or(0);
        let held = |key: &[u8], value: &[u8]| {
            leads
                && applied < last
                && raft
                    .log()
                    .any_entry(applied.saturating_add(1), last.saturating_add(1), |entry| {
                        wire::read_command(&entry.data)
                            .is_some_and(|command| command.key == key && command.value == value)
                    })
                    .unwrap_or(false)
        };
        let stray = self
            .app
            .writes
            .iter()
            .filter(|(key, (value, _))| !held(key, value))
            .count();
        u64::try_from(stray).unwrap_or(u64::MAX)
    }

    /// Whether this member leads; the refusal sent when not.
    fn leads(&mut self, asker: Asker) -> Result<bool, NodeError> {
        self.lead_or_let_go()?;
        if self.leading.is_some() {
            return Ok(true);
        }
        let leader = self.raw.raft.leader_id();
        self.respond(asker.address, asker.id, &Outcome::NotLeader(leader))?;
        Ok(false)
    }

    /// Whether one more asker fits beside `waiting`; the refusal sent when not.
    fn room(&mut self, asker: Asker, waiting: usize) -> Result<bool, NodeError> {
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

    /// The index of the entry of this member's durable log that writes `value` to `key`, if
    /// one does. A leader's entries not yet durable are its own proposals, which it waits on.
    fn held(&self, key: &[u8], value: &[u8]) -> Option<u64> {
        self.raw
            .store()
            .entries()
            .iter()
            .find(|entry| {
                wire::read_command(&entry.data)
                    .is_some_and(|command| command.key == key && command.value == value)
            })
            .map(|entry| entry.index)
    }

    fn write(&mut self, asker: Asker, key: &[u8], value: &[u8]) -> Result<(), NodeError> {
        if !self.leads(asker)? {
            return Ok(());
        }
        // Asked again while it waits: the asker who asked last is answered.
        if let Some((waited, at)) = self.app.writes.get_mut(key)
            && waited.as_slice() == value
        {
            *at = asker;
            return Ok(());
        }
        if !self.room(asker, self.app.writes.len())? {
            return Ok(());
        }
        match self.held(key, value) {
            // Applied: answered as the store holds it, the write in place or refused for room.
            Some(index) if index <= self.app.applied => {
                let outcome = if self.app.store.get(key).is_some_and(|held| held == value) {
                    Outcome::Put(index)
                } else {
                    Outcome::Busy
                };
                self.respond(asker.address, asker.id, &outcome)
            }
            // In the leader's log, which it commits whole in its term: answered when applied.
            Some(_) => {
                self.app
                    .writes
                    .insert(key.to_vec(), (value.to_vec(), asker));
                Ok(())
            }
            None => self.propose(asker, key, value),
        }
    }

    fn propose(&mut self, asker: Asker, key: &[u8], value: &[u8]) -> Result<(), NodeError> {
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
        match heard(self.raw.propose(Vec::new(), data))? {
            Some(()) => {
                self.app
                    .writes
                    .insert(key.to_vec(), (value.to_vec(), asker));
                Ok(())
            }
            None => self.respond(asker.address, asker.id, &Outcome::Busy),
        }
    }

    fn read(&mut self, asker: Asker, key: &[u8]) -> Result<(), NodeError> {
        if !self.leads(asker)? || !self.room(asker, self.reads.len())? {
            return Ok(());
        }
        // A leader knows what is committed only once it has committed in its own term; until
        // then the core drops the read (raft.rs `read_index`), so the asker is told to ask again.
        if !self.raw.raft.commit_to_current_term() {
            return self.respond(asker.address, asker.id, &Outcome::Busy);
        }
        let sequence = self.sequence();
        heard(self.raw.read_index(sequence.to_le_bytes().to_vec()))?;
        self.reads.insert(sequence, (asker, key.to_vec()));
        Ok(())
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
            return self.act_on_liveness().map(drop);
        }
        if let Some((id, hold)) = stream::read_hold(body) {
            self.respond(from, id, &Outcome::Done)?;
            hold_thread(hold);
            return Ok(());
        }
        if let Some((id, stall)) = stream::read_stall(body) {
            self.raw.store_mut().stall_flushes(stall);
            return self.respond(from, id, &Outcome::Done);
        }
        if let Some(id) = stream::read_account_ask(body) {
            let voters = self.raw.raft.configuration().voters().to_vec();
            let election = stream::timing(&self.liveness, self.settings.id, &voters)
                .map(|(_, span)| span.election);
            let account = stream::account(&self.liveness, &self.attached, election);
            stream::put_account(&mut self.sending, id, &account);
            if wire::seal(&mut self.sending, self.datagram) {
                self.reply().send(from)?;
            }
            return Ok(());
        }
        if let Some(id) = stream::read_report_ask(body) {
            // Its askers settled first: a member that stopped leading in this turn lets them go
            // before it says what it is.
            self.lead_or_let_go()?;
            let report = self.report();
            stream::put_report(&mut self.sending, id, &report);
            if wire::seal(&mut self.sending, self.datagram) {
                self.reply().send(from)?;
            }
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
                // Too long for a datagram: dropped, and Raft sends again.
                continue;
            }
            self.reply().send(address)?;
        }
        Ok(())
    }

    /// Wakes the core at the member's clock — what was armed since is timed from now, and what is
    /// due is done — and acts on each `Ready` it then has, waking it again after each, until it
    /// has none.
    fn drive(&mut self) -> Result<(), NodeError> {
        loop {
            let now = self.now();
            heard(self.raw.wake(now))?;
            if !self.raw.has_ready() {
                return Ok(());
            }
            self.ready()?;
        }
    }

    /// Everything one `Ready` asks of its owner, in the order Raft requires: a leader's messages
    /// at once; what is to persist, written from where the member holds it and flushed, each
    /// write handed to the stream as a flush its heartbeats may prove; a follower's messages
    /// once that is durable; then what is committed, applied from the log. Nothing is copied: the
    /// member gives its `Ready`s in place, and the log keeps the very entries the member gives up
    /// once they are durable.
    fn ready(&mut self) -> Result<(), NodeError> {
        let mut ready = heard(self.raw.ready_in_place())?.ok_or(NodeError::Raft(
            hyper_raft::Error::Invariant("a ready that would not come"),
        ))?;
        self.send_raft(ready.take_messages())?;
        let started = self.now();
        let persist = self.raw.to_persist();
        let hard = ready.hard_state();
        let wrote = !persist.entries.is_empty() || hard.is_some();
        persist.store.write(persist.entries, hard)?;
        if wrote {
            let durable = self.now();
            self.wrote(started, durable);
            self.liveness.on_durable(LiveWrite::Log, started, durable);
        }
        for read in ready.take_read_states() {
            self.confirm(read.index, &read.request_ctx);
        }
        if let Some((first, last)) = ready.committed_range() {
            self.apply(first, last)?;
        }
        self.send_raft(ready.take_persisted_messages())?;
        let mut light = heard(
            self.raw
                .advance_append_keeping(ready, |wal, kept| wal.keep(kept.entries)),
        )?
        .ok_or(NodeError::Raft(hyper_raft::Error::Invariant(
            "a ready that would not advance",
        )))?;
        self.raw.store_mut().damage()?;
        if let Some(commit) = light.commit_index() {
            // A commit need not be durable to be acted on (raft-rs's `must_sync`): it is
            // written with the next record, and a member that restarts learns it again.
            self.raw.store_mut().set_commit(commit);
        }
        self.send_raft(light.take_messages())?;
        if let Some((first, last)) = light.committed_range() {
            self.apply(first, last)?;
        }
        heard(self.raw.advance_apply_to(self.app.applied))?;
        self.lead_or_let_go()?;
        self.answer_reads()
    }

    /// Applies `[first, last]`, read where the log holds it.
    fn apply(&mut self, first: u64, last: u64) -> Result<(), NodeError> {
        let Self {
            raw,
            app,
            socket,
            sending,
            datagram,
            ..
        } = self;
        let entries = raw.store().held(first, last)?;
        app.apply(
            entries,
            &mut Reply {
                socket,
                sending,
                datagram: *datagram,
            },
        )
    }

    fn confirm(&mut self, index: u64, context: &[u8]) {
        let Ok(sequence) = <[u8; 8]>::try_from(context).map(u64::from_le_bytes) else {
            return;
        };
        if self.reads.contains_key(&sequence) && self.confirmed.len() < self.settings.max_pending {
            self.confirmed.push((index, sequence));
        }
    }

    /// A member that stopped leading the term it took its askers in answers every one of them:
    /// they ask the new leader. Judged by the term, not the role: a member that stepped down and
    /// led again between two looks lost to the term between what it proposed in the first, and
    /// what it kept waiting would wait for good.
    fn lead_or_let_go(&mut self) -> Result<(), NodeError> {
        let raft = &self.raw.raft;
        let leading = (raft.state() == StateRole::Leader).then(|| raft.term());
        if self.leading.is_some() && self.leading != leading {
            let leader = self.raw.raft.leader_id();
            let writes = std::mem::take(&mut self.app.writes);
            let reads = std::mem::take(&mut self.reads);
            self.confirmed.clear();
            for asker in writes
                .into_values()
                .map(|(_, asker)| asker)
                .chain(reads.into_values().map(|(asker, _)| asker))
            {
                self.respond(asker.address, asker.id, &Outcome::NotLeader(leader))?;
            }
        }
        self.leading = leading;
        Ok(())
    }

    fn answer_reads(&mut self) -> Result<(), NodeError> {
        let applied = self.app.applied;
        let mut at = 0;
        while let Some((index, sequence)) = self.confirmed.get(at).copied() {
            if index > applied {
                at = at.saturating_add(1);
                continue;
            }
            self.confirmed.swap_remove(at);
            if let Some((asker, key)) = self.reads.remove(&sequence) {
                let value = self.app.store.get(&key).cloned();
                self.respond(asker.address, asker.id, &Outcome::Value(value))?;
            }
        }
        Ok(())
    }
}
