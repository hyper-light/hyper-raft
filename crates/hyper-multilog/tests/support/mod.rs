//! Members of a multilog group as an owner drives them (`docs/multilog.md` §9), on storage the
//! harness can crash, compact and restart: each log's member persists each `Ready` before it
//! sends what the `Ready` asks to send, hands its committed entries to the layer, and the state
//! machine is what the merge applies, in order. The state machine is the history itself: each
//! key's commands with the epoch each saw, and the global commands with theirs.
#![allow(dead_code)]

use std::collections::BTreeMap;

use hyper_multilog::{Applied, Command, Flow, Installed, Limits, MultiLog, Point, Route};
use hyper_raft::proto::{ConfState, Entry, HardState, Message, Snapshot, SnapshotMetadata};
use hyper_raft::{Config, Elections, StateRole, Storage, StorageError};

/// The largest message the harness states its network carries: a MiB, many times the largest
/// append its commands make.
pub const MESSAGE: usize = 1 << 20;
/// The bytes each of a member's queues may hold: four of its messages, as `Limits::derive` asks.
pub const MEMORY: usize = 4 * MESSAGE;

/// The bounds every member states.
pub fn limits(members: usize) -> hyper_raft::Limits {
    hyper_raft::Limits::derive(hyper_raft::Stated {
        message: MESSAGE,
        members,
        memory: MEMORY,
        depth: 1,
    })
    .expect("the harness's statement gives its bounds")
}

/// A member's settings electing by suspicion (timing step L-2): its owner's detectors start its
/// campaigns, and it takes no ticks.
pub fn config_by_suspicion(id: u64, members: usize, seed: u64) -> Config {
    Config {
        elections: Elections::Suspicion,
        ..config(id, members, seed)
    }
}

/// A member's settings: pre-vote and check-quorum, elections on ticks (an election timeout of ten,
/// a heartbeat every two, as focal's shell sets a group), and a window of bytes.
pub fn config(id: u64, members: usize, seed: u64) -> Config {
    Config {
        election_tick: 10,
        heartbeat_tick: 2,
        max_size_per_msg: 64 * 1024,
        max_inflight_msgs: 64,
        max_inflight_bytes: 1 << 20,
        max_uncommitted_size: 1 << 22,
        max_committed_size_per_ready: 1 << 22,
        check_quorum: true,
        pre_vote: true,
        elections: Elections::Ticks,
        seed,
        ..Config::new(id, limits(members))
    }
}

/// What a log's storage holds: durable at once, as the harness persists a `Ready` before it acts
/// on it.
#[derive(Clone, Debug, Default)]
pub struct Disk {
    pub hard_state: HardState,
    /// The configuration the member opens with: the group's, or an image's for this log.
    pub conf: ConfState,
    pub snapshot: Snapshot,
    pub entries: Vec<Entry>,
}

impl Disk {
    pub fn new(conf: ConfState) -> Self {
        Self {
            conf,
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
    pub fn term_at(&self, index: u64) -> Option<u64> {
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
    pub fn append(&mut self, entries: &[Entry]) {
        for entry in entries {
            if entry.index < self.first_index() {
                continue;
            }
            assert!(
                entry.index <= self.last_index() + 1,
                "a gap in what is persisted"
            );
            let at = (entry.index - self.first_index()) as usize;
            self.entries.truncate(at);
            self.entries.push(entry.clone());
        }
    }
    pub fn install(&mut self, snapshot: &Snapshot) {
        let metadata = snapshot.metadata.clone().unwrap_or_default();
        self.conf = metadata.conf_state.clone().unwrap_or_default();
        self.hard_state.commit = self.hard_state.commit.max(metadata.index);
        let keep: Vec<Entry> = self
            .entries
            .iter()
            .filter(|entry| entry.index > metadata.index)
            .cloned()
            .collect();
        // An entry after the snapshot is kept only where the log matches it there.
        let matches = self.term_at(metadata.index) == Some(metadata.term);
        self.entries = if matches { keep } else { Vec::new() };
        self.snapshot = snapshot.clone();
    }
    /// Everything through `index` becomes the snapshot of `image`, at `term`, with `conf`.
    pub fn compact(&mut self, index: u64, term: u64, conf: ConfState, image: Vec<u8>) {
        if index <= self.snapshot_index() {
            return;
        }
        assert_eq!(
            self.term_at(index),
            Some(term),
            "the compacted index is held at its term"
        );
        let keep = (index + 1 - self.first_index()) as usize;
        self.entries.drain(..keep);
        self.snapshot = Snapshot {
            data: image,
            metadata: Some(SnapshotMetadata {
                conf_state: Some(conf),
                index,
                term,
            }),
        };
    }
}

/// A log's storage as the core reads it.
#[derive(Clone, Debug)]
pub struct Store(pub Disk);

impl Storage for Store {
    fn initial_state(&self) -> Result<hyper_raft::InitialState, StorageError> {
        Ok(hyper_raft::InitialState {
            hard_state: self.0.hard_state,
            configuration: self.0.conf.clone(),
            proposals: Vec::new(),
        })
    }
    fn entries(
        &self,
        low: u64,
        high: u64,
        _max_bytes: u64,
        into: &mut Vec<Entry>,
    ) -> Result<(), StorageError> {
        let disk = &self.0;
        if low < disk.first_index() {
            return Err(StorageError::Compacted);
        }
        if low > high || high > disk.last_index() + 1 {
            return Err(StorageError::Unavailable);
        }
        let first = disk.first_index();
        let page = &disk.entries[(low - first) as usize..(high - first) as usize];
        into.reserve_exact(page.len());
        into.extend_from_slice(page);
        Ok(())
    }
    fn any_entry(
        &self,
        low: u64,
        high: u64,
        predicate: &mut dyn FnMut(&Entry) -> bool,
    ) -> Result<bool, StorageError> {
        let disk = &self.0;
        if low < disk.first_index() {
            return Err(StorageError::Compacted);
        }
        if low > high || high > disk.last_index() + 1 {
            return Err(StorageError::Unavailable);
        }
        let first = disk.first_index();
        Ok(
            disk.entries[(low - first) as usize..(high - first) as usize]
                .iter()
                .any(predicate),
        )
    }
    fn term(&self, index: u64) -> Result<u64, StorageError> {
        if index < self.0.snapshot_index() {
            return Err(StorageError::Compacted);
        }
        self.0.term_at(index).ok_or(StorageError::Unavailable)
    }
    fn first_index(&self) -> Result<u64, StorageError> {
        Ok(self.0.first_index())
    }
    fn last_index(&self) -> Result<u64, StorageError> {
        Ok(self.0.last_index())
    }
    fn snapshot(&self, request_index: u64, _to: u64) -> Result<Snapshot, StorageError> {
        let disk = &self.0;
        if disk.snapshot_index() == 0 || disk.snapshot_index() < request_index {
            return Err(StorageError::SnapshotTemporarilyUnavailable);
        }
        Ok(disk.snapshot.clone())
    }
}

/// The state machine: every command applied, in each key's order (`None` for the global
/// commands), with the epoch each saw. Two members that applied the same history hold equal ones.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct App {
    pub keys: BTreeMap<Option<u64>, Vec<(Vec<u8>, u64)>>,
    /// Each log's commands, in the order applied, by index: the order the log fixes.
    pub logs: BTreeMap<u64, Vec<(u64, Vec<u8>)>>,
    pub applied: u64,
}

impl App {
    pub fn apply(&mut self, command: &Command<'_>) {
        self.keys
            .entry(command.key)
            .or_default()
            .push((command.data.to_vec(), command.epoch));
        self.logs
            .entry(command.log as u64)
            .or_default()
            .push((command.index, command.data.to_vec()));
        self.applied += 1;
    }
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&(self.keys.len() as u64).to_le_bytes());
        for (key, history) in &self.keys {
            out.extend_from_slice(&key.unwrap_or(u64::MAX).to_le_bytes());
            out.push(u8::from(key.is_some()));
            out.extend_from_slice(&(history.len() as u64).to_le_bytes());
            for (command, epoch) in history {
                put_bytes(out, command);
                out.extend_from_slice(&epoch.to_le_bytes());
            }
        }
        out.extend_from_slice(&(self.logs.len() as u64).to_le_bytes());
        for (log, applied) in &self.logs {
            out.extend_from_slice(&log.to_le_bytes());
            out.extend_from_slice(&(applied.len() as u64).to_le_bytes());
            for (index, command) in applied {
                out.extend_from_slice(&index.to_le_bytes());
                put_bytes(out, command);
            }
        }
        out.extend_from_slice(&self.applied.to_le_bytes());
    }
    pub fn decode(bytes: &[u8]) -> Self {
        let mut reader = Reader { bytes, at: 0 };
        let count = reader.u64();
        let mut keys = BTreeMap::new();
        for _ in 0..count {
            let raw = reader.u64();
            let some = reader.byte() == 1;
            let len = reader.u64();
            let mut history = Vec::new();
            for _ in 0..len {
                let command = reader.bytes();
                let epoch = reader.u64();
                history.push((command, epoch));
            }
            keys.insert(some.then_some(raw), history);
        }
        let count = reader.u64();
        let mut logs = BTreeMap::new();
        for _ in 0..count {
            let log = reader.u64();
            let len = reader.u64();
            let mut applied = Vec::new();
            for _ in 0..len {
                let index = reader.u64();
                applied.push((index, reader.bytes()));
            }
            logs.insert(log, applied);
        }
        let applied = reader.u64();
        assert_eq!(
            reader.at,
            bytes.len(),
            "an image's state ends where it says"
        );
        Self {
            keys,
            logs,
            applied,
        }
    }
}

fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
    out.extend_from_slice(bytes);
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> &[u8] {
        let taken = &self.bytes[self.at..self.at + n];
        self.at += n;
        taken
    }
    fn u64(&mut self) -> u64 {
        u64::from_le_bytes(self.take(8).try_into().unwrap())
    }
    fn byte(&mut self) -> u8 {
        self.take(1)[0]
    }
    fn bytes(&mut self) -> Vec<u8> {
        let size = self.u64() as usize;
        self.take(size).to_vec()
    }
}

/// An image: the point it was taken at and the state machine there, as a snapshot carries it.
pub fn encode_image(point: &Point, app: &App) -> Vec<u8> {
    let mut point_bytes = Vec::new();
    point.encode(&mut point_bytes).expect("a point encodes");
    let mut out = Vec::new();
    out.extend_from_slice(&(point_bytes.len() as u64).to_le_bytes());
    out.extend_from_slice(&point_bytes);
    app.encode(&mut out);
    out
}

pub fn decode_image(bytes: &[u8]) -> (Point, App) {
    let len = u64::from_le_bytes(bytes[..8].try_into().unwrap()) as usize;
    let point = Point::decode(&bytes[8..8 + len]).expect("an image's point decodes");
    let app = App::decode(&bytes[8 + len..]);
    (point, app)
}

/// What a member did that a test counts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub keyed_applied: u64,
    pub globals_applied: u64,
    pub refused: u64,
    pub barriers_proposed: u64,
    pub images: u64,
    pub installed: u64,
}

/// One member of a multilog group, with its durable state: each log's disk (inside its member)
/// and its last image.
pub struct Member {
    pub id: u64,
    pub members: usize,
    pub logs: usize,
    pub seed: u64,
    pub multi: MultiLog<Store>,
    pub app: App,
    /// The last image made durable: the base it restarts from.
    pub image: Option<(Point, App)>,
    pub boot: ConfState,
    pub limits: Limits,
    pub counts: Counts,
    /// Whether the member takes an image after the next global command it applies.
    pub image_at_next_global: bool,
    /// Whether [`Member::settle`] proposes the barriers owed; an explorer that makes their
    /// proposal an adversary's move turns it off.
    pub auto_barriers: bool,
    /// Whether its logs elect by suspicion rather than on ticks.
    pub suspicion: bool,
    /// Whether [`Member::settle`] leaves the merge to the test.
    pub hold_apply: bool,
}

impl Member {
    /// Member `id` of `voters`, holding `logs` logs, each a fresh log.
    pub fn new(id: u64, voters: &[u64], logs: usize, seed: u64, limits: Limits) -> Self {
        Self::open(id, voters, logs, seed, limits, false)
    }

    /// As [`Member::new`], its logs electing by suspicion where `suspicion`.
    pub fn open(
        id: u64,
        voters: &[u64],
        logs: usize,
        seed: u64,
        limits: Limits,
        suspicion: bool,
    ) -> Self {
        let boot = ConfState {
            voters: voters.to_vec(),
            ..ConfState::default()
        };
        let stores = (0..logs).map(|_| Store(Disk::new(boot.clone()))).collect();
        let point = Point::origin(vec![boot.clone(); logs]).unwrap();
        let settings = if suspicion {
            config_by_suspicion(id, voters.len(), seed)
        } else {
            config(id, voters.len(), seed)
        };
        let multi = MultiLog::open(&settings, stores, &point, limits).expect("a member opens");
        Self {
            id,
            members: voters.len(),
            logs,
            seed,
            multi,
            app: App::default(),
            image: None,
            boot,
            limits,
            counts: Counts::default(),
            image_at_next_global: false,
            auto_barriers: true,
            suspicion,
            hold_apply: false,
        }
    }

    /// The member stops and starts again on what it made durable: each log's disk, and its last
    /// image (`docs/multilog.md` §5.5).
    pub fn restart(&mut self) {
        let point = self
            .image
            .as_ref()
            .map(|(point, _)| point.clone())
            .unwrap_or_else(|| Point::origin(vec![self.boot.clone(); self.logs]).unwrap());
        let app = self
            .image
            .as_ref()
            .map(|(_, app)| app.clone())
            .unwrap_or_default();
        let mut stores = Vec::new();
        for log in 0..self.logs {
            let mut disk = self.multi.node(log).unwrap().store().0.clone();
            let next = point.cut.next()[log];
            let at = &point.logs[log];
            if disk.last_index() + 1 < next {
                // The log ends before the image's cut: it starts again at the image's snapshot.
                let image = encode_image(&point, &app);
                disk.entries.clear();
                disk.snapshot = Snapshot {
                    data: image,
                    metadata: Some(SnapshotMetadata {
                        conf_state: Some(at.configuration.clone()),
                        index: next - 1,
                        term: at.term,
                    }),
                };
                disk.hard_state.commit = disk.hard_state.commit.max(next - 1);
            }
            disk.conf = at.configuration.clone();
            stores.push(Store(disk));
        }
        let settings = if self.suspicion {
            config_by_suspicion(self.id, self.members, self.seed)
        } else {
            config(self.id, self.members, self.seed)
        };
        self.multi = MultiLog::open(&settings, stores, &point, self.limits)
            .expect("a member reopens on what it made durable");
        self.app = app;
    }

    /// Drives `log`'s member until it has nothing to do: each `Ready` persisted, its committed
    /// entries handed to the layer, and what it sends returned with the log's number.
    pub fn drive(&mut self, log: usize, out: &mut Vec<(usize, Message)>) {
        while self.multi.node(log).unwrap().has_ready() {
            let node = self.multi.node_mut(log).unwrap();
            let mut ready = node.ready().expect("a ready");
            let installed = ready.snapshot().map(|snapshot| {
                node.store_mut().0.install(snapshot);
                snapshot.data.clone()
            });
            node.store_mut().0.append(ready.entries());
            if let Some(hard) = ready.hard_state() {
                node.store_mut().0.hard_state = *hard;
            }
            out.extend(ready.take_messages().into_iter().map(|m| (log, m)));
            out.extend(
                ready
                    .take_persisted_messages()
                    .into_iter()
                    .map(|m| (log, m)),
            );
            let committed = ready.take_committed_entries();
            let mut light = node.advance_append(ready).expect("advanced");
            if let Some(commit) = light.commit_index() {
                node.store_mut().0.hard_state.commit = commit;
                node.commit_durable(commit).expect("a commit the log holds");
            }
            out.extend(light.take_messages().into_iter().map(|m| (log, m)));
            let more = light.take_committed_entries();
            if let Some(image) = installed {
                self.install(&image);
            }
            self.multi.hand_over(log, &committed).expect("handed over");
            self.multi.hand_over(log, &more).expect("handed over");
        }
    }

    /// A log's snapshot carried an image: the member's state is replaced where it is ahead, and
    /// the image becomes its base.
    fn install(&mut self, image: &[u8]) {
        let (point, app) = decode_image(image);
        // The core installs a snapshot only past its commit, so the image is past what the merge
        // read of that log, unless an image installed through another log's snapshot moved the
        // merge there already: by its canonical cut it is ahead of the member's whole state, or
        // held (`docs/multilog.md` §5.3).
        match self.multi.install(&point).expect("an image installs") {
            Installed::Ahead => {
                self.app = app.clone();
                self.image = Some((point, app));
                self.counts.installed += 1;
            }
            Installed::Held => {
                let held = self.image.as_ref().map(|(held, _)| held.cut.clone());
                assert!(
                    held.is_some_and(|cut| cut.holds(&point.cut)),
                    "member {}: an image held, but by no image installed",
                    self.id
                );
            }
        }
    }

    /// Applies what the logs allow, taking an image at a global command when asked.
    pub fn apply(&mut self) {
        loop {
            let mut stopped_at_global = false;
            let want_image = self.image_at_next_global;
            let app = &mut self.app;
            let counts = &mut self.counts;
            let advance = self
                .multi
                .apply(u64::MAX, &mut |applied| match applied {
                    Applied::Command(command) => {
                        app.apply(&command);
                        if command.key.is_none() {
                            counts.globals_applied += 1;
                            if want_image {
                                stopped_at_global = true;
                                return Flow::Stop;
                            }
                        } else {
                            counts.keyed_applied += 1;
                        }
                        Flow::Continue
                    }
                    Applied::Refused { .. } => {
                        counts.refused += 1;
                        Flow::Continue
                    }
                })
                .expect("the merge advances");
            if stopped_at_global {
                self.take_image();
                self.image_at_next_global = false;
            }
            if !advance.more {
                break;
            }
        }
    }

    /// An image at the merge's canonical cut, made durable, and every log compacted to it.
    pub fn take_image(&mut self) {
        let Some(point) = self.multi.point().expect("a point") else {
            return;
        };
        let bytes = encode_image(&point, &self.app);
        self.image = Some((point.clone(), self.app.clone()));
        for log in 0..self.logs {
            let index = point.cut.next()[log] - 1;
            let at = &point.logs[log];
            let node = self.multi.node_mut(log).unwrap();
            node.store_mut()
                .0
                .compact(index, at.term, at.configuration.clone(), bytes.clone());
        }
        self.counts.images += 1;
    }

    /// Proposes the barriers this member owes.
    pub fn barriers(&mut self) {
        let proposed = self.multi.barriers().expect("barriers");
        self.counts.barriers_proposed += proposed as u64;
    }

    /// Everything this member's logs have to do, once each: drive, apply, barriers, drive again.
    pub fn settle(&mut self, out: &mut Vec<(usize, Message)>) {
        for log in 0..self.logs {
            self.drive(log, out);
        }
        if !self.hold_apply {
            self.apply();
        }
        if self.auto_barriers {
            self.barriers();
        }
        for log in 0..self.logs {
            self.drive(log, out);
        }
    }

    pub fn leads(&self, log: usize) -> bool {
        self.multi.node(log).unwrap().raft.state() == StateRole::Leader
    }

    pub fn propose(&mut self, route: Route, command: Vec<u8>) -> bool {
        self.multi.propose(route, command).is_ok()
    }
}

/// A group of members on a network that delivers every message, in the order sent, unless a test
/// holds or drops it.
pub struct Group {
    pub members: Vec<Member>,
    pub flight: Vec<(u64, usize, Message)>,
    /// Directed pairs the network cuts: what one sends the other is lost.
    pub blocked: std::collections::BTreeSet<(u64, u64)>,
}

impl Group {
    pub fn new(voters: u64, logs: usize, seed: u64, limits: Limits) -> Self {
        let ids: Vec<u64> = (1..=voters).collect();
        let members = ids
            .iter()
            .map(|id| {
                Member::new(
                    *id,
                    &ids,
                    logs,
                    seed.wrapping_mul(1_000_003).wrapping_add(*id),
                    limits,
                )
            })
            .collect();
        Self {
            members,
            flight: Vec::new(),
            blocked: std::collections::BTreeSet::new(),
        }
    }
    /// Cuts both directions between `a` and `b`, or heals them.
    pub fn cut(&mut self, a: u64, b: u64, cut: bool) {
        for pair in [(a, b), (b, a)] {
            if cut {
                self.blocked.insert(pair);
            } else {
                self.blocked.remove(&pair);
            }
        }
    }
    /// Cuts `id` off from every other member, or heals it.
    pub fn isolate(&mut self, id: u64, cut: bool) {
        for other in 1..=self.members.len() as u64 {
            if other != id {
                self.cut(id, other, cut);
            }
        }
    }
    pub fn member(&mut self, id: u64) -> &mut Member {
        &mut self.members[(id - 1) as usize]
    }
    /// Settles member `id` and puts what it sends in flight.
    pub fn settle(&mut self, id: u64) {
        let mut out = Vec::new();
        self.member(id).settle(&mut out);
        self.flight
            .extend(out.into_iter().map(|(log, m)| (id, log, m)));
    }
    /// Delivers until nothing is in flight, settling each member a message reached.
    pub fn quiet(&mut self) {
        for id in 1..=self.members.len() as u64 {
            self.settle(id);
        }
        let mut rounds = 0;
        while !self.flight.is_empty() {
            rounds += 1;
            assert!(rounds < 10_000, "the group never went quiet");
            let flight = std::mem::take(&mut self.flight);
            let mut touched = Vec::new();
            for (from, log, message) in flight {
                let to = message.to;
                if self.blocked.contains(&(from, to)) {
                    continue;
                }
                let _ = self.member(to).multi.step(log, message);
                touched.push(to);
            }
            touched.sort_unstable();
            touched.dedup();
            for id in touched {
                self.settle(id);
            }
        }
    }
    /// Member `id` campaigns in `log` and the group goes quiet: it leads there.
    pub fn elect(&mut self, log: usize, id: u64) {
        self.member(id)
            .multi
            .node_mut(log)
            .unwrap()
            .campaign()
            .expect("a campaign");
        self.quiet();
        assert!(self.member(id).leads(log), "member {id} leads log {log}");
    }
    /// Every member ticks every log once, and the group goes quiet.
    pub fn tick(&mut self) {
        for member in &mut self.members {
            for log in 0..member.logs {
                let _ = member.multi.node_mut(log).unwrap().tick();
            }
        }
        self.quiet();
    }
}
