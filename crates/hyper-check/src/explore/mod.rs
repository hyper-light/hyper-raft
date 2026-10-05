//! Exhaustive search of a model's reachable classes (`docs/sim.md` §4.5, "Exhaustive, model
//! level"): slates' design (`crates/cluster/tests/support/exhaustive.rs`), built again here under
//! the production lints, with its memory held to a budget it accounts exactly.
//!
//! A [`Model`] names its states, its actions, its faults and a packed representative of a state's
//! class under the renamings its actions commute with (its symmetry: members that play alike,
//! values that are interchangeable). Two searches visit every reachable class once and check every
//! step:
//!
//! - [`shortest`]: breadth first on one thread, each class's key and parent kept in an id table,
//!   so the first fault it meets is reached by a shortest history, which it replays concretely
//!   (each representative renames its state, so the actions are found again step by step).
//! - [`explore`]: breadth first level by level on a stated number of scoped workers, over 128-bit
//!   fingerprints of the representatives held in one shard a worker. Each worker expands a slice of
//!   the frontier against a read-only view of the shards; then each shard takes the successors that
//!   hash to it. The workers share nothing mutably and are joined before each phase ends (slates'
//!   design, the one exception `docs/sim.md` §2 states to "no thread"). The classes it counts do not
//!   depend on the number of workers.
//!
//! Two classes share a fingerprint with probability 2⁻¹²⁸, so some pair among `n` does with
//! probability below `n²/2¹²⁹` (under 10⁻²¹ for a billion classes), and only then could a class be
//! skipped.
//!
//! **Memory.** Every table and vector a search holds is counted at its capacity, a growth counted
//! with the old buffer and the new one together, and a search that would pass its [`Budget`]
//! stops and says [`Outcome::Unknown`] with what it reached: never a pass. slates' search needed a
//! factor of two between resident and accounted memory for the growth it did not count
//! (`RESIDENT_PER_ACCOUNTED`, measured 2,689 MB against 1,556 MB); here growth is counted, and the
//! tests hold the counting allocator's peak to the budget.

mod pack;
pub mod rounds;
mod symmetry;

pub use pack::{PackError, Packer};
pub use symmetry::least_over_ties;

use std::fmt;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::num::NonZeroUsize;

use crate::search::{Budget, Spent};
use crate::table::{PrintSet, bytes_of};

/// A model the searches run.
pub trait Model: Sync {
    /// A state.
    type State: Copy + Eq + Send + Sync + fmt::Debug;
    /// One step.
    type Action: Copy + fmt::Debug + Send + Sync;
    /// Why a step is a violation.
    type Fault: Copy + fmt::Debug + PartialEq + Send + Sync;
    /// A packed representative of a state's class.
    type Key: Copy + Eq + Hash + Send + Sync;

    /// The names of the paths a step may take, in the order of [`Step::paths`]'s bits (at most 64).
    fn paths(&self) -> &'static [&'static str];
    /// The state every history starts from.
    fn initial(&self) -> Self::State;
    /// Every action worth trying from `state`; each is checked for being enabled when applied.
    fn actions(&self, state: &Self::State, out: &mut Vec<Self::Action>);
    /// `action` applied to `state`, or `None` when it is not enabled there or changes nothing.
    fn apply(
        &self,
        state: &Self::State,
        action: Self::Action,
    ) -> Option<Step<Self::State, Self::Fault>>;
    /// The packed representative of `state`'s class.
    fn canonical(&self, state: &Self::State) -> Self::Key;
    /// The state a packed representative stands for.
    fn unpack(&self, key: &Self::Key) -> Self::State;
}

/// What one step gave: the next state, a fault when the step is a violation, and the paths it
/// took (a bit each, in [`Model::paths`]' order: the non-vacuity counters, `docs/sim.md` §4.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Step<S, F> {
    /// The state after.
    pub state: S,
    /// The violation, if the step is one.
    pub fault: Option<F>,
    /// The paths taken.
    pub paths: u64,
}

impl<S, F> Step<S, F> {
    /// A step to `state` that takes no counted path and raises no fault.
    pub fn plain(state: S) -> Self {
        Self {
            state,
            fault: None,
            paths: 0,
        }
    }
}

/// What a search reached.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    /// Classes visited.
    pub classes: u64,
    /// How often each path was taken, in [`Model::paths`]' order.
    pub paths: Vec<u64>,
    /// Levels expanded: the depth of the deepest class, plus one.
    pub levels: u32,
    /// The most bytes the search held at once, by its own count.
    pub peak: usize,
}

impl Report {
    fn new(paths: usize) -> Self {
        Self {
            classes: 0,
            paths: vec![0; paths],
            levels: 0,
            peak: 0,
        }
    }

    /// How often the path named `name` of `model` was taken.
    pub fn taken<M: Model>(&self, model: &M, name: &str) -> u64 {
        model
            .paths()
            .iter()
            .position(|path| *path == name)
            .and_then(|at| self.paths.get(at))
            .copied()
            .unwrap_or(0)
    }

    fn tally(&mut self, taken: u64) {
        tally(&mut self.paths, taken);
    }
}

fn tally(paths: &mut [u64], taken: u64) {
    for (path, count) in paths.iter_mut().enumerate() {
        if path < 64 {
            *count = count.saturating_add((taken >> path) & 1);
        }
    }
}

/// How a search ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome<S, A, F> {
    /// Every reachable class was visited, and no step that stops the search was met.
    Exhausted(Report),
    /// A fault was met. [`shortest`] gives the whole history from the initial state, the fewest
    /// steps any history to such a fault takes; [`explore`] gives the state it was met from (a
    /// representative) and the step.
    Fault {
        /// What was reached up to it.
        report: Report,
        /// The actions from the start, the last one the step that raised it (empty from
        /// [`explore`]).
        history: Vec<A>,
        /// The state the faulting step was taken from.
        from: S,
        /// The faulting step.
        action: A,
        /// The fault.
        fault: F,
    },
    /// The budget was reached first: nothing is claimed of the classes not visited.
    Unknown {
        /// What was reached.
        report: Report,
        /// What refused.
        spent: Spent,
    },
}

/// How a search of `M` ended.
pub type Found<M> = Outcome<<M as Model>::State, <M as Model>::Action, <M as Model>::Fault>;

/// A search that could not run to an outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExploreError {
    /// More classes than an id table addresses (2³² − 1).
    Ids,
    /// A worker ended by unwinding: a model that panicked.
    Worker,
    /// The chain of representatives to a fault could not be stepped again: the model's
    /// `canonical` or `unpack` disagrees with its `apply`.
    Replay,
}

impl fmt::Display for ExploreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ids => f.write_str("more classes than an id table addresses"),
            Self::Worker => f.write_str("a worker unwound"),
            Self::Replay => {
                f.write_str("the representatives to a fault could not be stepped again")
            }
        }
    }
}

impl std::error::Error for ExploreError {}

/// A 128-bit fingerprint of a key: SipHash-1-3 (std's `DefaultHasher` under its fixed keys) over
/// it with a lane's byte, twice (slates' `fingerprint`; the search checker's `Sip`).
pub fn fingerprint<K: Hash>(key: &K) -> u128 {
    let lane = |which: u8| {
        let mut hasher = DefaultHasher::new();
        which.hash(&mut hasher);
        key.hash(&mut hasher);
        hasher.finish()
    };
    (u128::from(lane(0)) << 64) | u128::from(lane(1))
}

/// The bytes a vector of `capacity` elements of `T` holds.
fn vec_bytes<T>(capacity: usize) -> usize {
    bytes_of(capacity, size_of::<T>()).unwrap_or(usize::MAX)
}

/// `vec` given room for one more, doubling when full; refused when its old and new buffers and
/// the `others` bytes held beside would pass `memory` together. Returns the bytes it holds after.
fn room_for_one<T>(vec: &mut Vec<T>, others: usize, memory: usize) -> Result<usize, ()> {
    if vec.len() < vec.capacity() {
        return Ok(vec_bytes::<T>(vec.capacity()));
    }
    let capacity = vec.capacity().saturating_mul(2).max(64);
    let peak = others
        .checked_add(vec_bytes::<T>(vec.capacity()))
        .and_then(|held| held.checked_add(vec_bytes::<T>(capacity)))
        .ok_or(())?;
    if peak > memory {
        return Err(());
    }
    vec.try_reserve_exact(capacity.saturating_sub(vec.len()))
        .map_err(|_| ())?;
    Ok(vec_bytes::<T>(vec.capacity()))
}

/// A class the serial search reached: its representative, the class it was first reached from,
/// and its depth.
#[derive(Clone, Copy)]
struct Class<K> {
    key: K,
    parent: u32,
    depth: u32,
}

/// Breadth-first search on one thread from `start` until every class reachable from it is visited
/// or a step raises a fault `stops` accepts, whose shortest history it returns; other faults are
/// counted as paths are and passed. Holds at most `budget`'s bytes.
pub fn shortest<M: Model>(
    model: &M,
    start: M::State,
    stops: &dyn Fn(&M::Fault) -> bool,
    budget: Budget,
) -> Result<Found<M>, ExploreError> {
    let memory = budget.memory;
    let mut report = Report::new(model.paths().len());
    let first = model.canonical(&start);
    let mut seen = match PrintSet::new(memory, 1024) {
        Ok(seen) => seen,
        Err(spent) => return Ok(Outcome::Unknown { report, spent }),
    };
    let refused = |report: Report, seen: &PrintSet| Outcome::Unknown {
        report,
        spent: Spent::Budget {
            configurations: seen.len(),
        },
    };
    // The ids are breadth-first order, so the queue is a cursor over the table.
    let mut classes: Vec<Class<M::Key>> = Vec::new();
    if seen.insert(fingerprint(&first)).is_err()
        || room_for_one(&mut classes, seen.bytes(), memory).is_err()
    {
        return Ok(refused(report, &seen));
    }
    classes.push(Class {
        key: first,
        parent: 0,
        depth: 0,
    });
    report.classes = 1;
    report.peak = seen
        .bytes()
        .saturating_add(vec_bytes::<Class<M::Key>>(classes.capacity()));
    let mut actions = Vec::new();
    let mut cursor = 0usize;
    while let Some(class) = classes.get(cursor).copied() {
        let id = u32::try_from(cursor).map_err(|_| ExploreError::Ids)?;
        report.levels = report.levels.max(class.depth.saturating_add(1));
        let state = model.unpack(&class.key);
        actions.clear();
        model.actions(&state, &mut actions);
        for action in actions.iter().copied() {
            let Some(next) = model.apply(&state, action) else {
                continue;
            };
            report.tally(next.paths);
            if let Some(fault) = next.fault
                && stops(&fault)
            {
                let history = replay(model, start, &classes, id, action, fault)?;
                report.classes = seen.len();
                return Ok(Outcome::Fault {
                    report,
                    history,
                    from: state,
                    action,
                    fault,
                });
            }
            let representative = model.canonical(&next.state);
            let print = fingerprint(&representative);
            if seen.contains(print) {
                continue;
            }
            // The classes' vector once it holds one more, beside the table at its peak if it
            // must double; and the vector's own growth, its old and new buffers together, beside
            // the table as it is.
            let classes_after = if classes.len() < classes.capacity() {
                vec_bytes::<Class<M::Key>>(classes.capacity())
            } else {
                vec_bytes::<Class<M::Key>>(classes.capacity().saturating_mul(2).max(64))
            };
            let table_peak = seen.peak_for(1).unwrap_or(usize::MAX);
            if table_peak.saturating_add(classes_after) > memory
                || room_for_one(&mut classes, seen.bytes(), memory).is_err()
            {
                return Ok(refused(report, &seen));
            }
            if let Err(spent) = seen.insert(print) {
                return Ok(Outcome::Unknown { report, spent });
            }
            u32::try_from(classes.len()).map_err(|_| ExploreError::Ids)?;
            classes.push(Class {
                key: representative,
                parent: id,
                depth: class.depth.saturating_add(1),
            });
            report.classes = seen.len();
            report.peak = report.peak.max(
                seen.bytes()
                    .saturating_add(vec_bytes::<Class<M::Key>>(classes.capacity())),
            );
        }
        cursor = cursor.saturating_add(1);
    }
    report.classes = seen.len();
    Ok(Outcome::Exhausted(report))
}

/// The concrete actions from `start` along the chain of representatives ending at `id`, then
/// `last`, re-found from a concrete state that a renaming of `last`'s state reaches: each step is
/// the first action whose successor's representative is the next on the chain, and the last the
/// first that raises a fault of `fault`'s kind.
fn replay<M: Model>(
    model: &M,
    start: M::State,
    classes: &[Class<M::Key>],
    id: u32,
    last: M::Action,
    fault: M::Fault,
) -> Result<Vec<M::Action>, ExploreError> {
    let mut chain = vec![id];
    let mut at = id;
    while at != 0 {
        at = usize::try_from(at)
            .ok()
            .and_then(|at| classes.get(at))
            .map(|class| class.parent)
            .ok_or(ExploreError::Replay)?;
        chain.push(at);
        if chain.len() > classes.len() {
            return Err(ExploreError::Replay);
        }
    }
    chain.reverse();
    let mut state = start;
    let mut history = Vec::new();
    let mut tried = Vec::new();
    for step in chain.iter().skip(1) {
        let key = usize::try_from(*step)
            .ok()
            .and_then(|step| classes.get(step))
            .map(|class| &class.key)
            .ok_or(ExploreError::Replay)?;
        tried.clear();
        model.actions(&state, &mut tried);
        let (action, next) = tried
            .iter()
            .find_map(|action| {
                model
                    .apply(&state, *action)
                    .filter(|next| model.canonical(&next.state) == *key)
                    .map(|next| (*action, next.state))
            })
            .ok_or(ExploreError::Replay)?;
        history.push(action);
        state = next;
    }
    tried.clear();
    model.actions(&state, &mut tried);
    let kind = std::mem::discriminant(&fault);
    let found = tried
        .iter()
        .find(|action| {
            model.apply(&state, **action).is_some_and(|next| {
                next.fault
                    .is_some_and(|met| std::mem::discriminant(&met) == kind)
            })
        })
        .copied();
    history.push(found.unwrap_or(last));
    if found.is_none() {
        return Err(ExploreError::Replay);
    }
    Ok(history)
}

/// Adds one expansion's path counts to the report's.
fn tally_all(counts: &mut [u64], more: &[u64]) {
    for (count, more) in counts.iter_mut().zip(more) {
        *count = count.saturating_add(*more);
    }
}

/// What one worker made of its slice of a frontier.
struct Expansion<M: Model> {
    /// Its successors not yet visited, bucketed by the shard that owns their fingerprints.
    buckets: Vec<Vec<(u128, M::Key)>>,
    paths: Vec<u64>,
    fault: Option<(M::State, M::Action, M::Fault)>,
    /// Whether it stopped at its allowance of bucketed successors.
    full: bool,
}

/// The shard of `shards` that owns `print`: by its high half, its low half placing it within the
/// shard's table.
fn shard_of(print: u128, shards: usize) -> usize {
    let high = u64::try_from(print >> 64).unwrap_or(0);
    let shards = u64::try_from(shards).unwrap_or(1).max(1);
    usize::try_from(high.checked_rem(shards).unwrap_or(0)).unwrap_or(0)
}

fn expand<M: Model>(
    model: &M,
    slice: &[M::Key],
    shards: &[PrintSet],
    allowance: usize,
) -> Expansion<M> {
    let mut expansion = Expansion {
        buckets: (0..shards.len()).map(|_| Vec::new()).collect(),
        paths: vec![0; model.paths().len()],
        fault: None,
        full: false,
    };
    // Places the buckets hold by capacity, against the allowance.
    let mut held = 0usize;
    let mut actions = Vec::new();
    // The classes one state's steps reached, so that two of its steps to one class (a model's
    // actions often reach one state by several arguments) take one bucket's place: as many as
    // its actions, and cleared for the next state.
    let mut reached: Vec<u128> = Vec::new();
    for key in slice {
        let state = model.unpack(key);
        actions.clear();
        reached.clear();
        model.actions(&state, &mut actions);
        if reached.try_reserve(actions.len()).is_err() {
            expansion.full = true;
            return expansion;
        }
        for action in actions.iter().copied() {
            let Some(next) = model.apply(&state, action) else {
                continue;
            };
            tally(&mut expansion.paths, next.paths);
            if let Some(fault) = next.fault {
                expansion.fault = Some((state, action, fault));
                return expansion;
            }
            let representative = model.canonical(&next.state);
            let print = fingerprint(&representative);
            let shard = shard_of(print, shards.len());
            if reached.contains(&print) || shards.get(shard).is_some_and(|set| set.contains(print))
            {
                continue;
            }
            reached.push(print);
            // A bucket grows by doubling, within the bytes the allowance leaves: what it holds by
            // capacity is what the search counts.
            let Some(bucket) = expansion.buckets.get_mut(shard) else {
                expansion.full = true;
                return expansion;
            };
            if bucket.len() == bucket.capacity() {
                let grown = bucket.capacity().saturating_mul(2).max(16);
                let more = grown.saturating_sub(bucket.capacity());
                if held.saturating_add(more) > allowance || bucket.try_reserve_exact(more).is_err()
                {
                    expansion.full = true;
                    return expansion;
                }
                held = held.saturating_add(more);
            }
            bucket.push((print, representative));
        }
    }
    expansion
}

/// Inserts into one shard what every worker bucketed for it, returning the keys it had not seen.
fn settle<K: Copy>(shard: &mut PrintSet, buckets: &[&Vec<(u128, K)>]) -> Result<Vec<K>, Spent> {
    let incoming = buckets.iter().map(|bucket| bucket.len()).sum::<usize>();
    let mut fresh = Vec::new();
    fresh
        .try_reserve_exact(incoming)
        .map_err(|_| Spent::Memory)?;
    for (print, key) in buckets.iter().flat_map(|bucket| bucket.iter()) {
        if shard.insert(*print)? {
            fresh.push(*key);
        }
    }
    Ok(fresh)
}

/// The classes the tables hold.
fn classes(shards: &[PrintSet]) -> u64 {
    shards.iter().map(PrintSet::len).sum::<u64>()
}

/// The bytes the tables hold.
fn tables(shards: &[PrintSet]) -> usize {
    shards
        .iter()
        .map(PrintSet::bytes)
        .fold(0usize, usize::saturating_add)
}

/// What became of one part of a level.
enum Part<M: Model> {
    /// Its fresh classes joined the next level.
    Joined,
    /// Its successors did not fit beside the tables: it is expanded again in halves.
    Halve,
    /// The search ends here.
    End(Found<M>),
}

/// The search's shared state across one level: its tables, the level being expanded (by its
/// capacity) and the next one.
struct Level<'a, M: Model> {
    model: &'a M,
    shards: &'a mut [PrintSet],
    frontier_capacity: usize,
    next: &'a mut Vec<M::Key>,
    report: &'a mut Report,
    memory: usize,
    workers: usize,
}

impl<M: Model> Level<'_, M> {
    fn unknown(&mut self, spent: Spent) -> Part<M> {
        self.report.classes = classes(self.shards);
        Part::End(Outcome::Unknown {
            report: std::mem::replace(self.report, Report::new(0)),
            spent,
        })
    }
    fn over_budget(&mut self) -> Part<M> {
        let configurations = classes(self.shards);
        self.unknown(Spent::Budget { configurations })
    }

    /// Expands `slice` on the workers and settles what it reached into the tables; `halvable`
    /// when the part may be halved still.
    fn part(&mut self, slice: &[M::Key], halvable: bool) -> Result<Part<M>, ExploreError> {
        let workers = self.workers;
        let model = self.model;
        let held = tables(self.shards)
            .saturating_add(vec_bytes::<M::Key>(self.frontier_capacity))
            .saturating_add(vec_bytes::<M::Key>(self.next.capacity()));
        self.report.peak = self.report.peak.max(held);
        let bucket_bytes = size_of::<(u128, M::Key)>();
        let allowance = self
            .memory
            .saturating_sub(held)
            .checked_div(bucket_bytes.max(1))
            .and_then(|successors| successors.checked_div(workers))
            .unwrap_or(0);
        let share = slice.len().div_ceil(workers).max(1);
        let shards: &[PrintSet] = self.shards;
        let joined: Vec<std::thread::Result<Expansion<M>>> = std::thread::scope(|scope| {
            let running: Vec<_> = slice
                .chunks(share)
                .map(|slice| scope.spawn(move || expand(model, slice, shards, allowance)))
                .collect();
            running.into_iter().map(|worker| worker.join()).collect()
        });
        let mut expansions = Vec::with_capacity(joined.len());
        for expansion in joined {
            expansions.push(expansion.map_err(|_| ExploreError::Worker)?);
        }
        let bucketed = expansions
            .iter()
            .flat_map(|expansion| expansion.buckets.iter().map(Vec::capacity))
            .fold(0usize, usize::saturating_add);
        let buckets_held = bytes_of(bucketed, bucket_bytes).unwrap_or(usize::MAX);
        self.report.peak = self.report.peak.max(held.saturating_add(buckets_held));
        if let Some((from, action, fault)) = expansions.iter().find_map(|expansion| expansion.fault)
        {
            for expansion in &expansions {
                tally_all(&mut self.report.paths, &expansion.paths);
            }
            self.report.classes = classes(self.shards);
            return Ok(Part::End(Outcome::Fault {
                report: std::mem::replace(self.report, Report::new(0)),
                history: Vec::new(),
                from,
                action,
                fault,
            }));
        }
        let settling = self.settling(&expansions, buckets_held);
        if expansions.iter().any(|expansion| expansion.full) || settling > self.memory {
            return Ok(if halvable {
                Part::Halve
            } else {
                self.over_budget()
            });
        }
        for expansion in &expansions {
            tally_all(&mut self.report.paths, &expansion.paths);
        }
        self.report.peak = self.report.peak.max(settling);
        let settled: Vec<std::thread::Result<Result<Vec<M::Key>, Spent>>> =
            std::thread::scope(|scope| {
                let running: Vec<_> = self
                    .shards
                    .iter_mut()
                    .enumerate()
                    .map(|(index, shard)| {
                        let buckets: Vec<&Vec<(u128, M::Key)>> = expansions
                            .iter()
                            .filter_map(|expansion| expansion.buckets.get(index))
                            .collect();
                        scope.spawn(move || settle(shard, &buckets))
                    })
                    .collect();
                running.into_iter().map(|shard| shard.join()).collect()
            });
        drop(expansions);
        let mut fresh = Vec::with_capacity(settled.len());
        for part in settled {
            match part.map_err(|_| ExploreError::Worker)? {
                Ok(keys) => fresh.push(keys),
                Err(spent) => return Ok(self.unknown(spent)),
            }
        }
        Ok(self.join(fresh))
    }

    /// The bytes the tables, the levels and the buckets hold while the buckets settle.
    fn settling(&self, expansions: &[Expansion<M>], buckets_held: usize) -> usize {
        let key_bytes = size_of::<M::Key>();
        let mut settling = buckets_held;
        for (index, shard) in self.shards.iter().enumerate() {
            let incoming = expansions
                .iter()
                .filter_map(|expansion| expansion.buckets.get(index))
                .map(Vec::len)
                .fold(0usize, usize::saturating_add);
            let peak = shard.peak_for(incoming).unwrap_or(usize::MAX);
            settling = settling
                .saturating_add(peak)
                .saturating_add(bytes_of(incoming, key_bytes).unwrap_or(usize::MAX));
        }
        settling
            .saturating_add(vec_bytes::<M::Key>(self.frontier_capacity))
            .saturating_add(vec_bytes::<M::Key>(self.next.capacity()))
    }

    /// The fresh classes join the next level, which grows to hold them exactly, beside them, the
    /// tables and this level.
    fn join(&mut self, fresh: Vec<Vec<M::Key>>) -> Part<M> {
        let width = fresh
            .iter()
            .map(Vec::len)
            .fold(0usize, usize::saturating_add);
        let grown = self
            .next
            .len()
            .saturating_add(width)
            .max(self.next.capacity());
        let fresh_bytes = fresh
            .iter()
            .map(|part| vec_bytes::<M::Key>(part.capacity()))
            .fold(0usize, usize::saturating_add);
        let extending = tables(self.shards)
            .saturating_add(vec_bytes::<M::Key>(self.frontier_capacity))
            .saturating_add(vec_bytes::<M::Key>(grown))
            .saturating_add(fresh_bytes);
        if extending > self.memory {
            return self.over_budget();
        }
        if self
            .next
            .try_reserve_exact(grown.saturating_sub(self.next.len()))
            .is_err()
        {
            return self.unknown(Spent::Memory);
        }
        self.report.peak = self.report.peak.max(extending);
        for part in fresh {
            self.next.extend(part);
        }
        self.report.classes = classes(self.shards);
        Part::Joined
    }
}

/// Breadth-first search, level by level on `workers` scoped threads, over fingerprints of the
/// representatives, from the model's initial state: every reachable class visited once. It stops
/// at the first level with a fault, giving the step that met it ([`shortest`] gives the whole
/// history where its budget affords it). Holds at most `budget`'s bytes.
///
/// A level is expanded a part at a time: the successors one part buckets must fit beside the
/// tables, this level and the next, and a part whose successors do not is halved and expanded
/// again (expanding changes nothing, so a part given up costs only its time). The classes are
/// those of a level expanded whole: each part settles into the same tables before the next, and
/// every fresh class joins the next level.
pub fn explore<M: Model>(
    model: &M,
    workers: NonZeroUsize,
    budget: Budget,
) -> Result<Found<M>, ExploreError> {
    let memory = budget.memory;
    let workers = workers.get();
    let mut report = Report::new(model.paths().len());
    let mut shards = Vec::new();
    for _ in 0..workers {
        match PrintSet::new(memory, 1024) {
            Ok(set) => shards.push(set),
            Err(spent) => return Ok(Outcome::Unknown { report, spent }),
        }
    }
    let initial = model.canonical(&model.initial());
    let print = fingerprint(&initial);
    if let Some(shard) = shards.get_mut(shard_of(print, workers)) {
        let _ = shard.insert(print);
    }
    let mut frontier = vec![initial];
    report.classes = 1;
    while !frontier.is_empty() {
        report.levels = report.levels.saturating_add(1);
        let mut next: Vec<M::Key> = Vec::new();
        let mut level = Level {
            model,
            shards: &mut shards,
            frontier_capacity: frontier.capacity(),
            next: &mut next,
            report: &mut report,
            memory,
            workers,
        };
        let mut done = 0usize;
        let mut part_len = frontier.len();
        while done < frontier.len() {
            let end = done.saturating_add(part_len).min(frontier.len());
            let slice = frontier.get(done..end).unwrap_or(&[]);
            match level.part(slice, part_len > workers)? {
                Part::Joined => done = end,
                Part::Halve => part_len = part_len.div_ceil(2),
                Part::End(outcome) => return Ok(outcome),
            }
        }
        frontier = next;
    }
    Ok(Outcome::Exhausted(report))
}

/// Replays `actions` from `start`: the state they reach and the fault the last raised, or the
/// place of the first action not enabled where it is taken.
pub fn run_script<M: Model>(
    model: &M,
    start: M::State,
    actions: &[M::Action],
) -> Result<(M::State, Option<M::Fault>), usize> {
    let mut state = start;
    let mut fault = None;
    for (at, action) in actions.iter().enumerate() {
        let next = model.apply(&state, *action).ok_or(at)?;
        fault = next.fault;
        state = next.state;
    }
    Ok((state, fault))
}
