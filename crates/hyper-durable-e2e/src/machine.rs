//! The member's application: a key-value store kept in memory and rebuilt from the log at every
//! start, as focal's state machines are (the log is the state machine's durability). A key under
//! [`ACTS`] is one a member acts on at its next start, before its group tells it anything, as a
//! host acts on an upgrade fence (focal F17, `cli_upgrade`): the shell applies it only once the
//! member's durable commit covers it (`docs/durable.md` §4.1, I5).
use std::collections::BTreeMap;

use hyper_durable::{EntryRef, Fatal, Point, StateMachine};
use hyper_raft::proto::{ConfChangeV2, ConfState};
use hyper_raft_e2e::wire;

/// What a key under which a member acts at its next start begins with.
pub const ACTS: &[u8] = b"fence/";

/// A write applied: which member proposed it, its number there, and the index it was applied at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Applied {
    /// The member that proposed it.
    pub origin: u64,
    /// The proposer's number for it.
    pub sequence: u64,
    /// The index it was applied at.
    pub index: u64,
    /// Whether the store took it: a new key past the store's bound is refused, alike everywhere.
    pub taken: bool,
}

/// The store.
#[derive(Debug)]
pub struct Kv {
    rows: BTreeMap<Vec<u8>, Vec<u8>>,
    max_keys: usize,
    applied: Point,
    configuration: ConfState,
    digest: u64,
    /// Entries under [`ACTS`] applied since the member last looked, by index: what it acted on.
    acted: Vec<u64>,
}

impl Kv {
    /// An empty store of at most `max_keys` keys under `configuration`.
    pub fn new(configuration: ConfState, max_keys: usize) -> Self {
        Self {
            rows: BTreeMap::new(),
            max_keys,
            applied: Point::default(),
            configuration,
            digest: 0,
            acted: Vec::new(),
        }
    }

    /// The value of `key`.
    pub fn get(&self, key: &[u8]) -> Option<&Vec<u8>> {
        self.rows.get(key)
    }

    /// A digest of everything applied, in order (FNV-1a over each entry's index and data).
    pub fn digest(&self) -> u64 {
        self.digest
    }

    /// Takes the entries acted on at start since the last look.
    pub fn take_acted(&mut self) -> Vec<u64> {
        std::mem::take(&mut self.acted)
    }

    fn fold(&mut self, index: u64, data: &[u8]) {
        let mut digest = self.digest ^ 0xcbf2_9ce4_8422_2325;
        for byte in index.to_le_bytes().iter().chain(data) {
            digest = (digest ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3);
        }
        self.digest = digest;
    }
}

impl StateMachine for Kv {
    type Answer = Applied;

    fn apply(&mut self, entry: &EntryRef<'_>, answers: &mut Vec<Applied>) -> Result<(), Fatal> {
        self.fold(entry.index, entry.data);
        self.applied = Point {
            index: entry.index,
            term: entry.term,
        };
        let Some(command) = wire::read_command(entry.data) else {
            // A leader's empty entry, which changes no row.
            return Ok(());
        };
        if command.key.starts_with(ACTS) {
            self.acted.push(entry.index);
        }
        let taken = self.rows.len() < self.max_keys || self.rows.contains_key(command.key);
        if taken {
            self.rows
                .insert(command.key.to_vec(), command.value.to_vec());
        }
        answers.push(Applied {
            origin: command.origin,
            sequence: command.sequence,
            index: entry.index,
            taken,
        });
        Ok(())
    }

    fn apply_change(
        &mut self,
        at: Point,
        _change: &ConfChangeV2,
        configuration: &ConfState,
    ) -> Result<(), Fatal> {
        self.applied = at;
        self.configuration = configuration.clone();
        Ok(())
    }

    /// Nothing is durable of the store's own: a member rebuilds it from its log at every start.
    fn durable(&self) -> Point {
        Point::default()
    }

    fn configuration(&self) -> &ConfState {
        &self.configuration
    }

    fn acts_at_start(&self, entry: &EntryRef<'_>) -> bool {
        wire::read_command(entry.data).is_some_and(|c| c.key.starts_with(ACTS))
    }

    /// The members compact nothing, so none is ever behind the log's start: a store rebuilt from
    /// the log could not hold an installed image durably.
    fn image(&mut self, _into: &mut Vec<u8>) -> Result<(Point, ConfState), Fatal> {
        Err(Fatal("this store keeps no snapshots"))
    }

    fn image_bytes(&self) -> Option<u64> {
        None
    }

    fn install(&mut self, _: &[u8], _: Point, _: &ConfState) -> Result<(), Fatal> {
        Err(Fatal("this store keeps no snapshots"))
    }

    fn persist(&mut self) -> Result<(), Fatal> {
        Ok(())
    }
}
