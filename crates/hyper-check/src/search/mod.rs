//! The search checker (`docs/sim.md` §4.3): whether a history has an order at all, with no word
//! from the system.
//!
//! **Algorithm.** Horn and Kroening's Algorithm 1 (WGL) over Wing and Gong's linked list ([`list`]):
//! at each configuration the calls before the first return left in the list are the operations that
//! may take effect next; one is linearized when the specification gives its output, and lifted out
//! of the list; when none can be, the last one linearized is undone and the next tried. Lowe's memo
//! of configurations (§3.1, [`memo`]) keeps any configuration from being searched twice.
//!
//! **Just-in-time linearization** (Lowe §4). The operation whose return is the first left in the
//! list must take effect before anything after it, and Lowe's Lemma 6 shows that a history with an
//! order has one in which every operation takes effect as late as possible: just before its own
//! return, or just before the return of another that it precedes. So the search tries that
//! operation first, linearized just in time, and the others called before its return only after it
//! (Lemma 4's exchanges): first those after which the forced operation's output is the
//! specification's (the write a read that failed was waiting for), then the others that return,
//! then those that never do, which nothing forces and which can always wait. The order of trying is the only change from Algorithm 1, which tries the
//! calls in the list's order: the set tried at each configuration is the same, so the search decides
//! exactly what Algorithm 1 decides, and it finds the common order (each operation at its return)
//! without backtracking. A configuration of this search is Lowe's: the first return left in the
//! list, the operations linearized ahead of it, and the state; the set linearized is every operation
//! returned before that return and those, so a register's configurations number at most
//! `(N+1)·2^p·(p+1)` for `N` operations and `p` pending at once (Lowe §4's bound, which holds for
//! this memo by the same count).
//!
//! **Partitions.** A history of a map is linearizable exactly when each key's is (Herlihy and Wing
//! Theorem 1; Horn and Kroening Definition 6 and Theorem 1): [`search_partitions`] checks each part
//! alone, against a specification of one key.
//!
//! **Indeterminate operations.** An operation with no return returns at the end of time and accepts
//! any output (Herlihy and Wing §2.2: a pending invocation may be completed or dropped; one completed
//! last is one dropped, for nothing observes it).
//!
//! **Budget.** NP-complete in general (Gibbons and Korach, as Lowe §1 and Horn and Kroening §1
//! report it). The memo holds at most [`Budget::memory`] bytes; a search that would need more says
//! `Unknown`, apart from a refusal. Every step either holds a new configuration or tries one of the
//! at most `p` operations pending at a configuration held, or undoes one, so the steps are bounded
//! by the configurations held too.
//!
//! **Counterexample** (Lowe §3): the longest prefix linearized, the operation whose return no
//! order could pass after it, the state there, and the outputs it could have given: after the
//! prefix, and after the prefix and each other pending operation in turn.

mod list;
mod memo;

use std::fmt;

use hyper_sim::Seeded;

use crate::history::{Malformed, Operation, check_operations};
use crate::model::Model;
use list::List;
use memo::Memo;
pub use memo::{Budget, MEMORY_CEILING, Print, Sip, Spent};

/// Why a history could not be searched at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchError {
    /// An operation is not one: see [`Malformed`].
    Malformed(Malformed),
    /// More operations than the list's 32-bit places name.
    TooLong,
    /// The host refused the list's memory.
    Memory,
}

impl fmt::Display for SearchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(malformed) => write!(f, "{malformed}"),
            Self::TooLong => f.write_str("more operations than a search's list places"),
            Self::Memory => f.write_str("the host refused a search's list"),
        }
    }
}

impl std::error::Error for SearchError {}

impl From<Malformed> for SearchError {
    fn from(malformed: Malformed) -> Self {
        Self::Malformed(malformed)
    }
}

/// What a search found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict<S, O> {
    /// An order: the operations, by place in the history, in the order they take effect. Every
    /// completed operation is in it; an operation that never returned is in it or took no effect.
    Linearizable {
        /// The order.
        order: Vec<usize>,
    },
    /// No order, confirmed on whole keys.
    NotLinearizable(Counterexample<S, O>),
    /// The budget ran out first.
    Unknown(Spent),
}

/// Why no order exists (Lowe §3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Counterexample<S, O> {
    /// The longest prefix the search linearized: operations by place, in order.
    pub prefix: Vec<usize>,
    /// The operation whose return no order could pass after the prefix.
    pub stuck: usize,
    /// The state after the prefix.
    pub state: S,
    /// What `stuck` could have answered: after the prefix, then after the prefix and each other
    /// pending operation in turn; each output once.
    pub legal: Vec<O>,
    /// The other operations pending there, by place.
    pub pending: Vec<usize>,
}

/// What a search found, and what it cost.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Searched<S, O> {
    /// The verdict.
    pub verdict: Verdict<S, O>,
    /// Configurations the fingerprint search held.
    pub configurations: u64,
    /// Its steps: operations tried and undone.
    pub steps: u64,
    /// Configurations the whole-key search held, when a refusal was confirmed.
    pub confirmed: Option<u64>,
    /// Whether the whole-key search found an order the fingerprint search missed: a shared
    /// fingerprint.
    pub collision: bool,
}

/// Where the search is at a configuration: the operation linearized just in time is next, or the
/// calls from an entry on of one class ([`Class`]).
#[derive(Clone, Copy, Debug)]
enum Cursor {
    Forced,
    Scan { entry: u32, class: Class },
}

/// The order in which the calls before the first return are tried, after the operation whose
/// return it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Class {
    /// After it, the forced operation's output is the specification's: the operation a read that
    /// failed was waiting for.
    Enabling,
    /// Others that return.
    Returning,
    /// Others that never return, which nothing forces and which can always wait.
    Late,
}

impl Class {
    fn next(self) -> Option<Self> {
        match self {
            Self::Enabling => Some(Self::Returning),
            Self::Returning => Some(Self::Late),
            Self::Late => None,
        }
    }
}

/// An operation linearized, the state before it, and where the configuration before it was in
/// its trying.
struct Frame<S> {
    op: u32,
    prior: S,
    resume: Cursor,
}

/// The deepest dead end found so far: its prefix kept alongside the stack, the stack's first
/// `shared` operations being the prefix's.
struct Deepest<S> {
    prefix: Vec<u32>,
    shared: usize,
    at: Option<(u32, S, Vec<u32>)>,
}

/// How a run of the search ended.
enum Outcome {
    Linearizable,
    Refused,
    Unknown(Spent),
}

struct Search<'a, M: Model, Mm> {
    model: &'a M,
    ops: &'a [Operation<M::Input, M::Output>],
    keys: &'a [u128],
    list: List,
    memo: Mm,
    state: M::State,
    zobrist: u128,
    stack: Vec<Frame<M::State>>,
    cursor: Cursor,
    steps: u64,
    deepest: Deepest<M::State>,
}

impl<'a, M: Model, Mm: Memo<M::State>> Search<'a, M, Mm> {
    fn new(
        model: &'a M,
        ops: &'a [Operation<M::Input, M::Output>],
        keys: &'a [u128],
        memo: Mm,
    ) -> Result<Self, SearchError> {
        let calls: Vec<u64> = ops.iter().map(|op| op.call).collect();
        let rets: Vec<Option<u64>> = ops.iter().map(|op| op.ret).collect();
        let mut stack = Vec::new();
        stack
            .try_reserve_exact(ops.len())
            .map_err(|_| SearchError::Memory)?;
        Ok(Self {
            model,
            ops,
            keys,
            list: List::new(&calls, &rets)?,
            memo,
            state: model.init(),
            zobrist: 0,
            stack,
            cursor: Cursor::Forced,
            steps: 0,
            deepest: Deepest {
                prefix: Vec::new(),
                shared: 0,
                at: None,
            },
        })
    }

    fn run(&mut self) -> Outcome {
        loop {
            if self.list.is_empty() {
                return Outcome::Linearizable;
            }
            let first = self.list.first_return();
            let forced = self.list.op_of(first);
            match self.next_candidate(first, forced) {
                Some(op) => {
                    if let Err(spent) = self.attempt(op) {
                        return Outcome::Unknown(spent);
                    }
                }
                None => {
                    self.note_dead_end(first, forced);
                    if !self.back() {
                        return Outcome::Refused;
                    }
                }
            }
        }
    }

    /// The next operation to try at this configuration: the one whose return is first, then each
    /// call before that return in the list's order, by [`Class`].
    fn next_candidate(&mut self, first: u32, forced: u32) -> Option<u32> {
        let (mut entry, mut class) = match self.cursor {
            Cursor::Forced => {
                let entry = self.list.first();
                self.cursor = Cursor::Scan {
                    entry,
                    class: Class::Enabling,
                };
                return Some(forced);
            }
            Cursor::Scan { entry, class } => (entry, class),
        };
        loop {
            if let Some((op, next)) = self.scan(entry, first, forced, class) {
                self.cursor = Cursor::Scan { entry: next, class };
                return Some(op);
            }
            let Some(after) = class.next() else {
                break;
            };
            class = after;
            entry = self.list.first();
        }
        self.cursor = Cursor::Scan {
            entry: first,
            class: Class::Late,
        };
        None
    }

    /// The first call from `entry` on, before `first`, of an operation other than `forced` of
    /// `class`, and the entry after it.
    fn scan(&self, mut entry: u32, first: u32, forced: u32, class: Class) -> Option<(u32, u32)> {
        while entry != first && !self.list.is_return(entry) {
            let op = self.list.op_of(entry);
            entry = self.list.next_of(entry);
            if op != forced && self.class_of(op, forced) == class {
                return Some((op, entry));
            }
        }
        None
    }

    /// Which class `op` is of at this configuration, with `forced` the operation linearized just
    /// in time.
    fn class_of(&self, op: u32, forced: u32) -> Class {
        let (Some(candidate), Some(waiting)) = (self.operation(op), self.operation(forced)) else {
            return Class::Late;
        };
        let after = self.model.apply(&self.state, &candidate.input).1;
        let enables = waiting
            .output
            .as_ref()
            .is_none_or(|seen| self.model.apply(&after, &waiting.input).0 == *seen);
        match (enables, candidate.ret) {
            (true, _) => Class::Enabling,
            (false, Some(_)) => Class::Returning,
            (false, None) => Class::Late,
        }
    }

    fn operation(&self, op: u32) -> Option<&'a Operation<M::Input, M::Output>> {
        usize::try_from(op).ok().and_then(|op| self.ops.get(op))
    }

    fn key(&self, op: u32) -> u128 {
        usize::try_from(op)
            .ok()
            .and_then(|op| self.keys.get(op))
            .copied()
            .unwrap_or(0)
    }

    /// `op` linearized, when the specification gives its output and the configuration it leads to
    /// is new.
    fn attempt(&mut self, op: u32) -> Result<(), Spent> {
        self.steps = self.steps.saturating_add(1);
        let Some(operation) = self.operation(op) else {
            return Ok(());
        };
        let (output, next) = self.model.apply(&self.state, &operation.input);
        if operation
            .output
            .as_ref()
            .is_some_and(|seen| *seen != output)
        {
            return Ok(());
        }
        let zobrist = self.zobrist ^ self.key(op);
        let ret = self.list.ret_of(op);
        self.list.lift(op);
        self.memo.linearized(op, ret)?;
        let fresh =
            self.list.is_empty() || self.memo.fresh(zobrist, self.list.first_return(), &next)?;
        if !fresh {
            self.memo.undone(op, ret);
            self.list.unlift(op);
            return Ok(());
        }
        let prior = std::mem::replace(&mut self.state, next);
        self.stack.push(Frame {
            op,
            prior,
            resume: self.cursor,
        });
        if self.deepest.shared == self.stack.len().saturating_sub(1)
            && self.deepest.prefix.get(self.deepest.shared) == Some(&op)
        {
            self.deepest.shared = self.stack.len();
        }
        self.zobrist = zobrist;
        self.cursor = Cursor::Forced;
        Ok(())
    }

    /// The last operation linearized undone; false when there is none.
    fn back(&mut self) -> bool {
        let Some(frame) = self.stack.pop() else {
            return false;
        };
        self.steps = self.steps.saturating_add(1);
        self.list.unlift(frame.op);
        self.memo.undone(frame.op, self.list.ret_of(frame.op));
        self.zobrist ^= self.key(frame.op);
        self.state = frame.prior;
        self.cursor = frame.resume;
        self.deepest.shared = self.deepest.shared.min(self.stack.len());
        true
    }

    /// A configuration where nothing can be linearized: kept when it is deeper than any before.
    fn note_dead_end(&mut self, first: u32, forced: u32) {
        let depth = self.stack.len();
        let deeper = match &self.deepest.at {
            None => true,
            Some(_) => depth > self.deepest.prefix.len(),
        };
        if !deeper {
            return;
        }
        let shared = self.deepest.shared.min(self.deepest.prefix.len());
        self.deepest.prefix.truncate(shared);
        let more = self.stack.get(shared..).unwrap_or_default();
        self.deepest
            .prefix
            .extend(more.iter().map(|frame| frame.op));
        self.deepest.shared = depth;
        let mut pending = Vec::new();
        let mut entry = self.list.first();
        while entry != first && !self.list.is_return(entry) {
            let op = self.list.op_of(entry);
            if op != forced {
                pending.push(op);
            }
            entry = self.list.next_of(entry);
        }
        self.deepest.at = Some((forced, self.state.clone(), pending));
    }

    fn order(&self) -> Vec<usize> {
        self.stack
            .iter()
            .filter_map(|frame| usize::try_from(frame.op).ok())
            .collect()
    }

    fn counterexample(&self) -> Option<Counterexample<M::State, M::Output>> {
        let (stuck, state, pending) = self.deepest.at.as_ref()?;
        let stuck_op = self.operation(*stuck)?;
        let mut legal = vec![self.model.apply(state, &stuck_op.input).0];
        for other in pending {
            let Some(other_op) = self.operation(*other) else {
                continue;
            };
            let after = self.model.apply(state, &other_op.input).1;
            let output = self.model.apply(&after, &stuck_op.input).0;
            if !legal.contains(&output) {
                legal.push(output);
            }
        }
        let places = |ops: &[u32]| -> Vec<usize> {
            ops.iter()
                .filter_map(|op| usize::try_from(*op).ok())
                .collect()
        };
        Some(Counterexample {
            prefix: places(&self.deepest.prefix),
            stuck: usize::try_from(*stuck).ok()?,
            state: state.clone(),
            legal,
            pending: places(pending),
        })
    }
}

/// The value the operations' keys are drawn from: any fixed one, for the keys need only be the same
/// from run to run and independent of the histories searched.
const ZOBRIST_SEED: u64 = 0x5a0b_2157_0000_0001;

/// A 128-bit key for each of `ops` operations, from SplitMix64 ([`Seeded`]).
fn keys(ops: usize) -> Result<Vec<u128>, SearchError> {
    let mut keys = Vec::new();
    keys.try_reserve_exact(ops)
        .map_err(|_| SearchError::Memory)?;
    let mut draws = Seeded::new(ZOBRIST_SEED);
    for _ in 0..ops {
        let high = u128::from(draws.next_u64());
        keys.push((high << 64) | u128::from(draws.next_u64()));
    }
    Ok(keys)
}

/// Whether `ops`, one object's history, is linearizable with respect to `model`, within `budget`;
/// fingerprints from [`Sip`].
pub fn search<M: Model>(
    model: &M,
    ops: &[Operation<M::Input, M::Output>],
    budget: &Budget,
) -> Result<Searched<M::State, M::Output>, SearchError> {
    search_with(model, ops, budget, &Sip)
}

/// [`search`] with fingerprints from `print`: a refusal is confirmed on whole keys whatever the
/// fingerprints, so a weak `print` costs time, never a wrong verdict.
pub fn search_with<M: Model, P: Print>(
    model: &M,
    ops: &[Operation<M::Input, M::Output>],
    budget: &Budget,
    print: &P,
) -> Result<Searched<M::State, M::Output>, SearchError> {
    check_operations(ops)?;
    let keys = keys(ops.len())?;
    let memo = match memo::Prints::new(print, budget.memory, ops.len()) {
        Ok(memo) => memo,
        Err(spent) => return Ok(unknown(spent)),
    };
    let mut fingerprinted = Search::new(model, ops, &keys, memo)?;
    let outcome = fingerprinted.run();
    let configurations = fingerprinted.memo.count();
    let steps = fingerprinted.steps;
    let verdict = match outcome {
        Outcome::Linearizable => Verdict::Linearizable {
            order: fingerprinted.order(),
        },
        Outcome::Unknown(spent) => Verdict::Unknown(spent),
        Outcome::Refused => return confirm(model, ops, &keys, budget, configurations, steps),
    };
    Ok(Searched {
        verdict,
        configurations,
        steps,
        confirmed: None,
        collision: false,
    })
}

fn unknown<S, O>(spent: Spent) -> Searched<S, O> {
    Searched {
        verdict: Verdict::Unknown(spent),
        configurations: 0,
        steps: 0,
        confirmed: None,
        collision: false,
    }
}

/// The refusal of a fingerprint search confirmed on whole keys: the whole-key search's verdict.
fn confirm<M: Model>(
    model: &M,
    ops: &[Operation<M::Input, M::Output>],
    keys: &[u128],
    budget: &Budget,
    configurations: u64,
    steps: u64,
) -> Result<Searched<M::State, M::Output>, SearchError> {
    let memo = match memo::Whole::new(model, budget.memory, ops.len()) {
        Ok(memo) => memo,
        Err(spent) => return Ok(unknown(spent)),
    };
    let mut whole = Search::new(model, ops, keys, memo)?;
    let outcome = whole.run();
    let confirmed = Some(Memo::<M::State>::held(&whole.memo));
    let (verdict, collision) = match outcome {
        Outcome::Linearizable => (
            Verdict::Linearizable {
                order: whole.order(),
            },
            true,
        ),
        Outcome::Unknown(spent) => (Verdict::Unknown(spent), false),
        Outcome::Refused => match whole.counterexample() {
            Some(counterexample) => (Verdict::NotLinearizable(counterexample), false),
            None => (Verdict::Unknown(Spent::Memory), false),
        },
    };
    Ok(Searched {
        verdict,
        configurations,
        steps: steps.saturating_add(whole.steps),
        confirmed,
        collision,
    })
}

/// One part of a partitioned search: its key, its operations by place in the whole history, and
/// what its search found (orders and counterexamples name places in the whole history).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Part<K, S, O> {
    /// The part's key.
    pub key: K,
    /// Its operations, by place in the whole history.
    pub ops: Vec<usize>,
    /// What its search found.
    pub searched: Searched<S, O>,
}

/// Every part of a partitioned search of `M`'s operations, by key `K`.
pub type Parts<K, M> = Vec<Part<K, <M as Model>::State, <M as Model>::Output>>;

/// Each part of `ops` by `partition` searched alone against `model`, a specification of one part:
/// the history is linearizable exactly when every part is, for a specification that is
/// P-compositional for `partition` (Horn and Kroening Definition 6; a map's keys, Herlihy and
/// Wing's objects).
pub fn search_partitions<M: Model, K: Ord + Clone>(
    model: &M,
    ops: &[Operation<M::Input, M::Output>],
    partition: impl Fn(&Operation<M::Input, M::Output>) -> K,
    budget: &Budget,
) -> Result<Parts<K, M>, SearchError> {
    let mut parts: std::collections::BTreeMap<K, Vec<usize>> = std::collections::BTreeMap::new();
    for (at, op) in ops.iter().enumerate() {
        parts.entry(partition(op)).or_default().push(at);
    }
    let mut searched = Vec::new();
    for (key, places) in parts {
        let part: Vec<Operation<M::Input, M::Output>> = places
            .iter()
            .filter_map(|at| ops.get(*at).cloned())
            .collect();
        let mut found = search(model, &part, budget)?;
        to_places(&mut found.verdict, &places);
        searched.push(Part {
            key,
            ops: places,
            searched: found,
        });
    }
    Ok(searched)
}

/// A part's verdict renamed from its own places to the whole history's.
fn to_places<S, O>(verdict: &mut Verdict<S, O>, places: &[usize]) {
    let rename = |at: &mut usize| {
        if let Some(place) = places.get(*at) {
            *at = *place;
        }
    };
    match verdict {
        Verdict::Linearizable { order } => order.iter_mut().for_each(rename),
        Verdict::NotLinearizable(counterexample) => {
            counterexample.prefix.iter_mut().for_each(rename);
            counterexample.pending.iter_mut().for_each(rename);
            rename(&mut counterexample.stuck);
        }
        Verdict::Unknown(_) => {}
    }
}
