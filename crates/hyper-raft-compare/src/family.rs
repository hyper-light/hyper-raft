//! The cores that keep raft-rs's log and messages: raft-rs itself, focal-raft
//! at the revision hyper-raft came from and at the one mantle pins, and
//! hyper-raft. Each is driven as focal's shell drives it (hyper-raft
//! `tests/support/mod.rs`, `New::drain` and `Old::drain`): one `Ready` at a
//! time, its entries persisted, its messages sent, what it commits applied,
//! then advanced. hyper-raft in place gives each emptied queue of messages back
//! (`RawNode::recycle_messages`), as hyper-durable's replica does; the cores on
//! raft-proto have no such call.
//!
//! Every one of them keeps its log on the same in-memory [`Disk`], so what
//! the owner does is the same for each and is set aside from the counts of
//! what the core does (`hyper_measure::alloc::aside`).
/// One line's in-memory disk and the owner's work on it, over that line's types: raft-proto's
/// for raft-rs and focal-raft, hyper-raft's own for hyper-raft (`docs/raft.md` §3.1). Each
/// pages by its own measure of an entry's bytes.
macro_rules! line {
    ($types:path, $measure:expr, $propose:expr) => {
        use $types::{ConfState, Entry, HardState, Message, Snapshot, SnapshotMetadata};

        use crate::core::{App, Envelope};

        /// One member's durable log, in memory: a snapshot and the entries after
        /// it.
        #[derive(Default)]
        pub struct Disk {
            pub hard: HardState,
            pub conf: ConfState,
            pub snapshot: Snapshot,
            pub entries: Vec<Entry>,
        }

        impl Disk {
            pub fn new(voters: &[u64]) -> Self {
                Self {
                    conf: ConfState {
                        voters: voters.to_vec(),
                        ..ConfState::default()
                    },
                    ..Self::default()
                }
            }
            pub fn snapshot_index(&self) -> u64 {
                self.snapshot.metadata.as_ref().map_or(0, |m| m.index)
            }
            pub fn snapshot_term(&self) -> u64 {
                self.snapshot.metadata.as_ref().map_or(0, |m| m.term)
            }
            pub fn first_index(&self) -> u64 {
                self.snapshot_index() + 1
            }
            pub fn last_index(&self) -> u64 {
                self.snapshot_index() + self.entries.len() as u64
            }
            pub fn term(&self, index: u64) -> Option<u64> {
                if index == self.snapshot_index() {
                    return Some(self.snapshot_term());
                }
                if index < self.snapshot_index() {
                    return None;
                }
                self.entries
                    .get((index - self.first_index()) as usize)
                    .map(|entry| entry.term)
            }
            /// Takes the entries a `Ready` gave: what they replace is cut first.
            pub fn append(&mut self, entries: Vec<Entry>) {
                let Some(first) = entries.first() else {
                    return;
                };
                assert!(
                    first.index >= self.first_index(),
                    "an append below the snapshot"
                );
                assert!(
                    first.index <= self.last_index() + 1,
                    "a gap in what is persisted"
                );
                self.entries
                    .truncate((first.index - self.first_index()) as usize);
                self.entries.extend(entries);
            }
            pub fn install(&mut self, snapshot: Snapshot) {
                let metadata = snapshot.metadata.clone().unwrap_or_default();
                self.conf = metadata.conf_state.clone().unwrap_or_default();
                self.hard.commit = self.hard.commit.max(metadata.index);
                self.entries.clear();
                self.snapshot = snapshot;
            }
            /// Everything through `index` becomes the snapshot holding `data`.
            pub fn compact(&mut self, index: u64, data: Vec<u8>) {
                let term = self.term(index).expect("the compacted index is held");
                let keep = (index + 1 - self.first_index()) as usize;
                self.entries.drain(..keep);
                self.snapshot = Snapshot {
                    data,
                    metadata: Some(SnapshotMetadata {
                        conf_state: Some(self.conf.clone()),
                        index,
                        term,
                    }),
                };
            }
            /// The entries of `[first, last]`, where the store holds them: read by the in-place
            /// driver, which only hyper-raft's line has.
            #[allow(dead_code)]
            pub fn range(&self, first: u64, last: u64) -> &[Entry] {
                let base = self.first_index();
                &self.entries[(first - base) as usize..=(last - base) as usize]
            }
            /// The entries of `[low, high)` that `max_bytes` admit, one at least,
            /// appended to `into` with room reserved for exactly them.
            pub fn page(&self, low: u64, high: u64, max_bytes: u64, into: &mut Vec<Entry>) {
                let first = self.first_index();
                let range = &self.entries[(low - first) as usize..(high - first) as usize];
                let mut bytes = 0u64;
                let mut kept = 0usize;
                for entry in range {
                    bytes += $measure(entry);
                    if kept > 0 && bytes > max_bytes {
                        break;
                    }
                    kept += 1;
                }
                into.reserve_exact(kept);
                into.extend(range[..kept].iter().cloned());
            }
            fn bounds(&self, low: u64, high: u64) -> Result<(), bool> {
                if low < self.first_index() {
                    return Err(true);
                }
                if low > high || high > self.last_index() + 1 {
                    return Err(false);
                }
                Ok(())
            }
        }

        /// Applies what a `Ready` gave to apply. The owner's work.
        fn apply(app: &mut App, entries: &[Entry]) {
            for entry in entries {
                app.apply(entry.index, &entry.data);
            }
        }

        /// One `MsgPropose` of every proposal of `batch`, from `id` to itself.
        fn proposal(id: u64, batch: Vec<Vec<u8>>) -> Message {
            Message {
                msg_type: $propose,
                from: id,
                entries: batch
                    .into_iter()
                    .map(|data| Entry {
                        data,
                        ..Entry::default()
                    })
                    .collect(),
                ..Message::default()
            }
        }

        fn envelope(message: Message) -> Envelope<Message> {
            Envelope {
                from: message.from,
                to: message.to,
                message,
            }
        }
    };
}

/// The Storage trait of a core of focal's line, on [`Disk`]. `$any_entry`
/// says whether the trait asks for `any_entry`: focal added it after the
/// revision mantle pins.
macro_rules! focal_storage {
    ($krate:ident, any_entry) => {
        focal_storage!($krate, base);
        impl $krate::Storage for Disk {
            focal_storage!(@methods $krate);
            fn any_entry(
                &self,
                low: u64,
                high: u64,
                predicate: &mut dyn FnMut(&Entry) -> bool,
            ) -> Result<bool, $krate::StorageError> {
                self.bounds(low, high).map_err(|compacted| {
                    if compacted {
                        $krate::StorageError::Compacted
                    } else {
                        $krate::StorageError::Unavailable
                    }
                })?;
                let first = self.first_index();
                Ok(self.entries[(low - first) as usize..(high - first) as usize]
                    .iter()
                    .any(predicate))
            }
        }
    };
    ($krate:ident, no_any_entry) => {
        focal_storage!($krate, base);
        impl $krate::Storage for Disk {
            focal_storage!(@methods $krate);
        }
    };
    ($krate:ident, base) => {};
    (@methods $krate:ident) => {
        fn initial_state(&self) -> Result<$krate::InitialState, $krate::StorageError> {
            // Field by field: the cores' states differ in what they hold beside these, and
            // this storage holds nothing of the fast track.
            let mut state = $krate::InitialState::default();
            state.hard_state = self.hard.clone();
            state.configuration = self.conf.clone();
            Ok(state)
        }
        fn entries(
            &self,
            low: u64,
            high: u64,
            max_bytes: u64,
            into: &mut Vec<Entry>,
        ) -> Result<(), $krate::StorageError> {
            self.bounds(low, high).map_err(|compacted| {
                if compacted {
                    $krate::StorageError::Compacted
                } else {
                    $krate::StorageError::Unavailable
                }
            })?;
            self.page(low, high, max_bytes, into);
            Ok(())
        }
        fn term(&self, index: u64) -> Result<u64, $krate::StorageError> {
            if index < self.snapshot_index() {
                return Err($krate::StorageError::Compacted);
            }
            self.term(index).ok_or($krate::StorageError::Unavailable)
        }
        fn first_index(&self) -> Result<u64, $krate::StorageError> {
            Ok(Disk::first_index(self))
        }
        fn last_index(&self) -> Result<u64, $krate::StorageError> {
            Ok(Disk::last_index(self))
        }
        fn snapshot(&self, request_index: u64, _to: u64) -> Result<Snapshot, $krate::StorageError> {
            if self.snapshot_index() == 0 || self.snapshot_index() < request_index {
                return Err($krate::StorageError::SnapshotTemporarilyUnavailable);
            }
            Ok(self.snapshot.clone())
        }
    };
}

/// A core of focal's line, driven as focal's shell drives it.
macro_rules! focal_core {
    // A `Ready` copied, as raft-rs gives it: its entries taken, what it commits copied.
    (@flush copies, $self:ident, $out:ident) => {
        while $self.raw.has_ready() {
            let mut ready = $self.raw.ready().expect("a ready");
            alloc::aside();
            if let Some(snapshot) = ready.snapshot() {
                $self.app = App::decode(&snapshot.data);
                $self.raw.store_mut().install(snapshot.clone());
            }
            let entries = ready.take_entries();
            $self.raw.store_mut().append(entries);
            if let Some(hard) = ready.hard_state() {
                $self.raw.store_mut().hard = hard.clone();
            }
            alloc::back();
            let messages = ready.take_messages();
            let persisted = ready.take_persisted_messages();
            let committed = ready.take_committed_entries();
            alloc::aside();
            $out.extend(messages.into_iter().map(envelope));
            $out.extend(persisted.into_iter().map(envelope));
            apply(&mut $self.app, &committed);
            drop(committed);
            alloc::back();
            let mut light = $self.raw.advance_append(ready).expect("advanced");
            let messages = light.take_messages();
            let committed = light.take_committed_entries();
            alloc::aside();
            if let Some(commit) = light.commit_index() {
                $self.raw.store_mut().hard.commit = commit;
            }
            $out.extend(messages.into_iter().map(envelope));
            apply(&mut $self.app, &committed);
            drop(committed);
            alloc::back();
            $self.raw.advance_apply_to($self.app.index).expect("applied");
        }
    };
    // A `Ready` given in place (`RawNode::ready_in_place`): the entries persisted from where the
    // member holds them, what is committed applied from the owner's own store.
    (@flush in_place, $self:ident, $out:ident) => {
        while $self.raw.has_ready() {
            let mut ready = $self.raw.ready_in_place().expect("a ready");
            alloc::aside();
            if let Some(snapshot) = $self.raw.to_persist().snapshot {
                $self.app = App::decode(&snapshot.data);
            }
            // An in-memory store has nothing to write out: it keeps the entries themselves
            // when the ready advances (`advance_append_keeping`). A store that writes them out
            // reads them here, from `to_persist`.
            if let Some(hard) = ready.hard_state() {
                $self.raw.store_mut().hard = hard.clone();
            }
            alloc::back();
            let mut messages = ready.take_messages();
            let mut persisted = ready.take_persisted_messages();
            alloc::aside();
            $out.extend(messages.drain(..).map(envelope));
            $out.extend(persisted.drain(..).map(envelope));
            if let Some((first, last)) = ready.committed_range() {
                apply(&mut $self.app, $self.raw.store().range(first, last));
            }
            alloc::back();
            let mut light = $self
                .raw
                .advance_append_keeping(ready, |store, kept| {
                    alloc::aside();
                    if let Some(snapshot) = kept.snapshot {
                        store.install(snapshot);
                    }
                    store.append(kept.entries);
                    alloc::back();
                })
                .expect("advanced");
            // The emptied queues go back to the member, as hyper-durable's replica gives them
            // (`RawNode::recycle_messages`), so a later ready's queue is not grown again.
            $self.raw.recycle_messages(messages);
            $self.raw.recycle_messages(persisted);
            let mut messages = light.take_messages();
            alloc::aside();
            if let Some(commit) = light.commit_index() {
                $self.raw.store_mut().hard.commit = commit;
            }
            $out.extend(messages.drain(..).map(envelope));
            if let Some((first, last)) = light.committed_range() {
                apply(&mut $self.app, $self.raw.store().range(first, last));
            }
            alloc::back();
            $self.raw.recycle_messages(messages);
            $self.raw.advance_apply_to($self.app.index).expect("applied");
        }
    };
    // `$config` gives a member its settings from its identity and its group's voters: focal's
    // cores take an identity, this core what its owner states besides (`Limits::derive`).
    ($module:ident, $krate:ident, $name:literal, $mode:ident, $config:expr) => {
        pub mod $module {
            use hyper_measure::alloc;
            use super::{Disk, Message, apply, envelope};
            use crate::core::{App, Core, Envelope, Fast, Settings};

            pub struct Node {
                raw: $krate::RawNode<Disk>,
                app: App,
                heartbeat_tick: usize,
                state_bytes: usize,
            }

            fn heard<T>(outcome: $krate::Result<T>) -> Option<T> {
                match outcome {
                    Ok(value) => Some(value),
                    Err(error) => {
                        assert!(!error.is_fatal(), "the member stopped: {error}");
                        None
                    }
                }
            }

            impl Core for Node {
                type Message = Message;
                /// The name the tables give this core.
                const NAME: &'static str = $name;

                fn open(id: u64, voters: &[u64], settings: &Settings, seed: u64) -> Self {
                    let config = $krate::Config {
                        election_tick: settings.election_tick,
                        heartbeat_tick: settings.heartbeat_tick,
                        max_size_per_msg: settings.max_size_per_msg,
                        max_inflight_msgs: settings.max_inflight_msgs,
                        max_uncommitted_size: settings.max_uncommitted_size,
                        max_committed_size_per_ready: settings.max_committed_size_per_ready,
                        check_quorum: true,
                        pre_vote: true,
                        fast: settings.fast,
                        seed,
                        ..($config)(id, voters)
                    };
                    let raw = $krate::RawNode::new(&config, Disk::new(voters)).expect("opens");
                    Self {
                        raw,
                        app: App::default(),
                        heartbeat_tick: settings.heartbeat_tick,
                        state_bytes: settings.state_bytes,
                    }
                }
                fn id(&self) -> u64 {
                    self.raw.raft.id()
                }
                fn leader(&self) -> u64 {
                    self.raw.raft.leader_id()
                }
                fn term(&self) -> u64 {
                    self.raw.raft.term()
                }
                fn applied(&self) -> u64 {
                    self.app.index
                }
                fn digest(&self) -> u64 {
                    self.app.digest
                }
                fn fast_committed(&self) -> u64 {
                    self.raw.raft.fast_stats().committed
                }
                fn campaign(&mut self, out: &mut Vec<Envelope<Message>>) {
                    heard(self.raw.campaign());
                    self.flush(out);
                }
                fn period(&mut self, out: &mut Vec<Envelope<Message>>) {
                    for _ in 0..self.heartbeat_tick {
                        heard(self.raw.tick());
                    }
                    self.flush(out);
                }
                fn propose(&mut self, data: Vec<u8>) -> bool {
                    heard(self.raw.propose(Vec::new(), data)).is_some()
                }
                fn propose_batch(&mut self, batch: Vec<Vec<u8>>) -> bool {
                    alloc::aside();
                    let message = super::proposal(self.raw.raft.id(), batch);
                    alloc::back();
                    heard(self.raw.step(message)).is_some()
                }
                fn propose_fast(
                    &mut self,
                    data: Vec<u8>,
                    out: &mut Vec<Envelope<Message>>,
                ) -> Fast {
                    let proposed = heard(self.raw.propose_fast(Vec::new(), data)).is_some();
                    self.flush(out);
                    if proposed {
                        Fast::Proposed
                    } else {
                        Fast::Refused
                    }
                }
                fn transfer(&mut self, to: u64, out: &mut Vec<Envelope<Message>>) -> bool {
                    let done = heard(self.raw.transfer_leader(to)).is_some();
                    self.flush(out);
                    done
                }
                fn step(
                    &mut self,
                    _from: u64,
                    message: Message,
                    _out: &mut Vec<Envelope<Message>>,
                ) {
                    heard(self.raw.step(message));
                }
                fn flush(&mut self, out: &mut Vec<Envelope<Message>>) {
                    focal_core!(@flush $mode, self, out);
                }
                fn compact(&mut self) {
                    alloc::aside();
                    let index = self.app.index;
                    let disk = self.raw.store_mut();
                    if index > disk.snapshot_index() && index <= disk.last_index() {
                        disk.compact(index, self.app.encode(self.state_bytes));
                    }
                    alloc::back();
                }
            }
        }
    };
}

/// The cores on raft-proto's types: focal-raft at two revisions, and raft-rs.
pub mod pb {
    use raft_proto::protocompat::PbMessage;

    line!(
        raft_proto::eraftpb,
        |entry: &Entry| entry.encoded_len() as u64,
        raft_proto::eraftpb::MessageType::MsgPropose as i32
    );

    focal_storage!(focal_raft_control, any_entry);
    focal_storage!(focal_raft_mantle, no_any_entry);

    focal_core!(
        control,
        focal_raft_control,
        "focal-raft a8e95f7",
        copies,
        |id, _: &[u64]| focal_raft_control::Config::new(id)
    );
    focal_core!(
        mantle,
        focal_raft_mantle,
        "focal-raft 1395e22 (mantle)",
        copies,
        |id, _: &[u64]| focal_raft_mantle::Config::new(id)
    );

    impl raft::Storage for Disk {
        fn initial_state(&self) -> raft::Result<raft::RaftState> {
            Ok(raft::RaftState {
                hard_state: self.hard.clone(),
                conf_state: self.conf.clone(),
            })
        }
        fn entries(
            &self,
            low: u64,
            high: u64,
            max_size: impl Into<Option<u64>>,
            _context: raft::GetEntriesContext,
        ) -> raft::Result<Vec<Entry>> {
            self.bounds(low, high).map_err(|compacted| {
                if compacted {
                    raft::Error::Store(raft::StorageError::Compacted)
                } else {
                    raft::Error::Store(raft::StorageError::Unavailable)
                }
            })?;
            let mut page = Vec::new();
            self.page(low, high, max_size.into().unwrap_or(u64::MAX), &mut page);
            Ok(page)
        }
        fn term(&self, index: u64) -> raft::Result<u64> {
            if index < self.snapshot_index() {
                return Err(raft::Error::Store(raft::StorageError::Compacted));
            }
            self.term(index)
                .ok_or(raft::Error::Store(raft::StorageError::Unavailable))
        }
        fn first_index(&self) -> raft::Result<u64> {
            Ok(Disk::first_index(self))
        }
        fn last_index(&self) -> raft::Result<u64> {
            Ok(Disk::last_index(self))
        }
        fn snapshot(&self, request_index: u64, _to: u64) -> raft::Result<Snapshot> {
            if self.snapshot_index() == 0 || self.snapshot_index() < request_index {
                return Err(raft::Error::Store(
                    raft::StorageError::SnapshotTemporarilyUnavailable,
                ));
            }
            Ok(self.snapshot.clone())
        }
    }

    /// raft-rs, driven as focal's shell drove it before focal-raft
    /// (`tests/support/mod.rs`, `Old`).
    pub mod raftrs {
        use super::{Disk, Message, apply, envelope};
        use crate::core::{App, Core, Envelope, Fast, Settings};
        use hyper_measure::alloc;

        pub struct Node {
            raw: raft::RawNode<Disk>,
            app: App,
            heartbeat_tick: usize,
            state_bytes: usize,
        }

        impl Core for Node {
            type Message = Message;
            /// The name the tables give this core.
            const NAME: &'static str = "raft-rs 8e4cef1";

            fn open(id: u64, voters: &[u64], settings: &Settings, _seed: u64) -> Self {
                let config = raft::Config {
                    id,
                    election_tick: settings.election_tick,
                    heartbeat_tick: settings.heartbeat_tick,
                    max_size_per_msg: settings.max_size_per_msg,
                    max_inflight_msgs: settings.max_inflight_msgs,
                    max_uncommitted_size: settings.max_uncommitted_size,
                    max_committed_size_per_ready: settings.max_committed_size_per_ready,
                    check_quorum: true,
                    pre_vote: true,
                    ..raft::Config::default()
                };
                let logger = slog::Logger::root(slog::Discard, slog::o!());
                let raw = raft::RawNode::new(&config, Disk::new(voters), &logger).expect("opens");
                Self {
                    raw,
                    app: App::default(),
                    heartbeat_tick: settings.heartbeat_tick,
                    state_bytes: settings.state_bytes,
                }
            }
            fn id(&self) -> u64 {
                self.raw.raft.id
            }
            fn leader(&self) -> u64 {
                self.raw.raft.leader_id
            }
            fn term(&self) -> u64 {
                self.raw.raft.term
            }
            fn applied(&self) -> u64 {
                self.app.index
            }
            fn digest(&self) -> u64 {
                self.app.digest
            }
            fn campaign(&mut self, out: &mut Vec<Envelope<Message>>) {
                let _ = self.raw.campaign();
                self.flush(out);
            }
            fn period(&mut self, out: &mut Vec<Envelope<Message>>) {
                for _ in 0..self.heartbeat_tick {
                    self.raw.tick();
                }
                self.flush(out);
            }
            fn propose(&mut self, data: Vec<u8>) -> bool {
                self.raw.propose(Vec::new(), data).is_ok()
            }
            fn propose_batch(&mut self, batch: Vec<Vec<u8>>) -> bool {
                alloc::aside();
                let message = super::proposal(self.raw.raft.id, batch);
                alloc::back();
                self.raw.step(message).is_ok()
            }
            fn propose_fast(&mut self, _data: Vec<u8>, _out: &mut Vec<Envelope<Message>>) -> Fast {
                Fast::Unsupported
            }
            fn transfer(&mut self, to: u64, out: &mut Vec<Envelope<Message>>) -> bool {
                self.raw.transfer_leader(to);
                self.flush(out);
                true
            }
            fn step(&mut self, _from: u64, message: Message, _out: &mut Vec<Envelope<Message>>) {
                let _ = self.raw.step(message);
            }
            fn flush(&mut self, out: &mut Vec<Envelope<Message>>) {
                while self.raw.has_ready() {
                    let mut ready = self.raw.ready();
                    alloc::aside();
                    if !ready.snapshot().is_empty() {
                        let snapshot = ready.snapshot().clone();
                        self.app = App::decode(&snapshot.data);
                        self.raw.mut_store().install(snapshot);
                    }
                    let entries = ready.take_entries();
                    self.raw.mut_store().append(entries);
                    if let Some(hard) = ready.hs() {
                        self.raw.mut_store().hard = hard.clone();
                    }
                    alloc::back();
                    let messages = ready.take_messages();
                    let persisted = ready.take_persisted_messages();
                    let committed = ready.take_committed_entries();
                    alloc::aside();
                    out.extend(messages.into_iter().map(envelope));
                    out.extend(persisted.into_iter().map(envelope));
                    apply(&mut self.app, &committed);
                    drop(committed);
                    alloc::back();
                    let mut light = self.raw.advance_append(ready);
                    let messages = light.take_messages();
                    let committed = light.take_committed_entries();
                    alloc::aside();
                    if let Some(commit) = light.commit_index() {
                        self.raw.mut_store().hard.commit = commit;
                    }
                    out.extend(messages.into_iter().map(envelope));
                    apply(&mut self.app, &committed);
                    drop(committed);
                    alloc::back();
                    self.raw.advance_apply_to(self.app.index);
                }
            }
            fn compact(&mut self) {
                alloc::aside();
                let index = self.app.index;
                let disk = self.raw.mut_store();
                if index > disk.snapshot_index() && index <= disk.last_index() {
                    disk.compact(index, self.app.encode(self.state_bytes));
                }
                alloc::back();
            }
        }
    }
}

/// hyper-raft on its own types and format.
pub mod own {
    line!(
        hyper_raft::proto,
        hyper_raft::proto::encoded_bytes,
        hyper_raft::proto::MessageType::MsgPropose
    );

    focal_storage!(hyper_raft, any_entry);

    /// What each member states (`hyper_raft::Limits::derive`), as the core's own test harness
    /// does: a message of 8 MiB, twice the largest append the shell's settings send; its group's
    /// voters; queues of four such messages each; one write out at a time.
    fn config(id: u64, voters: &[u64]) -> hyper_raft::Config {
        let limits = hyper_raft::Limits::derive(hyper_raft::Stated {
            message: 8 << 20,
            members: voters.len(),
            memory: 32 << 20,
            depth: 1,
        })
        .expect("the comparison's statement gives its bounds");
        hyper_raft::Config::new(id, limits)
    }

    focal_core!(hyper, hyper_raft, "hyper-raft", in_place, super::config);
    focal_core!(
        hyper_copy,
        hyper_raft,
        "hyper-raft, copying Ready",
        copies,
        super::config
    );
}
