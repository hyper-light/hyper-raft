//! `docs/models/FastTrack.tla` as a [`Model`] of `hyper_check::explore` (`docs/sim.md` §4.5): the
//! same variables, the same actions and the same invariants, step for step, so that a scope TLC
//! cannot reach in CI's model job is searched here, by orbit under renaming the members a change
//! keeps and the values, with the search's memory counted.
//!
//! Each action below is the TLA+ action of the same name, its guards in the same order where an
//! order matters to no outcome, and each comment that says what an action does says it of both. An
//! index is one-based where the specification's is (`Indexes == 1..MaxLen`) and held at the slot
//! one below it here.
//!
//! What it must agree with: with no symmetry (`Scope::symmetric` false) a search's class count is
//! the number of distinct states TLC finds with no `SYMMETRY`; the configurations
//! `docs/models/*Plain.cfg` state those counts, which CI's model job holds TLC to.

use std::fmt;

use hyper_check::explore::{Model, Packer, Step};

pub const MAX_SERVERS: usize = 5;
pub const MAX_LEN: usize = 3;
/// A term packs in three bits: `0..=MAX_TERM`.
pub const MAX_TERM: u8 = 4;
const TERMS: usize = MAX_TERM as usize + 1;
const WORDS: usize = 6;

/// No entry, no vote held, nothing said.
pub const NOTHING: u8 = 0;
/// What a leader's own first entry states.
pub const NOOP: u8 = 1;
/// The first proposed value; the `k`-th is `V1 + k − 1`.
pub const V1: u8 = 2;
pub const MAX_VALUES: u8 = 2;
pub const CHANGE: u8 = 4;
pub const ENTER: u8 = 5;
pub const LEAVE: u8 = 6;

/// No vote.
const NOBODY: u8 = 0;

/// The configuration a member counts by, or a leader was elected under.
pub const NO_CONFIGURATION: u8 = 0;
const INITIAL_ONLY: u8 = 1;
const TARGET_ONLY: u8 = 2;
const JOINT_CONF: u8 = 3;

/// `Rule`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rule {
    /// The entry most held among the quorum (the core).
    Most,
    /// The entry least held (refused).
    Least,
    /// slates' ballot rule: of the reports of the highest term, the value at least
    /// `|Q| + |F| − n` of them hold, else the index is free.
    Ballot,
}

/// `Marks`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Marks {
    Core,
    SelfVote,
    Whole,
}

/// `Counts`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Counts {
    Round,
    Any,
}

/// `Configs`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Configs {
    Term,
    Current,
}

/// `Releases`: when a member no longer holds what it approved by itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Releases {
    /// Once its log reaches the index (hyper-raft before the fix; slates' `DropCovered`).
    Log,
    /// Once it knows the index committed by a classic quorum.
    Classic,
}

/// `Votes`: what a fast quorum counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Votes {
    /// What members hold by themselves, and what they hold from the leader (before the fix).
    Logs,
    /// What members hold by themselves only.
    Held,
}

/// `Reports`: what a voter says it holds at an index, to a candidate's recovery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reports {
    /// What it holds by itself (the core).
    Held,
    /// Every entry it acknowledged above what it knows committed by a classic quorum: its log's
    /// entry where its log reaches the index, what it holds by itself where it does not (design B,
    /// slates' `ReportLogsToo` over most-held recovery).
    Acknowledged,
}

/// The invariants a configuration names beside the five every one checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reach {
    None,
    NoFastByHeld,
    NoFastByHeldAfterChange,
    NoMarkedLeader,
    NoRestamp,
}

/// A configuration of the specification.
#[derive(Clone, Copy, Debug)]
pub struct Scope {
    pub servers: usize,
    /// Bit `s` for server `s` (zero-based).
    pub initial: u8,
    pub target: u8,
    pub joint: bool,
    pub values: u8,
    pub max_term: u8,
    pub max_len: usize,
    /// Bit `i` for index `i` (one-based).
    pub held_at: u8,
    pub losers: u8,
    pub rule: Rule,
    pub marks: Marks,
    pub counts: Counts,
    pub configs: Configs,
    pub releases: Releases,
    pub votes: Votes,
    pub reports: Reports,
    pub reach: Reach,
    /// Whether states are counted by orbit under renaming the members a change keeps.
    pub rename_servers: bool,
    /// Whether states are counted by orbit under renaming the values.
    pub rename_values: bool,
    /// `Leads`: the member that may be elected in each term, by term (`ANY` for any): a
    /// scenario's order of leaders, which the search is held to.
    pub leads: [u8; TERMS],
    /// `Proposed`: the values proposed in each term, by term, a bit a value from `V1` (a
    /// scenario's proposals; every value in every term for none): what a member holds as of a
    /// term and what a leader of it takes besides its no-op.
    pub proposed: [u8; TERMS],
    /// Whether a state is counted by what any step can still read of it (`reduced`, the
    /// scenario's `ScenarioView`).
    pub reduce: bool,
    /// The specification as it was before the fix (`docs/models/FastTrack.tla` at `df54729`):
    /// a leader's append goes through the end of its log, a classic commit only moves the commit,
    /// and a leader's own log is of its round only by its acknowledgements. With `Releases::Log`
    /// and `Votes::Logs` the counts are those its configurations stated, which is how this model
    /// is checked against the specification it mirrors (`the_model_counts_what_tlc_counted`).
    pub legacy: bool,
}

/// Every value in the term.
pub const EVERY: u8 = u8::MAX;

/// Any member may be elected in the term.
pub const ANY: u8 = u8::MAX;

impl Scope {
    fn checked(self) -> Self {
        assert!(self.servers <= MAX_SERVERS && self.max_len <= MAX_LEN);
        assert!(self.values >= 1 && self.values <= MAX_VALUES && self.max_term <= MAX_TERM);
        self
    }
    fn indexes(self) -> std::ops::RangeInclusive<usize> {
        1..=self.max_len
    }
    fn proposals(self) -> std::ops::RangeInclusive<u8> {
        V1..=V1 + self.values - 1
    }
    /// `Stated == Values \cup {Noop}`.
    fn stated(self) -> impl Iterator<Item = u8> {
        std::iter::once(NOOP).chain(self.proposals())
    }
    fn conf_in(self, c: u8) -> u8 {
        match c {
            INITIAL_ONLY => self.initial,
            TARGET_ONLY | JOINT_CONF => self.target,
            _ => 0,
        }
    }
    fn conf_out(self, c: u8) -> u8 {
        if c == JOINT_CONF { self.initial } else { 0 }
    }
    fn voters(self, c: u8) -> u8 {
        self.conf_in(c) | self.conf_out(c)
    }
    fn all(self) -> u8 {
        u8::try_from((1usize << self.servers) - 1).unwrap()
    }
}

fn bit(s: usize) -> u8 {
    1 << s
}
fn has(mask: u8, s: usize) -> bool {
    mask & bit(s) != 0
}
fn card(mask: u8) -> usize {
    mask.count_ones() as usize
}
/// `Majority(C, H)`.
fn majority(c: u8, h: u8) -> bool {
    2 * card(h & c) > card(c)
}
/// `FastOf(C, H)`.
fn fast_of(c: u8, h: u8) -> bool {
    4 * card(h & c) >= 3 * card(c)
}
fn members(mask: u8, servers: usize) -> impl Iterator<Item = usize> {
    (0..servers).filter(move |s| has(mask, *s))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Entry {
    pub term: u8,
    pub value: u8,
}

/// What a member last said it holds at an index: `NotChosen` when it said nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Said {
    pub value: u8,
    pub term: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Mark {
    pub index: u8,
    pub term: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct State {
    pub term: [u8; MAX_SERVERS],
    /// `NOBODY`, or a server plus one.
    pub vote: [u8; MAX_SERVERS],
    pub leader: [bool; MAX_SERVERS],
    pub len: [u8; MAX_SERVERS],
    pub log: [[Entry; MAX_LEN]; MAX_SERVERS],
    pub held: [[u8; MAX_LEN]; MAX_SERVERS],
    pub commit: [u8; MAX_SERVERS],
    pub says: [[Said; MAX_LEN]; MAX_SERVERS],
    pub acks: [[u8; TERMS]; MAX_SERVERS],
    pub under: [u8; MAX_SERVERS],
    pub chosen: [Said; MAX_LEN],
    pub mark: [Mark; MAX_SERVERS],
    pub marked_led: bool,
    pub classic: [u8; MAX_SERVERS],
}

impl State {
    fn entries(&self, s: usize) -> &[Entry] {
        &self.log[s][..usize::from(self.len[s])]
    }
    /// `log[s][i]`, one-based.
    fn at(&self, s: usize, i: usize) -> Entry {
        self.log[s][i - 1]
    }
    fn last_term(&self, s: usize) -> u8 {
        self.entries(s).last().map_or(0, |e| e.term)
    }
    fn set_log(&mut self, s: usize, entries: &[Entry]) {
        self.log[s] = [Entry::default(); MAX_LEN];
        self.log[s][..entries.len()].copy_from_slice(entries);
        self.len[s] = u8::try_from(entries.len()).unwrap();
    }
    fn marked(&self, s: usize) -> bool {
        self.mark[s] != Mark::default()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Hold {
        m: u8,
        i: u8,
        v: u8,
    },
    Say {
        m: u8,
        i: u8,
    },
    Take {
        l: u8,
        v: u8,
    },
    Reconfigure {
        l: u8,
    },
    FastCommit {
        l: u8,
    },
    ClassicCommit {
        l: u8,
        i: u8,
    },
    Replicate {
        l: u8,
        m: u8,
        p: u8,
        k: u8,
    },
    /// `choice` names the value taken at each index the election recovers, in base 4 from the
    /// first index above the candidate's log.
    Elect {
        c: u8,
        q: u8,
        v: u8,
        choice: u8,
    },
    Lose {
        m: u8,
        k: u8,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    TypeOk,
    Agreement,
    Committed,
    LeaderHolds,
    OneLeader,
    LogMatching,
    NoFastByHeld,
    NoFastByHeldAfterChange,
    NoMarkedLeader,
    NoRestamp,
}

pub type Key = [u64; WORDS];

pub struct FastTrack {
    scope: Scope,
    /// The state every history starts from: the specification's `Init`, or a state a script
    /// reached, to search every run from it.
    start: Option<State>,
    /// Every renaming of the members a change keeps, as a map from a server to its new name.
    renamings: Vec<[usize; MAX_SERVERS]>,
    /// Every renaming of the values.
    revaluings: Vec<[u8; 8]>,
}

const PATHS: &[&str] = &[
    "fast commits",
    "fast commits by what members hold by themselves only",
    "classic commits",
    "elections",
    "recoveries",
    "a log cut short of an index a member held",
    "releases",
];
const FAST_COMMIT: u64 = 1;
const FAST_BY_HELD: u64 = 1 << 1;
const CLASSIC_COMMIT: u64 = 1 << 2;
const ELECTED: u64 = 1 << 3;
const RECOVERED: u64 = 1 << 4;
const CUT_SHORT: u64 = 1 << 5;
const RELEASED: u64 = 1 << 6;

fn permutations(items: &[usize]) -> Vec<Vec<usize>> {
    if items.is_empty() {
        return vec![Vec::new()];
    }
    let mut out = Vec::new();
    for (at, first) in items.iter().enumerate() {
        let mut rest = items.to_vec();
        rest.remove(at);
        for mut tail in permutations(&rest) {
            tail.insert(0, *first);
            out.push(tail);
        }
    }
    out
}

impl FastTrack {
    pub fn at(scope: Scope) -> Self {
        let scope = scope.checked();
        let kept: Vec<usize> = members(scope.initial & scope.target, scope.servers).collect();
        let identity: [usize; MAX_SERVERS] = std::array::from_fn(|s| s);
        let mut renamings = vec![identity];
        let mut revaluings = vec![std::array::from_fn(|v| v as u8)];
        if scope.rename_servers {
            renamings = permutations(&kept)
                .into_iter()
                .map(|order| {
                    let mut map = identity;
                    for (from, to) in kept.iter().zip(&order) {
                        map[*from] = *to;
                    }
                    map
                })
                .collect();
        }
        if scope.rename_values {
            let values: Vec<usize> = scope.proposals().map(usize::from).collect();
            revaluings = permutations(&values)
                .into_iter()
                .map(|order| {
                    let mut map: [u8; 8] = std::array::from_fn(|v| v as u8);
                    for (from, to) in values.iter().zip(&order) {
                        map[*from] = *to as u8;
                    }
                    map
                })
                .collect();
        }
        Self {
            scope,
            start: None,
            renamings,
            revaluings,
        }
    }

    /// The model searched from `start` rather than from `Init`.
    pub fn from_state(scope: Scope, start: State) -> Self {
        Self {
            start: Some(start),
            ..Self::at(scope)
        }
    }

    pub fn scope(&self) -> Scope {
        self.scope
    }

    /// `ConfigurationThrough(log[s], upto)`.
    fn configuration_through(state: &State, s: usize, upto: usize) -> u8 {
        let entries = &state.entries(s)[..upto];
        let states = |x: u8| entries.iter().any(|e| e.value == x);
        if states(CHANGE) || states(LEAVE) {
            TARGET_ONLY
        } else if states(ENTER) {
            JOINT_CONF
        } else {
            INITIAL_ONLY
        }
    }
    /// `ConfigurationOf(s)`: the newest its log states.
    fn configuration_of(&self, state: &State, s: usize) -> u8 {
        Self::configuration_through(state, s, usize::from(state.len[s]))
    }
    /// `NewestChangeAt(s)`, zero for none.
    fn newest_change_at(state: &State, s: usize) -> usize {
        state
            .entries(s)
            .iter()
            .rposition(|e| matches!(e.value, CHANGE | ENTER | LEAVE))
            .map_or(0, |at| at + 1)
    }
    /// `Stands(c)`.
    fn stands(&self, state: &State, c: usize) -> bool {
        if has(self.scope.voters(self.configuration_of(state, c)), c) {
            return true;
        }
        let at = Self::newest_change_at(state, c);
        usize::from(state.commit[c]) < at
            && has(
                self.scope
                    .voters(Self::configuration_through(state, c, at - 1)),
                c,
            )
    }
    /// `Pending(s)`.
    fn pending(&self, state: &State, s: usize) -> bool {
        state.entries(s)[usize::from(state.commit[s])..]
            .iter()
            .any(|e| matches!(e.value, CHANGE | ENTER | LEAVE))
    }
    /// `ClassicOf(c, H)`.
    fn classic_of(&self, c: u8, h: u8) -> bool {
        let out = self.scope.conf_out(c);
        majority(self.scope.conf_in(c), h) && (out == 0 || majority(out, h))
    }
    /// `Release(h, length)`: what is held at or below `length` is held no more.
    fn release(held: &mut [u8; MAX_LEN], length: usize) -> bool {
        let mut released = false;
        for slot in held.iter_mut().take(length) {
            released |= *slot != NOTHING;
            *slot = NOTHING;
        }
        released
    }
    /// What a member holds once its log is `length` long and it knows `known` committed by a
    /// classic quorum, by `Releases`.
    fn release_by(&self, held: &mut [u8; MAX_LEN], length: usize, known: usize) -> bool {
        match self.scope.releases {
            Releases::Log => Self::release(held, length),
            Releases::Classic => Self::release(held, known),
        }
    }
    /// `Resolves(k, l)` and `Settled(s, l)`.
    fn settled(state: &State, s: usize, entries: &[Entry]) -> Mark {
        let mark = state.mark[s];
        let last = entries.last().map_or(0, |e| e.term);
        if state.marked(s) && (entries.len() >= usize::from(mark.index) || last > mark.term) {
            Mark::default()
        } else {
            mark
        }
    }

    // ------------------------------------------------------------------ actions

    /// `Hold(m, i, v)`.
    fn hold(&self, state: &State, m: usize, i: usize, v: u8) -> Option<Step<State, Fault>> {
        let c = self.configuration_of(state, m);
        if !has(self.scope.voters(c), m)
            || !self.proposed_in(state.term[m], v)
            || i <= usize::from(state.len[m])
            || state.held[m][i - 1] != NOTHING
        {
            return None;
        }
        let mut next = *state;
        next.held[m][i - 1] = v;
        next.says[m][i - 1] = Said {
            value: v,
            term: state.term[m],
        };
        Some(Step::plain(next))
    }

    /// `Say(m, i)`.
    fn say(&self, state: &State, m: usize, i: usize) -> Option<Step<State, Fault>> {
        let held = state.held[m][i - 1];
        if held == NOTHING || state.says[m][i - 1].term == state.term[m] {
            return None;
        }
        let mut next = *state;
        next.says[m][i - 1] = Said {
            value: held,
            term: state.term[m],
        };
        Some(Step::plain(next))
    }

    /// `Write(l, v)`.
    fn write(&self, state: &State, l: usize, v: u8) -> Step<State, Fault> {
        let mut next = *state;
        let mut entries = state.entries(l).to_vec();
        entries.push(Entry {
            term: state.term[l],
            value: v,
        });
        next.set_log(l, &entries);
        let mut paths = 0;
        if self.scope.releases == Releases::Log && Self::release(&mut next.held[l], entries.len()) {
            paths |= RELEASED;
        }
        Step {
            state: next,
            fault: None,
            paths,
        }
    }

    /// `ProposedIn(t, v)`: the scenario proposes `v` in term `t`.
    fn proposed_in(&self, t: u8, v: u8) -> bool {
        v == NOOP || has(self.scope.proposed[usize::from(t)], usize::from(v - V1))
    }
    /// `Take(l, v)`.
    fn take(&self, state: &State, l: usize, v: u8) -> Option<Step<State, Fault>> {
        if !state.leader[l]
            || !self.proposed_in(state.term[l], v)
            || usize::from(state.len[l]) >= self.scope.max_len
        {
            return None;
        }
        Some(self.write(state, l, v))
    }

    /// `Reconfigure(l)`.
    fn reconfigure(&self, state: &State, l: usize) -> Option<Step<State, Fault>> {
        let c = self.configuration_of(state, l);
        let v = if self.scope.conf_out(c) != 0 {
            LEAVE
        } else if self.scope.joint {
            ENTER
        } else {
            CHANGE
        };
        let older_committed = state
            .entries(l)
            .iter()
            .enumerate()
            .all(|(at, e)| e.term >= state.term[l] || at < usize::from(state.commit[l]));
        if !state.leader[l]
            || self.scope.initial == self.scope.target
            || !has(self.scope.target, l)
            || c == TARGET_ONLY
            || usize::from(state.len[l]) >= self.scope.max_len
            || self.pending(state, l)
            || !older_committed
        {
            return None;
        }
        Some(self.write(state, l, v))
    }

    /// `OfTheRound(l, m)`.
    fn of_the_round(&self, state: &State, l: usize, m: usize) -> bool {
        let a = usize::from(state.acks[m][usize::from(state.term[l])]);
        a >= 1 && a <= usize::from(state.len[l]) && state.at(l, a).term == state.term[l]
    }
    /// `HoldsByItself(l, m, i)`: a leader's own log is of its round.
    fn holds_by_itself(&self, state: &State, l: usize, m: usize, i: usize) -> bool {
        (self.scope.counts == Counts::Any
            || (m == l && !self.scope.legacy)
            || self.of_the_round(state, l, m))
            && state.says[m][i - 1]
                == Said {
                    value: state.at(l, i).value,
                    term: state.term[l],
                }
    }
    /// `HoldsFromLeader(l, m, i)`.
    fn holds_from_leader(state: &State, l: usize, m: usize, i: usize) -> bool {
        m == l || usize::from(state.acks[m][usize::from(state.term[l])]) >= i
    }
    /// `Holds(l, m, i)`: what a fast quorum counts, by `Votes`.
    fn holds(&self, state: &State, l: usize, m: usize, i: usize) -> bool {
        self.holds_by_itself(state, l, m, i)
            || (self.scope.votes == Votes::Logs && Self::holds_from_leader(state, l, m, i))
    }
    /// `FastOfTheTerm(l, H)`.
    fn fast_of_the_term(&self, state: &State, l: usize, h: u8) -> bool {
        let c = self.configuration_of(state, l);
        let under = state.under[l];
        fast_of(self.scope.conf_in(c), h)
            && (self.scope.configs == Configs::Current
                || (self.scope.conf_out(under) == 0 && fast_of(self.scope.conf_in(under), h)))
    }

    /// `FastCommit(l)`.
    fn fast_commit(&self, state: &State, l: usize) -> Option<Step<State, Fault>> {
        let i = usize::from(state.commit[l]) + 1;
        let c = self.configuration_of(state, l);
        if !state.leader[l]
            || i > usize::from(state.len[l])
            || state.at(l, i).term != state.term[l]
            || self.pending(state, l)
            || self.scope.conf_out(c) != 0
        {
            return None;
        }
        let holders = members(self.scope.conf_in(c), self.scope.servers)
            .filter(|m| self.holds(state, l, *m, i))
            .fold(0u8, |mask, m| mask | bit(m));
        if !self.fast_of_the_term(state, l, holders) {
            return None;
        }
        let mut next = *state;
        next.commit[l] = u8::try_from(i).unwrap();
        if next.chosen[i - 1] == Said::default() {
            next.chosen[i - 1] = Said {
                value: state.at(l, i).value,
                term: state.term[l],
            };
        }
        let mut paths = FAST_COMMIT;
        let by_held = members(self.scope.conf_in(c), self.scope.servers)
            .filter(|m| self.holds_by_itself(state, l, *m, i))
            .fold(0u8, |mask, m| mask | bit(m));
        if self.fast_of_the_term(state, l, by_held) {
            paths |= FAST_BY_HELD;
        }
        Some(Step {
            state: next,
            fault: None,
            paths,
        })
    }

    /// `ClassicCommit(l, i)`: a classic quorum holds the leader's entry of its term at `i`; the
    /// leader knows `i` committed by it, and commits it if it had not.
    fn classic_commit(&self, state: &State, l: usize, i: usize) -> Option<Step<State, Fault>> {
        let c = self.configuration_of(state, l);
        let known = if self.scope.legacy {
            state.commit[l]
        } else {
            state.classic[l]
        };
        if !state.leader[l]
            || i <= usize::from(known)
            || i > usize::from(state.len[l])
            || state.at(l, i).term != state.term[l]
        {
            return None;
        }
        let holders = members(self.scope.voters(c), self.scope.servers)
            .filter(|m| Self::holds_from_leader(state, l, *m, i))
            .fold(0u8, |mask, m| mask | bit(m));
        if !self.classic_of(c, holders) {
            return None;
        }
        let mut next = *state;
        let commit = usize::from(state.commit[l]);
        for j in commit + 1..=i {
            if next.chosen[j - 1] == Said::default() {
                next.chosen[j - 1] = Said {
                    value: state.at(l, j).value,
                    term: state.term[l],
                };
            }
        }
        next.commit[l] = u8::try_from(commit.max(i)).unwrap();
        if !self.scope.legacy {
            next.classic[l] = u8::try_from(i).unwrap();
        }
        let mut paths = CLASSIC_COMMIT;
        if self.scope.releases == Releases::Classic && Self::release(&mut next.held[l], i) {
            paths |= RELEASED;
        }
        Some(Step {
            state: next,
            fault: None,
            paths,
        })
    }

    /// `Replicate(l, m, p, k)`: a member of the leader's configuration takes from it what follows
    /// the point `p`, through `k`.
    fn replicate(
        &self,
        state: &State,
        l: usize,
        m: usize,
        p: usize,
        k: usize,
    ) -> Option<Step<State, Fault>> {
        let lc = self.configuration_of(state, l);
        let len_l = usize::from(state.len[l]);
        let len_m = usize::from(state.len[m]);
        let commit_m = usize::from(state.commit[m]);
        let point = p == 0
            || p <= commit_m
            || (p >= 1 && p <= len_m && state.at(m, p).term == state.at(l, p).term);
        if l == m
            || (self.scope.legacy && k != len_l)
            || !state.leader[l]
            || !has(self.scope.voters(lc), m)
            || state.term[m] > state.term[l]
            || p > k
            || k > len_l
            || !point
        {
            return None;
        }
        let from = p.max(commit_m) + 1;
        let differs =
            (from..=k).find(|i| *i > len_m || state.at(m, *i).term != state.at(l, *i).term);
        let taken: Vec<Entry> = match differs {
            None => state.entries(m).to_vec(),
            Some(c) => state.entries(m)[..c - 1]
                .iter()
                .chain(&state.entries(l)[c - 1..k])
                .copied()
                .collect(),
        };
        let mut next = *state;
        let mut paths = 0;
        if taken.len() < len_m
            && (taken.len()..len_m)
                .any(|at| state.held[m][at] != NOTHING || state.says[m][at] != Said::default())
        {
            paths |= CUT_SHORT;
        }
        next.set_log(m, &taken);
        let classic = usize::from(state.classic[m]).max(usize::from(state.classic[l]).min(k));
        next.classic[m] = u8::try_from(classic).unwrap();
        if self.release_by(&mut next.held[m], taken.len(), classic) {
            paths |= RELEASED;
        }
        next.commit[m] = u8::try_from(commit_m.max(usize::from(state.commit[l]).min(k))).unwrap();
        next.mark[m] = Self::settled(state, m, &taken);
        next.term[m] = state.term[l];
        if state.term[m] != state.term[l] {
            next.vote[m] = NOBODY;
        }
        next.leader[m] = false;
        next.under[m] = NO_CONFIGURATION;
        let t = usize::from(state.term[l]);
        next.acks[m][t] = next.acks[m][t].max(u8::try_from(k).unwrap());
        Some(Step {
            state: next,
            fault: None,
            paths,
        })
    }

    /// `Lose(m, k)`.
    fn lose(&self, state: &State, m: usize, k: usize) -> Option<Step<State, Fault>> {
        if !has(self.scope.losers, m) || k >= usize::from(state.len[m]) {
            return None;
        }
        let old = Mark {
            index: state.len[m],
            term: state.last_term(m),
        };
        let merged = if state.marked(m) {
            Mark {
                index: state.mark[m].index.max(old.index),
                term: state.mark[m].term.max(old.term),
            }
        } else {
            old
        };
        let mut next = *state;
        let entries = state.entries(m)[..k].to_vec();
        next.set_log(m, &entries);
        let k8 = u8::try_from(k).unwrap();
        next.commit[m] = state.commit[m].min(k8);
        next.classic[m] = state.classic[m].min(k8);
        next.mark[m] = merged;
        next.leader[m] = false;
        next.under[m] = NO_CONFIGURATION;
        Some(Step::plain(next))
    }

    /// `Claim(m)`.
    fn claim(&self, state: &State, m: usize) -> Mark {
        if state.marked(m) && self.scope.marks != Marks::Whole {
            state.mark[m]
        } else {
            Mark {
                index: state.len[m],
                term: state.last_term(m),
            }
        }
    }
    /// `Current(c, m)`.
    fn current(&self, state: &State, c: usize, m: usize) -> bool {
        let claim = self.claim(state, m);
        let last = state.last_term(c);
        last > claim.term || (last == claim.term && state.len[c] >= claim.index)
    }
    /// `Campaigns(c)`.
    fn campaigns(&self, state: &State, c: usize) -> bool {
        state.term[c] < self.scope.max_term && self.stands(state, c)
    }
    /// `MayLead(c)`: the scenario lets `c` be elected in the next term.
    fn may_lead(&self, state: &State, c: usize) -> bool {
        let lead = self.scope.leads[usize::from(state.term[c]) + 1];
        lead == ANY || usize::from(lead) == c
    }
    /// `Asked(c, Q)`.
    fn asked(&self, state: &State, c: usize, q: u8) -> bool {
        !has(q, c)
            && members(q, self.scope.servers).all(|m| {
                (state.term[m] < state.term[c] + 1
                    || (state.term[m] == state.term[c] + 1 && state.vote[m] == NOBODY))
                    && self.current(state, c, m)
            })
    }
    /// `Own(c)`.
    fn own(&self, state: &State, c: usize) -> u8 {
        if state.marked(c) && self.scope.marks != Marks::SelfVote {
            0
        } else {
            bit(c)
        }
    }
    /// `Quorums(c, Q)`, the empty set first.
    fn quorums(&self, state: &State, c: usize, q: u8) -> Vec<u8> {
        let counted = self.configuration_of(state, c);
        let own = self.own(state, c);
        let pool = (q | own) & self.scope.voters(counted);
        let mut out = vec![0];
        for v in 1..=self.scope.all() {
            let own_counted = own & self.scope.voters(counted);
            if v & !pool == 0 && own_counted & !v == 0 && self.classic_of(counted, v) {
                out.push(v);
            }
        }
        out
    }
    /// `Report(m, i)`.
    fn report(&self, state: &State, m: usize, i: usize) -> u8 {
        if self.scope.reports == Reports::Acknowledged
            && i <= usize::from(state.len[m])
            && i > usize::from(state.classic[m])
        {
            state.at(m, i).value
        } else {
            state.held[m][i - 1]
        }
    }
    /// `Count(V, i, v)`.
    fn count(&self, state: &State, v_mask: u8, i: usize, v: u8) -> usize {
        members(v_mask, self.scope.servers)
            .filter(|m| self.report(state, *m, i) == v)
            .count()
    }
    /// `Recovered(V, i, v)`, with the configuration the candidate counts by.
    fn recovered(&self, state: &State, counted: u8, v_mask: u8, i: usize, v: u8) -> bool {
        let counts: Vec<(u8, usize)> = self
            .scope
            .proposals()
            .map(|w| (w, self.count(state, v_mask, i, w)))
            .collect();
        if counts.iter().all(|(_, n)| *n == 0) {
            return v == NOOP;
        }
        let of = |x: u8| counts.iter().find(|(w, _)| *w == x).map_or(0, |(_, n)| *n);
        match self.scope.rule {
            Rule::Most => of(v) > 0 && counts.iter().all(|(_, n)| *n <= of(v)),
            Rule::Least => of(v) > 0 && counts.iter().all(|(_, n)| *n == 0 || of(v) <= *n),
            Rule::Ballot => {
                let reports: Vec<usize> = members(v_mask, self.scope.servers)
                    .filter(|m| state.held[*m][i - 1] != NOTHING)
                    .collect();
                let top = reports
                    .iter()
                    .map(|m| state.says[*m][i - 1].term)
                    .max()
                    .unwrap_or(0);
                let voters = self.scope.conf_in(counted);
                let n = card(voters);
                let fast = (1..=n).find(|f| 4 * f >= 3 * n).unwrap_or(n);
                let threshold = (card(v_mask & voters) + fast).saturating_sub(n);
                let at_top = |w: u8| {
                    reports
                        .iter()
                        .filter(|m| {
                            state.says[**m][i - 1].term == top && state.held[**m][i - 1] == w
                        })
                        .count()
                };
                let forced = self.scope.proposals().find(|w| at_top(*w) >= threshold);
                match forced {
                    Some(w) => v == w,
                    None => v == NOOP,
                }
            }
        }
    }

    /// `Elect(c, Q, V)` with the recovered values `choice` names.
    fn elect(
        &self,
        state: &State,
        c: usize,
        q: u8,
        v_mask: u8,
        choice: u8,
    ) -> Option<Step<State, Fault>> {
        if !self.campaigns(state, c)
            || !self.may_lead(state, c)
            || !self.asked(state, c, q)
            || !self.quorums(state, c, q).contains(&v_mask)
        {
            return None;
        }
        let t = state.term[c] + 1;
        let voted = q | bit(c);
        let counted = self.configuration_of(state, c);
        let mut next = *state;
        for m in members(voted, self.scope.servers) {
            next.term[m] = t;
            next.vote[m] = u8::try_from(c + 1).unwrap();
            for i in self.scope.indexes() {
                if state.held[m][i - 1] != NOTHING {
                    next.says[m][i - 1] = Said {
                        value: state.held[m][i - 1],
                        term: t,
                    };
                }
            }
        }
        if v_mask == 0 {
            if choice != 0 {
                return None;
            }
            for m in members(voted, self.scope.servers) {
                next.leader[m] = false;
                next.under[m] = NO_CONFIGURATION;
            }
            return Some(Step::plain(next));
        }
        let length = usize::from(state.len[c]);
        let top = self
            .scope
            .indexes()
            .filter(|i| {
                *i > length
                    && self
                        .scope
                        .proposals()
                        .any(|v| self.count(state, v_mask, *i, v) > 0)
            })
            .max()
            .unwrap_or(length);
        let mut entries = state.entries(c).to_vec();
        let mut rest = choice;
        let stated: Vec<u8> = self.scope.stated().collect();
        for i in length + 1..=top {
            let value = *stated.get(usize::from(rest % 4))?;
            rest /= 4;
            if !self.recovered(state, counted, v_mask, i, value) {
                return None;
            }
            entries.push(Entry { term: t, value });
        }
        if rest != 0 {
            return None;
        }
        next.set_log(c, &entries);
        let mut paths = ELECTED;
        if top > length {
            paths |= RECOVERED;
        }
        if self.scope.releases == Releases::Log && Self::release(&mut next.held[c], top) {
            paths |= RELEASED;
        }
        for m in members(voted, self.scope.servers) {
            next.leader[m] = m == c;
            next.under[m] = if m == c { counted } else { NO_CONFIGURATION };
        }
        next.mark[c] = Mark::default();
        next.marked_led = state.marked_led || state.marked(c);
        Some(Step {
            state: next,
            fault: None,
            paths,
        })
    }

    // --------------------------------------------------------------- invariants

    /// Two logs hold an entry of one term at one index and, below it, entries of one value under
    /// different terms: a member kept a committed entry's stamp where a later leader's election
    /// took it again under its own (`docs/models/FastTrack.tla`, `Restamped`).
    fn restamped(state: &State, servers: usize) -> bool {
        (0..servers).any(|m| {
            (0..servers).any(|o| {
                let both = usize::from(state.len[m].min(state.len[o]));
                (1..=both).any(|i| {
                    state.at(m, i).term == state.at(o, i).term
                        && (1..i).any(|j| state.at(m, j).term != state.at(o, j).term)
                })
            })
        })
    }

    fn fast_by_held(&self, state: &State, after_change: bool) -> bool {
        (0..self.scope.servers).any(|l| {
            let c = self.configuration_of(state, l);
            state.leader[l]
                && self.scope.indexes().any(|i| {
                    usize::from(state.commit[l]) >= i
                        && state.chosen[i - 1].term == state.term[l]
                        && !self.classic_of(
                            c,
                            members(self.scope.voters(c), self.scope.servers)
                                .filter(|m| Self::holds_from_leader(state, l, *m, i))
                                .fold(0, |mask, m| mask | bit(m)),
                        )
                        && (!after_change
                            || (state.under[l] != c
                                && (1..i).any(|j| {
                                    matches!(state.at(l, j).value, CHANGE | ENTER | LEAVE)
                                        && state.at(l, j).term == state.term[l]
                                })))
                })
        })
    }

    fn violation(&self, state: &State) -> Option<Fault> {
        let n = self.scope.servers;
        let servers = || 0..n;
        if self.scope.releases == Releases::Log
            && servers()
                .any(|s| (0..usize::from(state.len[s])).any(|at| state.held[s][at] != NOTHING))
        {
            return Some(Fault::TypeOk);
        }
        if servers().any(|s| state.classic[s] > state.commit[s]) {
            return Some(Fault::TypeOk);
        }
        for m in servers() {
            for o in servers() {
                let both = usize::from(state.commit[m].min(state.commit[o]));
                if (1..=both).any(|i| state.at(m, i).value != state.at(o, i).value) {
                    return Some(Fault::Agreement);
                }
            }
        }
        for m in servers() {
            for i in 1..=usize::from(state.commit[m]) {
                let chosen = state.chosen[i - 1];
                if chosen == Said::default() || state.at(m, i).value != chosen.value {
                    return Some(Fault::Committed);
                }
            }
        }
        for l in servers().filter(|l| state.leader[*l]) {
            for i in self.scope.indexes() {
                let chosen = state.chosen[i - 1];
                if chosen != Said::default()
                    && chosen.term < state.term[l]
                    && (i > usize::from(state.len[l]) || state.at(l, i).value != chosen.value)
                {
                    return Some(Fault::LeaderHolds);
                }
            }
        }
        for l in servers() {
            for m in servers() {
                if l != m && state.leader[l] && state.leader[m] && state.term[l] == state.term[m] {
                    return Some(Fault::OneLeader);
                }
            }
        }
        for m in servers() {
            for o in servers() {
                let both = usize::from(state.len[m].min(state.len[o]));
                for i in 1..=both {
                    if state.at(m, i).term != state.at(o, i).term {
                        continue;
                    }
                    let kept = |j: usize| {
                        j <= usize::from(state.commit[m]) || j <= usize::from(state.commit[o])
                    };
                    if (1..=i).any(|j| {
                        state.at(m, j).value != state.at(o, j).value
                            || (!kept(j) && state.at(m, j).term != state.at(o, j).term)
                    }) {
                        return Some(Fault::LogMatching);
                    }
                }
            }
        }
        match self.scope.reach {
            Reach::NoFastByHeld if self.fast_by_held(state, false) => Some(Fault::NoFastByHeld),
            Reach::NoFastByHeldAfterChange if self.fast_by_held(state, true) => {
                Some(Fault::NoFastByHeldAfterChange)
            }
            Reach::NoMarkedLeader if state.marked_led => Some(Fault::NoMarkedLeader),
            Reach::NoRestamp if Self::restamped(state, self.scope.servers) => {
                Some(Fault::NoRestamp)
            }
            _ => None,
        }
    }

    // ------------------------------------------------------------------ packing

    /// `state` with what no step and no invariant can read any more set to its initial value,
    /// for a scope whose voters never change (one leader a term, so a term without a leader now
    /// never has one again):
    /// - what members said they hold as of a term that has no leader now, at an index they hold
    ///   nothing at: only a leader of that term reads it (`HoldsByItself`), and `Say` and the
    ///   ballot rule read only what is said of what is held;
    /// - what members acknowledged to a leader of a term that has no leader now: only that
    ///   leader reads it;
    /// - for whom a member voted: only whether it voted is read (`Asked`).
    fn reduced(&self, state: &State) -> State {
        let n = self.scope.servers;
        let led = |t: u8| (0..n).any(|l| state.leader[l] && state.term[l] == t);
        let mut out = *state;
        for m in 0..n {
            for at in 0..self.scope.max_len {
                if state.held[m][at] == NOTHING && !led(state.says[m][at].term) {
                    out.says[m][at] = Said::default();
                }
            }
            for t in 0..TERMS {
                if !led(u8::try_from(t).unwrap()) {
                    out.acks[m][t] = 0;
                }
            }
            if out.vote[m] != NOBODY {
                out.vote[m] = 1;
            }
        }
        out
    }

    fn renamed(&self, state: &State, map: &[usize; MAX_SERVERS], values: &[u8; 8]) -> State {
        let n = self.scope.servers;
        let value = |v: u8| values[usize::from(v)];
        let mut out = *state;
        for s in 0..n {
            let to = map[s];
            out.term[to] = state.term[s];
            out.vote[to] = match state.vote[s] {
                NOBODY => NOBODY,
                // Reduced, a vote says only that there was one.
                voted if self.scope.reduce => voted,
                voted => u8::try_from(map[usize::from(voted - 1)] + 1).unwrap(),
            };
            out.leader[to] = state.leader[s];
            out.len[to] = state.len[s];
            for at in 0..MAX_LEN {
                out.log[to][at] = Entry {
                    term: state.log[s][at].term,
                    value: value(state.log[s][at].value),
                };
                out.held[to][at] = value(state.held[s][at]);
                out.says[to][at] = Said {
                    value: value(state.says[s][at].value),
                    term: state.says[s][at].term,
                };
            }
            out.commit[to] = state.commit[s];
            out.acks[to] = state.acks[s];
            out.under[to] = state.under[s];
            out.mark[to] = state.mark[s];
            out.classic[to] = state.classic[s];
        }
        for at in 0..MAX_LEN {
            out.chosen[at] = Said {
                value: value(state.chosen[at].value),
                term: state.chosen[at].term,
            };
        }
        out
    }

    fn pack(&self, state: &State) -> Key {
        let mut p = Packer::<WORDS>::new();
        let n = self.scope.servers;
        let l = self.scope.max_len;
        let mut put = |width: usize, value: u8| p.put(width, u64::from(value)).unwrap();
        for s in 0..n {
            put(3, state.term[s]);
            put(3, state.vote[s]);
            put(1, u8::from(state.leader[s]));
            put(2, state.len[s]);
            for at in 0..l {
                put(3, state.log[s][at].term);
                put(3, state.log[s][at].value);
                put(2, state.held[s][at]);
                put(3, state.says[s][at].term);
                put(2, state.says[s][at].value);
            }
            put(2, state.commit[s]);
            for t in 0..=usize::from(self.scope.max_term) {
                put(2, state.acks[s][t]);
            }
            put(2, state.under[s]);
            put(2, state.mark[s].index);
            put(3, state.mark[s].term);
            put(2, state.classic[s]);
        }
        for at in 0..l {
            put(3, state.chosen[at].value);
            put(3, state.chosen[at].term);
        }
        put(1, u8::from(state.marked_led));
        p.words()
    }

    fn unpacked(&self, key: &Key) -> State {
        let mut p = Packer::<WORDS>::over(*key);
        let n = self.scope.servers;
        let l = self.scope.max_len;
        let mut take = |width: usize| p.small(width).unwrap();
        let mut state = self.initial();
        for s in 0..n {
            state.term[s] = take(3);
            state.vote[s] = take(3);
            state.leader[s] = take(1) == 1;
            state.len[s] = take(2);
            for at in 0..l {
                state.log[s][at].term = take(3);
                state.log[s][at].value = take(3);
                state.held[s][at] = take(2);
                state.says[s][at].term = take(3);
                state.says[s][at].value = take(2);
            }
            state.commit[s] = take(2);
            for t in 0..=usize::from(self.scope.max_term) {
                state.acks[s][t] = take(2);
            }
            state.under[s] = take(2);
            state.mark[s].index = take(2);
            state.mark[s].term = take(3);
            state.classic[s] = take(2);
        }
        for at in 0..l {
            state.chosen[at].value = take(3);
            state.chosen[at].term = take(3);
        }
        state.marked_led = take(1) == 1;
        state
    }
}

impl fmt::Debug for FastTrack {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "FastTrack({:?})", self.scope)
    }
}

impl Model for FastTrack {
    type State = State;
    type Action = Action;
    type Fault = Fault;
    type Key = Key;

    fn paths(&self) -> &'static [&'static str] {
        PATHS
    }

    fn initial(&self) -> State {
        if let Some(start) = self.start {
            return start;
        }
        State {
            term: [0; MAX_SERVERS],
            vote: [NOBODY; MAX_SERVERS],
            leader: [false; MAX_SERVERS],
            len: [0; MAX_SERVERS],
            log: [[Entry::default(); MAX_LEN]; MAX_SERVERS],
            held: [[NOTHING; MAX_LEN]; MAX_SERVERS],
            commit: [0; MAX_SERVERS],
            says: [[Said::default(); MAX_LEN]; MAX_SERVERS],
            acks: [[0; TERMS]; MAX_SERVERS],
            under: [NO_CONFIGURATION; MAX_SERVERS],
            chosen: [Said::default(); MAX_LEN],
            mark: [Mark::default(); MAX_SERVERS],
            marked_led: false,
            classic: [0; MAX_SERVERS],
        }
    }

    fn actions(&self, state: &State, out: &mut Vec<Action>) {
        let scope = self.scope;
        let n = scope.servers;
        let small = |x: usize| u8::try_from(x).unwrap();
        for m in 0..n {
            for i in scope.indexes().filter(|i| has(scope.held_at, *i)) {
                for v in scope.proposals() {
                    out.push(Action::Hold {
                        m: small(m),
                        i: small(i),
                        v,
                    });
                }
                out.push(Action::Say {
                    m: small(m),
                    i: small(i),
                });
            }
        }
        for l in (0..n).filter(|l| state.leader[*l]) {
            for v in scope.stated() {
                out.push(Action::Take { l: small(l), v });
            }
            out.push(Action::Reconfigure { l: small(l) });
            out.push(Action::FastCommit { l: small(l) });
            for i in scope.indexes() {
                out.push(Action::ClassicCommit {
                    l: small(l),
                    i: small(i),
                });
            }
            let len = usize::from(state.len[l]);
            for m in (0..n).filter(|m| *m != l) {
                for p in 0..=len {
                    for k in p..=len {
                        out.push(Action::Replicate {
                            l: small(l),
                            m: small(m),
                            p: small(p),
                            k: small(k),
                        });
                    }
                }
            }
        }
        for c in (0..n).filter(|c| self.campaigns(state, *c) && self.may_lead(state, *c)) {
            let others = scope.all() & !bit(c);
            let mut q = 0u8;
            loop {
                if self.asked(state, c, q) {
                    for v in self.quorums(state, c, q) {
                        let length = usize::from(state.len[c]);
                        let recovering = if v == 0 {
                            0
                        } else {
                            scope
                                .indexes()
                                .filter(|i| {
                                    *i > length
                                        && scope
                                            .proposals()
                                            .any(|w| self.count(state, v, *i, w) > 0)
                                })
                                .max()
                                .map_or(0, |top| top - length)
                        };
                        for choice in 0..4u8.pow(u32::try_from(recovering).unwrap()) {
                            out.push(Action::Elect {
                                c: small(c),
                                q,
                                v,
                                choice,
                            });
                        }
                    }
                }
                // The next subset of the others.
                q = (q.wrapping_sub(others)) & others;
                if q == 0 {
                    break;
                }
            }
        }
        for m in (0..n).filter(|m| has(scope.losers, *m)) {
            for k in 0..=scope.max_len {
                out.push(Action::Lose {
                    m: small(m),
                    k: small(k),
                });
            }
        }
    }

    fn apply(&self, state: &State, action: Action) -> Option<Step<State, Fault>> {
        let u = usize::from;
        let mut step = match action {
            Action::Hold { m, i, v } => self.hold(state, u(m), u(i), v),
            Action::Say { m, i } => self.say(state, u(m), u(i)),
            Action::Take { l, v } => self.take(state, u(l), v),
            Action::Reconfigure { l } => self.reconfigure(state, u(l)),
            Action::FastCommit { l } => self.fast_commit(state, u(l)),
            Action::ClassicCommit { l, i } => self.classic_commit(state, u(l), u(i)),
            Action::Replicate { l, m, p, k } => self.replicate(state, u(l), u(m), u(p), u(k)),
            Action::Elect { c, q, v, choice } => self.elect(state, u(c), q, v, choice),
            Action::Lose { m, k } => self.lose(state, u(m), u(k)),
        }?;
        if step.state == *state {
            return None;
        }
        step.fault = self.violation(&step.state);
        Some(step)
    }

    fn canonical(&self, state: &State) -> Key {
        let reduced;
        let state = if self.scope.reduce {
            reduced = self.reduced(state);
            &reduced
        } else {
            state
        };
        let mut least: Option<Key> = None;
        for map in &self.renamings {
            for values in &self.revaluings {
                let key = self.pack(&self.renamed(state, map, values));
                if least.is_none_or(|least| key < least) {
                    least = Some(key);
                }
            }
        }
        least.unwrap()
    }

    fn unpack(&self, key: &Key) -> State {
        self.unpacked(key)
    }
}
