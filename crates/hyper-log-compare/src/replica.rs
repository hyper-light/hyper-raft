//! The replica's path: a group of three members, each a hyper-raft core on its own log on a
//! simulated device, driven in one process to commit one entry at a time, as mantle's range
//! group is (mantle `crates/range/tests/group.rs` at `shared-log` 03bda33, its `settle`; the
//! measurement in mantle `docs/measurements/2026-10-01-shared-log.md`, "The replica's path").
//!
//! Each member is driven as mantle's replica drives its core with `drive` (mantle
//! `crates/range/src/replica.rs`, `drive_ready` with `wait`): the core's `Ready` taken, the
//! messages a leader sends before its write given out, the ready's update written in the log's
//! parts and waited for, then the messages that waited for it, the committed entries applied
//! and the core advanced. Its storage is mantle's `LogStore` (`crates/range/src/store.rs`) over
//! each log: for hyper-log as mantle has it on hyper-log, keeping the group's bounds between its
//! own writes, and for mantle-log as it was on mantle-log at `a2021df`. Applying an entry folds
//! it into a digest, which every member must agree on.
//!
//! Per committed entry the run counts the wall time, every call into the log by kind, and every
//! allocation and reallocation in the process, the logs' threads included.

use std::cell::Cell;
use std::collections::VecDeque;
use std::time::Instant;

use hyper_measure::alloc;
use hyper_raft::proto::{ConfState, Entry, EntryType, HardState, Message, Snapshot};
use hyper_raft::{Config, InitialState, RawNode, Ready, Storage, StorageError};

/// The group's members.
const MEMBERS: u64 = 3;
/// The range's group, as mantle's group test names it.
const GROUP: u128 = 0x0072_616e_6765;
/// Entries committed before the measured ones, so that every buffer has grown.
pub const WARM: u64 = 50;
/// Entries measured.
pub const MEASURED: u64 = 300;
/// Bytes of each entry's data: a session's registration, as mantle's group test commits.
const ENTRY_BYTES: usize = 32;
/// Rounds of delivery `settle` allows before it calls the group stuck, as mantle's does.
const SETTLE_ROUNDS: usize = 10_000;

/// Calls a member's storage made into its log.
#[derive(Debug, Default, Clone, Copy)]
pub struct Calls {
    pub writes: u64,
    pub views: u64,
    pub terms: u64,
    pub fetches: u64,
    /// Reads answered by another thread: a round trip to the log's owner.
    pub asked: u64,
}

impl Calls {
    fn add(&mut self, other: Calls) {
        self.writes += other.writes;
        self.views += other.views;
        self.terms += other.terms;
        self.fetches += other.fetches;
        self.asked += other.asked;
    }
}

/// The counters a store keeps of its calls.
#[derive(Default)]
struct Counted {
    writes: Cell<u64>,
    views: Cell<u64>,
    terms: Cell<u64>,
    fetches: Cell<u64>,
}

impl Counted {
    fn bump(cell: &Cell<u64>) {
        cell.set(cell.get() + 1);
    }

    fn read(&self) -> Calls {
        Calls {
            writes: self.writes.get(),
            views: self.views.get(),
            terms: self.terms.get(),
            fetches: self.fetches.get(),
            asked: 0,
        }
    }
}

/// What a member's storage does besides the core's `Storage`: write a ready's update.
pub trait Durable: Storage + Sized {
    fn open(id: u64) -> Self;
    fn persist(&self, ready: &Ready) -> Result<(), String>;
    fn calls(&self) -> Calls;
}

/// An entry's bytes in the log: its kind, its context's length, its context and its data, as
/// mantle's store encodes it.
fn encode_entry(e: &Entry) -> Vec<u8> {
    let mut out = Vec::with_capacity(e.context.len() + e.data.len() + 5);
    out.push(e.entry_type.byte());
    out.extend_from_slice(&(e.context.len() as u32).to_le_bytes());
    out.extend_from_slice(&e.context);
    out.extend_from_slice(&e.data);
    out
}

fn decode_entry(index: u64, term: u64, bytes: &[u8]) -> Option<Entry> {
    let (&kind, rest) = bytes.split_first()?;
    let entry_type = EntryType::from_byte(kind)?;
    let (len, rest) = rest.split_at_checked(4)?;
    let len = u32::from_le_bytes(len.try_into().ok()?) as usize;
    let (context, data) = rest.split_at_checked(len)?;
    Some(Entry {
        entry_type,
        term,
        index,
        data: data.to_vec(),
        context: context.to_vec(),
    })
}

fn conf() -> ConfState {
    ConfState {
        voters: (1..=MEMBERS).collect(),
        ..ConfState::default()
    }
}

/// mantle's group test's log (`log_config`), for either log's `Config`.
macro_rules! log_config {
    ($krate:ident) => {
        $krate::Config {
            segment_bytes: 64 * 4096,
            max_segments: 16,
            max_groups: 16,
            group_entries: 1 << 18,
            group_bytes: 1 << 24,
            group_cache: 1 << 16,
            queue_submissions: 64,
            waits: $krate::Waits::Measured,
        }
    };
}

/// A ready's update, as mantle's replica lays it out (`update_of`), for either log's types.
macro_rules! update_of {
    ($krate:ident, $ready:expr) => {{
        let ready: &Ready = $ready;
        let entries = match ready.entries() {
            [] => None,
            all @ [first, ..] => Some($krate::Entries {
                first: first.index,
                entries: all
                    .iter()
                    .map(|e| $krate::Entry {
                        term: e.term,
                        bytes: encode_entry(e).into(),
                    })
                    .collect(),
            }),
        };
        let hard_state = ready.hard_state().map(|h| $krate::HardState {
            term: h.term,
            vote: h.vote,
            commit: h.commit,
        });
        (entries.is_some() || hard_state.is_some()).then(|| $krate::Update {
            entries,
            hard_state,
            ..$krate::Update::default()
        })
    }};
}

/// hyper-log, its group's storage on the group's handle (`GroupLog`): the store asks the handle
/// for everything, and counts the reads the handle had to ask the log's owner for.
pub mod hyper {
    use super::*;
    use std::cell::RefCell;

    use hyper_block::buf::Alignment;
    use hyper_block::sim::SimFile;
    use hyper_log::{Fetched, GroupLog, Log, LogError};

    pub struct Store {
        // Dropped before the log, so that its release reaches the owner while it runs.
        group: RefCell<GroupLog<SimFile>>,
        log: Log<SimFile>,
        fetched: Cell<Option<Fetched>>,
        counted: Counted,
    }

    fn storage(e: &LogError) -> StorageError {
        match e {
            LogError::Compacted { .. } => StorageError::Compacted,
            LogError::Unavailable { .. } => StorageError::Unavailable,
            _ => StorageError::Other("the log failed"),
        }
    }

    impl Store {
        fn bounds(&self) -> Result<(u64, u64), StorageError> {
            Counted::bump(&self.counted.views);
            let (start, last) = self.group.borrow().bounds().map_err(|e| storage(&e))?;
            Ok((start.index, last))
        }

        fn fetch(&self, low: u64, high: u64, max: u64) -> Result<Fetched, StorageError> {
            Counted::bump(&self.counted.fetches);
            let into = self.fetched.take().unwrap_or_default();
            self.group
                .borrow()
                .fetch(low, high, max, into)
                .map_err(|e| storage(&e))
        }

        fn keep(&self, fetched: Fetched) {
            let held: u64 = fetched.iter().map(|(_, b)| b.len() as u64).sum();
            if held <= self.log.config().segment_bytes {
                self.fetched.set(Some(fetched));
            }
        }
    }

    impl Storage for Store {
        fn initial_state(&self) -> Result<InitialState, StorageError> {
            Counted::bump(&self.counted.views);
            let view = self.group.borrow().view().map_err(|e| storage(&e))?;
            let hard_state = view
                .and_then(|v| v.hard_state)
                .map(|h| HardState {
                    term: h.term,
                    vote: h.vote,
                    commit: h.commit,
                })
                .unwrap_or_default();
            Ok(InitialState {
                hard_state,
                configuration: conf(),
                proposals: Vec::new(),
            })
        }

        fn entries(
            &self,
            low: u64,
            high: u64,
            max_bytes: u64,
            into: &mut Vec<Entry>,
        ) -> Result<(), StorageError> {
            if low >= high {
                return Ok(());
            }
            let fetched = self.fetch(low, high, max_bytes)?;
            let decoded = (low..).zip(fetched.iter()).try_for_each(|(i, (t, b))| {
                into.push(decode_entry(i, t, b).ok_or(StorageError::Other("decode"))?);
                Ok(())
            });
            self.keep(fetched);
            decoded
        }

        fn any_entry(
            &self,
            low: u64,
            high: u64,
            predicate: &mut dyn FnMut(&Entry) -> bool,
        ) -> Result<bool, StorageError> {
            let page = self.log.config().segment_bytes;
            let mut next = low;
            while next < high {
                let fetched = self.fetch(next, high, page)?;
                if fetched.is_empty() {
                    return Err(StorageError::Unavailable);
                }
                let mut found = false;
                for (t, b) in fetched.iter() {
                    let e = decode_entry(next, t, b).ok_or(StorageError::Other("decode"))?;
                    if predicate(&e) {
                        found = true;
                        break;
                    }
                    next += 1;
                }
                self.keep(fetched);
                if found {
                    return Ok(true);
                }
            }
            Ok(false)
        }

        fn term(&self, index: u64) -> Result<u64, StorageError> {
            Counted::bump(&self.counted.terms);
            match self.group.borrow().term(index) {
                Ok(t) => Ok(t),
                Err(LogError::Unavailable { .. }) if index == 0 => Ok(0),
                Err(e) => Err(storage(&e)),
            }
        }

        fn first_index(&self) -> Result<u64, StorageError> {
            Ok(self.bounds()?.0 + 1)
        }

        fn last_index(&self) -> Result<u64, StorageError> {
            Ok(self.bounds()?.1)
        }

        fn snapshot(&self, _request_index: u64, _to: u64) -> Result<Snapshot, StorageError> {
            Err(StorageError::SnapshotTemporarilyUnavailable)
        }
    }

    impl Durable for Store {
        fn open(id: u64) -> Self {
            let file = SimFile::new(
                Alignment::new(4096).unwrap(),
                Alignment::new(512).unwrap(),
                id,
            )
            .unwrap();
            let log = Log::create(file, log_config!(hyper_log), 0x6c6f67 + u128::from(id)).unwrap();
            let group = RefCell::new(log.group(GROUP).unwrap());
            Self {
                group,
                log,
                fetched: Cell::new(None),
                counted: Counted::default(),
            }
        }

        fn persist(&self, ready: &Ready) -> Result<(), String> {
            let Some(update) = update_of!(hyper_log, ready) else {
                return Ok(());
            };
            let parts = self.log.parts(GROUP, update).map_err(|e| e.to_string())?;
            let mut group = self.group.borrow_mut();
            for part in parts {
                Counted::bump(&self.counted.writes);
                group.write(part).map_err(|e| e.to_string())?;
            }
            Ok(())
        }

        fn calls(&self) -> Calls {
            Calls {
                asked: self.group.borrow().asked(),
                ..self.counted.read()
            }
        }
    }
}

/// mantle-log at mantle `a2021df`, with mantle's store on it as it was there.
pub mod mantle {
    use super::*;
    use mantle_disk::buf::Alignment;
    use mantle_disk::sim::SimFile;
    use mantle_log::{Log, LogError};

    pub struct Store {
        log: Log<SimFile>,
        counted: Counted,
    }

    fn storage(e: &LogError) -> StorageError {
        match e {
            LogError::Compacted { .. } => StorageError::Compacted,
            LogError::Unavailable { .. } => StorageError::Unavailable,
            _ => StorageError::Other("the log failed"),
        }
    }

    impl Store {
        fn bounds(&self) -> Result<(u64, u64), StorageError> {
            Counted::bump(&self.counted.views);
            let view = self.log.view(GROUP).map_err(|e| storage(&e))?;
            Ok(view.map_or((0, 0), |v| (v.start.index, v.last)))
        }

        fn entries_of(&self, low: u64, high: u64, max: u64) -> Result<Vec<Entry>, StorageError> {
            Counted::bump(&self.counted.fetches);
            let got = self
                .log
                .entries(GROUP, low, high, max)
                .map_err(|e| storage(&e))?;
            (low..)
                .zip(got)
                .map(|(i, e)| {
                    decode_entry(i, e.term, &e.bytes).ok_or(StorageError::Other("decode"))
                })
                .collect()
        }
    }

    impl Storage for Store {
        fn initial_state(&self) -> Result<InitialState, StorageError> {
            Counted::bump(&self.counted.views);
            let view = self.log.view(GROUP).map_err(|e| storage(&e))?;
            let hard_state = view
                .and_then(|v| v.hard_state)
                .map(|h| HardState {
                    term: h.term,
                    vote: h.vote,
                    commit: h.commit,
                })
                .unwrap_or_default();
            Ok(InitialState {
                hard_state,
                configuration: conf(),
                proposals: Vec::new(),
            })
        }

        fn entries(
            &self,
            low: u64,
            high: u64,
            max_bytes: u64,
            into: &mut Vec<Entry>,
        ) -> Result<(), StorageError> {
            if low >= high {
                return Ok(());
            }
            into.extend(self.entries_of(low, high, max_bytes)?);
            Ok(())
        }

        fn any_entry(
            &self,
            low: u64,
            high: u64,
            predicate: &mut dyn FnMut(&Entry) -> bool,
        ) -> Result<bool, StorageError> {
            let page = self.log.config().segment_bytes;
            let mut next = low;
            while next < high {
                let page = self.entries_of(next, high, page)?;
                if page.is_empty() {
                    return Err(StorageError::Unavailable);
                }
                for e in &page {
                    if predicate(e) {
                        return Ok(true);
                    }
                    next += 1;
                }
            }
            Ok(false)
        }

        fn term(&self, index: u64) -> Result<u64, StorageError> {
            Counted::bump(&self.counted.terms);
            match self.log.term(GROUP, index) {
                Ok(t) => Ok(t),
                Err(LogError::Unavailable { .. }) if index == 0 => Ok(0),
                Err(e) => Err(storage(&e)),
            }
        }

        fn first_index(&self) -> Result<u64, StorageError> {
            Ok(self.bounds()?.0 + 1)
        }

        fn last_index(&self) -> Result<u64, StorageError> {
            Ok(self.bounds()?.1)
        }

        fn snapshot(&self, _request_index: u64, _to: u64) -> Result<Snapshot, StorageError> {
            Err(StorageError::SnapshotTemporarilyUnavailable)
        }
    }

    impl Durable for Store {
        fn open(id: u64) -> Self {
            let file = SimFile::new(
                Alignment::new(4096).unwrap(),
                Alignment::new(512).unwrap(),
                id,
            )
            .unwrap();
            let log =
                Log::create(file, log_config!(mantle_log), 0x6c6f67 + u128::from(id)).unwrap();
            Self {
                log,
                counted: Counted::default(),
            }
        }

        fn persist(&self, ready: &Ready) -> Result<(), String> {
            let Some(update) = update_of!(mantle_log, ready) else {
                return Ok(());
            };
            let parts = self.log.parts(GROUP, update).map_err(|e| e.to_string())?;
            for part in parts {
                Counted::bump(&self.counted.writes);
                self.log
                    .submit_waiting(GROUP, part)
                    .and_then(|p| p.wait())
                    .map_err(|e| e.to_string())?;
            }
            Ok(())
        }

        fn calls(&self) -> Calls {
            self.counted.read()
        }
    }
}

/// What each member states (`hyper_raft::Limits::derive`): a message of its appends' 64 KiB and
/// its fixed record, the group's members, queues of 16 MiB each, past anything the run holds, and
/// one write out at a time.
fn limits() -> hyper_raft::Limits {
    hyper_raft::Limits::derive(hyper_raft::Stated {
        message: (1 << 16) + hyper_raft::wire::MESSAGE_RECORD_FIXED_BYTES,
        members: MEMBERS as usize,
        memory: 16 << 20,
        depth: 1,
    })
    .expect("the run's statement gives its bounds")
}

/// One member: its core and what it has applied.
struct Member<S: Durable> {
    node: RawNode<S>,
    applied: u64,
    digest: u64,
}

impl<S: Durable> Member<S> {
    fn open(id: u64) -> Self {
        let config = Config {
            election_tick: 10,
            heartbeat_tick: 2,
            max_size_per_msg: 1 << 16,
            max_inflight_msgs: 16,
            max_uncommitted_size: 1 << 20,
            max_committed_size_per_ready: 1 << 22,
            check_quorum: true,
            pre_vote: true,
            seed: id,
            ..Config::new(id, limits())
        };
        Self {
            node: RawNode::new(&config, S::open(id)).unwrap(),
            applied: 0,
            digest: 0xcbf2_9ce4_8422_2325,
        }
    }

    fn apply(&mut self, entries: Vec<Entry>) {
        for e in entries {
            for &b in e.data.iter().chain(&e.index.to_le_bytes()) {
                self.digest = (self.digest ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
            }
            self.applied = e.index;
        }
    }

    /// mantle's `drive` with `wait`: every ready persisted and finished.
    fn drive(&mut self, out: &mut VecDeque<Message>) {
        while self.node.has_ready() {
            let mut ready = self.node.ready().unwrap();
            out.extend(ready.take_messages());
            self.node.store().persist(&ready).unwrap();
            out.extend(ready.take_persisted_messages());
            let committed = ready.take_committed_entries();
            self.apply(committed);
            let mut light = self.node.advance_append(ready).unwrap();
            out.extend(light.take_messages());
            let committed = light.take_committed_entries();
            self.apply(committed);
            self.node.advance_apply_to(self.applied).unwrap();
        }
    }
}

/// Drives every member until no message is left in flight, delivering in order (mantle's
/// `settle`).
fn settle<S: Durable>(members: &mut [Member<S>], wire: &mut VecDeque<Message>) {
    for _ in 0..SETTLE_ROUNDS {
        for m in members.iter_mut() {
            m.drive(wire);
        }
        let Some(message) = wire.pop_front() else {
            return;
        };
        let to = message.to as usize - 1;
        let _ = members[to].node.step(message);
    }
    panic!("the group never settled");
}

/// What one run measured, per committed entry: the wall time, the storage's calls by kind
/// (writes, views, terms, fetches, and the reads another thread answered), and the process's
/// allocations and reallocations.
#[derive(Debug, Clone, Copy, Default)]
pub struct Run {
    pub wall_ns: f64,
    pub calls: [f64; 5],
    pub allocations: f64,
    pub reallocations: f64,
    /// Context switches the process's threads took: the hand-offs between them.
    pub switches: f64,
}

impl Run {
    pub fn line(&self) -> String {
        format!(
            "{} {} {} {} {} {} {} {} {}",
            self.wall_ns,
            self.calls[0],
            self.calls[1],
            self.calls[2],
            self.calls[3],
            self.calls[4],
            self.allocations,
            self.reallocations,
            self.switches
        )
    }

    pub fn parse(line: &str) -> Option<Self> {
        let f: Vec<&str> = line.split_whitespace().collect();
        Some(Self {
            wall_ns: f.first()?.parse().ok()?,
            calls: [
                f.get(1)?.parse().ok()?,
                f.get(2)?.parse().ok()?,
                f.get(3)?.parse().ok()?,
                f.get(4)?.parse().ok()?,
                f.get(5)?.parse().ok()?,
            ],
            allocations: f.get(6)?.parse().ok()?,
            reallocations: f.get(7)?.parse().ok()?,
            switches: f.get(8)?.parse().ok()?,
        })
    }
}

fn calls<S: Durable>(members: &[Member<S>]) -> Calls {
    let mut all = Calls::default();
    for m in members {
        all.add(m.node.store().calls());
    }
    all
}

/// One run: the group elects member 1, commits `WARM` entries, then `MEASURED` more, each
/// proposed by the leader and settled before the next.
pub fn run<S: Durable>() -> Run {
    let mut members: Vec<Member<S>> = (1..=MEMBERS).map(Member::open).collect();
    let mut wire = VecDeque::new();
    members[0].node.campaign().unwrap();
    settle(&mut members, &mut wire);
    assert_eq!(members[0].node.raft.leader_id(), 1, "member 1 leads");
    let propose = |members: &mut Vec<Member<S>>, wire: &mut VecDeque<Message>, i: u64| {
        let mut data = vec![0u8; ENTRY_BYTES];
        data[..8].copy_from_slice(&i.to_le_bytes());
        members[0].node.propose(Vec::new(), data).unwrap();
        settle(members, wire);
    };
    for i in 0..WARM {
        propose(&mut members, &mut wire, i);
    }
    let before = calls(&members);
    let applied = members[0].applied;
    let switched = hyper_measure::faults::switches().unwrap_or(0);
    alloc::begin_process();
    let started = Instant::now();
    for i in WARM..WARM + MEASURED {
        propose(&mut members, &mut wire, i);
    }
    let elapsed = started.elapsed();
    let counts = alloc::end_process();
    let switches = hyper_measure::faults::switches()
        .unwrap_or(0)
        .saturating_sub(switched);
    let committed = members[0].applied - applied;
    assert_eq!(committed, MEASURED, "every proposal committed once");
    assert!(
        members
            .iter()
            .all(|m| m.digest == members[0].digest && m.applied == members[0].applied),
        "the members applied the same entries"
    );
    let after = calls(&members);
    let n = committed as f64;
    let per = |a: u64, b: u64| (a - b) as f64 / n;
    Run {
        wall_ns: elapsed.as_nanos() as f64 / n,
        calls: [
            per(after.writes, before.writes),
            per(after.views, before.views),
            per(after.terms, before.terms),
            per(after.fetches, before.fetches),
            per(after.asked, before.asked),
        ],
        allocations: counts.allocations as f64 / n,
        reallocations: counts.reallocations as f64 / n,
        switches: switches as f64 / n,
    }
}
