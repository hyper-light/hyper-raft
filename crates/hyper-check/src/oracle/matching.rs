//! The log's three properties of Ongaro's thesis, Figure 3.2:
//!
//! - **Log Matching**: "if two logs contain an entry with the same index and term, then the logs
//!   are identical in all entries up through the given index." It rests on two facts (thesis
//!   §3.5): a leader creates at most one entry at an index in its term and an entry never changes
//!   its index, and the consistency check makes an entry follow the entry its leader's log held
//!   before it. So it is held entry by entry, over the whole history: every entry of a term at an
//!   index any log ever held states the same and follows the same term. By induction on the index
//!   that gives the thesis's statement for any two logs at any time, and a log's terms never
//!   decrease along it (slates' explorer). In a group with the fast track a successor takes an entry
//!   its predecessor committed again under its own term (below), so two logs can hold one value at
//!   an index under two terms, and an entry can follow either: there the oracle holds each index and
//!   term to one value, and terms to never decrease but after a member's committed prefix, which it
//!   keeps as it holds it ([`LogMatching::holds_at`]), and leaves the prefix's agreement, by what
//!   its entries state, to State Machine Safety.
//! - **Leader Completeness**: "if a log entry is committed in a given term, then that entry will be
//!   present in the logs of the leaders for all higher-numbered terms" (the TLA+ model's
//!   `LeaderHolds`): held once a leadership, when it is first seen, against every entry committed
//!   by then (slates' explorer), an entry under the leader's snapshot counting as held.
//! - **State Machine Safety**: "if a server has applied a log entry at a given index to its state
//!   machine, no other server will ever apply a different log entry for the same index", the entry
//!   compared by what it states and, but in a group with the fast track, by its term: there the
//!   leader that took an entry and the leader that took it again at its election each stamp it with
//!   their own (hyper-raft's `chosen`).

use std::collections::{BTreeMap, BTreeSet};

use super::{Violation, room};

/// Whether an entry is compared by its term as well as by what it states.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Terms {
    /// A committed entry is one term's: a classic group.
    Compared,
    /// What it states alone: a group with the fast track, where a successor takes an entry its
    /// predecessor committed again under its own term.
    Ignored,
}

/// A member's log, as a leader's is held to the committed entries.
pub trait LogView<V> {
    /// The index through which the log holds a snapshot alone.
    fn start(&self) -> u64;
    /// The term and the value of the entry the log holds at `index`, above its start.
    fn entry(&self, index: u64) -> Option<(u64, &V)>;
}

/// Every entry any log held, by index and term: what it states, and the term of the entry before it.
#[derive(Clone, Debug)]
pub struct LogMatching<V> {
    entries: BTreeMap<(u64, u64), (V, u64)>,
    terms: Terms,
    bound: usize,
}

impl<V: Clone + Eq> LogMatching<V> {
    /// The oracle, holding at most `bound` entries; with [`Terms::Ignored`], a group with the fast
    /// track, an entry is not held to the term before it.
    pub fn new(terms: Terms, bound: usize) -> Self {
        Self {
            entries: BTreeMap::new(),
            terms,
            bound,
        }
    }

    /// `member`'s log holds, at `index`, an entry of `term` stating `value`, after an entry (or a
    /// snapshot) of term `before`; zero before the first index.
    pub fn holds(
        &mut self,
        member: u64,
        index: u64,
        term: u64,
        value: &V,
        before: u64,
    ) -> Result<(), Violation> {
        self.holds_at(member, index, term, value, before, false)
    }

    /// As [`LogMatching::holds`], where `before_committed` says the entry before is committed at
    /// `member`. With [`Terms::Ignored`] a term may fall there and nowhere else: a member keeps its
    /// committed prefix as it holds it and takes its leader's entries after it, whatever terms the
    /// two stamped the committed entries with (hyper-raft's `Log::append_after`;
    /// `docs/models/FastTrack.tla`, `Replicate`, which takes from `Max(p, commit[m]) + 1`), so an
    /// entry its leader holds under an older term may follow a committed one a later leader
    /// stamped (the swarm's fast seed 34,957, `docs/sim.md` §15.9).
    pub fn holds_at(
        &mut self,
        member: u64,
        index: u64,
        term: u64,
        value: &V,
        before: u64,
        before_committed: bool,
    ) -> Result<(), Violation> {
        let restamped = self.terms == Terms::Ignored && before_committed;
        if before > term && !restamped {
            return Err(Violation::TermsDecrease {
                member,
                index,
                before,
                term,
            });
        }
        match self.entries.get(&(index, term)) {
            Some((held, held_before))
                if held == value && (self.terms == Terms::Ignored || *held_before == before) =>
            {
                Ok(())
            }
            Some(_) => Err(Violation::LogsDisagree {
                member,
                index,
                term,
            }),
            None => {
                room(self.entries.len(), self.bound, "Log Matching")?;
                self.entries.insert((index, term), (value.clone(), before));
                Ok(())
            }
        }
    }

    /// The entries held so far.
    pub fn entries(&self) -> usize {
        self.entries.len()
    }
}

/// Every index's committed entry, as the first member to commit it committed it.
#[derive(Clone, Debug)]
pub struct StateMachineSafety<V> {
    committed: BTreeMap<u64, (u64, V)>,
    terms: Terms,
    bound: usize,
}

impl<V: Clone + Eq> StateMachineSafety<V> {
    /// The oracle, holding at most `bound` indexes, comparing terms as `terms` says.
    pub fn new(terms: Terms, bound: usize) -> Self {
        Self {
            committed: BTreeMap::new(),
            terms,
            bound,
        }
    }

    /// Whether the entry of `term` stating `value` is the one of `held_term` stating `held`.
    fn same(&self, held_term: u64, held: &V, term: u64, value: &V) -> bool {
        held == value && (self.terms == Terms::Ignored || held_term == term)
    }

    /// `member` committed (or applied) the entry of `term` stating `value` at `index`.
    pub fn committed(
        &mut self,
        member: u64,
        index: u64,
        term: u64,
        value: &V,
    ) -> Result<(), Violation> {
        match self.committed.get(&index) {
            Some((held_term, held)) if self.same(*held_term, held, term, value) => Ok(()),
            Some(_) => Err(Violation::TwoCommitted { member, index }),
            None => {
                room(self.committed.len(), self.bound, "State Machine Safety")?;
                self.committed.insert(index, (term, value.clone()));
                Ok(())
            }
        }
    }

    /// The highest index committed so far.
    pub fn highest(&self) -> u64 {
        self.committed.keys().next_back().copied().unwrap_or(0)
    }

    /// The indexes committed so far.
    pub fn len(&self) -> usize {
        self.committed.len()
    }

    /// Whether nothing was committed.
    pub fn is_empty(&self) -> bool {
        self.committed.is_empty()
    }

    /// The entry committed at `index`: its term and value.
    pub fn at(&self, index: u64) -> Option<(u64, &V)> {
        self.committed
            .get(&index)
            .map(|(term, value)| (*term, value))
    }

    /// Every committed entry, by index.
    pub fn iter(&self) -> impl Iterator<Item = (u64, u64, &V)> {
        self.committed
            .iter()
            .map(|(index, (term, value))| (*index, *term, value))
    }
}

/// The leaderships already held to the committed entries, and the term each index was committed
/// in: a leader must hold what was committed in an earlier term (thesis §3.6: "if a log entry is
/// committed in a given term, then that entry will be present in the logs of the leaders for all
/// higher-numbered terms"), not what a later term committed while it had yet to hear of it.
#[derive(Clone, Debug)]
pub struct LeaderCompleteness {
    checked: BTreeSet<(u64, u64)>,
    /// Each run of indexes committed in one term: through which index, and the term.
    terms: Vec<(u64, u64)>,
    bound: usize,
}

impl LeaderCompleteness {
    /// The oracle, holding at most `bound` leaderships and runs of commits.
    pub fn new(bound: usize) -> Self {
        Self {
            checked: BTreeSet::new(),
            terms: Vec::new(),
            bound,
        }
    }

    /// The group's commit reached `through`, first at a member of `term`: every index past the
    /// commit noted before was committed in `term`.
    pub fn committed_in(&mut self, through: u64, term: u64) -> Result<(), Violation> {
        if self
            .terms
            .last()
            .is_some_and(|(noted, _)| *noted >= through)
        {
            return Ok(());
        }
        room(self.terms.len(), self.bound, "Leader Completeness")?;
        self.terms.push((through, term));
        Ok(())
    }

    /// The term `index` was committed in, where it was noted.
    fn term_of(&self, index: u64) -> Option<u64> {
        let at = self.terms.partition_point(|(through, _)| *through < index);
        self.terms.get(at).map(|(_, term)| *term)
    }

    /// `member` was seen leading `term` with `log`: the first time, its log must hold every entry
    /// `committed` holds that was committed in an earlier term (or in a term not noted), compared
    /// as `committed` compares them.
    pub fn leader<V: Clone + Eq, L: LogView<V>>(
        &mut self,
        member: u64,
        term: u64,
        log: &L,
        committed: &StateMachineSafety<V>,
    ) -> Result<(), Violation> {
        if self.checked.contains(&(member, term)) {
            return Ok(());
        }
        room(self.checked.len(), self.bound, "Leader Completeness")?;
        self.checked.insert((member, term));
        let start = log.start();
        let earlier = |index: u64| self.term_of(index).is_none_or(|committed| committed < term);
        for (index, held_term, value) in committed
            .iter()
            .filter(|(index, ..)| *index > start && earlier(*index))
        {
            let holds = log.entry(index).is_some_and(|(entry_term, entry)| {
                committed.same(held_term, value, entry_term, entry)
            });
            if !holds {
                return Err(Violation::LeaderLacks {
                    member,
                    term,
                    index,
                });
            }
        }
        Ok(())
    }

    /// The leaderships held so far.
    pub fn leaderships(&self) -> usize {
        self.checked.len()
    }
}
