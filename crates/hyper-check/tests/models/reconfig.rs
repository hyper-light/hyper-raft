//! `docs/models/Reconfig.tla` as a [`Model`] of `hyper_check::explore`: the same variables, actions
//! and invariants, with no symmetry and no reduction, so that a search's class count is the number
//! of distinct states TLC finds for the same configuration (`docs/models/Reconfig*.cfg` state the
//! counts this search reaches, and CI's model job holds TLC to them).
//!
//! Each action is the specification's of the same name. A member is its place, 0 to 3 for `A` to
//! `D`; an index is one-based where the specification's is.

use hyper_check::explore::{Model, Packer, Step};

pub const MAX_SERVERS: usize = 4;
pub const MAX_LEN: usize = 3;
/// A term packs in two bits: `0..=MAX_TERM`.
pub const MAX_TERM: u8 = 3;
const TERMS: usize = MAX_TERM as usize + 1;
const WORDS: usize = 4;

/// A leader's own first entry.
pub const NOOP: u8 = 0;
/// The first proposed value; `V1 + k` the `k`-th after it.
pub const V1: u8 = 1;
/// The entry naming configuration `k` of the chain (one-based, from 2) is `CONF + k`.
pub const CONF: u8 = 4;
const NOBODY: u8 = 0;

/// `Elections`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Elections {
    /// The newest configuration in the candidate's log (the core's).
    Newest,
    /// The configuration of its committed log (before).
    Applied,
}

/// `Stand`: who campaigns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stand {
    /// A voter of the configuration it counts by.
    Voter,
    /// That, or, while the newest configuration entry in its log is past its commit, a voter of
    /// the configuration before it: the group may still need it (Ongaro's thesis §4.2.2). Its own
    /// vote counts only where it is a voter. Meaningful where elections count by the newest.
    Needed,
}

/// `Scenario`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scenario {
    Promote,
    Joint,
    Single,
    SingleJoint,
}

/// A configuration: voters, the voters it leaves, learners; a bit a member.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Conf {
    pub voters: u8,
    pub outgoing: u8,
    pub learners: u8,
}

const fn conf(voters: u8, outgoing: u8, learners: u8) -> Conf {
    Conf {
        voters,
        outgoing,
        learners,
    }
}

const A: u8 = 1;
const B: u8 = 2;
const C: u8 = 4;
const D: u8 = 8;

const PROMOTE: [Conf; 3] = [
    conf(A | B | C, 0, D),
    conf(A | B | C | D, 0, 0),
    conf(B | C | D, 0, A),
];
const JOINT: [Conf; 3] = [
    conf(A | B | C, 0, 0),
    conf(B | C | D, A | B | C, 0),
    conf(B | C | D, 0, 0),
];
const SINGLE: [Conf; 2] = [conf(A, 0, B), conf(A | B, 0, 0)];
const SINGLE_JOINT: [Conf; 3] = [conf(A, 0, 0), conf(A | B, A, 0), conf(A | B, 0, 0)];

impl Scenario {
    pub fn chain(self) -> &'static [Conf] {
        match self {
            Self::Promote => &PROMOTE,
            Self::Joint => &JOINT,
            Self::Single => &SINGLE,
            Self::SingleJoint => &SINGLE_JOINT,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Scope {
    pub servers: usize,
    pub values: u8,
    pub max_term: u8,
    pub max_len: usize,
    pub elections: Elections,
    /// `Commits`: what a leader counts commitment by, as `Elections` names it.
    pub commits: Elections,
    /// `Stand`.
    pub stand: Stand,
    pub scenario: Scenario,
    /// Whether `NoElectedOnPending` is checked (the scope must reach it).
    pub reached: bool,
    /// Whether `NoElectedUnnamed` is checked (the scope must reach it).
    pub stood: bool,
    /// Whether a state is counted by what any step can still read of it (`reduced`); with
    /// neither this nor `symmetry`, a count is the number of distinct states TLC finds.
    pub reduce: bool,
    /// Whether a state is counted by orbit under swapping B and C, whom every configuration of
    /// the joint and promote chains treats alike. A history found with it may name B for C; one
    /// found without it replays on the states it names.
    pub symmetry: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct Entry {
    pub term: u8,
    pub value: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct State {
    pub term: [u8; MAX_SERVERS],
    pub vote: [u8; MAX_SERVERS],
    pub leader: [bool; MAX_SERVERS],
    pub len: [u8; MAX_SERVERS],
    pub log: [[Entry; MAX_LEN]; MAX_SERVERS],
    pub commit: [u8; MAX_SERVERS],
    pub acks: [[u8; TERMS]; MAX_SERVERS],
    /// Value and term; `(NOOP, 0)` for not chosen.
    pub chosen: [Entry; MAX_LEN],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Take {
        l: u8,
        v: u8,
    },
    Reconfigure {
        l: u8,
    },
    Commit {
        l: u8,
        i: u8,
    },
    Replicate {
        l: u8,
        m: u8,
        p: u8,
        k: u8,
        learns: bool,
    },
    Elect {
        c: u8,
        q: u8,
        v: u8,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    TypeOk,
    OneLeader,
    Agreement,
    Committed,
    LeaderHolds,
    LogMatching,
    NoElectedOnPending,
    NoElectedUnnamed,
}

pub type Key = [u64; WORDS];

pub struct Reconfig {
    scope: Scope,
}

const PATHS: &[&str] = &[
    "elections",
    "elections by a configuration past the candidate's commit",
    "commits",
    "configuration entries written",
    "configuration entries overwritten",
    "elections of a candidate its newest configuration names no voter",
];
const ELECTED: u64 = 1;
const ON_PENDING: u64 = 1 << 1;
const COMMITTED: u64 = 1 << 2;
const RECONFIGURED: u64 = 1 << 3;
const OVERWRITTEN: u64 = 1 << 4;
const STOOD: u64 = 1 << 5;

fn bit(s: usize) -> u8 {
    1 << s
}
fn has(mask: u8, s: usize) -> bool {
    mask & bit(s) != 0
}
fn majority(set: u8, h: u8) -> bool {
    2 * (h & set).count_ones() > set.count_ones()
}

impl Reconfig {
    pub fn at(scope: Scope) -> Self {
        assert!(scope.servers <= MAX_SERVERS && scope.max_len <= MAX_LEN);
        assert!(scope.max_term <= MAX_TERM && scope.values <= CONF - V1);
        Self { scope }
    }

    fn chain(&self) -> &'static [Conf] {
        self.scope.scenario.chain()
    }
    fn is_conf(value: u8) -> bool {
        value > CONF
    }
    /// `ConfIndex(l, upto)`, one-based into the chain.
    fn conf_index(state: &State, s: usize, upto: usize) -> usize {
        (0..upto)
            .rev()
            .find(|at| Self::is_conf(state.log[s][*at].value))
            .map_or(1, |at| usize::from(state.log[s][at].value - CONF))
    }
    fn conf_at(&self, k: usize) -> Conf {
        self.chain()[k - 1]
    }
    fn applied(&self, state: &State, s: usize) -> Conf {
        self.conf_at(Self::conf_index(state, s, usize::from(state.commit[s])))
    }
    fn newest(&self, state: &State, s: usize) -> Conf {
        self.conf_at(Self::conf_index(state, s, usize::from(state.len[s])))
    }
    fn election_conf(&self, state: &State, s: usize) -> Conf {
        match self.scope.elections {
            Elections::Newest => self.newest(state, s),
            Elections::Applied => self.applied(state, s),
        }
    }
    fn commit_conf(&self, state: &State, s: usize) -> Conf {
        match self.scope.commits {
            Elections::Newest => self.newest(state, s),
            Elections::Applied => self.applied(state, s),
        }
    }
    fn pending(state: &State, s: usize) -> bool {
        (usize::from(state.commit[s])..usize::from(state.len[s]))
            .any(|at| Self::is_conf(state.log[s][at].value))
    }
    fn voters(c: Conf) -> u8 {
        c.voters | c.outgoing
    }
    fn members(c: Conf) -> u8 {
        Self::voters(c) | c.learners
    }
    fn quorum_of(c: Conf, h: u8) -> bool {
        majority(c.voters, h) && (c.outgoing == 0 || majority(c.outgoing, h))
    }
    fn last_term(state: &State, s: usize) -> u8 {
        let len = usize::from(state.len[s]);
        if len == 0 {
            0
        } else {
            state.log[s][len - 1].term
        }
    }
    fn entries(state: &State, s: usize) -> &[Entry] {
        &state.log[s][..usize::from(state.len[s])]
    }
    fn set_log(state: &mut State, s: usize, entries: &[Entry]) {
        state.log[s] = [Entry::default(); MAX_LEN];
        state.log[s][..entries.len()].copy_from_slice(entries);
        state.len[s] = u8::try_from(entries.len()).unwrap();
    }

    fn write(&self, state: &State, l: usize, v: u8) -> Option<State> {
        let len = usize::from(state.len[l]);
        if len >= self.scope.max_len {
            return None;
        }
        let mut next = *state;
        next.log[l][len] = Entry {
            term: state.term[l],
            value: v,
        };
        next.len[l] += 1;
        Some(next)
    }

    fn take(&self, state: &State, l: usize, v: u8) -> Option<Step<State, Fault>> {
        if !state.leader[l] {
            return None;
        }
        self.write(state, l, v).map(Step::plain)
    }

    fn reconfigure(&self, state: &State, l: usize) -> Option<Step<State, Fault>> {
        let k = Self::conf_index(state, l, usize::from(state.commit[l]));
        let older_committed = Self::entries(state, l)
            .iter()
            .enumerate()
            .all(|(at, e)| e.term >= state.term[l] || at < usize::from(state.commit[l]));
        if !state.leader[l]
            || k >= self.chain().len()
            || Self::pending(state, l)
            || !older_committed
        {
            return None;
        }
        let next = self.write(state, l, CONF + u8::try_from(k + 1).unwrap())?;
        Some(Step {
            state: next,
            fault: None,
            paths: RECONFIGURED,
        })
    }

    fn commit(&self, state: &State, l: usize, i: usize) -> Option<Step<State, Fault>> {
        let c = self.commit_conf(state, l);
        if !state.leader[l]
            || i <= usize::from(state.commit[l])
            || i > usize::from(state.len[l])
            || state.log[l][i - 1].term != state.term[l]
        {
            return None;
        }
        let t = usize::from(state.term[l]);
        let holders = (0..self.scope.servers)
            .filter(|m| has(Self::voters(c), *m))
            .filter(|m| *m == l || usize::from(state.acks[*m][t]) >= i)
            .fold(0u8, |mask, m| mask | bit(m));
        if !Self::quorum_of(c, holders) {
            return None;
        }
        let mut next = *state;
        for j in usize::from(state.commit[l]) + 1..=i {
            if next.chosen[j - 1] == Entry::default() {
                next.chosen[j - 1] = Entry {
                    term: state.term[l],
                    value: state.log[l][j - 1].value,
                };
            }
        }
        next.commit[l] = u8::try_from(i).unwrap();
        Some(Step {
            state: next,
            fault: None,
            paths: COMMITTED,
        })
    }

    fn replicate(
        &self,
        state: &State,
        l: usize,
        m: usize,
        p: usize,
        k: usize,
        learns: bool,
    ) -> Option<Step<State, Fault>> {
        let len_l = usize::from(state.len[l]);
        let len_m = usize::from(state.len[m]);
        let commit_m = usize::from(state.commit[m]);
        let point = p == 0
            || p <= commit_m
            || (p >= 1 && p <= len_m && state.log[m][p - 1].term == state.log[l][p - 1].term);
        if l == m
            || !state.leader[l]
            || !has(
                Self::members(self.applied(state, l)) | Self::members(self.newest(state, l)),
                m,
            )
            || state.term[m] > state.term[l]
            || p > k
            || k > len_l
            || !point
        {
            return None;
        }
        let from = p.max(commit_m) + 1;
        let differs = (from..=k)
            .find(|i| *i > len_m || state.log[m][*i - 1].term != state.log[l][*i - 1].term);
        let taken: Vec<Entry> = match differs {
            None => Self::entries(state, m).to_vec(),
            Some(c) => Self::entries(state, m)[..c - 1]
                .iter()
                .chain(&Self::entries(state, l)[c - 1..k])
                .copied()
                .collect(),
        };
        let mut paths = 0;
        let overwrote = |at: usize| taken.get(at) != Some(&state.log[m][at]);
        if (0..len_m).any(|at| Self::is_conf(state.log[m][at].value) && overwrote(at)) {
            paths |= OVERWRITTEN;
        }
        let mut next = *state;
        Self::set_log(&mut next, m, &taken);
        if learns {
            let learned = usize::from(state.commit[l]).min(k);
            next.commit[m] = u8::try_from(commit_m.max(learned)).unwrap();
        }
        next.term[m] = state.term[l];
        if state.term[m] != state.term[l] {
            next.vote[m] = NOBODY;
        }
        next.leader[m] = false;
        let t = usize::from(state.term[l]);
        next.acks[m][t] = next.acks[m][t].max(u8::try_from(k).unwrap());
        Some(Step {
            state: next,
            fault: None,
            paths,
        })
    }

    fn current(state: &State, c: usize, m: usize) -> bool {
        let (lc, lm) = (Self::last_term(state, c), Self::last_term(state, m));
        lc > lm || (lc == lm && state.len[c] >= state.len[m])
    }
    /// `Stands(c)`.
    fn stands(&self, state: &State, c: usize) -> bool {
        if has(Self::voters(self.election_conf(state, c)), c) {
            return true;
        }
        if self.scope.stand != Stand::Needed || self.scope.elections != Elections::Newest {
            return false;
        }
        let len = usize::from(state.len[c]);
        let Some(at) = (0..len)
            .rev()
            .find(|at| Self::is_conf(state.log[c][*at].value))
        else {
            return false;
        };
        usize::from(state.commit[c]) <= at
            && has(
                Self::voters(self.conf_at(Self::conf_index(state, c, at))),
                c,
            )
    }
    fn campaigns(&self, state: &State, c: usize) -> bool {
        state.term[c] < self.scope.max_term && self.stands(state, c)
    }
    fn asked(&self, state: &State, c: usize, q: u8) -> bool {
        !has(q, c)
            && (0..self.scope.servers).filter(|m| has(q, *m)).all(|m| {
                (state.term[m] < state.term[c] + 1
                    || (state.term[m] == state.term[c] + 1 && state.vote[m] == NOBODY))
                    && Self::current(state, c, m)
            })
    }
    fn quorums(&self, state: &State, c: usize, q: u8) -> Vec<u8> {
        let counted = self.election_conf(state, c);
        let pool = (q | bit(c)) & Self::voters(counted);
        let all = u8::try_from((1usize << self.scope.servers) - 1).unwrap();
        let mut out = vec![0];
        for v in 1..=all {
            // Its own vote is among them where it is a voter.
            let own = has(v, c) || !has(Self::voters(counted), c);
            if v & !pool == 0 && own && Self::quorum_of(counted, v) {
                out.push(v);
            }
        }
        out
    }

    fn elect(&self, state: &State, c: usize, q: u8, v: u8) -> Option<Step<State, Fault>> {
        if !self.campaigns(state, c)
            || !self.asked(state, c, q)
            || !self.quorums(state, c, q).contains(&v)
        {
            return None;
        }
        let t = state.term[c] + 1;
        let voted = q | bit(c);
        let mut next = *state;
        for m in (0..self.scope.servers).filter(|m| has(voted, *m)) {
            next.term[m] = t;
            next.vote[m] = u8::try_from(c + 1).unwrap();
            next.leader[m] = m == c && v != 0;
        }
        let mut paths = 0;
        if v != 0 {
            paths |= ELECTED;
            if Self::pending(state, c) {
                paths |= ON_PENDING;
            }
            if !has(Self::voters(self.election_conf(state, c)), c) {
                paths |= STOOD;
            }
        }
        Some(Step {
            state: next,
            fault: None,
            paths,
        })
    }

    fn violation(&self, state: &State) -> Option<Fault> {
        let n = self.scope.servers;
        if (0..n).any(|s| state.commit[s] > state.len[s]) {
            return Some(Fault::TypeOk);
        }
        for l in 0..n {
            for m in 0..n {
                if l != m && state.leader[l] && state.leader[m] && state.term[l] == state.term[m] {
                    return Some(Fault::OneLeader);
                }
            }
        }
        for m in 0..n {
            for o in 0..n {
                let both = usize::from(state.commit[m].min(state.commit[o]));
                if (0..both).any(|at| state.log[m][at] != state.log[o][at]) {
                    return Some(Fault::Agreement);
                }
            }
        }
        for m in 0..n {
            for at in 0..usize::from(state.commit[m]) {
                let chosen = state.chosen[at];
                if chosen == Entry::default() || state.log[m][at].value != chosen.value {
                    return Some(Fault::Committed);
                }
            }
        }
        for l in (0..n).filter(|l| state.leader[*l]) {
            for at in 0..self.scope.max_len {
                let chosen = state.chosen[at];
                if chosen != Entry::default()
                    && chosen.term < state.term[l]
                    && (at >= usize::from(state.len[l]) || state.log[l][at].value != chosen.value)
                {
                    return Some(Fault::LeaderHolds);
                }
            }
        }
        for m in 0..n {
            for o in 0..n {
                let both = usize::from(state.len[m].min(state.len[o]));
                for at in 0..both {
                    if state.log[m][at].term == state.log[o][at].term
                        && (0..=at).any(|j| state.log[m][j] != state.log[o][j])
                    {
                        return Some(Fault::LogMatching);
                    }
                }
            }
        }
        let elected_on_pending = |l: usize| {
            state.leader[l]
                && (usize::from(state.commit[l])..usize::from(state.len[l])).any(|at| {
                    Self::is_conf(state.log[l][at].value) && state.log[l][at].term < state.term[l]
                })
        };
        if self.scope.reached && (0..n).any(elected_on_pending) {
            return Some(Fault::NoElectedOnPending);
        }
        let unnamed =
            |l: usize| state.leader[l] && !has(Self::voters(self.election_conf(state, l)), l);
        if self.scope.stood && (0..n).any(unnamed) {
            return Some(Fault::NoElectedUnnamed);
        }
        None
    }

    /// `state` with what no step and no invariant can read set to its initial value:
    /// - for whom a member voted, only whether it did (`Asked` reads `vote = Nobody` alone);
    /// - what members acknowledged to a leader of term `t`, once no member leads `t` and every
    ///   member's term is at least `t`: only a leader of `t` reads it, terms never fall, and an
    ///   election into `t` needs a candidate of a lower term.
    fn reduced(&self, state: &State) -> State {
        let n = self.scope.servers;
        let mut out = *state;
        for t in 0..=usize::from(self.scope.max_term) {
            let t8 = u8::try_from(t).unwrap();
            let led = (0..n).any(|l| state.leader[l] && state.term[l] == t8);
            let past = (0..n).all(|s| state.term[s] >= t8);
            if !led && past {
                for m in 0..n {
                    out.acks[m][t] = 0;
                }
            }
        }
        for m in 0..n {
            if out.vote[m] != NOBODY {
                out.vote[m] = 1;
            }
        }
        out
    }

    /// `state` with B and C swapped.
    fn swapped(state: &State) -> State {
        let mut out = *state;
        for (x, y) in [(1usize, 2usize), (2, 1)] {
            out.term[y] = state.term[x];
            out.vote[y] = state.vote[x];
            out.leader[y] = state.leader[x];
            out.len[y] = state.len[x];
            out.log[y] = state.log[x];
            out.commit[y] = state.commit[x];
            out.acks[y] = state.acks[x];
        }
        out
    }

    fn pack(&self, state: &State) -> Key {
        let mut p = Packer::<WORDS>::new();
        let mut put = |width: usize, value: u8| p.put(width, u64::from(value)).unwrap();
        for s in 0..self.scope.servers {
            put(2, state.term[s]);
            put(3, state.vote[s]);
            put(1, u8::from(state.leader[s]));
            put(2, state.len[s]);
            for at in 0..self.scope.max_len {
                put(2, state.log[s][at].term);
                put(3, state.log[s][at].value);
            }
            put(2, state.commit[s]);
            for t in 0..=usize::from(self.scope.max_term) {
                put(2, state.acks[s][t]);
            }
        }
        for at in 0..self.scope.max_len {
            put(2, state.chosen[at].term);
            put(3, state.chosen[at].value);
        }
        p.words()
    }

    fn unpacked(&self, key: &Key) -> State {
        let mut p = Packer::<WORDS>::over(*key);
        let mut take = |width: usize| p.small(width).unwrap();
        let mut state = self.initial();
        for s in 0..self.scope.servers {
            state.term[s] = take(2);
            state.vote[s] = take(3);
            state.leader[s] = take(1) == 1;
            state.len[s] = take(2);
            for at in 0..self.scope.max_len {
                state.log[s][at].term = take(2);
                state.log[s][at].value = take(3);
            }
            state.commit[s] = take(2);
            for t in 0..=usize::from(self.scope.max_term) {
                state.acks[s][t] = take(2);
            }
        }
        for at in 0..self.scope.max_len {
            state.chosen[at].term = take(2);
            state.chosen[at].value = take(3);
        }
        state
    }
}

impl Model for Reconfig {
    type State = State;
    type Action = Action;
    type Fault = Fault;
    type Key = Key;

    fn paths(&self) -> &'static [&'static str] {
        PATHS
    }

    fn initial(&self) -> State {
        State {
            term: [0; MAX_SERVERS],
            vote: [NOBODY; MAX_SERVERS],
            leader: [false; MAX_SERVERS],
            len: [0; MAX_SERVERS],
            log: [[Entry::default(); MAX_LEN]; MAX_SERVERS],
            commit: [0; MAX_SERVERS],
            acks: [[0; TERMS]; MAX_SERVERS],
            chosen: [Entry::default(); MAX_LEN],
        }
    }

    fn actions(&self, state: &State, out: &mut Vec<Action>) {
        let n = self.scope.servers;
        let small = |x: usize| u8::try_from(x).unwrap();
        for l in (0..n).filter(|l| state.leader[*l]) {
            for v in std::iter::once(NOOP).chain(V1..V1 + self.scope.values) {
                out.push(Action::Take { l: small(l), v });
            }
            out.push(Action::Reconfigure { l: small(l) });
            for i in 1..=self.scope.max_len {
                out.push(Action::Commit {
                    l: small(l),
                    i: small(i),
                });
            }
            let len = usize::from(state.len[l]);
            for m in (0..n).filter(|m| *m != l) {
                for p in 0..=len {
                    for k in p..=len {
                        for learns in [false, true] {
                            out.push(Action::Replicate {
                                l: small(l),
                                m: small(m),
                                p: small(p),
                                k: small(k),
                                learns,
                            });
                        }
                    }
                }
            }
        }
        for c in (0..n).filter(|c| self.campaigns(state, *c)) {
            let others = u8::try_from((1usize << n) - 1).unwrap() & !bit(c);
            let mut q = 0u8;
            loop {
                if self.asked(state, c, q) {
                    for v in self.quorums(state, c, q) {
                        out.push(Action::Elect { c: small(c), q, v });
                    }
                }
                q = q.wrapping_sub(others) & others;
                if q == 0 {
                    break;
                }
            }
        }
    }

    fn apply(&self, state: &State, action: Action) -> Option<Step<State, Fault>> {
        let u = usize::from;
        let mut step = match action {
            Action::Take { l, v } => self.take(state, u(l), v),
            Action::Reconfigure { l } => self.reconfigure(state, u(l)),
            Action::Commit { l, i } => self.commit(state, u(l), u(i)),
            Action::Replicate { l, m, p, k, learns } => {
                self.replicate(state, u(l), u(m), u(p), u(k), learns)
            }
            Action::Elect { c, q, v } => self.elect(state, u(c), q, v),
        }?;
        if step.state == *state {
            return None;
        }
        step.fault = self.violation(&step.state);
        Some(step)
    }

    fn canonical(&self, state: &State) -> Key {
        let reduced = if self.scope.reduce {
            self.reduced(state)
        } else {
            *state
        };
        let mut key = self.pack(&reduced);
        if self.scope.symmetry && matches!(self.scope.scenario, Scenario::Joint | Scenario::Promote)
        {
            key = key.min(self.pack(&Self::swapped(&reduced)));
        }
        key
    }

    fn unpack(&self, key: &Key) -> State {
        self.unpacked(key)
    }
}
