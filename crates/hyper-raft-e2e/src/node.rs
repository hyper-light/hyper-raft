//! One member as a process: hyper-raft driven over a UDP socket and a [`Wal`], with a key-value
//! store as its application. One thread does everything: it waits on the socket until the next
//! tick, steps what arrived, ticks when the tick is due, and drives the member's `Ready`.
//!
//! A write is answered once it is committed and applied, so an answered write is on a majority
//! of the members' disks; a read is answered by ReadIndex, once the leader has confirmed with a
//! quorum that it still leads and has applied through the index it was given, so a read never
//! returns what a newer leader has replaced.
use std::{
    collections::BTreeMap,
    io::ErrorKind,
    net::{SocketAddr, UdpSocket},
    time::{Duration, Instant},
};

use hyper_raft::{
    Config, RawNode, StateRole,
    proto::{Entry, Message, protocompat::PbMessage},
};

use crate::{
    wal::{Wal, WalError},
    wire::{self, Command, Control, Kind, Op, Outcome, Status},
};

/// The bytes of a Raft message's fields other than its entries, and the datagram's own header,
/// with room to spare: thirteen varint fields of at most eleven bytes with their tags (143
/// bytes; raft-rs `eraftpb.proto`) and the 13-byte header ([`wire`]), rounded up to a power of
/// two. An append's entries are bounded to a datagram less this.
const MESSAGE_ROOM: usize = 256;

/// Why the member stopped.
#[derive(Debug)]
pub enum NodeError {
    /// Its log refused.
    Wal(WalError),
    /// The core found its own state no longer adds up.
    Raft(hyper_raft::Error),
    /// The socket refused.
    Io(std::io::Error),
}

impl std::fmt::Display for NodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Wal(error) => write!(f, "{error}"),
            Self::Raft(error) => write!(f, "the core stopped: {error}"),
            Self::Io(error) => write!(f, "the socket: {error}"),
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
    /// The time between ticks.
    pub tick: Duration,
    /// When the process ends whatever happens, so that a member whose test died does not
    /// outlive it.
    pub deadline: Instant,
    /// The most keys the store holds; a write of a new key past it is refused, alike on every
    /// member.
    pub max_keys: usize,
    /// The most writes and reads one member waits to answer.
    pub max_pending: usize,
    /// The most entries the log holds.
    pub max_entries: usize,
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
        if wire::seal(self.sending) {
            self.send(to)?;
        }
        Ok(())
    }
}

/// The application: the key-value store, what it applied, and the writes it answers once
/// applied.
struct App {
    id: u64,
    max_keys: usize,
    store: BTreeMap<Vec<u8>, Vec<u8>>,
    applied: u64,
    digest: u64,
    writes: BTreeMap<u64, Asker>,
}

impl App {
    /// Applies `entries`, read where the log holds them, answering the writes this member
    /// proposed.
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
            if command.origin == self.id
                && let Some(asker) = self.writes.remove(&command.sequence)
            {
                reply.respond(asker.address, asker.id, &outcome)?;
            }
        }
        Ok(())
    }
}

/// A member, its socket, its store and who waits on it.
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
    leading: bool,
    received: Vec<u8>,
    sending: Vec<u8>,
    command: Vec<u8>,
}

impl Node {
    /// The member `settings` names, on `socket`, opened on `wal`.
    pub fn open(settings: Settings, socket: UdpSocket, wal: Wal) -> Result<Self, NodeError> {
        let max_size_per_msg =
            u64::try_from(wire::MAX_DATAGRAM.saturating_sub(MESSAGE_ROOM)).unwrap_or(u64::MAX);
        let config = Config {
            // focal's shell's settings (hyper-raft tests/support/mod.rs, Settings::shell).
            election_tick: 10,
            heartbeat_tick: 2,
            max_size_per_msg,
            check_quorum: true,
            pre_vote: true,
            seed: settings.id,
            ..Config::new(settings.id)
        };
        let raw = heard(RawNode::new(&config, wal))?.ok_or(NodeError::Raft(
            hyper_raft::Error::Settings("the member would not open"),
        ))?;
        Ok(Self {
            raw,
            socket,
            peers: Vec::new(),
            isolated: false,
            app: App {
                id: settings.id,
                max_keys: settings.max_keys,
                store: BTreeMap::new(),
                applied: 0,
                digest: 0,
                writes: BTreeMap::new(),
            },
            next_sequence: 0,
            reads: BTreeMap::new(),
            confirmed: Vec::new(),
            leading: false,
            received: vec![0; wire::MAX_DATAGRAM],
            sending: Vec::with_capacity(wire::MAX_DATAGRAM),
            command: Vec::new(),
            settings,
        })
    }

    /// Runs until the deadline, or until the member stops.
    pub fn run(&mut self) -> Result<(), NodeError> {
        let mut next_tick = Instant::now()
            .checked_add(self.settings.tick)
            .unwrap_or(self.settings.deadline);
        while Instant::now() < self.settings.deadline {
            self.receive_until(next_tick)?;
            let now = Instant::now();
            if now >= next_tick {
                heard(self.raw.tick())?;
                next_tick = next_tick
                    .checked_add(self.settings.tick)
                    .unwrap_or(self.settings.deadline)
                    .max(now);
            }
            self.drive()?;
        }
        Ok(())
    }

    /// Waits for a datagram until `until`, then takes what else has arrived without waiting,
    /// at most what one turn of the loop takes before it drives the member again.
    fn receive_until(&mut self, until: Instant) -> Result<(), NodeError> {
        let wait = until.saturating_duration_since(Instant::now());
        if wait.is_zero() {
            return Ok(());
        }
        self.socket.set_read_timeout(Some(wait))?;
        if !self.receive_one()? {
            return Ok(());
        }
        self.socket.set_nonblocking(true)?;
        // A turn takes as many datagrams as the member has voters to hear from and askers to
        // answer, so that one busy peer never holds the others' answers back past a turn.
        let turn = self
            .settings
            .max_pending
            .saturating_add(self.settings.voters.len());
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
        outcome
    }

    /// One datagram, if one arrives in time; false when none did.
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

    /// Whether this member leads and has room for one more asker; the refusal sent when not.
    fn admits(&mut self, asker: Asker, waiting: usize) -> Result<bool, NodeError> {
        if self.raw.raft.state() != StateRole::Leader {
            let leader = self.raw.raft.leader_id();
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
        if !self.admits(asker, self.app.writes.len())? {
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
        match heard(self.raw.propose(Vec::new(), data))? {
            Some(()) => {
                self.app.writes.insert(sequence, asker);
                Ok(())
            }
            None => self.respond(asker.address, asker.id, &Outcome::Busy),
        }
    }

    fn read(&mut self, asker: Asker, key: &[u8]) -> Result<(), NodeError> {
        if !self.admits(asker, self.reads.len())? {
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
            if message.encode(&mut self.sending).is_err() || !wire::seal(&mut self.sending) {
                // Too long for a datagram: dropped, and Raft sends again.
                continue;
            }
            self.reply().send(address)?;
        }
        Ok(())
    }

    /// Everything the member asks of its owner, in the order Raft requires: a leader's messages
    /// at once; what is to persist, written from where the member holds it and flushed; a
    /// follower's messages once that is durable; then what is committed, applied from the log.
    /// Nothing is copied: the member gives its `Ready`s in place, and the log keeps the very
    /// entries the member gives up once they are durable.
    fn drive(&mut self) -> Result<(), NodeError> {
        while self.raw.has_ready() {
            let mut ready = heard(self.raw.ready_in_place())?.ok_or(NodeError::Raft(
                hyper_raft::Error::Invariant("a ready that would not come"),
            ))?;
            self.send_raft(ready.take_messages())?;
            let persist = self.raw.to_persist();
            persist.store.write(persist.entries, ready.hard_state())?;
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
            self.answer_reads()?;
        }
        Ok(())
    }

    /// Applies `[first, last]`, read where the log holds it.
    fn apply(&mut self, first: u64, last: u64) -> Result<(), NodeError> {
        let Self {
            raw,
            app,
            socket,
            sending,
            ..
        } = self;
        let entries = raw.store().held(first, last)?;
        app.apply(entries, &mut Reply { socket, sending })
    }

    fn confirm(&mut self, index: u64, context: &[u8]) {
        let Ok(sequence) = <[u8; 8]>::try_from(context).map(u64::from_le_bytes) else {
            return;
        };
        if self.reads.contains_key(&sequence) && self.confirmed.len() < self.settings.max_pending {
            self.confirmed.push((index, sequence));
        }
    }

    /// A member that stopped leading answers everyone it kept waiting: they ask the new leader.
    fn lead_or_let_go(&mut self) -> Result<(), NodeError> {
        let leading = self.raw.raft.state() == StateRole::Leader;
        if self.leading && !leading {
            let leader = self.raw.raft.leader_id();
            let writes = std::mem::take(&mut self.app.writes);
            let reads = std::mem::take(&mut self.reads);
            self.confirmed.clear();
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
