//! Core step R-8's commits on entries held in memory (`docs/raft.md` §3.7) as a [`Model`] of
//! `hyper_check::explore`: three voters, each its own fate, whose followers answer for memory under
//! a durable promise, a leader that counts its own disk, crashes that lose what was not written,
//! promises' marks judged in elections as R-5's and R-7's are, and a leader's narrowing of a
//! restarted member's mark.
//!
//! **What it holds.** Each member keeps its term and vote, whether it leads, the leader it follows
//! and how far its log is known to match that leader's (`through`); its log in memory and on disk;
//! its durable promise; at most one write out (a log and a promise, `Issue`, made durable by
//! `Complete`: the core's readies ahead of persistence at a depth of one); the greatest entry it
//! answered for in memory in this incarnation; its mark. A leader keeps what each member said it
//! holds in memory (`held`) and durably (`matched`). Messages are not modelled one by one: an answer
//! acts when it is given, and what a leader was told stays told, so a commit after the answerer's
//! crash is a run the model has. A crash loses the log in memory and the write out, and opens the
//! member marked through its durable promise where its durable log does not reach it.
//!
//! **Checked on every step:** Leader Completeness (a leader of a term after an entry's commit
//! holds it, in memory, at its index) and agreement (no index committed twice to different
//! entries). Each rule of the core has a [`Mutant`] that takes it out; the search must find the
//! defect it is for, or it checks nothing of the rule.

use std::fmt;

use hyper_check::explore::{Model, Step};

/// The voters, each its own fate.
pub const SERVERS: usize = 3;
/// The most indexes a scope models.
pub const MAX_LEN: usize = 3;
/// A majority of the voters.
const QUORUM: usize = SERVERS / 2 + 1;

/// A rule of the core taken out (`docs/raft.md` §3.7), or none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mutant {
    /// The core.
    None,
    /// A leader counts its memory toward a held quorum, with no promise of its own.
    LeaderMemory,
    /// A follower answers for memory with no promise covering the entries.
    AnswerPastPromise,
    /// A follower answers under its durable promise alone, past a lower one out.
    DurableCoverOnly,
    /// A write states a promise below what the member answered for and its log does not hold.
    LowerPromise,
    /// A promise's mark ends as storage's does, by index alone.
    IndexResolution,
    /// A mark narrows to what the member's own durable log holds, with no leader's word.
    NarrowToOwnLog,
}

/// What a scope searches.
#[derive(Clone, Copy, Debug)]
pub struct Scope {
    /// The highest term.
    pub max_term: u8,
    /// The longest log.
    pub max_len: usize,
    /// The distinct fates a held commit's holders and its leader must span.
    pub spanning: usize,
    /// The rule taken out.
    pub mutant: Mutant,
}

/// The last entry of a log, or a promise or a mark: `(term, index)`, ordered as Raft orders logs
/// (thesis §3.6.1); `(0, 0)` for none.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Point {
    pub term: u8,
    pub index: u8,
}

impl Point {
    const NONE: Self = Self { term: 0, index: 0 };
    fn is_none(self) -> bool {
        self.term == 0
    }
}

/// A log: each entry's term, one-based index `i` at slot `i − 1`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Log {
    pub len: u8,
    pub terms: [u8; MAX_LEN],
}

impl Log {
    fn last(&self) -> Point {
        if self.len == 0 {
            return Point::NONE;
        }
        Point {
            term: self.terms[usize::from(self.len) - 1],
            index: self.len,
        }
    }
    fn term(&self, index: u8) -> u8 {
        if index == 0 || index > self.len {
            return 0;
        }
        self.terms[usize::from(index) - 1]
    }
    /// The prefix through `k`.
    fn prefix(&self, k: u8) -> Self {
        let mut out = *self;
        out.len = k.min(self.len);
        for slot in usize::from(out.len)..MAX_LEN {
            out.terms[slot] = 0;
        }
        out
    }
    /// Whether the two logs hold the same entries through `k`.
    fn matches(&self, other: &Self, k: u8) -> bool {
        k <= self.len && k <= other.len && (1..=k).all(|i| self.term(i) == other.term(i))
    }
}

/// Whether a log whose last entry is `last` reaches `mark` in the log's order (`Lost::reached_by`).
fn reached(last: Point, mark: Point) -> bool {
    last.term > mark.term || (last.term == mark.term && last.index >= mark.index)
}

/// One member.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Server {
    pub term: u8,
    /// Whom it voted for in its term, plus one; zero for none.
    pub vote: u8,
    pub leads: bool,
    /// The leader it follows in its term, plus one; zero for none.
    pub leader: u8,
    pub through: u8,
    pub mlog: Log,
    pub dlog: Log,
    pub promise: Point,
    pub issued: bool,
    pub ilog: Log,
    pub ipromise: Point,
    pub answered: Point,
    pub mark: Point,
    /// While it leads: what each member said it holds in memory, and durably.
    pub held: [u8; SERVERS],
    pub matched: [u8; SERVERS],
}

/// What was committed at an index: the entry's term, and the term of the leader that committed
/// it; zero for nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Chosen {
    pub entry: u8,
    pub by: u8,
}

/// The group, and what was committed at each index.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct State {
    pub servers: [Server; SERVERS],
    pub chosen: [Chosen; MAX_LEN],
}

/// One step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// `c` is elected in `term` by the voters of `grants` (a bitmask, `c` counted only unmarked).
    Elect { c: u8, term: u8, grants: u8 },
    /// The leader `l` appends an entry of its term.
    Append { l: u8 },
    /// The follower `f` takes the leader `l`'s log through `k`, as Raft's append does.
    Replicate { l: u8, f: u8, k: u8 },
    /// The follower `f` answers its leader for memory, as far as its promises cover.
    HeldAnswer { f: u8 },
    /// The follower `f` answers its leader for what its disk holds of the leader's log.
    DurableAnswer { f: u8 },
    /// `s` issues a write of its log in memory with the promise `promise`.
    Issue { s: u8, promise: Point },
    /// The write `s` issued is durable.
    Complete { s: u8 },
    /// `s` crashes and opens on what is durable.
    Crash { s: u8 },
    /// The leader `l` commits through `index`.
    Commit { l: u8, index: u8 },
    /// The leader `l` states where its log ends to the marked `m`.
    Narrow { l: u8, m: u8 },
}

/// Why a step is a violation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// An index committed to a second entry.
    Agreement,
    /// A leader of a later term lacks a committed entry.
    LeaderHolds,
}

/// The paths a step may take, counted (the non-vacuity counters, `docs/sim.md` §4.4).
const PATHS: &[&str] = &[
    "held answer",
    "held commit",
    "marked at open",
    "narrowed",
    "mark ended",
    "marked leader",
    "stale tail",
];
const HELD_ANSWER: u64 = 1;
const HELD_COMMIT: u64 = 1 << 1;
const MARKED: u64 = 1 << 2;
const NARROWED: u64 = 1 << 3;
const ENDED: u64 = 1 << 4;
const MARKED_LEADER: u64 = 1 << 5;
const STALE_TAIL: u64 = 1 << 6;

/// The model of a scope.
pub struct Held {
    pub scope: Scope,
}

impl fmt::Debug for Held {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Held({:?})", self.scope)
    }
}

impl Held {
    fn ends(&self, s: &Server, mark: Point) -> bool {
        let last = s.dlog.last();
        match self.scope.mutant {
            Mutant::IndexResolution => last.index >= mark.index || last.term > mark.term,
            _ => reached(last, mark),
        }
    }
    /// What `s` answers for in an election: its log's last entry, or its mark, the later.
    fn claim(s: &Server) -> Point {
        s.mlog.last().max(s.mark)
    }
    /// The highest index `s` matches its own log in memory through on its disk.
    fn durable_match(s: &Server) -> u8 {
        (0..=s.mlog.len.min(s.dlog.len))
            .rev()
            .find(|k| s.dlog.matches(&s.mlog, *k))
            .unwrap_or(0)
    }
    /// What every promise out covers: the durable one's, and the write out's.
    fn cover(&self, s: &Server) -> Point {
        if s.issued && self.scope.mutant != Mutant::DurableCoverOnly {
            s.promise.min(s.ipromise)
        } else {
            s.promise
        }
    }
    /// The leader `l` of `state`, if `l` leads.
    fn leader(state: &State, l: u8) -> Option<&Server> {
        state.servers.get(usize::from(l)).filter(|s| s.leads)
    }
    /// A follower's leader, where it follows one that leads its term now.
    fn followed(state: &State, f: usize) -> Option<usize> {
        let s = &state.servers[f];
        if s.leads || s.leader == 0 {
            return None;
        }
        let l = usize::from(s.leader - 1);
        let leader = &state.servers[l];
        (leader.leads && leader.term == s.term).then_some(l)
    }
    fn elect(&self, state: &State, c: u8, term: u8, grants: u8) -> Option<Step<State, Fault>> {
        let ci = usize::from(c);
        let cand = state.servers[ci];
        if term <= cand.term
            || term > self.scope.max_term
            || usize::from(cand.mlog.len) >= self.scope.max_len
        {
            return None;
        }
        let marked = !cand.mark.is_none();
        let mut counted = 0usize;
        for v in 0..SERVERS {
            if grants & (1 << v) == 0 {
                continue;
            }
            if v == ci {
                if marked {
                    return None;
                }
                counted += 1;
                continue;
            }
            let voter = &state.servers[v];
            let free = voter.term < term
                || (voter.term == term && (voter.vote == 0 || voter.vote == c + 1));
            if !free || cand.mlog.last() < Self::claim(voter) {
                return None;
            }
            counted += 1;
        }
        if counted < QUORUM {
            return None;
        }
        let mut next = *state;
        for v in 0..SERVERS {
            if grants & (1 << v) == 0 || v == ci {
                continue;
            }
            let voter = &mut next.servers[v];
            voter.term = term;
            voter.vote = c + 1;
            voter.leads = false;
            voter.leader = 0;
            voter.through = 0;
            voter.held = [0; SERVERS];
            voter.matched = [0; SERVERS];
        }
        let s = &mut next.servers[ci];
        s.term = term;
        s.vote = c + 1;
        s.leads = true;
        s.leader = c + 1;
        s.through = 0;
        let mut paths = 0;
        if marked {
            paths |= MARKED_LEADER;
        }
        // Elected without its own vote by a quorum that answered for no more than its log holds:
        // its mark ends (R-7), and its first entry is of its term.
        s.mark = Point::NONE;
        s.held = [0; SERVERS];
        s.matched = [0; SERVERS];
        let at = usize::from(s.mlog.len);
        s.mlog.terms[at] = term;
        s.mlog.len += 1;
        Some(self.checked(next, paths))
    }
    fn replicate(&self, state: &State, l: u8, f: u8, k: u8) -> Option<Step<State, Fault>> {
        let leader = *Self::leader(state, l)?;
        let fi = usize::from(f);
        if l == f || k == 0 || k > leader.mlog.len {
            return None;
        }
        let follower = state.servers[fi];
        if follower.term > leader.term {
            return None;
        }
        let new = leader.mlog.prefix(k);
        let conflict =
            (1..=k).find(|i| *i <= follower.mlog.len && follower.mlog.term(*i) != new.term(*i));
        let mlog = match conflict {
            Some(_) => new,
            None if follower.mlog.len >= k => follower.mlog,
            None => new,
        };
        let same_leader = follower.term == leader.term && follower.leader == l + 1;
        let through = if same_leader {
            follower.through.max(k)
        } else {
            k
        };
        let mut next = *state;
        let s = &mut next.servers[fi];
        if s.term < leader.term {
            s.term = leader.term;
            s.vote = 0;
        }
        if s.mlog == mlog && s.leader == l + 1 && s.through == through && !s.leads {
            return None;
        }
        s.leads = false;
        s.held = [0; SERVERS];
        s.matched = [0; SERVERS];
        s.leader = l + 1;
        s.mlog = mlog;
        s.through = through;
        Some(self.checked(next, 0))
    }
    fn held_answer(&self, state: &State, f: u8) -> Option<Step<State, Fault>> {
        let fi = usize::from(f);
        let li = Self::followed(state, fi)?;
        let s = state.servers[fi];
        if !s.mark.is_none() || s.through == 0 {
            return None;
        }
        let through = if self.scope.mutant == Mutant::AnswerPastPromise {
            s.through
        } else {
            let cover = self.cover(&s);
            if cover.term != s.term {
                return None;
            }
            s.through.min(cover.index)
        };
        if through <= state.servers[li].held[fi] {
            return None;
        }
        let mut next = *state;
        next.servers[li].held[fi] = through;
        let answered = Point {
            term: s.term,
            index: through,
        };
        let me = &mut next.servers[fi];
        me.answered = me.answered.max(answered);
        Some(self.checked(next, HELD_ANSWER))
    }
    fn durable_answer(&self, state: &State, f: u8) -> Option<Step<State, Fault>> {
        let fi = usize::from(f);
        let li = Self::followed(state, fi)?;
        let s = state.servers[fi];
        let leader = state.servers[li];
        let j = (0..=s.through)
            .rev()
            .find(|k| s.dlog.matches(&leader.mlog, *k))
            .unwrap_or(0);
        if j <= leader.matched[fi] {
            return None;
        }
        let mut next = *state;
        next.servers[li].matched[fi] = j;
        Some(self.checked(next, 0))
    }
    /// The promises `s` may state in a write of its log in memory: none, its owed, or one of its
    /// term past an index where it follows a leader whose log it matches.
    fn promises(&self, s: &Server, out: &mut Vec<Point>) {
        out.push(Point::NONE);
        let owed = s.answered.max(s.mark);
        if !owed.is_none() {
            out.push(owed);
        }
        if !s.leads && s.leader != 0 && s.through != 0 && s.mlog.last().term == s.term {
            for index in 1..=self.scope.max_len as u8 {
                out.push(Point {
                    term: s.term,
                    index,
                });
            }
        }
    }
    fn issue(&self, state: &State, s: u8, promise: Point) -> Option<Step<State, Fault>> {
        let si = usize::from(s);
        let me = state.servers[si];
        if me.issued {
            return None;
        }
        let owed = me.answered.max(me.mark);
        let last = me.mlog.last();
        let covered = owed.is_none() || reached(last, owed);
        if self.scope.mutant != Mutant::LowerPromise && !covered && promise < owed {
            return None;
        }
        // A promise its log reaches is stated as none, as the core does.
        let promise = if !promise.is_none() && reached(last, promise) {
            Point::NONE
        } else {
            promise
        };
        let mut next = *state;
        let m = &mut next.servers[si];
        m.issued = true;
        m.ilog = m.mlog;
        m.ipromise = promise;
        Some(self.checked(next, 0))
    }
    fn complete(&self, state: &State, s: u8) -> Option<Step<State, Fault>> {
        let si = usize::from(s);
        if !state.servers[si].issued {
            return None;
        }
        let mut next = *state;
        let mut paths = 0;
        let m = &mut next.servers[si];
        m.issued = false;
        m.dlog = m.ilog;
        m.promise = m.ipromise;
        if !m.mark.is_none() {
            let stale = m.dlog.last().term < m.mark.term && m.dlog.len >= m.mark.index;
            if stale {
                paths |= STALE_TAIL;
            }
            let mark = m.mark;
            if self.ends(m, mark) {
                m.mark = Point::NONE;
                paths |= ENDED;
            }
        }
        Some(self.checked(next, paths))
    }
    fn crash(&self, state: &State, s: u8) -> Option<Step<State, Fault>> {
        let si = usize::from(s);
        let mut next = *state;
        let mut paths = 0;
        let m = &mut next.servers[si];
        m.mlog = m.dlog;
        m.issued = false;
        m.ilog = Log::default();
        m.ipromise = Point::NONE;
        m.leads = false;
        m.leader = 0;
        m.through = 0;
        m.answered = Point::NONE;
        m.held = [0; SERVERS];
        m.matched = [0; SERVERS];
        let promise = m.promise;
        m.mark = if !promise.is_none() && !reached(m.dlog.last(), promise) {
            paths |= MARKED;
            if m.dlog.last().term < promise.term && m.dlog.len >= promise.index {
                paths |= STALE_TAIL;
            }
            promise
        } else {
            Point::NONE
        };
        if next == *state {
            return None;
        }
        Some(self.checked(next, paths))
    }
    fn commit(&self, state: &State, l: u8, index: u8) -> Option<Step<State, Fault>> {
        let leader = *Self::leader(state, l)?;
        let li = usize::from(l);
        if index == 0 || index > leader.mlog.len || leader.mlog.term(index) != leader.term {
            return None;
        }
        let own_disk = Self::durable_match(&leader);
        let own_held = if self.scope.mutant == Mutant::LeaderMemory {
            leader.mlog.len
        } else {
            own_disk
        };
        let classic = (0..SERVERS)
            .filter(|v| {
                let at = if *v == li {
                    own_disk
                } else {
                    leader.matched[*v]
                };
                at >= index
            })
            .count();
        let holders: Vec<usize> = (0..SERVERS)
            .filter(|v| {
                let at = if *v == li {
                    own_held
                } else {
                    leader.held[*v].max(leader.matched[*v])
                };
                at >= index
            })
            .collect();
        // Each member its own fate; the leader's counts whether or not its disk does. Where a
        // holder's disk holds the entry, no failure of memory loses it, and the span is waived.
        let span = holders.len() + usize::from(!holders.contains(&li));
        let on_disk = holders.iter().any(|v| {
            if *v == li {
                own_disk >= index
            } else {
                leader.matched[*v] >= index
            }
        });
        let by_held = holders.len() >= QUORUM && (on_disk || span >= self.scope.spanning);
        if classic < QUORUM && !by_held {
            return None;
        }
        let mut next = *state;
        let mut fault = None;
        let mut changed = false;
        for i in 1..=index {
            let entry = leader.mlog.term(i);
            let slot = &mut next.chosen[usize::from(i) - 1];
            if slot.entry == 0 {
                *slot = Chosen {
                    entry,
                    by: leader.term,
                };
                changed = true;
            } else if slot.entry != entry {
                fault = Some(Fault::Agreement);
            }
        }
        if !changed && fault.is_none() {
            return None;
        }
        let paths = if classic < QUORUM { HELD_COMMIT } else { 0 };
        let mut step = self.checked(next, paths);
        if step.fault.is_none() {
            step.fault = fault;
        }
        Some(step)
    }
    fn narrow(&self, state: &State, l: u8, m: u8) -> Option<Step<State, Fault>> {
        let mi = usize::from(m);
        let target = state.servers[mi];
        if target.mark.is_none() {
            return None;
        }
        let end = if self.scope.mutant == Mutant::NarrowToOwnLog {
            if l != m {
                return None;
            }
            target.dlog.len
        } else {
            let leader = *Self::leader(state, l)?;
            if l == m || target.mark.term != leader.term || target.term != leader.term {
                return None;
            }
            leader.mlog.len
        };
        if end >= target.mark.index {
            return None;
        }
        let mut next = *state;
        let mut paths = NARROWED;
        let s = &mut next.servers[mi];
        s.mark.index = end;
        let mark = s.mark;
        if self.ends(s, mark) {
            s.mark = Point::NONE;
            paths |= ENDED;
        }
        Some(self.checked(next, paths))
    }
    /// `next` held to the invariants checked on every step.
    fn checked(&self, next: State, paths: u64) -> Step<State, Fault> {
        let mut fault = None;
        for s in &next.servers {
            if !s.leads {
                continue;
            }
            for (slot, chosen) in next.chosen.iter().enumerate() {
                if chosen.entry == 0 || s.term <= chosen.by {
                    continue;
                }
                let index = slot as u8 + 1;
                if s.mlog.term(index) != chosen.entry {
                    fault = Some(Fault::LeaderHolds);
                }
            }
        }
        Step {
            state: next,
            fault,
            paths,
        }
    }
}

/// Every order of three members.
const PERMUTATIONS: [[usize; SERVERS]; 6] = [
    [0, 1, 2],
    [0, 2, 1],
    [1, 0, 2],
    [1, 2, 0],
    [2, 0, 1],
    [2, 1, 0],
];

/// `state` with member `i` renamed `order[i]`.
fn renamed(state: &State, order: &[usize; SERVERS]) -> State {
    let name = |plus_one: u8| -> u8 {
        if plus_one == 0 {
            0
        } else {
            order[usize::from(plus_one) - 1] as u8 + 1
        }
    };
    let mut out = *state;
    for (i, s) in state.servers.iter().enumerate() {
        let mut t = *s;
        t.vote = name(s.vote);
        t.leader = name(s.leader);
        for (j, to) in order.iter().enumerate() {
            t.held[*to] = s.held[j];
            t.matched[*to] = s.matched[j];
        }
        out.servers[order[i]] = t;
    }
    out
}

impl Model for Held {
    type State = State;
    type Action = Action;
    type Fault = Fault;
    type Key = State;

    fn paths(&self) -> &'static [&'static str] {
        PATHS
    }
    fn initial(&self) -> State {
        State::default()
    }
    fn actions(&self, state: &State, out: &mut Vec<Action>) {
        let n = SERVERS as u8;
        for c in 0..n {
            for term in 1..=self.scope.max_term {
                for grants in 1..(1u8 << SERVERS) {
                    out.push(Action::Elect { c, term, grants });
                }
            }
        }
        let mut promises = Vec::new();
        for s in 0..n {
            out.push(Action::Append { l: s });
            out.push(Action::HeldAnswer { f: s });
            out.push(Action::DurableAnswer { f: s });
            out.push(Action::Complete { s });
            out.push(Action::Crash { s });
            promises.clear();
            self.promises(&state.servers[usize::from(s)], &mut promises);
            for promise in &promises {
                out.push(Action::Issue {
                    s,
                    promise: *promise,
                });
            }
            for index in 1..=self.scope.max_len as u8 {
                out.push(Action::Commit { l: s, index });
            }
            for other in 0..n {
                out.push(Action::Narrow { l: s, m: other });
                for k in 1..=self.scope.max_len as u8 {
                    out.push(Action::Replicate { l: s, f: other, k });
                }
            }
        }
    }
    fn apply(&self, state: &State, action: Action) -> Option<Step<State, Fault>> {
        match action {
            Action::Elect { c, term, grants } => self.elect(state, c, term, grants),
            Action::Append { l } => {
                let leader = *Self::leader(state, l)?;
                if usize::from(leader.mlog.len) >= self.scope.max_len {
                    return None;
                }
                let mut next = *state;
                let s = &mut next.servers[usize::from(l)];
                let at = usize::from(s.mlog.len);
                s.mlog.terms[at] = s.term;
                s.mlog.len += 1;
                Some(self.checked(next, 0))
            }
            Action::Replicate { l, f, k } => self.replicate(state, l, f, k),
            Action::HeldAnswer { f } => self.held_answer(state, f),
            Action::DurableAnswer { f } => self.durable_answer(state, f),
            Action::Issue { s, promise } => self.issue(state, s, promise),
            Action::Complete { s } => self.complete(state, s),
            Action::Crash { s } => self.crash(state, s),
            Action::Commit { l, index } => self.commit(state, l, index),
            Action::Narrow { l, m } => self.narrow(state, l, m),
        }
    }
    fn canonical(&self, state: &State) -> State {
        // The members play alike (each its own fate, every rule the same for each): a class is
        // its states under every renaming of them, and its key the least.
        PERMUTATIONS
            .iter()
            .map(|order| renamed(state, order))
            .min()
            .unwrap_or(*state)
    }
    fn unpack(&self, key: &State) -> State {
        *key
    }
}
