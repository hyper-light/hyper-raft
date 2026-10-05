//! The prefix model: the design slates' dialect builds, Raft's in-order log with only what the
//! fast track and parallel replication need above it (slates' `tests/prefix_model.rs`, its record's
//! §3.5, §3.7 and §4; slices 9 and 10), here built again on hyper-check's search.
//!
//! Two steps of that design lie outside the slot model's proof, and this model checks them:
//! - **A candidate keeps its own log** and recovers only the indexes above its last entry: Raft's
//!   election rule and log matching make the entries below it safe.
//! - **A slot goes only under a classic commit.** A follower accepts out-of-order entries and fast
//!   votes only from the leader whose no-op its log holds (it is synced to it), and drops a slot only
//!   once it knows an index at or above it committed classically, held by a majority's logs.
//!
//! **What it holds.** Per member: a Raft log (leader-approved entries in order, each with its
//! leader's term), the classic commit it knows, a window of slots (a leader's entry that arrived out
//! of order, or a fast vote, each with its term), the term it is synced to, and while it leads, how
//! far each member's log matches its own, who holds each index in a window, its no-op's index (the
//! sync point) and where the fast track opens. A new leader recovers each index above its log from
//! the highest ballot among its voters' reports (a decision re-proposed; a fast ballot's value held
//! by at least `|Q| + |F| − n` of the reports; otherwise free), fills a free index below the last
//! recovered one with a no-op, writes its own no-op, keeps its window, and either opens the fast
//! track after its no-op or proposes classically (both searched).
//!
//! **Checked after every step:** agreement, P2c, log matching (Raft's strict form), one leader a
//! term, and leader completeness (a new leader holds every committed value at its index).
//!
//! **The rejected variants** ([`Variant`]) are each one rule of the design changed; the search
//! must refuse each.
//!
//! **The class.** Members and proposed values are interchangeable (the no-op is not). A log's
//! entries past its length are cleared whenever it shrinks, so a state's fields are all it holds
//! and its key is one per orbit of the renamings.

use std::fmt;

use hyper_check::explore::{Model, Packer, Step, least_over_ties};

pub const MAX_NODES: usize = 5;
/// A length packs in two bits.
pub const MAX_INDICES: usize = 3;
/// A term packs in three bits.
pub const MAX_TERMS: u8 = 4;
/// Proposed values beside the no-op; a value packs in two bits.
pub const MAX_VALUES: u8 = 3;
/// The value a no-op carries: never proposed, never renamed.
pub const NOOP: u8 = 0;
const WORDS: usize = 8;

/// The design, or one rule of it changed as slates' search measured and rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variant {
    /// A voter reports its window's slots.
    Design,
    /// A voter reports its log's entries above the candidate's too: safe, but a classic leader's
    /// recovery then brings back a deposed leader's uncommitted entries, which Raft discards.
    ReportLogsToo,
    /// A follower drops a slot once its log covers the index: Raft's truncation can then erase the
    /// only record of a fast vote.
    DropCovered,
    /// A leader counts a window's copy of its entry toward a commit: truncation can then erase a
    /// replica of a committed entry.
    CommitFromWindows,
    /// A synced member drops its slots of older terms at the sync, and a new leader clears its
    /// window: a later truncation can then erase the last record of a chosen value.
    DropAtSync,
    /// A member prunes its slots at a commit that counts fast commits, whose values are in no
    /// majority's logs.
    PruneAtFastCommit,
}

/// What a log keeps past its length once an append cuts it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tail {
    /// Nothing: the entries past its length are cleared, so a state is its fields and its key is
    /// one per orbit of the renamings.
    Cleared,
    /// The stale entries, as slates' model keeps them: its signature reads all of a log's places,
    /// so a state with a stale tail may sort its members apart from the same state without it,
    /// and one orbit then takes more than one key. With it the counts are slates' recorded ones.
    Stale,
}

#[derive(Clone, Copy, Debug)]
pub struct Scope {
    pub nodes: usize,
    pub indices: usize,
    /// Proposed values, besides the no-op: `1..=values`.
    pub values: u8,
    pub terms: u8,
    pub variant: Variant,
    pub tail: Tail,
}

impl Scope {
    fn majority(self) -> usize {
        self.nodes / 2 + 1
    }

    /// The smallest `f` with `2f + q > 2n` (Fast Paxos; ⌈3n/4⌉).
    fn fast(self) -> usize {
        (1..=self.nodes)
            .find(|f| 2 * f + self.majority() > 2 * self.nodes)
            .unwrap()
    }

    fn checked(self) -> Self {
        assert!(self.nodes <= MAX_NODES && self.indices <= MAX_INDICES);
        assert!(self.values <= MAX_VALUES && self.terms <= MAX_TERMS);
        assert_eq!(self.fast(), (3 * self.nodes).div_ceil(4));
        self
    }

    fn proposals(self) -> std::ops::RangeInclusive<u8> {
        1..=self.values
    }

    fn members(self, mask: u8) -> impl Iterator<Item = usize> {
        (0..self.nodes).filter(move |m| mask & (1 << m) != 0)
    }

    fn quorums(self) -> impl Iterator<Item = u8> {
        let all = u8::try_from((1usize << self.nodes) - 1).unwrap();
        (1..=all).filter(move |mask| mask.count_ones() as usize >= self.majority())
    }
}

/// A log entry: its leader's term and its value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Entry {
    term: u8,
    value: u8,
}

const NO_ENTRY: Entry = Entry { term: 0, value: 0 };

/// A window's slot: a leader's entry out of order, or a fast vote.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Slot {
    term: u8,
    fast: bool,
    value: u8,
}

impl Slot {
    /// Its term, and within a term a decision outranks a fast vote.
    fn ballot(self) -> (u8, bool) {
        (self.term, !self.fast)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Leading {
    /// Per member, how far its log is known to match this leader's.
    matched: [u8; MAX_NODES],
    /// Per index, who holds this leader's entry in a window (counted only under
    /// [`Variant::CommitFromWindows`]).
    window_acks: [u8; MAX_INDICES],
    /// Its no-op's index from one, or zero when its log had no room.
    sync_index: u8,
    /// The first index open to the fast track, or zero when it proposes classically.
    open_from: u8,
    committed: [bool; MAX_INDICES],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Node {
    term: u8,
    vote: Option<u8>,
    length: u8,
    log: [Entry; MAX_INDICES],
    window: [Option<Slot>; MAX_INDICES],
    /// The term of the leader whose no-op the log holds (zero before any).
    synced: u8,
    /// The prefix known committed classically (under [`Variant::PruneAtFastCommit`], at all).
    commit: u8,
    leading: Option<Leading>,
}

impl Node {
    fn entries(&self) -> &[Entry] {
        &self.log[..usize::from(self.length)]
    }

    /// Its last entry's term and its length, as Raft's election compares them.
    fn last(&self) -> (u8, u8) {
        (self.entries().last().map_or(0, |e| e.term), self.length)
    }

    /// The log cut or grown to `length`, its entries past it cleared.
    fn set_length(&mut self, length: usize) {
        self.length = length as u8;
        for entry in &mut self.log[length..] {
            *entry = NO_ENTRY;
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chosen {
    value: u8,
    term: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct State {
    nodes: [Node; MAX_NODES],
    chosen: [Option<Chosen>; MAX_INDICES],
}

const BLANK: Node = Node {
    term: 0,
    vote: None,
    length: 0,
    log: [NO_ENTRY; MAX_INDICES],
    window: [None; MAX_INDICES],
    synced: 0,
    commit: 0,
    leading: None,
};

pub const START: State = State {
    nodes: [BLANK; MAX_NODES],
    chosen: [None; MAX_INDICES],
};

/// One atomic step; indexes from zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Timeout {
        node: usize,
    },
    Learn {
        node: usize,
        term: u8,
    },
    /// Wins by `quorum` and recovers; `fast` says whether it opens the fast track.
    Elect {
        node: usize,
        quorum: u8,
        fast: bool,
    },
    /// The leader's log through `through` (a length) reaches `node` in order.
    Append {
        leader: usize,
        node: usize,
        through: u8,
    },
    /// The leader's entry at `index` reaches a synced member out of order.
    Scatter {
        leader: usize,
        node: usize,
        index: usize,
    },
    /// A synced member casts a fast vote at an open index.
    Vote {
        node: usize,
        index: usize,
        value: u8,
    },
    /// The leader decides its next index from the fast votes of `voters`.
    Decide {
        leader: usize,
        value: u8,
        voters: u8,
    },
    /// The leader proposes at its next index, classically.
    Propose {
        leader: usize,
        value: u8,
    },
    Commit {
        leader: usize,
        index: usize,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    Disagreement {
        index: usize,
        first: u8,
        second: u8,
    },
    OverwroteChosen {
        index: usize,
        chosen: u8,
        sent: u8,
        term: u8,
    },
    LogsDiverge {
        index: usize,
    },
    TwoLeaders {
        term: u8,
    },
    LeaderIncomplete {
        index: usize,
    },
}

const FAST_COMMIT: u64 = 1;
const CLASSIC_COMMIT: u64 = 1 << 1;
const OUT_OF_ORDER_COMMIT: u64 = 1 << 2;
const RECOVERED_FROM_A_LOG: u64 = 1 << 3;
const RECOVERED_FROM_A_WINDOW_DECISION: u64 = 1 << 4;
const RECOVERED_A_FAST_CHOICE: u64 = 1 << 5;
const FILLED_A_HOLE: u64 = 1 << 6;
const SYNC_DROPPED_A_SLOT: u64 = 1 << 7;
const TRUNCATED_A_LOG: u64 = 1 << 8;
const SCATTERED: u64 = 1 << 9;
const KEPT_A_COMMITTED_TERM: u64 = 1 << 10;
const PRUNED_AT_COMMIT: u64 = 1 << 11;
pub const PATHS: [&str; 12] = [
    "fast commits",
    "classic commits",
    "commits above an uncommitted index",
    "recoveries from a log above the candidate's",
    "recoveries from a window decision",
    "recoveries of a possible fast choice",
    "holes filled with no-ops",
    "slots dropped at a sync",
    "logs truncated",
    "entries scattered",
    "committed entries kept under an older term",
    "slots pruned at a commit",
];

/// The paths every scope of the design takes (a recovery from a log is a rejected variant's, a
/// slot dropped at a sync another's, and a hole needs three indexes).
pub const DESIGN_PATHS: [&str; 8] = [
    "fast commits",
    "classic commits",
    "commits above an uncommitted index",
    "recoveries from a window decision",
    "recoveries of a possible fast choice",
    "logs truncated",
    "entries scattered",
    "slots pruned at a commit",
];

type Next = Step<State, Fault>;

pub struct PrefixModel {
    pub scope: Scope,
}

impl PrefixModel {
    pub fn at(scope: Scope) -> Self {
        Self {
            scope: scope.checked(),
        }
    }

    fn variant(&self) -> Variant {
        self.scope.variant
    }

    fn leader_of(&self, state: &State, term: u8) -> Option<usize> {
        (0..self.scope.nodes)
            .find(|m| state.nodes[*m].term == term && state.nodes[*m].leading.is_some())
    }

    fn timeout(&self, state: &State, node: usize) -> Option<Next> {
        let mut next = *state;
        let me = &mut next.nodes[node];
        if me.term >= self.scope.terms {
            return None;
        }
        me.term += 1;
        me.vote = Some(node as u8);
        me.leading = None;
        Some(Step::plain(next))
    }

    fn learn(&self, state: &State, node: usize, term: u8) -> Option<Next> {
        if term <= state.nodes[node].term || self.leader_of(state, term).is_none() {
            return None;
        }
        let mut next = *state;
        let me = &mut next.nodes[node];
        me.term = term;
        me.vote = None;
        me.leading = None;
        Some(Step::plain(next))
    }

    /// Raft's vote: a later term, or this term with no other vote, and a candidate whose last
    /// entry is at least as up to date.
    fn grants(&self, state: &State, voter: usize, candidate: usize) -> bool {
        let (v, c) = (&state.nodes[voter], &state.nodes[candidate]);
        let by_term = v.term < c.term
            || (v.term == c.term && v.vote.is_none_or(|chose| usize::from(chose) == candidate));
        by_term && c.last() >= v.last()
    }

    /// A voter's report at `index`: its window's slot (under [`Variant::ReportLogsToo`], the higher
    /// ballot of that and its log's entry there), and whether the report came from its log.
    fn report(&self, node: &Node, index: usize) -> Option<(Slot, bool)> {
        let slot = node.window[index];
        if self.variant() != Variant::ReportLogsToo {
            return slot.map(|slot| (slot, false));
        }
        let logged = (index < usize::from(node.length)).then(|| Slot {
            term: node.log[index].term,
            fast: false,
            value: node.log[index].value,
        });
        let chosen = match (logged, slot) {
            (Some(entry), Some(slot)) if slot.ballot() > entry.ballot() => Some(slot),
            (Some(entry), _) => Some(entry),
            (None, slot) => slot,
        }?;
        Some((chosen, node.window[index] != Some(chosen)))
    }

    /// What the recovery decides at `index` from `quorum`'s reports: a value to re-propose and
    /// the path that constrained it, or `None` for a free index.
    fn recovered(&self, state: &State, quorum: u8, index: usize) -> Option<(u8, u64)> {
        let reports: Vec<(Slot, bool)> = self
            .scope
            .members(quorum)
            .filter_map(|m| self.report(&state.nodes[m], index))
            .collect();
        let highest = reports.iter().map(|(slot, _)| slot.ballot()).max()?;
        let top: Vec<&(Slot, bool)> = reports
            .iter()
            .filter(|(slot, _)| slot.ballot() == highest)
            .collect();
        if highest.1 {
            let (slot, from_log) = top[0];
            let path = if *from_log {
                RECOVERED_FROM_A_LOG
            } else {
                RECOVERED_FROM_A_WINDOW_DECISION
            };
            return Some((slot.value, path));
        }
        let threshold = quorum.count_ones() as usize + self.scope.fast() - self.scope.nodes;
        self.scope
            .proposals()
            .find(|v| top.iter().filter(|(slot, _)| slot.value == *v).count() >= threshold)
            .map(|v| (v, RECOVERED_A_FAST_CHOICE))
    }

    fn elect(&self, state: &State, node: usize, quorum: u8, fast: bool) -> Option<Next> {
        let candidate = state.nodes[node];
        if quorum & (1 << node) == 0
            || candidate.leading.is_some()
            || candidate.vote != Some(node as u8)
        {
            return None;
        }
        if !self
            .scope
            .members(quorum)
            .filter(|m| *m != node)
            .all(|m| self.grants(state, m, node))
        {
            return None;
        }
        let term = candidate.term;
        let mut next = Step::plain(*state);
        for m in self.scope.members(quorum) {
            let voter = &mut next.state.nodes[m];
            voter.term = term;
            voter.vote = Some(node as u8);
            voter.leading = None;
        }
        if self.leader_of(state, term).is_some() {
            next.fault = Some(Fault::TwoLeaders { term });
            return Some(next);
        }
        self.recover(state, node, quorum, fast, &mut next);
        Some(next)
    }

    /// The recovery above the candidate's log, its holes filled with no-ops, then its own no-op.
    fn recover(&self, state: &State, node: usize, quorum: u8, fast: bool, next: &mut Next) {
        let indices = self.scope.indices;
        let term = state.nodes[node].term;
        let kept = usize::from(state.nodes[node].length);
        let found: Vec<Option<(u8, u64)>> = (kept..indices)
            .map(|index| self.recovered(state, quorum, index))
            .collect();
        let end = found
            .iter()
            .rposition(Option::is_some)
            .map_or(kept, |at| kept + at + 1);
        let leader = &mut next.state.nodes[node];
        for (index, recovered) in (kept..end).zip(&found) {
            let value = match recovered {
                Some((value, path)) => {
                    next.paths |= path;
                    *value
                }
                None => {
                    next.paths |= FILLED_A_HOLE;
                    NOOP
                }
            };
            leader.log[index] = Entry { term, value };
        }
        let mut length = end;
        let sync_index = if length < indices {
            leader.log[length] = Entry { term, value: NOOP };
            length += 1;
            length as u8
        } else {
            0
        };
        leader.set_length(length);
        if self.variant() == Variant::DropAtSync {
            leader.window = [None; MAX_INDICES];
        }
        leader.synced = term;
        leader.leading = Some(Leading {
            matched: [0; MAX_NODES],
            window_acks: [0; MAX_INDICES],
            sync_index,
            open_from: if fast && sync_index > 0 {
                sync_index + 1
            } else {
                0
            },
            committed: [false; MAX_INDICES],
        });
        next.paths |= self.settle(&mut next.state.nodes[node]);
        let leader = next.state.nodes[node];
        for index in 0..indices {
            if let Some(chosen) = next.state.chosen[index] {
                let holds =
                    index < usize::from(leader.length) && leader.log[index].value == chosen.value;
                if !holds && next.fault.is_none() {
                    next.fault = Some(Fault::LeaderIncomplete { index });
                }
            }
        }
        for (index, entry) in leader.entries().iter().enumerate().skip(kept) {
            guard(next, index, entry.value, term);
        }
    }

    /// The leader's log through `through` reaches `node` in order: Raft's append from where the
    /// two agree, the follower's first conflicting entry and all after it cut above its committed
    /// prefix (which it keeps as it holds it), the leader's commit learnt as far as the append
    /// reaches, and the follower synced once its log holds the leader's no-op.
    fn append(&self, state: &State, leader: usize, node: usize, through: u8) -> Option<Next> {
        let source = state.nodes[leader];
        let leading = source.leading?;
        let target = state.nodes[node];
        if node == leader || target.term > source.term || through > source.length {
            return None;
        }
        let kept = usize::from(target.commit).min(usize::from(target.length));
        let agreed = kept + agreeing(&source, &target, kept);
        let mut next = Step::plain(*state);
        let follower = &mut next.state.nodes[node];
        if follower.term < source.term {
            follower.term = source.term;
            follower.vote = None;
            follower.leading = None;
        }
        let through = usize::from(through);
        if through > agreed {
            if usize::from(target.length) > agreed {
                next.paths |= TRUNCATED_A_LOG;
            }
            follower.log[kept..through].copy_from_slice(&source.log[kept..through]);
            match self.scope.tail {
                Tail::Cleared => follower.set_length(through),
                Tail::Stale => follower.length = through as u8,
            }
        }
        if (0..kept).any(|i| follower.log[i].term != source.log[i].term) {
            next.paths |= KEPT_A_COMMITTED_TERM;
        }
        let learnt = source.commit.min(through as u8);
        follower.commit = follower.commit.max(learnt);
        let kept_now = usize::from(follower.commit).min(usize::from(follower.length));
        if leading.sync_index > 0
            && kept_now + agreeing(&source, follower, kept_now) >= usize::from(leading.sync_index)
            && follower.synced < source.term
        {
            follower.synced = source.term;
        }
        next.paths |= self.settle(follower);
        let matched = through.min(usize::from(follower.length)) as u8;
        let acks = &mut next.state.nodes[leader].leading.as_mut()?.matched[node];
        *acks = (*acks).max(matched);
        for (index, entry) in source.entries().iter().enumerate().take(through) {
            guard(&mut next, index, entry.value, source.term);
        }
        Some(next)
    }

    /// A window after its member's commit or sync moved: a slot at a classically committed index
    /// goes; under [`Variant::DropAtSync`] a synced member's slots of older terms too, and under
    /// [`Variant::DropCovered`] a slot its log covers.
    fn settle(&self, node: &mut Node) -> u64 {
        let (length, commit, synced) = (
            usize::from(node.length),
            usize::from(node.commit),
            node.synced,
        );
        let mut paths = 0;
        for (index, slot) in node.window.iter_mut().enumerate() {
            let Some(held) = *slot else {
                continue;
            };
            let covered = self.variant() == Variant::DropCovered && index < length;
            let older = self.variant() == Variant::DropAtSync && held.term < synced;
            if index < commit {
                paths |= PRUNED_AT_COMMIT;
                *slot = None;
            } else if covered || older {
                paths |= SYNC_DROPPED_A_SLOT;
                *slot = None;
            }
        }
        paths
    }

    /// A leader's known commit once it committed through `through` classically; under
    /// [`Variant::PruneAtFastCommit`] extended over each index it committed in its term, fast ones
    /// included.
    fn leader_commit(&self, node: &mut Node, through: usize) {
        let mut commit = usize::from(node.commit).max(through);
        if self.variant() == Variant::PruneAtFastCommit
            && let Some(leading) = node.leading
        {
            while leading.committed.get(commit).copied().unwrap_or(false) {
                commit += 1;
            }
        }
        node.commit = commit as u8;
    }

    fn scatter(&self, state: &State, leader: usize, node: usize, index: usize) -> Option<Next> {
        let source = state.nodes[leader];
        source.leading?;
        let target = state.nodes[node];
        let entry = *source.entries().get(index)?;
        if node == leader
            || entry.term != source.term
            || target.term != source.term
            || target.synced != source.term
            || index < usize::from(target.length)
        {
            return None;
        }
        let slot = Slot {
            term: entry.term,
            fast: false,
            value: entry.value,
        };
        if target.window[index].is_some_and(|held| held.ballot() >= slot.ballot()) {
            return None;
        }
        let mut next = Step::plain(*state);
        next.paths |= SCATTERED;
        next.state.nodes[node].window[index] = Some(slot);
        if self.variant() == Variant::CommitFromWindows {
            next.state.nodes[leader].leading.as_mut()?.window_acks[index] |= 1 << node;
        }
        guard(&mut next, index, entry.value, source.term);
        Some(next)
    }

    fn vote(&self, state: &State, node: usize, index: usize, value: u8) -> Option<Next> {
        let voter = state.nodes[node];
        let leading = state.nodes[self.leader_of(state, voter.term)?].leading?;
        let position = (index + 1) as u8;
        if voter.synced != voter.term
            || leading.open_from == 0
            || position < leading.open_from
            || index < usize::from(voter.length)
            || voter.window[index].is_some_and(|held| held.ballot() >= (voter.term, false))
        {
            return None;
        }
        let mut next = *state;
        next.nodes[node].window[index] = Some(Slot {
            term: voter.term,
            fast: true,
            value,
        });
        Some(Step::plain(next))
    }

    fn decide(&self, state: &State, leader: usize, value: u8, voters: u8) -> Option<Next> {
        let deciding = state.nodes[leader];
        let leading = deciding.leading?;
        let index = usize::from(deciding.length);
        if leading.open_from == 0
            || ((index + 1) as u8) < leading.open_from
            || index >= self.scope.indices
        {
            return None;
        }
        let votes: Vec<u8> = self
            .scope
            .members(voters)
            .map(|m| {
                let node = state.nodes[m];
                node.window[index]
                    .filter(|slot| {
                        slot.fast && slot.term == deciding.term && node.term == deciding.term
                    })
                    .map(|slot| slot.value)
            })
            .collect::<Option<_>>()?;
        let count = |v: u8| votes.iter().filter(|vote| **vote == v).count();
        let threshold = votes.len() + self.scope.fast() - self.scope.nodes;
        if self
            .scope
            .proposals()
            .find(|other| count(*other) >= threshold)
            .is_some_and(|forced| forced != value)
        {
            return None;
        }
        let mut next = Step::plain(*state);
        guard(&mut next, index, value, deciding.term);
        let me = &mut next.state.nodes[leader];
        me.log[index] = Entry {
            term: deciding.term,
            value,
        };
        me.length += 1;
        if count(value) >= self.scope.fast() {
            me.leading.as_mut()?.committed[index] = true;
            self.leader_commit(me, 0);
            next.paths |= self.settle(me);
            commit_record(&mut next, index, value, deciding.term, FAST_COMMIT);
        }
        Some(next)
    }

    fn propose(&self, state: &State, leader: usize, value: u8) -> Option<Next> {
        let me = state.nodes[leader];
        let index = usize::from(me.length);
        if me.leading?.open_from != 0 || index >= self.scope.indices {
            return None;
        }
        let mut next = Step::plain(*state);
        guard(&mut next, index, value, me.term);
        let placed = &mut next.state.nodes[leader];
        placed.log[index] = Entry {
            term: me.term,
            value,
        };
        placed.length += 1;
        Some(next)
    }

    /// The leader commits its entry of its term at `index` once a majority's logs hold it, and
    /// every index below with it (those logs agree up to it); a window's copy is counted only
    /// under [`Variant::CommitFromWindows`], which commits the index alone.
    fn commit(&self, state: &State, leader: usize, index: usize) -> Option<Next> {
        let source = state.nodes[leader];
        let leading = source.leading?;
        let entry = *source.entries().get(index)?;
        if entry.term != source.term || leading.committed[index] {
            return None;
        }
        let position = (index + 1) as u8;
        let holders = (0..self.scope.nodes)
            .filter(|m| {
                *m == leader
                    || leading.matched[*m] >= position
                    || leading.window_acks[index] & (1 << m) != 0
            })
            .count();
        if holders < self.scope.majority() {
            return None;
        }
        let windows = self.variant() == Variant::CommitFromWindows;
        let from = if windows { index } else { 0 };
        let mut next = Step::plain(*state);
        let me = &mut next.state.nodes[leader];
        let committed = &mut me.leading.as_mut()?.committed;
        for flag in &mut committed[from..=index] {
            *flag = true;
        }
        self.leader_commit(me, if windows { 0 } else { index + 1 });
        next.paths |= self.settle(me);
        for (below, held) in source
            .entries()
            .iter()
            .enumerate()
            .take(index + 1)
            .skip(from)
        {
            commit_record(&mut next, below, held.value, source.term, CLASSIC_COMMIT);
        }
        Some(next)
    }

    fn signature(&self, state: &State, me: usize) -> Signature {
        let node = &state.nodes[me];
        let nodes = &state.nodes[..self.scope.nodes];
        let mut matched_by = [0u8; MAX_NODES];
        let mut by: Vec<u8> = nodes
            .iter()
            .filter_map(|n| n.leading.map(|l| l.matched[me]))
            .collect();
        by.sort_unstable();
        for (slot, value) in matched_by.iter_mut().zip(by) {
            *slot = value;
        }
        Signature {
            term: node.term,
            candidate: node.vote.map(|chose| usize::from(chose) == me),
            voters: nodes.iter().filter(|n| n.vote == Some(me as u8)).count(),
            length: node.length,
            log: node.log.map(|e| (e.term, e.value == NOOP)),
            window: node
                .window
                .map(|slot| slot.map(|s| (s.term, s.fast, s.value == NOOP))),
            synced: node.synced,
            commit: node.commit,
            leading: node.leading.map(|l| {
                let mut matched = l.matched;
                matched.sort_unstable();
                (
                    l.sync_index,
                    l.open_from,
                    l.committed,
                    matched,
                    l.window_acks.map(u8::count_ones),
                )
            }),
            matched_by,
            held_by: std::array::from_fn(|i| {
                nodes
                    .iter()
                    .filter(|n| n.leading.is_some_and(|l| l.window_acks[i] & (1 << me) != 0))
                    .count()
            }),
        }
    }

    fn arranged(&self, state: &State, order: &[usize]) -> Key {
        let mut rename = [0usize; MAX_NODES];
        for (new, old) in order.iter().enumerate() {
            rename[*old] = new;
        }
        let remap = |mask: u8| {
            self.scope
                .members(mask)
                .fold(0u8, |out, m| out | (1 << rename[m]))
        };
        let mut out = *state;
        for (new, old) in order.iter().enumerate() {
            let mut node = state.nodes[*old];
            node.vote = node.vote.map(|chose| rename[usize::from(chose)] as u8);
            if let Some(leading) = node.leading.as_mut() {
                let mut matched = [0u8; MAX_NODES];
                for (from, value) in leading.matched.iter().enumerate().take(self.scope.nodes) {
                    matched[rename[from]] = *value;
                }
                leading.matched = matched;
                leading.window_acks = leading.window_acks.map(remap);
            }
            out.nodes[new] = node;
        }
        let mut first: Vec<u8> = Vec::new();
        let appearing = out.nodes[..self.scope.nodes]
            .iter()
            .flat_map(|n| {
                n.entries()
                    .iter()
                    .map(|e| e.value)
                    .chain(n.window.iter().flatten().map(|s| s.value))
                    .collect::<Vec<_>>()
            })
            .chain(out.chosen.iter().flatten().map(|c| c.value))
            .chain(self.scope.proposals())
            .filter(|v| *v != NOOP);
        for value in appearing {
            if !first.contains(&value) {
                first.push(value);
            }
        }
        let label = |value: u8| {
            if value == NOOP {
                NOOP
            } else {
                first.iter().position(|v| *v == value).unwrap() as u8 + 1
            }
        };
        for node in &mut out.nodes[..self.scope.nodes] {
            let length = usize::from(node.length);
            for entry in &mut node.log[..length] {
                entry.value = label(entry.value);
            }
            for slot in node.window.iter_mut().flatten() {
                slot.value = label(slot.value);
            }
        }
        for chosen in out.chosen.iter_mut().flatten() {
            chosen.value = label(chosen.value);
        }
        pack(&out)
    }
}

/// How many entries of `target`'s log from `from` on agree with `source`'s at the same indexes.
fn agreeing(source: &Node, target: &Node, from: usize) -> usize {
    let theirs = target.entries().get(from..).unwrap_or(&[]);
    source
        .entries()
        .get(from.min(source.entries().len())..)
        .unwrap_or(&[])
        .iter()
        .zip(theirs)
        .take_while(|(a, b)| a == b)
        .count()
}

fn guard(next: &mut Next, index: usize, value: u8, term: u8) {
    if let Some(chosen) = next.state.chosen[index]
        && term >= chosen.term
        && value != chosen.value
        && next.fault.is_none()
    {
        next.fault = Some(Fault::OverwroteChosen {
            index,
            chosen: chosen.value,
            sent: value,
            term,
        });
    }
}

fn commit_record(next: &mut Next, index: usize, value: u8, term: u8, path: u64) {
    next.paths |= path;
    if index > 0 && next.state.chosen[index - 1].is_none() {
        next.paths |= OUT_OF_ORDER_COMMIT;
    }
    match next.state.chosen[index] {
        None => next.state.chosen[index] = Some(Chosen { value, term }),
        Some(first) if first.value != value => {
            next.fault = Some(Fault::Disagreement {
                index,
                first: first.value,
                second: value,
            });
        }
        Some(_) => {}
    }
}

/// Log matching: two logs holding an entry of one term at one index agree up to it.
fn diverging(scope: Scope, state: &State) -> Option<Fault> {
    for a in 0..scope.nodes {
        for b in a + 1..scope.nodes {
            let (left, right) = (state.nodes[a].entries(), state.nodes[b].entries());
            for index in 0..left.len().min(right.len()) {
                if left[index].term == right[index].term && left[..=index] != right[..=index] {
                    return Some(Fault::LogsDiverge { index });
                }
            }
        }
    }
    None
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Signature {
    term: u8,
    candidate: Option<bool>,
    voters: usize,
    length: u8,
    log: [(u8, bool); MAX_INDICES],
    window: [Option<(u8, bool, bool)>; MAX_INDICES],
    synced: u8,
    commit: u8,
    #[allow(clippy::type_complexity)]
    leading: Option<(
        u8,
        u8,
        [bool; MAX_INDICES],
        [u8; MAX_NODES],
        [u32; MAX_INDICES],
    )>,
    matched_by: [u8; MAX_NODES],
    held_by: [usize; MAX_INDICES],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Key([u64; WORDS]);

const TERM_BITS: usize = 3;
const NODE_BITS: usize = 3;
const VALUE_BITS: usize = 2;
const OPTIONAL_VALUE_BITS: usize = 3;
const LENGTH_BITS: usize = 2;
const INDEX_BITS: usize = 3;

fn pack(state: &State) -> Key {
    let mut p = Packer::<WORDS>::new();
    for node in &state.nodes {
        p.put(TERM_BITS, u64::from(node.term)).unwrap();
        p.put_option(NODE_BITS, node.vote).unwrap();
        p.put(LENGTH_BITS, u64::from(node.length)).unwrap();
        for entry in node.entries() {
            p.put(TERM_BITS, u64::from(entry.term)).unwrap();
            p.put(VALUE_BITS, u64::from(entry.value)).unwrap();
        }
        for slot in node.window {
            p.put(1, u64::from(slot.is_some())).unwrap();
            if let Some(slot) = slot {
                p.put(TERM_BITS, u64::from(slot.term)).unwrap();
                p.put(1, u64::from(slot.fast)).unwrap();
                p.put(VALUE_BITS, u64::from(slot.value)).unwrap();
            }
        }
        p.put(TERM_BITS, u64::from(node.synced)).unwrap();
        p.put(LENGTH_BITS, u64::from(node.commit)).unwrap();
        p.put(1, u64::from(node.leading.is_some())).unwrap();
        if let Some(leading) = node.leading {
            for matched in leading.matched {
                p.put(LENGTH_BITS, u64::from(matched)).unwrap();
            }
            for acks in leading.window_acks {
                p.put(MAX_NODES, u64::from(acks)).unwrap();
            }
            p.put(INDEX_BITS, u64::from(leading.sync_index)).unwrap();
            p.put(INDEX_BITS, u64::from(leading.open_from)).unwrap();
            for committed in leading.committed {
                p.put(1, u64::from(committed)).unwrap();
            }
        }
    }
    for chosen in state.chosen {
        p.put_option(OPTIONAL_VALUE_BITS, chosen.map(|c| c.value))
            .unwrap();
        p.put(TERM_BITS, chosen.map_or(0, |c| u64::from(c.term)))
            .unwrap();
    }
    Key(p.words())
}

fn unpack(key: Key) -> State {
    let mut p = Packer::<WORDS>::over(key.0);
    let mut state = START;
    for node in &mut state.nodes {
        node.term = p.small(TERM_BITS).unwrap();
        node.vote = p.take_option(NODE_BITS).unwrap();
        node.length = p.small(LENGTH_BITS).unwrap();
        for entry in &mut node.log[..usize::from(node.length)] {
            entry.term = p.small(TERM_BITS).unwrap();
            entry.value = p.small(VALUE_BITS).unwrap();
        }
        for slot in &mut node.window {
            *slot = (p.take(1).unwrap() == 1).then(|| Slot {
                term: p.small(TERM_BITS).unwrap(),
                fast: p.take(1).unwrap() == 1,
                value: p.small(VALUE_BITS).unwrap(),
            });
        }
        node.synced = p.small(TERM_BITS).unwrap();
        node.commit = p.small(LENGTH_BITS).unwrap();
        if p.take(1).unwrap() == 1 {
            let mut leading = Leading {
                matched: [0; MAX_NODES],
                window_acks: [0; MAX_INDICES],
                sync_index: 0,
                open_from: 0,
                committed: [false; MAX_INDICES],
            };
            for matched in &mut leading.matched {
                *matched = p.small(LENGTH_BITS).unwrap();
            }
            for acks in &mut leading.window_acks {
                *acks = p.small(MAX_NODES).unwrap();
            }
            leading.sync_index = p.small(INDEX_BITS).unwrap();
            leading.open_from = p.small(INDEX_BITS).unwrap();
            for committed in &mut leading.committed {
                *committed = p.take(1).unwrap() == 1;
            }
            node.leading = Some(leading);
        }
    }
    for chosen in &mut state.chosen {
        let value = p.take_option(OPTIONAL_VALUE_BITS).unwrap();
        let term = p.small(TERM_BITS).unwrap();
        *chosen = value.map(|value| Chosen { value, term });
    }
    state
}

const NAMES: [char; MAX_NODES] = ['A', 'B', 'C', 'D', 'E'];

fn names(mask: u8) -> String {
    (0..MAX_NODES)
        .filter(|m| mask & (1 << m) != 0)
        .map(|m| NAMES[m])
        .collect()
}

impl fmt::Display for Action {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Timeout { node } => write!(out, "{} times out", NAMES[node]),
            Self::Learn { node, term } => write!(out, "{} learns term {term}", NAMES[node]),
            Self::Elect { node, quorum, fast } => write!(
                out,
                "{} is elected by {}{}",
                NAMES[node],
                names(quorum),
                if fast { ", fast" } else { "" }
            ),
            Self::Append {
                leader,
                node,
                through,
            } => {
                write!(
                    out,
                    "{} appends to {} through {through}",
                    NAMES[leader], NAMES[node]
                )
            }
            Self::Scatter {
                leader,
                node,
                index,
            } => {
                write!(out, "{} scatters {index} to {}", NAMES[leader], NAMES[node])
            }
            Self::Vote { node, index, value } => {
                write!(out, "{} fast-votes v{value} at {index}", NAMES[node])
            }
            Self::Decide {
                leader,
                value,
                voters,
            } => {
                write!(
                    out,
                    "{} decides v{value} from {}",
                    NAMES[leader],
                    names(voters)
                )
            }
            Self::Propose { leader, value } => write!(out, "{} proposes v{value}", NAMES[leader]),
            Self::Commit { leader, index } => write!(out, "{} commits {index}", NAMES[leader]),
        }
    }
}

impl Model for PrefixModel {
    type State = State;
    type Action = Action;
    type Fault = Fault;
    type Key = Key;

    fn paths(&self) -> &'static [&'static str] {
        &PATHS
    }

    fn initial(&self) -> State {
        START
    }

    fn actions(&self, state: &State, out: &mut Vec<Action>) {
        let scope = self.scope;
        for node in 0..scope.nodes {
            out.push(Action::Timeout { node });
            out.extend((1..=scope.terms).map(|term| Action::Learn { node, term }));
            for quorum in scope.quorums().filter(|q| q & (1 << node) != 0) {
                out.push(Action::Elect {
                    node,
                    quorum,
                    fast: false,
                });
                out.push(Action::Elect {
                    node,
                    quorum,
                    fast: true,
                });
            }
            for index in 0..scope.indices {
                out.extend(
                    scope
                        .proposals()
                        .map(|value| Action::Vote { node, index, value }),
                );
            }
            if state.nodes[node].leading.is_none() {
                continue;
            }
            let leader = node;
            for value in scope.proposals() {
                out.push(Action::Propose { leader, value });
                out.extend(scope.quorums().map(|voters| Action::Decide {
                    leader,
                    value,
                    voters,
                }));
            }
            for index in 0..scope.indices {
                out.push(Action::Commit { leader, index });
            }
            for node in (0..scope.nodes).filter(|m| *m != leader) {
                for through in 1..=scope.indices {
                    out.push(Action::Append {
                        leader,
                        node,
                        through: through as u8,
                    });
                }
                for index in 0..scope.indices {
                    out.push(Action::Scatter {
                        leader,
                        node,
                        index,
                    });
                }
            }
        }
    }

    fn apply(&self, state: &State, action: Action) -> Option<Next> {
        let mut next = match action {
            Action::Timeout { node } => self.timeout(state, node),
            Action::Learn { node, term } => self.learn(state, node, term),
            Action::Elect { node, quorum, fast } => self.elect(state, node, quorum, fast),
            Action::Append {
                leader,
                node,
                through,
            } => self.append(state, leader, node, through),
            Action::Scatter {
                leader,
                node,
                index,
            } => self.scatter(state, leader, node, index),
            Action::Vote { node, index, value } => self.vote(state, node, index, value),
            Action::Decide {
                leader,
                value,
                voters,
            } => self.decide(state, leader, value, voters),
            Action::Propose { leader, value } => self.propose(state, leader, value),
            Action::Commit { leader, index } => self.commit(state, leader, index),
        }?;
        if next.fault.is_none() {
            next.fault = diverging(self.scope, &next.state);
        }
        (next.state != *state || next.fault.is_some()).then_some(next)
    }

    fn canonical(&self, state: &State) -> Key {
        let signatures: Vec<Signature> = (0..self.scope.nodes)
            .map(|m| self.signature(state, m))
            .collect();
        least_over_ties(&signatures, &mut |order| self.arranged(state, order)).unwrap()
    }

    fn unpack(&self, key: &Key) -> State {
        unpack(*key)
    }
}
