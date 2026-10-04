//! One member's share of a multilog group (`docs/multilog.md` §9): its member of each of the `n`
//! logs, the merge over them, the barriers it owes, and the configurations as of the merge.
//!
//! The owner drives each log's member as it drives a single group ([`MultiLog::node_mut`]: `ready`,
//! persist, send, `on_persist`), and in four places goes through the layer instead: proposals
//! ([`MultiLog::propose`]), messages ([`MultiLog::step`]), the committed entries a `Ready` gives
//! ([`MultiLog::hand_over`]), and the state machine's commands ([`MultiLog::apply`], in the merged
//! order). After every drive it asks for the barriers it owes ([`MultiLog::barriers`]).

use std::collections::VecDeque;

use hyper_raft::proto::{
    ConfChange, ConfChangeV2, ConfState, Entry, EntryType, Message, MessageType,
};
use hyper_raft::wire::Record;
use hyper_raft::{Config, NodeId, RawNode, StateRole, Storage, StorageError, proto};

use crate::entry::{self, Stated};
use crate::error::{Error, Result};
use crate::merge::{Advance, Applied, Flow, Logs, Merge};
use crate::point::{At, Point};
use crate::route::{Route, log_of, mix};

/// What the layer bounds beyond what each log's member bounds (`docs/multilog.md` §6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// The entries a log may hold past what its merge consumed before its leader refuses a
    /// client's proposal there: the retained log the owner allows between images. Barriers are
    /// always taken, being what lets the merge move.
    pub unmerged: u64,
}

/// Whether an image the owner installs is ahead of the member's state (`docs/multilog.md` §5.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Installed {
    /// It is: the owner replaces its state machine with the image's, and the merge resumes at its
    /// cut.
    Ahead,
    /// The member's state holds it already: nothing changes.
    Held,
}

/// What a leader does with a forwarded proposal (`docs/multilog.md` §3.1, §3.2).
enum Screened {
    /// Steps it into the log's member.
    Pass,
    /// Drops it: barriers a barrier of this leader's or its merge's covers already.
    Drop,
}

/// A log's configuration as of the merge's position, and the changes applied since, in order,
/// each with its index (`docs/multilog.md` §5.4).
#[derive(Clone, Debug, Default)]
struct Configurations {
    at_merge: ConfState,
    since: VecDeque<(u64, ConfState)>,
}

/// One member's share of a multilog group.
pub struct MultiLog<S> {
    nodes: Vec<RawNode<S>>,
    merge: Merge,
    /// For each log, the term this member last proposed a barrier there in, and the global it
    /// named.
    proposed: Vec<(u64, u64)>,
    /// The highest log-0 index of a global command this member's log 0 has given to apply.
    latest_global: u64,
    configurations: Vec<Configurations>,
    /// The voters, best first, as the owner last ranked them ([`MultiLog::spread`]).
    ranked: Vec<NodeId>,
    limits: Limits,
}

/// The logs' storage as the merge reads it.
struct Stores<'a, S> {
    nodes: &'a [RawNode<S>],
}

impl<S: Storage> Logs for Stores<'_, S> {
    fn count(&self) -> usize {
        self.nodes.len()
    }
    fn through(&self, log: usize) -> u64 {
        self.nodes.get(log).map_or(0, RawNode::given_to_apply)
    }
    fn walk(
        &self,
        log: usize,
        from: u64,
        through: u64,
        visit: &mut dyn FnMut(&Entry) -> bool,
    ) -> std::result::Result<(), StorageError> {
        let node = self
            .nodes
            .get(log)
            .ok_or(StorageError::Other("no log of that number"))?;
        let high = through
            .checked_add(1)
            .ok_or(StorageError::Other("an index past the last"))?;
        node.store().any_entry(from, high, visit).map(|_| ())
    }
}

/// The seed log `log`'s member draws its election timeouts from: SplitMix64's finalizer of the
/// member's seed and the log's own hash, so that two logs of one member draw apart (`docs/raft.md`
/// §3.2, R7).
fn seed_of(seed: u64, log: usize) -> u64 {
    mix(seed ^ mix(u64::try_from(log).unwrap_or(u64::MAX)))
}

/// The change a committed entry states, as the core's shells read it (hyper-durable's
/// `change_of`): a change of one member as a joint change, an empty one as leaving a joint
/// configuration.
fn change_of(entry: &Entry) -> Result<ConfChangeV2> {
    let decoded = match entry.entry_type {
        EntryType::EntryNormal => return Err(Error::Invariant("a normal entry read as a change")),
        EntryType::EntryConfChange if entry.data.is_empty() => {
            Ok(proto::joint(&ConfChange::default()))
        }
        EntryType::EntryConfChange => {
            ConfChange::decode(&entry.data).map(|change| proto::joint(&change))
        }
        EntryType::EntryConfChangeV2 if entry.data.is_empty() => Ok(ConfChangeV2::default()),
        EntryType::EntryConfChangeV2 => ConfChangeV2::decode(&entry.data),
    };
    decoded.map_err(|_| Error::Invariant("a committed change does not decode"))
}

impl<S: Storage> MultiLog<S> {
    /// Member `member.id` of `stores.len()` logs, each opened on its store, resuming at `point`:
    /// the image the owner restarts from, or the origin. Every log's member is `member`'s
    /// settings, with an election seed of its own and the cut's position as what is applied.
    /// Refused for no log, more logs than a `u32` numbers, a point of another count, storage that
    /// does not hold the cut, the fast track or entries applied before they are durable
    /// (`docs/multilog.md` §9).
    pub fn open(member: &Config, stores: Vec<S>, point: &Point, limits: Limits) -> Result<Self> {
        let count = stores.len();
        Self::check(member, count, point, limits)?;
        let mut nodes = Vec::new();
        nodes
            .try_reserve_exact(count)
            .map_err(|_| Error::Capacity("logs"))?;
        for (log, (store, next)) in stores.into_iter().zip(point.cut.next()).enumerate() {
            holds_cut(&store, *next).map_err(|error| Error::of(log, error))?;
            let mut config = member.clone();
            config.seed = seed_of(member.seed, log);
            config.applied = next
                .checked_sub(1)
                .ok_or(Error::Invariant("a cut at index zero"))?;
            nodes.push(RawNode::new(&config, store).map_err(|error| Error::of(log, error))?);
        }
        let configurations = point
            .logs
            .iter()
            .map(|at| Configurations {
                at_merge: at.configuration.clone(),
                since: VecDeque::new(),
            })
            .collect();
        Ok(Self {
            nodes,
            merge: Merge::at(&point.cut),
            proposed: vec![(0, 0); count],
            latest_global: point.cut.epoch(),
            configurations,
            ranked: Vec::new(),
            limits,
        })
    }

    fn check(member: &Config, count: usize, point: &Point, limits: Limits) -> Result<()> {
        if count == 0 {
            return Err(Error::Settings("no log"));
        }
        u32::try_from(count).map_err(|_| Error::Settings("more logs than a u32 numbers"))?;
        if point.cut.logs() != count || point.logs.len() != count {
            return Err(Error::Settings("a point of another count of logs"));
        }
        if member.fast {
            return Err(Error::Settings("the fast track in a multilog"));
        }
        if member.apply_unpersisted {
            return Err(Error::Settings(
                "entries given before they are durable, where the merge reads storage",
            ));
        }
        if limits.unmerged == 0 {
            return Err(Error::Settings("no entry allowed past the merge"));
        }
        Ok(())
    }

    /// How many logs.
    pub fn count(&self) -> usize {
        self.nodes.len()
    }
    /// This member's member of `log`.
    pub fn node(&self, log: usize) -> Option<&RawNode<S>> {
        self.nodes.get(log)
    }
    /// This member's member of `log`, to drive as a single group's.
    pub fn node_mut(&mut self, log: usize) -> Option<&mut RawNode<S>> {
        self.nodes.get_mut(log)
    }
    /// The merge.
    pub fn merge(&self) -> &Merge {
        &self.merge
    }
    /// The log `route` names.
    pub fn route(&self, route: Route) -> usize {
        route.log(self.nodes.len())
    }
    /// The last index of `log` the merge has consumed: a read of `log` asked at an index at or
    /// below it may be served (`docs/multilog.md` §8).
    pub fn merged_through(&self, log: usize) -> Option<u64> {
        self.merge.next(log).and_then(|next| next.checked_sub(1))
    }
    /// The highest log-0 index of a global command this member's log 0 has given to apply.
    pub fn latest_global(&self) -> u64 {
        self.latest_global
    }

    fn at(&mut self, log: usize) -> Result<&mut RawNode<S>> {
        self.nodes.get_mut(log).ok_or(Error::NoLog(log))
    }

    /// Proposes `command` where `route` sends it: appended where this member leads that log, and
    /// forwarded to its leader otherwise, as the core forwards proposals. The log's number.
    /// Refused at a leader whose log holds [`Limits::unmerged`] entries past its merge.
    pub fn propose(&mut self, route: Route, command: Vec<u8>) -> Result<usize> {
        let log = self.route(route);
        self.admit(log)?;
        let data = match route {
            Route::Global => entry::global(command)?,
            Route::Key(key) => entry::keyed_command(command, key)?,
        };
        self.at(log)?
            .propose(Vec::new(), data)
            .map_err(|error| Error::of(log, error))?;
        Ok(log)
    }

    /// Refuses a client's command at a leader of `log` whose log holds [`Limits::unmerged`]
    /// entries past what its merge consumed.
    fn admit(&self, log: usize) -> Result<()> {
        let node = self.nodes.get(log).ok_or(Error::NoLog(log))?;
        if node.raft.state() != StateRole::Leader {
            return Ok(());
        }
        let last = node
            .raft
            .log()
            .last_index()
            .map_err(|error| Error::of(log, error))?;
        let merged = self.merged_through(log).ok_or(Error::NoLog(log))?;
        // A log that ends before its merge (moved there by an image) holds nothing past it.
        if last.saturating_sub(merged) >= self.limits.unmerged {
            return Err(Error::Capacity("unmerged"));
        }
        Ok(())
    }

    /// A message for `log` from the network. A forwarded proposal is screened first: one out of
    /// place is refused, and one of barriers a leader's barrier covers is dropped
    /// (`docs/multilog.md` §3.1, §3.2).
    pub fn step(&mut self, log: usize, message: Message) -> Result<()> {
        if message.msg_type == MessageType::MsgPropose {
            if let Screened::Drop = self.screen(log, &message)? {
                return Ok(());
            }
            let barrier = message
                .entries
                .iter()
                .filter_map(|entry| match entry::read(entry) {
                    Stated::Barrier(named) => Some(named),
                    _ => None,
                })
                .max();
            self.step_member(log, message)?;
            if let Some(named) = barrier {
                self.leader_took(log, named)?;
            }
            return Ok(());
        }
        self.step_member(log, message)
    }

    fn step_member(&mut self, log: usize, message: Message) -> Result<()> {
        self.at(log)?
            .step(message)
            .map_err(|error| Error::of(log, error))
    }

    /// A leader of `log` took a forwarded barrier naming `named`: it covers that global this term.
    fn leader_took(&mut self, log: usize, named: u64) -> Result<()> {
        let node = self.nodes.get(log).ok_or(Error::NoLog(log))?;
        if node.raft.state() != StateRole::Leader {
            return Ok(());
        }
        let term = node.raft.term();
        if let Some(proposed) = self.proposed.get_mut(log) {
            let held = if proposed.0 == term { proposed.1 } else { 0 };
            *proposed = (term, held.max(named));
        }
        Ok(())
    }

    /// What a forwarded proposal for `log` is: refused if out of place, dropped if it is barriers
    /// this member, leading `log`, covers.
    fn screen(&self, log: usize, message: &Message) -> Result<Screened> {
        let mut barriers_covered = !message.entries.is_empty();
        let mut commands = false;
        for entry in &message.entries {
            match entry::read(entry) {
                Stated::Own => barriers_covered = false,
                Stated::Global(_) if log == 0 => commands = true,
                Stated::Global(_) => {
                    return Err(Error::Violation("a global command outside log 0"));
                }
                Stated::Keyed { key, .. } if log_of(key, self.nodes.len()) == log => {
                    commands = true
                }
                Stated::Keyed { .. } => {
                    return Err(Error::Violation("a keyed command in another log"));
                }
                Stated::Barrier(_) if log == 0 => {
                    return Err(Error::Violation("a barrier in log 0"));
                }
                Stated::Barrier(named) => barriers_covered &= self.leads_covering(log, named)?,
                Stated::Malformed => return Err(Error::Violation("bytes no member writes")),
            }
        }
        if commands {
            self.admit(log)?;
            return Ok(Screened::Pass);
        }
        Ok(if barriers_covered {
            Screened::Drop
        } else {
            Screened::Pass
        })
    }

    /// Whether this member leads `log` and a barrier it appended this term, or one its merge read
    /// there, names `named` or later.
    fn leads_covering(&self, log: usize, named: u64) -> Result<bool> {
        let node = self.nodes.get(log).ok_or(Error::NoLog(log))?;
        Ok(node.raft.state() == StateRole::Leader && self.covered(log)? >= named)
    }

    /// The highest global a barrier in `log` this member knows of covers: one its merge read
    /// there, one it proposed in the log's current term, or any global already applied.
    fn covered(&self, log: usize) -> Result<u64> {
        let node = self.nodes.get(log).ok_or(Error::NoLog(log))?;
        let (term, named) = self.proposed.get(log).copied().unwrap_or_default();
        let proposed = if term == node.raft.term() { named } else { 0 };
        Ok(self
            .merge
            .epoch()
            .max(self.merge.head(log).unwrap_or(0))
            .max(proposed))
    }

    /// The committed entries `log`'s member gave to apply (a `Ready`'s or a notice's): their
    /// changes of configuration are applied to the member now, in order, and the member is told
    /// they are applied; the state machine's commands come from [`MultiLog::apply`]. The
    /// configuration the last change made, for an owner that acts on its members.
    pub fn hand_over(&mut self, log: usize, entries: &[Entry]) -> Result<Option<ConfState>> {
        let mut changed = None;
        for entry in entries {
            if entry.entry_type == EntryType::EntryNormal {
                self.note_global(log, entry);
            } else if let Some(configuration) = self.change(log, entry)? {
                changed = Some(configuration);
            }
        }
        if let Some(last) = entries.last() {
            self.at(log)?
                .advance_apply_to(last.index)
                .map_err(|error| Error::of(log, error))?;
        }
        Ok(changed)
    }

    /// Notes a global command log 0 gave to apply: the barriers owed name it.
    fn note_global(&mut self, log: usize, entry: &Entry) {
        if log == 0 && matches!(entry::read(entry), Stated::Global(_)) {
            self.latest_global = self.latest_global.max(entry.index);
        }
    }

    /// Applies the change `entry` states to `log`'s member. A change the core refuses is refused
    /// alike by every member and leaves the configuration as it was.
    fn change(&mut self, log: usize, entry: &Entry) -> Result<Option<ConfState>> {
        let change = change_of(entry)?;
        let configuration = match self.at(log)?.apply_conf_change(&change) {
            Ok(configuration) => configuration,
            Err(error) if error.is_fatal() => return Err(Error::of(log, error)),
            Err(_) => return Ok(None),
        };
        let below_merge = self.merge.next(log).is_some_and(|next| entry.index < next);
        if !below_merge {
            let held = self.configurations.get_mut(log).ok_or(Error::NoLog(log))?;
            held.since
                .try_reserve(1)
                .map_err(|_| Error::Capacity("configurations past the merge"))?;
            held.since.push_back((entry.index, configuration.clone()));
        }
        Ok(Some(configuration))
    }

    /// Hands `apply` the commands the logs now allow, in the merged order, until no log moves, the
    /// commands' bytes reach `budget` (one at least), or `apply` says stop (`docs/multilog.md`
    /// §4.1).
    pub fn apply(
        &mut self,
        budget: u64,
        apply: &mut dyn FnMut(Applied<'_>) -> Flow,
    ) -> Result<Advance> {
        let advance = self
            .merge
            .advance(&Stores { nodes: &self.nodes }, budget, apply)?;
        self.settle_configurations();
        Ok(advance)
    }

    /// The changes the merge has passed become each log's configuration as of the merge.
    fn settle_configurations(&mut self) {
        for (log, held) in self.configurations.iter_mut().enumerate() {
            let next = self.merge.next(log).unwrap_or(0);
            while let Some((_, configuration)) = held.since.pop_front_if(|(index, _)| *index < next)
            {
                held.at_merge = configuration;
            }
        }
    }

    /// Proposes, in every log but log 0, a barrier naming the latest global command this member's
    /// log 0 has given to apply, where none it knows of covers it (`docs/multilog.md` §3.1): at
    /// most one a log for each term and global. How many it proposed.
    pub fn barriers(&mut self) -> Result<usize> {
        let mut proposed = 0usize;
        for log in 1..self.nodes.len() {
            let latest = self.latest_global;
            if latest <= self.covered(log)? {
                continue;
            }
            let data = entry::barrier_naming(latest)?;
            let node = self.at(log)?;
            let term = node.raft.term();
            match node.propose(Vec::new(), data) {
                Ok(()) => {
                    if let Some(made) = self.proposed.get_mut(log) {
                        *made = (term, latest);
                    }
                    proposed = proposed.saturating_add(1);
                }
                Err(error) if error.is_fatal() => return Err(Error::of(log, error)),
                // No leader to forward to, or one handing over: asked again at the next call.
                Err(_) => {}
            }
        }
        Ok(proposed)
    }

    /// Ranks the voters, best first, for every log (`docs/multilog.md` §7): log `k` prefers the
    /// voter at place `k` of `ranked`, cyclically, and falls back through the same order. This
    /// member's priority in each log is the count of ranked voters it is ahead of or level with
    /// there, from the count of them for the preferred down to one; a member not ranked has none.
    pub fn spread(&mut self, ranked: &[NodeId]) -> Result<()> {
        let mut held = Vec::new();
        held.try_reserve_exact(ranked.len())
            .map_err(|_| Error::Capacity("ranked voters"))?;
        held.extend_from_slice(ranked);
        self.ranked = held;
        for log in 0..self.nodes.len() {
            let priority = self.priority_in(log);
            self.at(log)?.set_priority(priority);
        }
        Ok(())
    }

    /// This member's priority in `log` by the last ranking.
    fn priority_in(&self, log: usize) -> i64 {
        let len = self.ranked.len();
        let Some(id) = self.nodes.first().map(|node| node.raft.id()) else {
            return 0;
        };
        let Some(place) = self.ranked.iter().position(|voter| *voter == id) else {
            return 0;
        };
        let shift = log.checked_rem(len).unwrap_or(0);
        let rotated = place
            .checked_add(len)
            .and_then(|at| at.checked_sub(shift))
            .and_then(|at| at.checked_rem(len))
            .unwrap_or(0);
        i64::try_from(len.saturating_sub(rotated)).unwrap_or(i64::MAX)
    }

    /// The voter `log` prefers by the last ranking.
    pub fn preferred(&self, log: usize) -> Option<NodeId> {
        let at = log.checked_rem(self.ranked.len())?;
        self.ranked.get(at).copied()
    }

    /// The voter this member, leading `log`, would hand it to: the log's preferred voter, where
    /// that is another, its leader hears it, and it holds the leader's whole log. When to hand
    /// over is the owner's policy (`docs/multilog.md` §7).
    pub fn hand_off(&self, log: usize) -> Option<NodeId> {
        let node = self.nodes.get(log)?;
        if node.raft.state() != StateRole::Leader {
            return None;
        }
        let preferred = self.preferred(log)?;
        if preferred == node.raft.id() {
            return None;
        }
        let progress = node.raft.tracker().get(preferred)?;
        let last = node.raft.log().last_index().ok()?;
        (progress.recent_active && progress.matched == last).then_some(preferred)
    }

    /// The point an image may be taken at now, if the merge's position is canonical
    /// (`docs/multilog.md` §5.2): the cut, and each log's term and configuration there.
    pub fn point(&self) -> Result<Option<Point>> {
        if !self.merge.canonical() {
            return Ok(None);
        }
        let cut = self.merge.cut();
        let mut logs = Vec::new();
        logs.try_reserve_exact(self.nodes.len())
            .map_err(|_| Error::Capacity("logs"))?;
        for (log, (node, held)) in self.nodes.iter().zip(&self.configurations).enumerate() {
            let last = cut
                .next()
                .get(log)
                .and_then(|next| next.checked_sub(1))
                .ok_or(Error::Invariant("a cut at index zero"))?;
            let term = if last == 0 {
                0
            } else {
                node.raft
                    .log()
                    .term(last)
                    .map_err(|error| Error::of(log, error))?
            };
            logs.push(At {
                term,
                configuration: held.at_merge.clone(),
            });
        }
        Ok(Some(Point { cut, logs }))
    }

    /// An image at `point` reached this member through a log's snapshot (`docs/multilog.md` §5.3):
    /// if it is ahead of the member's state, the merge resumes at its cut and each log's
    /// configuration as of the merge is the point's.
    pub fn install(&mut self, point: &Point) -> Result<Installed> {
        if point.cut.logs() != self.nodes.len() || point.logs.len() != self.nodes.len() {
            return Err(Error::Invariant("an image of another count of logs"));
        }
        if self.merge.cut().holds(&point.cut) {
            return Ok(Installed::Held);
        }
        self.merge = Merge::at(&point.cut);
        for ((held, at), next) in self
            .configurations
            .iter_mut()
            .zip(&point.logs)
            .zip(point.cut.next())
        {
            held.at_merge = at.configuration.clone();
            held.since.retain(|(index, _)| index >= next);
        }
        self.latest_global = self.latest_global.max(point.cut.epoch());
        Ok(Installed::Ahead)
    }
}

/// Whether `store` holds the cut's next index of its log: the entry before it is its snapshot's
/// or held, and the log does not end before it (`docs/multilog.md` §5.5).
fn holds_cut<S: Storage>(store: &S, next: u64) -> hyper_raft::Result<()> {
    let first = store.first_index()?;
    let last = store.last_index()?;
    let after = last
        .checked_add(1)
        .ok_or(hyper_raft::Error::Invariant("a log past the last index"))?;
    if next < first || next > after {
        return Err(hyper_raft::Error::Invariant(
            "storage does not hold the image's cut",
        ));
    }
    Ok(())
}
