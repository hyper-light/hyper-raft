//! The slot model: the log the fast track and parallel replication share, each index above the
//! committed prefix a single-decree instance (slates' `tests/slot_model.rs`, its research record's
//! §3.5, §3.7 and §4), here built again on hyper-check's search.
//!
//! **What it holds.** Each member keeps, per index, the slot it accepted (a value, the term it was
//! accepted at, and whether a leader decided it or it arrived straight from a proposer), its term
//! and vote, and while it leads its term's acknowledgements, what it may still do at each index and
//! what it committed there. A lost or late message is an action not taken or taken later; a crashed
//! member one that takes no more actions.
//!
//! **Two recoveries.** *Published* is Fast Raft as its authors give it (Castiglia, Goldberg and
//! Patterson, arXiv:2004.06215 §IV): an entry self-approved or leader-approved, an election that
//! compares the last leader-approved entry, a leader deciding an index by the entries its followers
//! hold there; its decision loop read three ways ([`Reading`]). *Ballots* is the rule slates'
//! record states: a slot's ballot is its term and, within a term, a decision outranks a fast vote;
//! per index the highest ballot among a quorum's reports decides; a fast ballot re-proposes the
//! value at least `|Q| + |F| − n` of the reports hold (Lamport, *Fast Paxos*, 2006); only a free
//! index is opened to the fast track.
//!
//! **Checked on every step:** agreement (one value committed per index), Paxos's P2c (no leader of
//! the term a value was chosen in, or later, sends another value there), one leader a term, one
//! decision a classic ballot.
//!
//! **The class.** Members and values are interchangeable: a state's key is the least packing over
//! the orders of its members sorted by a signature renaming leaves alone, values renumbered by first
//! appearance, which is one key per orbit of the renamings.

use std::fmt;

use hyper_check::explore::{Model, Packer, Step, least_over_ties};

/// The most members a scope models.
pub const MAX_NODES: usize = 5;
/// The most indexes a scope models.
pub const MAX_INDICES: usize = 2;
/// The highest term a scope reaches: a term packs in three bits.
pub const MAX_TERMS: u8 = 4;
/// The most values a scope proposes: a value packs in two bits.
pub const MAX_VALUES: u8 = 3;
/// Words a key takes: 37 bits a member at most, and 20 for the rest.
const WORDS: usize = 4;

/// How Fast Raft's decision loop (§IV-B, "while there exists a k = commitIndex + 1 for which at
/// least a classic quorum of votes has been received") is read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reading {
    /// As written: the loop decides whenever a classic quorum of votes is in.
    Literal,
    /// A leader decides an index at most once in its term.
    OncePerTerm,
    /// Once a term, and never by votes where it holds a leader-approved entry (§IV-C's "treated
    /// the same as they are treated in classic Raft").
    KeepLeaderApproved,
}

/// The recovery a new leader runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rule {
    /// Fast Raft as published, under a reading of its loop.
    Published(Reading),
    /// The ballot rule.
    Ballots,
}

/// The bounds of a search.
#[derive(Clone, Copy, Debug)]
pub struct Scope {
    pub nodes: usize,
    pub indices: usize,
    pub values: u8,
    pub terms: u8,
    pub rule: Rule,
}

impl Scope {
    fn majority(self) -> usize {
        self.nodes / 2 + 1
    }

    /// The smallest `f` with `2f + q > 2n`, `q` the classic quorum: Fast Paxos's requirement,
    /// which Fast Raft states as ⌈3n/4⌉ (`checked` holds the two equal).
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

    fn published(self) -> bool {
        matches!(self.rule, Rule::Published(_))
    }

    fn members(self, mask: u8) -> impl Iterator<Item = usize> {
        (0..self.nodes).filter(move |m| mask & (1 << m) != 0)
    }

    /// Every set of at least a classic quorum, as masks.
    fn quorums(self) -> impl Iterator<Item = u8> {
        let all = u8::try_from((1usize << self.nodes) - 1).unwrap();
        (1..=all).filter(move |mask| mask.count_ones() as usize >= self.majority())
    }
}

/// An accepted slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Slot {
    pub term: u8,
    /// Decided by a leader (leader-approved; under the ballot rule a classic ballot), not
    /// self-approved or a fast vote.
    pub classic: bool,
    pub value: u8,
}

impl Slot {
    fn ballot(self) -> (u8, bool) {
        (self.term, self.classic)
    }
}

/// What a leader may still do at an index in its term.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Phase {
    Free,
    Opened,
    Decided,
}

/// A leader's state for its term. It keeps no tally of votes: a decision reads the votes of the
/// quorum it is taken from at once, which is the tally a leader holds when every vote it counted is
/// still its voter's entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Leading {
    acks: [u8; MAX_INDICES],
    phase: [Phase; MAX_INDICES],
    committed: [bool; MAX_INDICES],
}

const FRESH: Leading = Leading {
    acks: [0; MAX_INDICES],
    phase: [Phase::Free; MAX_INDICES],
    committed: [false; MAX_INDICES],
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Node {
    term: u8,
    vote: Option<u8>,
    slots: [Option<Slot>; MAX_INDICES],
    leading: Option<Leading>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chosen {
    value: u8,
    term: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct State {
    nodes: [Node; MAX_NODES],
    /// Under the ballot rule, whether the leader of each term opened each index to the fast track.
    opened: [[bool; MAX_INDICES]; MAX_TERMS as usize + 1],
    /// What was first committed at each index, the history the checks read.
    chosen: [Option<Chosen>; MAX_INDICES],
}

pub const START: State = State {
    nodes: [Node {
        term: 0,
        vote: None,
        slots: [None; MAX_INDICES],
        leading: None,
    }; MAX_NODES],
    opened: [[false; MAX_INDICES]; MAX_TERMS as usize + 1],
    chosen: [None; MAX_INDICES],
};

/// One atomic step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// A member times out into the next term, voting for itself.
    Timeout { node: usize },
    /// A member learns a leader's term from its message, without voting.
    Learn { node: usize, term: u8 },
    /// A candidate wins with the votes of `quorum` and runs its rule's recovery.
    Elect { node: usize, quorum: u8 },
    /// Published: a proposal lands in an empty slot, self-approved.
    Insert {
        node: usize,
        index: usize,
        value: u8,
    },
    /// Ballots: a member accepts a proposal as a fast vote at an index its leader opened.
    Accept {
        node: usize,
        index: usize,
        value: u8,
    },
    /// Ballots: a leader opens a free index to the fast track.
    Open { leader: usize, index: usize },
    /// Ballots: a leader proposes its own value at a free index.
    Propose {
        leader: usize,
        index: usize,
        value: u8,
    },
    /// A leader decides an index from the entries `voters` hold there.
    Decide {
        leader: usize,
        index: usize,
        value: u8,
        voters: u8,
    },
    /// A leader's decided entry reaches a member, which acknowledges it.
    Replicate {
        leader: usize,
        node: usize,
        index: usize,
    },
    /// A leader commits its entry at an index a majority acknowledged.
    Commit { leader: usize, index: usize },
}

/// Why a step is a violation.
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
    TwoLeaders {
        term: u8,
    },
    TwoDecisions {
        index: usize,
        term: u8,
    },
}

pub const FAST_COMMIT: u64 = 1;
pub const CLASSIC_COMMIT: u64 = 1 << 1;
pub const RECOVERED_DECISION: u64 = 1 << 2;
pub const RECOVERED_FAST_CHOICE: u64 = 1 << 3;
pub const DECIDED_FROM_VOTES: u64 = 1 << 4;
pub const OVERWROTE_STALE: u64 = 1 << 5;
pub const OUT_OF_ORDER_COMMIT: u64 = 1 << 6;
/// The paths, in bit order.
pub const PATHS: [&str; 7] = [
    "fast commits",
    "classic commits",
    "recoveries a decision constrained",
    "recoveries a possible fast choice constrained",
    "decisions from fast votes",
    "stale values overwritten",
    "commits above an uncommitted index",
];

type Next = Step<State, Fault>;

/// The slot model at a scope.
pub struct SlotModel {
    pub scope: Scope,
}

impl SlotModel {
    pub fn at(scope: Scope) -> Self {
        Self {
            scope: scope.checked(),
        }
    }

    fn leader_of(&self, state: &State, term: u8) -> Option<usize> {
        (0..self.scope.nodes).find(|m| {
            let node = &state.nodes[*m];
            node.term == term && node.leading.is_some()
        })
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

    /// Fast Raft's election compares the last leader-approved entry: its index from one (zero for
    /// none) and its term.
    fn last_approved(&self, node: &Node) -> (usize, u8) {
        (0..self.scope.indices)
            .rev()
            .find_map(|i| {
                node.slots[i]
                    .filter(|slot| slot.classic)
                    .map(|slot| (i + 1, slot.term))
            })
            .unwrap_or((0, 0))
    }

    /// Whether `voter` grants `candidate` its vote. The ballot rule needs no log comparison for
    /// safety, so it is searched with none: any majority may elect any candidate.
    fn grants(&self, state: &State, voter: usize, candidate: usize) -> bool {
        let (v, c) = (&state.nodes[voter], &state.nodes[candidate]);
        let by_term = v.term < c.term
            || (v.term == c.term && v.vote.is_none_or(|chose| usize::from(chose) == candidate));
        if !by_term || !self.scope.published() {
            return by_term;
        }
        // §IV-C: "candLastLogIndex ≥ lastLeaderIndex and candLastLogTerm ≥
        // log[lastLeaderIndex].term, or candLastLogTerm > lastLeaderIndex.term".
        let (ci, ct) = self.last_approved(c);
        let (vi, vt) = self.last_approved(v);
        (ci >= vi && ct >= vt) || ct > vt
    }

    fn elect(&self, state: &State, node: usize, quorum: u8) -> Option<Next> {
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
        let mut next = Step::plain(*state);
        for m in self.scope.members(quorum) {
            let voter = &mut next.state.nodes[m];
            voter.term = candidate.term;
            voter.vote = Some(node as u8);
            voter.leading = None;
        }
        if self.leader_of(state, candidate.term).is_some() {
            next.fault = Some(Fault::TwoLeaders {
                term: candidate.term,
            });
            return Some(next);
        }
        next.state.nodes[node].leading = Some(FRESH);
        if self.scope.rule == Rule::Ballots {
            self.recover(state, node, quorum, &mut next);
        }
        Some(next)
    }

    /// What the reports of `quorum` constrain a new leader to at `index` under the ballot rule,
    /// with the path that did; `None` when nothing could have been chosen there.
    fn constrained(
        &self,
        state: &State,
        quorum: u8,
        index: usize,
    ) -> Result<Option<(u8, u64)>, Fault> {
        let reports: Vec<Slot> = self
            .scope
            .members(quorum)
            .filter_map(|m| state.nodes[m].slots[index])
            .collect();
        let Some(highest) = reports.iter().map(|slot| slot.ballot()).max() else {
            return Ok(None);
        };
        let top: Vec<u8> = reports
            .iter()
            .filter(|slot| slot.ballot() == highest)
            .map(|slot| slot.value)
            .collect();
        let (term, classic) = highest;
        if classic {
            if top.iter().any(|value| *value != top[0]) {
                return Err(Fault::TwoDecisions { index, term });
            }
            return Ok(Some((top[0], RECOVERED_DECISION)));
        }
        let threshold = quorum.count_ones() as usize + self.scope.fast() - self.scope.nodes;
        Ok((0..self.scope.values)
            .find(|v| top.iter().filter(|held| **held == *v).count() >= threshold)
            .map(|v| (v, RECOVERED_FAST_CHOICE)))
    }

    /// The ballot rule's recovery: each index the reports constrain is re-proposed at the new
    /// term's classic ballot; every other is free.
    fn recover(&self, state: &State, node: usize, quorum: u8, next: &mut Next) {
        let term = state.nodes[node].term;
        for index in 0..self.scope.indices {
            match self.constrained(state, quorum, index) {
                Err(fault) => next.fault = Some(fault),
                Ok(None) => {}
                Ok(Some((value, path))) => {
                    guard(next, index, value, term);
                    let leader = &mut next.state.nodes[node];
                    leader.slots[index] = Some(Slot {
                        term,
                        classic: true,
                        value,
                    });
                    if let Some(leading) = leader.leading.as_mut() {
                        leading.phase[index] = Phase::Decided;
                        leading.acks[index] = 1 << node;
                    }
                    next.paths |= path;
                }
            }
        }
    }

    fn insert(&self, state: &State, node: usize, index: usize, value: u8) -> Option<Next> {
        if state.nodes[node].slots[index].is_some() {
            return None;
        }
        let mut next = *state;
        next.nodes[node].slots[index] = Some(Slot {
            term: state.nodes[node].term,
            classic: false,
            value,
        });
        Some(Step::plain(next))
    }

    fn accept(&self, state: &State, node: usize, index: usize, value: u8) -> Option<Next> {
        let me = state.nodes[node];
        if !state.opened[usize::from(me.term)][index] {
            return None;
        }
        let held = me.slots[index];
        if held.is_some_and(|slot| slot.ballot() >= (me.term, false)) {
            return None;
        }
        let mut next = Step::plain(*state);
        next.state.nodes[node].slots[index] = Some(Slot {
            term: me.term,
            classic: false,
            value,
        });
        if held.is_some_and(|slot| slot.value != value) {
            next.paths |= OVERWROTE_STALE;
        }
        Some(next)
    }

    fn open(&self, state: &State, leader: usize, index: usize) -> Option<Next> {
        let mut next = *state;
        let term = state.nodes[leader].term;
        let leading = next.nodes[leader].leading.as_mut()?;
        if leading.phase[index] != Phase::Free {
            return None;
        }
        leading.phase[index] = Phase::Opened;
        next.opened[usize::from(term)][index] = true;
        Some(Step::plain(next))
    }

    fn propose(&self, state: &State, leader: usize, index: usize, value: u8) -> Option<Next> {
        let term = state.nodes[leader].term;
        if state.nodes[leader].leading?.phase[index] != Phase::Free {
            return None;
        }
        let mut next = Step::plain(*state);
        guard(&mut next, index, value, term);
        let me = &mut next.state.nodes[leader];
        me.slots[index] = Some(Slot {
            term,
            classic: true,
            value,
        });
        let leading = me.leading.as_mut()?;
        leading.phase[index] = Phase::Decided;
        leading.acks[index] = 1 << leader;
        Some(next)
    }

    /// The votes of `voters` at `index` as the leader of `term` hears them, or `None` when one
    /// cannot vote: out of the term, holding nothing there, or (ballot rule) holding anything but a
    /// fast vote of the term.
    fn votes(&self, state: &State, term: u8, index: usize, voters: u8) -> Option<Vec<u8>> {
        self.scope
            .members(voters)
            .map(|m| {
                let voter = state.nodes[m];
                let slot = voter.slots[index].filter(|_| voter.term == term)?;
                let fast_vote = slot.term == term && !slot.classic;
                (self.scope.published() || fast_vote).then_some(slot.value)
            })
            .collect()
    }

    fn decide(
        &self,
        state: &State,
        leader: usize,
        index: usize,
        value: u8,
        voters: u8,
    ) -> Option<Next> {
        let deciding = state.nodes[leader];
        let leading = deciding.leading?;
        let votes = self.votes(state, deciding.term, index, voters)?;
        let count = |v: u8| votes.iter().filter(|vote| **vote == v).count();
        let allowed = match self.scope.rule {
            Rule::Published(reading) => {
                let next_open = (0..self.scope.indices).find(|i| !leading.committed[*i]);
                let most = (0..self.scope.values).map(count).max().unwrap_or(0);
                let decided = leading.phase[index] == Phase::Decided;
                let approved = deciding.slots[index].is_some_and(|slot| slot.classic);
                let reading_allows = match reading {
                    Reading::Literal => true,
                    Reading::OncePerTerm => !decided,
                    Reading::KeepLeaderApproved => !decided && !approved,
                };
                next_open == Some(index) && count(value) == most && reading_allows
            }
            Rule::Ballots => {
                let threshold = votes.len() + self.scope.fast() - self.scope.nodes;
                let forced = (0..self.scope.values).find(|v| count(*v) >= threshold);
                leading.phase[index] == Phase::Opened && forced.is_none_or(|f| f == value)
            }
        };
        if !allowed {
            return None;
        }
        let mut next = Step::plain(*state);
        guard(&mut next, index, value, deciding.term);
        let decided = Slot {
            term: deciding.term,
            classic: true,
            value,
        };
        let changed = deciding.slots[index] != Some(decided);
        let me = &mut next.state.nodes[leader];
        me.slots[index] = Some(decided);
        let leading = me.leading.as_mut()?;
        if changed {
            leading.acks[index] = 1 << leader;
        }
        leading.phase[index] = Phase::Decided;
        if self.scope.rule == Rule::Ballots {
            next.paths |= DECIDED_FROM_VOTES;
        }
        if count(value) >= self.scope.fast() && !leading.committed[index] {
            leading.committed[index] = true;
            commit_record(&mut next, index, value, deciding.term, FAST_COMMIT);
        }
        Some(next)
    }

    fn replicate(&self, state: &State, leader: usize, node: usize, index: usize) -> Option<Next> {
        let source = state.nodes[leader];
        source.leading?;
        let slot = source.slots[index].filter(|slot| slot.classic)?;
        // The ballot rule replicates only its term's decisions (what it recovered it re-proposed
        // at its term); Fast Raft's leader sends every leader-approved entry.
        if self.scope.rule == Rule::Ballots && slot.term != source.term {
            return None;
        }
        let target = state.nodes[node];
        if node == leader || target.term > source.term {
            return None;
        }
        let mut next = Step::plain(*state);
        guard(&mut next, index, slot.value, source.term);
        let follower = &mut next.state.nodes[node];
        if follower.term < source.term {
            follower.term = source.term;
            follower.vote = None;
            follower.leading = None;
        }
        follower.slots[index] = Some(slot);
        next.state.nodes[leader].leading.as_mut()?.acks[index] |= 1 << node;
        if target.slots[index].is_some_and(|held| held.value != slot.value) {
            next.paths |= OVERWROTE_STALE;
        }
        Some(next)
    }

    fn commit(&self, state: &State, leader: usize, index: usize) -> Option<Next> {
        let source = state.nodes[leader];
        let leading = source.leading?;
        let slot = source.slots[index].filter(|slot| slot.classic && slot.term == source.term)?;
        let holders = (leading.acks[index] | (1 << leader)).count_ones() as usize;
        if leading.committed[index] || holders < self.scope.majority() {
            return None;
        }
        let mut next = Step::plain(*state);
        next.state.nodes[leader].leading.as_mut()?.committed[index] = true;
        commit_record(&mut next, index, slot.value, source.term, CLASSIC_COMMIT);
        Some(next)
    }

    /// The renaming-invariant part of member `me`: what it holds without naming another member or
    /// a value, and how others name it.
    fn signature(&self, state: &State, me: usize) -> Signature {
        let node = &state.nodes[me];
        let nodes = &state.nodes[..self.scope.nodes];
        Signature {
            term: node.term,
            candidate: node.vote.map(|chose| usize::from(chose) == me),
            voters: nodes.iter().filter(|n| n.vote == Some(me as u8)).count(),
            slots: node
                .slots
                .map(|slot| slot.map(|slot| (slot.term, slot.classic))),
            leading: node
                .leading
                .map(|l| (l.phase, l.committed, l.acks.map(u8::count_ones))),
            acked_by: std::array::from_fn(|i| {
                nodes
                    .iter()
                    .filter(|n| n.leading.is_some_and(|l| l.acks[i] & (1 << me) != 0))
                    .count()
            }),
        }
    }

    /// `state` with member `order[k]` as member `k`, its values renumbered by first appearance,
    /// packed.
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
                leading.acks = leading.acks.map(remap);
            }
            out.nodes[new] = node;
        }
        let mut first: Vec<u8> = Vec::new();
        let appearing = out.nodes[..self.scope.nodes]
            .iter()
            .flat_map(|n| {
                n.slots[..self.scope.indices]
                    .iter()
                    .flatten()
                    .map(|s| s.value)
            })
            .chain(out.chosen.iter().flatten().map(|c| c.value))
            .chain(0..self.scope.values);
        for value in appearing {
            if !first.contains(&value) {
                first.push(value);
            }
        }
        let label = |value: u8| first.iter().position(|v| *v == value).unwrap() as u8;
        for node in &mut out.nodes[..self.scope.nodes] {
            for slot in node.slots.iter_mut().flatten() {
                slot.value = label(slot.value);
            }
        }
        for chosen in out.chosen.iter_mut().flatten() {
            chosen.value = label(chosen.value);
        }
        pack(&out)
    }
}

/// Faults a leader of `term` sending `value` at `index` when another was committed there in
/// `term` or before: the step Paxos's P2c forbids.
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

/// `value` committed at `index` in `term`: recorded first, or the disagreement with what was.
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

/// What the representative sorts members by.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Signature {
    term: u8,
    candidate: Option<bool>,
    voters: usize,
    slots: [Option<(u8, bool)>; MAX_INDICES],
    leading: Option<(
        [Phase; MAX_INDICES],
        [bool; MAX_INDICES],
        [u32; MAX_INDICES],
    )>,
    acked_by: [usize; MAX_INDICES],
}

/// A state packed in [`WORDS`] words.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Key([u64; WORDS]);

const TERM_BITS: usize = 3;
const NODE_BITS: usize = 3;
const VALUE_BITS: usize = 2;
const PHASE_BITS: usize = 2;

fn phase_code(phase: Phase) -> u64 {
    match phase {
        Phase::Free => 0,
        Phase::Opened => 1,
        Phase::Decided => 2,
    }
}

fn pack(state: &State) -> Key {
    let mut p = Packer::<WORDS>::new();
    for node in &state.nodes {
        p.put(TERM_BITS, u64::from(node.term)).unwrap();
        p.put_option(NODE_BITS, node.vote).unwrap();
        for slot in node.slots {
            p.put(1, u64::from(slot.is_some())).unwrap();
            if let Some(slot) = slot {
                p.put(VALUE_BITS, u64::from(slot.value)).unwrap();
                p.put(TERM_BITS, u64::from(slot.term)).unwrap();
                p.put(1, u64::from(slot.classic)).unwrap();
            }
        }
        p.put(1, u64::from(node.leading.is_some())).unwrap();
        if let Some(leading) = node.leading {
            for i in 0..MAX_INDICES {
                p.put(MAX_NODES, u64::from(leading.acks[i])).unwrap();
                p.put(PHASE_BITS, phase_code(leading.phase[i])).unwrap();
                p.put(1, u64::from(leading.committed[i])).unwrap();
            }
        }
    }
    for opened in state.opened.iter().flatten() {
        p.put(1, u64::from(*opened)).unwrap();
    }
    for chosen in state.chosen {
        p.put_option(VALUE_BITS, chosen.map(|c| c.value)).unwrap();
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
        for slot in &mut node.slots {
            *slot = (p.take(1).unwrap() == 1).then(|| Slot {
                value: p.small(VALUE_BITS).unwrap(),
                term: p.small(TERM_BITS).unwrap(),
                classic: p.take(1).unwrap() == 1,
            });
        }
        node.leading = (p.take(1).unwrap() == 1).then(|| {
            let mut leading = FRESH;
            for i in 0..MAX_INDICES {
                leading.acks[i] = p.small(MAX_NODES).unwrap();
                leading.phase[i] = match p.take(PHASE_BITS).unwrap() {
                    0 => Phase::Free,
                    1 => Phase::Opened,
                    _ => Phase::Decided,
                };
                leading.committed[i] = p.take(1).unwrap() == 1;
            }
            leading
        });
    }
    for opened in state.opened.iter_mut().flatten() {
        *opened = p.take(1).unwrap() == 1;
    }
    for chosen in &mut state.chosen {
        let value = p.take_option(VALUE_BITS).unwrap();
        let term = p.small(TERM_BITS).unwrap();
        *chosen = value.map(|value| Chosen { value, term });
    }
    state
}

/// Letters members print as.
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
            Self::Elect { node, quorum } => {
                write!(out, "{} is elected by {}", NAMES[node], names(quorum))
            }
            Self::Insert { node, index, value } => {
                write!(out, "{} inserts v{value} at {index}", NAMES[node])
            }
            Self::Accept { node, index, value } => {
                write!(out, "{} fast-accepts v{value} at {index}", NAMES[node])
            }
            Self::Open { leader, index } => write!(out, "{} opens {index}", NAMES[leader]),
            Self::Propose {
                leader,
                index,
                value,
            } => {
                write!(out, "{} proposes v{value} at {index}", NAMES[leader])
            }
            Self::Decide {
                leader,
                index,
                value,
                voters,
            } => write!(
                out,
                "{} decides v{value} at {index} from {}",
                NAMES[leader],
                names(voters)
            ),
            Self::Replicate {
                leader,
                node,
                index,
            } => {
                write!(
                    out,
                    "{} replicates {index} to {}",
                    NAMES[leader], NAMES[node]
                )
            }
            Self::Commit { leader, index } => write!(out, "{} commits {index}", NAMES[leader]),
        }
    }
}

impl Model for SlotModel {
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
            out.extend(
                scope
                    .quorums()
                    .filter(|q| q & (1 << node) != 0)
                    .map(|quorum| Action::Elect { node, quorum }),
            );
            for index in 0..scope.indices {
                for value in 0..scope.values {
                    out.push(match scope.rule {
                        Rule::Published(_) => Action::Insert { node, index, value },
                        Rule::Ballots => Action::Accept { node, index, value },
                    });
                }
            }
            if state.nodes[node].leading.is_none() {
                continue;
            }
            let leader = node;
            for index in 0..scope.indices {
                out.push(Action::Commit { leader, index });
                if scope.rule == Rule::Ballots {
                    out.push(Action::Open { leader, index });
                    out.extend((0..scope.values).map(|value| Action::Propose {
                        leader,
                        index,
                        value,
                    }));
                }
                for value in 0..scope.values {
                    out.extend(scope.quorums().map(|voters| Action::Decide {
                        leader,
                        index,
                        value,
                        voters,
                    }));
                }
                out.extend((0..scope.nodes).filter(|m| *m != leader).map(|node| {
                    Action::Replicate {
                        leader,
                        node,
                        index,
                    }
                }));
            }
        }
    }

    fn apply(&self, state: &State, action: Action) -> Option<Next> {
        let next = match action {
            Action::Timeout { node } => self.timeout(state, node),
            Action::Learn { node, term } => self.learn(state, node, term),
            Action::Elect { node, quorum } => self.elect(state, node, quorum),
            Action::Insert { node, index, value } => self.insert(state, node, index, value),
            Action::Accept { node, index, value } => self.accept(state, node, index, value),
            Action::Open { leader, index } => self.open(state, leader, index),
            Action::Propose {
                leader,
                index,
                value,
            } => self.propose(state, leader, index, value),
            Action::Decide {
                leader,
                index,
                value,
                voters,
            } => self.decide(state, leader, index, value, voters),
            Action::Replicate {
                leader,
                node,
                index,
            } => self.replicate(state, leader, node, index),
            Action::Commit { leader, index } => self.commit(state, leader, index),
        }?;
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
