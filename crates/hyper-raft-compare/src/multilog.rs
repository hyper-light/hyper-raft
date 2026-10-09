//! hyper-multilog against slates' MLRaft (slates `crates/cluster/src/multilog.rs` at `5cce86a`) on
//! slates' MLRaft workload: three voters holding `n` logs led apart, a stream of commands keyed over
//! 64 keys with one global command in every eleven (slates' timed simulation proposes twenty keyed
//! and two global commands a second, `tests/multilog_timed.rs`), each command proposed at the
//! leader of the log it routes to, the group then quiet and every member applying in the merged
//! order. Each layer is driven as its owner drives it:
//! - slates' as its timed simulation drives it: elections by `start_election` and replies, each
//!   log's leader sending every follower behind it an append at once (`replicate_to`, as its
//!   simulation's `lead` does), the followers' replies, then the replies folded, until no follower
//!   is behind; barriers appended at every member (`append_barriers`), `apply_ready`, and its
//!   retention acknowledged whenever pending. As `slates-multilog-in-turn`, as its unit tests drive
//!   it instead: one follower at a time, its reply folded before the next follower's append is
//!   made, so that the commit the first reply moves rides the second follower's first append, an
//!   order no network gives a leader that sends to both at once. As
//!   `slates-multilog+publication`, the retained state also copied out (`saved`) at every
//!   acknowledgment, slates' server's durability, whose copy of the whole log grows with it (the
//!   core's comparison does the same, `docs/benchmarks.md`, "slates with its retained-state
//!   publication"), so it runs one command a round only;
//! - this one as `docs/multilog.md` §9 states: each log's member's `Ready` persisted to an in-memory
//!   disk and advanced, its committed entries handed over, its messages delivered, barriers proposed,
//!   the merge applied; a member is driven when a message reached it, as an owner driven by its
//!   events drives it.
//!
//! What both owners do apart from the layer (the network's queues, this crate's disk) is set aside
//! from the counts. A command is measured once, applied at every member.
//!
//! What is reported (`docs/tails.md` §1, §1a): each round's time, from its first proposal to the
//! group quiet with every member having applied it, as quantiles with their distribution-free
//! intervals (§3.3); closed loop and in one process, so a round's time is the layers' service time
//! on one thread, with no network and no device; and, per command, the process's account
//! (`hyper_measure::usage`: CPU time, instructions, cycles, energy) over the timed rounds, its peak
//! footprint, and from a separate counting process the allocations and the bytes every message
//! would take on each layer's own wire (hyper-raft's `Message::encode`, slates' `RaftMessage::encode`).
use std::time::{Duration, Instant};

use hyper_measure::{alloc, faults, usage};

use crate::core::splitmix;

/// What one run measured.
#[derive(Clone, Debug, Default)]
pub struct Measured {
    pub ops: u64,
    pub elapsed: Duration,
    pub total: alloc::Counts,
    pub aside: alloc::Counts,
    pub faults: faults::Faults,
    /// Each timed round's nanoseconds.
    pub rounds: Vec<u64>,
    /// The process's account over the timed rounds.
    pub usage: usage::Usage,
    /// The messages sent over the counted rounds, and their bytes on the layer's wire.
    pub messages: u64,
    pub wire: u64,
}

/// A run's shape.
#[derive(Clone, Copy, Debug)]
pub struct Spec {
    pub logs: usize,
    pub batch: usize,
    pub bytes: usize,
    pub rounds: usize,
}

/// Format: one command in this many is global (slates' timed streams: 20 keyed, 2 global a second).
const GLOBAL_EVERY: u64 = 11;
/// Format: the keys the commands write (slates' timed simulation).
const KEYS: u64 = 64;
/// The voters (slates' unit test of three logs led apart).
const VOTERS: u64 = 3;
/// The bytes of entries one append carries: focal's shell's `max_size_per_msg`, every core's here.
const BUDGET: usize = 4 * 1024 * 1024 + 1024;

/// The commands of round `round`: ids from `first`, routed by the seed's draw.
fn commands(state: &mut u64, first: u64, spec: &Spec, reserve: usize) -> Vec<(Option<u64>, Vec<u8>)> {
    (0..spec.batch as u64)
        .map(|at| {
            let id = first + at;
            let route = if id % GLOBAL_EVERY == 0 {
                None
            } else {
                Some(splitmix(state) % KEYS)
            };
            let mut data = Vec::with_capacity(spec.bytes.max(8) + reserve);
            data.extend_from_slice(&id.to_le_bytes());
            data.resize(spec.bytes.max(8), 0x5a);
            (route, data)
        })
        .collect()
}

/// A digest of what a member applied, as `crate::core::App` digests a core's.
fn fold(digest: &mut u64, data: &[u8], epoch: u64) {
    let first = u64::from_le_bytes(data[..8].try_into().unwrap());
    for value in [first, data.len() as u64, epoch] {
        *digest = (*digest ^ value).wrapping_mul(0x0000_0100_0000_01b3);
    }
}

pub mod hyper {
    //! hyper-multilog, each log's member on an in-memory disk.
    use hyper_multilog::{Applied, Flow, Limits, MultiLog, Point, Route, entry};
    use hyper_raft::proto::{ConfState, Entry, HardState, Message, Snapshot};
    use hyper_raft::{Config, Storage, StorageError, wire::Record};
    use hyper_measure::alloc;

    use super::{BUDGET, Spec, VOTERS};

    #[derive(Default)]
    pub struct Disk {
        hard: HardState,
        conf: ConfState,
        entries: Vec<Entry>,
    }

    impl Disk {
        fn last_index(&self) -> u64 {
            self.entries.len() as u64
        }
        fn append(&mut self, entries: Vec<Entry>) {
            let Some(first) = entries.first() else {
                return;
            };
            self.entries.truncate((first.index - 1) as usize);
            self.entries.extend(entries);
        }
    }

    impl Storage for Disk {
        fn initial_state(&self) -> Result<hyper_raft::InitialState, StorageError> {
            Ok(hyper_raft::InitialState {
                hard_state: self.hard,
                configuration: self.conf.clone(),
                proposals: Vec::new(),
                released: 0,
            })
        }
        fn entries(&self, low: u64, high: u64, _max: u64, into: &mut Vec<Entry>) -> Result<(), StorageError> {
            if high > self.last_index() + 1 || low > high {
                return Err(StorageError::Unavailable);
            }
            let page = &self.entries[(low - 1) as usize..(high - 1) as usize];
            into.reserve_exact(page.len());
            into.extend_from_slice(page);
            Ok(())
        }
        fn any_entry(&self, low: u64, high: u64, predicate: &mut dyn FnMut(&Entry) -> bool) -> Result<bool, StorageError> {
            if high > self.last_index() + 1 || low > high || low == 0 {
                return Err(StorageError::Unavailable);
            }
            Ok(self.entries[(low - 1) as usize..(high - 1) as usize].iter().any(predicate))
        }
        fn term(&self, index: u64) -> Result<u64, StorageError> {
            if index == 0 {
                return Ok(0);
            }
            self.entries.get((index - 1) as usize).map(|e| e.term).ok_or(StorageError::Unavailable)
        }
        fn first_index(&self) -> Result<u64, StorageError> {
            Ok(1)
        }
        fn last_index(&self) -> Result<u64, StorageError> {
            Ok(self.last_index())
        }
        fn snapshot(&self, _request: u64, _to: u64) -> Result<Snapshot, StorageError> {
            Err(StorageError::SnapshotTemporarilyUnavailable)
        }
    }

    pub struct Group {
        members: Vec<MultiLog<Disk>>,
        flight: Vec<(usize, Message)>,
        next: Vec<(usize, Message)>,
        digests: Vec<u64>,
        applied: u64,
        /// Whether messages are encoded to count their bytes (the counting run only).
        pub wire: bool,
        pub messages: u64,
        pub bytes: u64,
    }

    fn config(id: u64, seed: u64) -> Config {
        let limits = hyper_raft::Limits::derive(hyper_raft::Stated {
            message: 8 << 20,
            members: VOTERS as usize,
            memory: 32 << 20,
            depth: 1,
        })
        .unwrap();
        Config {
            election_tick: 10,
            heartbeat_tick: 2,
            max_size_per_msg: BUDGET as u64,
            max_inflight_msgs: 128,
            max_uncommitted_size: 32 * 1024 * 1024,
            max_committed_size_per_ready: 16 * 1024 * 1024,
            check_quorum: true,
            pre_vote: true,
            seed,
            ..Config::new(id, limits)
        }
    }

    impl Group {
        pub fn new(spec: &Spec, seed: u64) -> Self {
            let voters: Vec<u64> = (1..=VOTERS).collect();
            let conf = ConfState { voters: voters.clone(), ..ConfState::default() };
            let members = voters
                .iter()
                .map(|id| {
                    let stores = (0..spec.logs).map(|_| Disk { conf: conf.clone(), ..Disk::default() }).collect();
                    let point = Point::origin(vec![conf.clone(); spec.logs]).unwrap();
                    MultiLog::open(&config(*id, seed ^ id), stores, &point, Limits { unmerged: u64::MAX }).unwrap()
                })
                .collect();
            let mut group = Self {
                members,
                flight: Vec::with_capacity(1 << 16),
                next: Vec::with_capacity(1 << 16),
                digests: vec![0; VOTERS as usize],
                applied: 0,
                wire: false,
                messages: 0,
                bytes: 0,
            };
            for log in 0..spec.logs {
                let leader = log % VOTERS as usize;
                group.members[leader].node_mut(log).unwrap().campaign().unwrap();
                group.quiet();
                assert!(group.members[leader].node(log).unwrap().raft.state() == hyper_raft::StateRole::Leader);
            }
            group
        }

        fn drive(&mut self, at: usize) {
            let Self { members, next, digests, applied, wire, messages: sent_count, bytes, .. } = self;
            let member = &mut members[at];
            for log in 0..member.count() {
                while member.node(log).unwrap().has_ready() {
                    let node = member.node_mut(log).unwrap();
                    let mut ready = node.ready().unwrap();
                    alloc::aside();
                    node.store_mut().append(ready.take_entries());
                    if let Some(hard) = ready.hard_state() {
                        node.store_mut().hard = *hard;
                    }
                    alloc::back();
                    let messages = ready.take_messages();
                    let persisted = ready.take_persisted_messages();
                    let committed = ready.take_committed_entries();
                    let mut light = node.advance_append(ready).unwrap();
                    let more = light.take_committed_entries();
                    let sent = light.take_messages();
                    alloc::aside();
                    for message in messages.into_iter().chain(persisted).chain(sent) {
                        if *wire {
                            // The record hyper-raft's wire encodes, and the log's number, as the
                            // multilog member's datagram carries it.
                            *bytes += Record::encoded_len(&message) as u64 + 8;
                            *sent_count += 1;
                        }
                        next.push((log, message));
                    }
                    alloc::back();
                    member.hand_over(log, &committed).unwrap();
                    member.hand_over(log, &more).unwrap();
                }
            }
            let digest = &mut digests[at];
            member
                .apply(u64::MAX, &mut |applied_now| {
                    if let Applied::Command(command) = applied_now {
                        super::fold(digest, command.data, command.epoch);
                        *applied += 1;
                    }
                    Flow::Continue
                })
                .unwrap();
            member.barriers().unwrap();
        }

        fn quiet(&mut self) {
            for at in 0..self.members.len() {
                self.drive(at);
            }
            let mut waves = 0;
            while !self.next.is_empty() {
                waves += 1;
                assert!(waves < 100_000, "the group never quiets");
                std::mem::swap(&mut self.flight, &mut self.next);
                let mut touched = [false; VOTERS as usize];
                for (log, message) in self.flight.drain(..) {
                    let to = (message.to - 1) as usize;
                    let _ = self.members[to].step(log, message);
                    touched[to] = true;
                }
                for (at, touched) in touched.iter().enumerate() {
                    if *touched {
                        self.drive(at);
                    }
                }
            }
        }

        /// Proposes `batch` at the leaders of the logs they route to, one proposal a log, and goes
        /// quiet.
        pub fn round(&mut self, batch: Vec<(Option<u64>, Vec<u8>)>) {
            let logs = self.members[0].count();
            alloc::aside();
            let mut by_log: Vec<Vec<(Route, Vec<u8>)>> = (0..logs).map(|_| Vec::new()).collect();
            alloc::back();
            for (route, data) in batch {
                let route = route.map_or(Route::Global, Route::Key);
                let log = self.members[0].route(route);
                alloc::aside();
                by_log[log].push((route, data));
                alloc::back();
            }
            for (log, commands) in by_log.into_iter().enumerate() {
                if commands.is_empty() {
                    continue;
                }
                let leader = (0..self.members.len())
                    .find(|at| self.members[*at].node(log).unwrap().raft.state() == hyper_raft::StateRole::Leader)
                    .unwrap();
                self.members[leader].propose_in(log, commands).unwrap();
            }
            self.quiet();
        }

        pub fn applied(&self) -> u64 {
            self.applied
        }
        pub fn digests(&self) -> &[u64] {
            &self.digests
        }
    }

    /// The room an owner that knows the layer reserves.
    pub const RESERVE: usize = entry::SUFFIX_BYTES;
}

pub mod slates {
    //! slates' MultiLog, driven as its tests drive it.
    use slates_cluster::multilog::{MultiLog, Route};
    use slates_cluster::raft::SavedRaft;
    use slates_cluster::raft_wire::RaftMessage;
    use slates_db::register::HostId;
    use hyper_measure::alloc;

    use super::{BUDGET, Spec, VOTERS};

    pub struct Group {
        members: Vec<MultiLog>,
        retained: Vec<Vec<SavedRaft>>,
        digests: Vec<u64>,
        applied: u64,
        /// Whether messages are encoded to count their bytes (the counting run only).
        pub wire: bool,
        pub messages: u64,
        pub bytes: u64,
        /// Whether the retained state is copied out at every transition, as slates' server
        /// publishes it before any reply; otherwise its retention is acknowledged without a copy.
        publish: bool,
        /// Whether each leader sends every follower its append at once, as slates' timed
        /// simulation's leader does (`lead`, one `replicate_to` a peer, then the network), and as
        /// hyper-multilog's members send theirs; otherwise one follower at a time, its reply folded
        /// before the next follower's append is made, as slates' unit tests drive it.
        waves: bool,
    }

    impl Group {
        pub fn new(spec: &Spec, _seed: u64, publish: bool, waves: bool) -> Self {
            let voters: Vec<HostId> = (1..=VOTERS).map(HostId).collect();
            let members: Vec<MultiLog> = voters.iter().map(|id| MultiLog::new(*id, voters.clone(), spec.logs)).collect();
            let retained = members.iter().map(MultiLog::saved).collect();
            let mut group = Self {
                members,
                retained,
                digests: vec![0; VOTERS as usize],
                applied: 0,
                wire: false,
                messages: 0,
                bytes: 0,
                publish,
                waves,
            };
            for log in 0..spec.logs {
                group.elect(log, log % VOTERS as usize);
            }
            group
        }

        /// Counts `message`'s bytes on slates' wire, in the counting run.
        fn sent(&mut self, message: impl FnOnce() -> RaftMessage) {
            if self.wire {
                alloc::aside();
                // The log's number beside it, as hyper-multilog's count adds: either layer's datagram carries one.
                self.bytes += message().encode().len() as u64 + 8;
                self.messages += 1;
                alloc::back();
            }
        }

        fn publish(&mut self, at: usize) {
            if self.members[at].retention_pending() {
                if self.publish {
                    let saved = self.members[at].saved();
                    alloc::aside();
                    self.retained[at] = saved;
                    alloc::back();
                }
                self.members[at].retained();
            }
        }

        fn elect(&mut self, log: usize, leader: usize) {
            let requests = self.members[leader].log_mut(log).unwrap().start_election().unwrap();
            self.publish(leader);
            let voters: Vec<usize> = (0..self.members.len()).filter(|at| *at != leader).collect();
            for (voter, request) in voters.into_iter().zip(requests) {
                let reply = self.members[voter].log_mut(log).unwrap().on_request_vote(request);
                self.publish(voter);
                self.members[leader].log_mut(log).unwrap().on_vote_reply(reply);
                self.publish(leader);
            }
            let node = self.members[leader].log_mut(log).unwrap();
            assert!(node.is_leader());
            assert!(node.append_command(Vec::new()));
            self.publish(leader);
            self.replicate();
        }

        /// Each log's leader replicates to its followers until every follower holds its log and
        /// its commit: in waves, or one follower at a time.
        fn replicate(&mut self) {
            if self.waves {
                self.replicate_in_waves();
            } else {
                self.replicate_in_turn();
            }
        }

        /// Each log's leader sends every follower behind it an append at once; the followers
        /// answer, and the leader folds the answers; until no follower is behind.
        fn replicate_in_waves(&mut self) {
            let logs = self.members[0].count();
            for log in 0..logs {
                let Some(leader) = (0..self.members.len()).find(|at| self.members[*at].log(log).unwrap().is_leader()) else {
                    continue;
                };
                for wave in 0.. {
                    assert!(wave < 8, "log {log} never caught up");
                    let (last, commit) = {
                        let node = self.members[leader].log(log).unwrap();
                        (node.last_log_index(), node.commit_index())
                    };
                    let mut appends = Vec::new();
                    for follower in 0..self.members.len() {
                        if follower == leader {
                            continue;
                        }
                        let held = self.members[follower].log(log).unwrap();
                        if held.last_log_index() == last && held.commit_index() == commit {
                            continue;
                        }
                        let id = HostId(follower as u64 + 1);
                        if let Some(append) = self.members[leader].log_mut(log).unwrap().replicate_to(id, BUDGET) {
                            self.publish(leader);
                            self.sent(|| RaftMessage::AppendEntries(append.clone()));
                            alloc::aside();
                            appends.push((follower, append));
                            alloc::back();
                        }
                    }
                    if appends.is_empty() {
                        break;
                    }
                    let mut replies = Vec::new();
                    for (follower, append) in appends {
                        let reply = self.members[follower].log_mut(log).unwrap().on_append_entries(append);
                        self.publish(follower);
                        self.sent(|| RaftMessage::AppendReply(reply.clone()));
                        alloc::aside();
                        replies.push(reply);
                        alloc::back();
                    }
                    for reply in replies {
                        self.members[leader].log_mut(log).unwrap().on_append_reply(reply);
                        self.publish(leader);
                    }
                }
            }
        }

        fn replicate_in_turn(&mut self) {
            let logs = self.members[0].count();
            for log in 0..logs {
                let Some(leader) = (0..self.members.len()).find(|at| self.members[*at].log(log).unwrap().is_leader()) else {
                    continue;
                };
                for follower in 0..self.members.len() {
                    if follower == leader {
                        continue;
                    }
                    for _ in 0..4 {
                        let (last, commit) = {
                            let node = self.members[leader].log(log).unwrap();
                            (node.last_log_index(), node.commit_index())
                        };
                        let held = self.members[follower].log(log).unwrap();
                        if held.last_log_index() == last && held.commit_index() == commit {
                            break;
                        }
                        let id = HostId(follower as u64 + 1);
                        let Some(append) = self.members[leader].log_mut(log).unwrap().replicate_to(id, BUDGET) else {
                            break;
                        };
                        self.publish(leader);
                        self.sent(|| RaftMessage::AppendEntries(append.clone()));
                        let reply = self.members[follower].log_mut(log).unwrap().on_append_entries(append);
                        self.publish(follower);
                        self.sent(|| RaftMessage::AppendReply(reply.clone()));
                        self.members[leader].log_mut(log).unwrap().on_append_reply(reply);
                        self.publish(leader);
                    }
                }
            }
        }

        fn apply_all(&mut self) -> usize {
            let mut barriers = 0;
            for at in 0..self.members.len() {
                for applied in self.members[at].apply_ready() {
                    super::fold(&mut self.digests[at], &applied.command, applied.epoch);
                    self.applied += 1;
                }
                barriers += self.members[at].append_barriers();
                self.publish(at);
            }
            barriers
        }

        pub fn round(&mut self, batch: Vec<(Option<u64>, Vec<u8>)>) {
            for (route, data) in batch {
                let route = route.map_or(Route::Global, Route::Key);
                let log = self.members[0].route(route);
                let leader = (0..self.members.len()).find(|at| self.members[*at].log(log).unwrap().is_leader()).unwrap();
                assert!(self.members[leader].propose(route, data));
                self.publish(leader);
            }
            loop {
                self.replicate();
                // Commits reach the followers with the next append.
                self.replicate();
                if self.apply_all() == 0 {
                    break;
                }
            }
        }

        pub fn applied(&self) -> u64 {
            self.applied
        }
        pub fn digests(&self) -> &[u64] {
            &self.digests
        }
    }
}

/// The layers, by name.
pub const LAYERS: [&str; 4] = ["hyper-multilog", "slates-multilog", "slates-multilog-in-turn", "slates-multilog+publication"];

/// One measured run of `layer` on `spec`: `rounds` rounds of `batch` commands after the setup.
pub fn run(layer: &str, spec: &Spec, seed: u64, counting: bool) -> Measured {
    let mut state = seed;
    let mut measured = Measured::default();
    let mut first = 0u64;
    macro_rules! drive {
        ($group:expr, $reserve:expr) => {{
            let mut group = $group;
            // Warm: one round, outside the count.
            group.round(commands(&mut state, first, spec, $reserve));
            first += spec.batch as u64;
            group.wire = counting;
            measured.rounds.reserve_exact(spec.rounds);
            if counting {
                alloc::begin();
            }
            let before = faults::read().expect("the OS counts faults");
            let account = usage::this().expect("the OS accounts for the process");
            let started = Instant::now();
            for _ in 0..spec.rounds {
                let batch = commands(&mut state, first, spec, $reserve);
                first += spec.batch as u64;
                let round = Instant::now();
                group.round(batch);
                let took = round.elapsed().as_nanos() as u64;
                alloc::aside();
                measured.rounds.push(took);
                alloc::back();
            }
            measured.elapsed = started.elapsed();
            measured.usage = usage::this().expect("the OS accounts for the process").since(&account);
            measured.faults = faults::read().expect("the OS counts faults").since(&before);
            if counting {
                measured.total = alloc::end();
                measured.aside = alloc::read_aside();
            }
            measured.messages = group.messages;
            measured.wire = group.bytes;
            let digests = group.digests();
            assert!(digests.windows(2).all(|pair| pair[0] == pair[1]) || spec.logs > 1, "members applied alike");
            assert_eq!(group.applied(), (spec.rounds as u64 + 1) * spec.batch as u64 * VOTERS);
        }};
    }
    match layer {
        "hyper-multilog" => drive!(hyper::Group::new(spec, seed), hyper::RESERVE),
        "slates-multilog" => drive!(slates::Group::new(spec, seed, false, true), 0),
        "slates-multilog-in-turn" => drive!(slates::Group::new(spec, seed, false, false), 0),
        "slates-multilog+publication" => drive!(slates::Group::new(spec, seed, true, true), 0),
        _ => panic!("no layer named {layer}"),
    }
    measured.ops = (spec.rounds * spec.batch) as u64;
    measured
}

/// The 1-based ranks of the order statistics that bracket the `q`-quantile of `n` samples with at
/// least `coverage` (David and Nagaraja, *Order Statistics*, §7.1): the count of samples below the
/// quantile is Binomial(`n`, `q`), so `[X(l), X(u)]` covers it with probability
/// `P(l <= B < u)`. `None` for the upper rank when it would pass the largest sample: too few
/// samples to bound the quantile from above.
pub fn ranks(n: usize, q: f64, coverage: f64) -> (usize, Option<usize>) {
    let tail = (1.0 - coverage) / 2.0;
    // The binomial's cumulative distribution, in log space for the pmf so no term underflows
    // before it is added.
    let (lq, lp) = (q.ln(), (1.0 - q).ln());
    let mut ln_pmf = n as f64 * lp;
    let mut cdf = Vec::with_capacity(n + 1);
    let mut sum = 0.0;
    for k in 0..=n {
        sum += ln_pmf.exp();
        cdf.push(sum.min(1.0));
        if k < n {
            ln_pmf += ((n - k) as f64).ln() - ((k + 1) as f64).ln() + lq - lp;
        }
    }
    // l: the largest rank with P(B < l) <= tail; u: the smallest with P(B < u) >= 1 - tail.
    let below = |rank: usize| if rank == 0 { 0.0 } else { cdf[rank - 1] };
    let mut lower = 1;
    while lower < n && below(lower + 1) <= tail {
        lower += 1;
    }
    let upper = (lower..=n).find(|rank| below(*rank) >= 1.0 - tail);
    (lower, upper)
}

/// The coverage of every reported quantile's interval.
pub const COVERAGE: f64 = 0.95;

/// `q`'s estimate (the order statistic at rank ceil(n·q)) and its interval in `sorted`, or the
/// estimate with an unresolved upper end.
pub fn quantile(sorted: &[u64], q: f64) -> (u64, u64, Option<u64>) {
    let n = sorted.len();
    let at = ((n as f64 * q).ceil() as usize).clamp(1, n);
    let (lower, upper) = ranks(n, q, COVERAGE);
    (sorted[at - 1], sorted[lower - 1], upper.map(|rank| sorted[rank - 1]))
}

/// One measurement's line, as `one-multilog` prints it: the counts, then every round's time.
pub fn print(measured: &Measured) -> String {
    let core = measured.total.less(&measured.aside);
    let u = &measured.usage;
    let mut line = format!(
        "{} {} {} {} {} {} {} {} {} {} {} {} {} {}",
        measured.ops,
        measured.elapsed.as_nanos(),
        core.allocations,
        core.reallocations,
        core.bytes,
        u.user_ns,
        u.system_ns,
        u.instructions.unwrap_or(0),
        u.cycles.unwrap_or(0),
        u.energy_nj.unwrap_or(0),
        u.wakeups.unwrap_or(0),
        u.peak_footprint.unwrap_or(0),
        measured.messages,
        measured.wire,
    );
    for round in &measured.rounds {
        line.push(' ');
        line.push_str(&round.to_string());
    }
    line
}

/// A printed line read back.
struct Line {
    ops: f64,
    allocs: f64,
    reallocs: f64,
    bytes: f64,
    user: f64,
    system: f64,
    instructions: f64,
    cycles: f64,
    energy: f64,
    wakeups: f64,
    peak: f64,
    messages: f64,
    wire: f64,
    rounds: Vec<u64>,
}

fn parse(line: &str) -> Line {
    let words: Vec<u64> = line.split_whitespace().map(|word| word.parse().expect("a number")).collect();
    let f = |at: usize| words[at] as f64;
    Line {
        ops: f(0),
        allocs: f(2),
        reallocs: f(3),
        bytes: f(4),
        user: f(5),
        system: f(6),
        instructions: f(7),
        cycles: f(8),
        energy: f(9),
        wakeups: f(10),
        peak: f(11),
        messages: f(12),
        wire: f(13),
        rounds: words[14..].to_vec(),
    }
}

fn spawn(layer: &str, spec: &Spec, seed: u64, mode: &str) -> Line {
    let output = std::process::Command::new(std::env::current_exe().expect("this program"))
        .arg("one-multilog")
        .arg(layer)
        .args([spec.logs, spec.batch, spec.bytes, spec.rounds].map(|v| v.to_string()))
        .arg(seed.to_string())
        .arg(mode)
        .output()
        .expect("a run");
    assert!(output.status.success(), "{layer} {spec:?}: {}", String::from_utf8_lossy(&output.stderr));
    parse(String::from_utf8_lossy(&output.stdout).lines().last().expect("a line"))
}

/// The machine's load averages, as `uptime` prints them.
fn load() -> String {
    let output = std::process::Command::new("uptime").output().expect("uptime");
    let text = String::from_utf8_lossy(&output.stdout).to_string();
    text.rsplit_once("load average").map_or(text.clone(), |(_, rest)| rest.trim_start_matches(['s', ':', ' ']).trim().to_string())
}

fn ms(ns: u64) -> String {
    format!("{:.3}", ns as f64 / 1e6)
}

/// hyper-multilog against slates' MLRaft on slates' workload: one and three logs, a command a round
/// and 64, `runs` processes a layer a shape, interleaved, every round of every run pooled for the
/// quantiles.
pub fn table(runs: usize, rounds: usize) {
    println!("| workload | layer | load before | round p50 [95% interval] | p99 | p99.9 | max | CPU ns/command (user+sys) | instructions/command | cycles/command | energy nJ/command | wakeups/1k commands | allocs/command | reallocs/command | alloc bytes/command | messages/command | wire bytes/command | peak footprint MiB |");
    println!("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|");
    for (logs, batch) in [(1, 1), (3, 1), (1, 64), (3, 64)] {
        let spec = Spec { logs, batch, bytes: 64, rounds };
        let mut timed: Vec<Vec<Line>> = Vec::new();
        timed.resize_with(LAYERS.len(), Vec::new);
        let mut counted: Vec<Vec<Line>> = Vec::new();
        counted.resize_with(LAYERS.len(), Vec::new);
        let mut loads: Vec<String> = vec![String::new(); LAYERS.len()];
        for run in 0..runs {
            for (at, layer) in LAYERS.iter().enumerate() {
                if batch > 1 && layer.ends_with("publication") {
                    continue;
                }
                if run == 0 {
                    loads[at] = load();
                }
                timed[at].push(spawn(layer, &spec, 1_000 + run as u64, "time"));
                counted[at].push(spawn(layer, &spec, 1_000 + run as u64, "count"));
            }
        }
        for (at, layer) in LAYERS.iter().enumerate() {
            if timed[at].is_empty() {
                continue;
            }
            let mut all: Vec<u64> = timed[at].iter().flat_map(|line| line.rounds.iter().copied()).collect();
            all.sort_unstable();
            let ops: f64 = timed[at].iter().map(|line| line.ops).sum();
            let sum = |lines: &[Line], field: fn(&Line) -> f64| lines.iter().map(field).sum::<f64>();
            let cell = |q: f64| {
                let (estimate, lower, upper) = quantile(&all, q);
                match upper {
                    Some(upper) => format!("{} [{}–{}]", ms(estimate), ms(lower), ms(upper)),
                    None => format!("{} [{}–unresolved]", ms(estimate), ms(lower)),
                }
            };
            let counted_ops: f64 = sum(&counted[at], |line| line.ops);
            let peak = timed[at].iter().map(|line| line.peak).fold(0.0, f64::max);
            println!(
                "| {logs} logs b{batch} 64B | {layer} | {} | {} | {} | {} | {} | {:.0} | {:.0} | {:.0} | {:.0} | {:.2} | {:.2} | {:.3} | {:.0} | {:.2} | {:.0} | {:.1} |",
                loads[at],
                cell(0.5),
                cell(0.99),
                cell(0.999),
                ms(*all.last().unwrap()),
                (sum(&timed[at], |l| l.user) + sum(&timed[at], |l| l.system)) / ops,
                sum(&timed[at], |l| l.instructions) / ops,
                sum(&timed[at], |l| l.cycles) / ops,
                sum(&timed[at], |l| l.energy) / ops,
                1e3 * sum(&timed[at], |l| l.wakeups) / ops,
                sum(&counted[at], |l| l.allocs) / counted_ops,
                sum(&counted[at], |l| l.reallocs) / counted_ops,
                sum(&counted[at], |l| l.bytes) / counted_ops,
                sum(&counted[at], |l| l.messages) / counted_ops,
                sum(&counted[at], |l| l.wire) / counted_ops,
                peak / (1u64 << 20) as f64,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    /// The ranks bracketing the median of 100 samples at 95%, and those of the 99th percentile of
    /// 2,000, as the exact binomial gives them (computed apart in exact rationals: coverage 0.9648
    /// and 0.9578); the 99.9th of 2,000 has no upper rank within 2,000 at 95%: unresolved.
    #[test]
    fn the_ranks_are_the_binomials() {
        assert_eq!(super::ranks(100, 0.5, 0.95), (40, Some(61)));
        assert_eq!(super::ranks(2_000, 0.99, 0.95), (1971, Some(1989)));
        assert_eq!(super::ranks(2_000, 0.999, 0.95), (1995, None));
    }
}
