//! The persisted class's commits, a leader answering ahead of its own disk (core step R-6,
//! `Config::apply_unpersisted`; kiln's CP-T2.2, Focal's condition 2), as a [`Model`] of
//! `hyper_check::explore`: three voters whose commits rest on a majority's disks, no fates.
//!
//! **What it holds.** Each member keeps its term and vote, whether it leads, the leader it follows
//! and how far its log is known to match that leader's (`through`); its log in memory and on disk;
//! at most one write out (`Issue`, made durable by `Complete`: the core's readies ahead of
//! persistence at a depth of one). A leader keeps what each member said its disk holds
//! (`matched`, its durable acknowledgements, I2). It commits the greatest index of its own term a
//! majority's disks hold, counting its own only through its durable log (I3, thesis §10.2.1), and
//! answers a committed entry of its own term whether or not its own disk holds it (I4, R-6): the
//! persisted class's acknowledgement. A crash loses the log in memory and the write out; a member's
//! term and vote are durable before it grants (I1), as `held.rs` has them. Messages are not
//! modelled one by one: an answer acts when it is given, and what a leader was told stays told.
//!
//! **Checked on every step:** an entry answered is held by a majority's disks at its answer, the
//! leader's memory not counted (`OffMajority`); no index is committed to two entries
//! (`Agreement`); and every leader of a later term holds every entry committed or answered
//! (`LeaderHolds`). [`Mutant::LeaderMemory`] counts the leader's log in memory toward the majority,
//! as Raft's TLA+ specification's `Agree` set counts the leader whatever its disk holds: sound
//! there only because its log is durable at once. The search must find the defect it is for.
//!
//! **Acknowledgements in flight** (`Scope::in_flight`, Focal's condition 2's advisory): a durable
//! answer goes into a bag of that many instead of acting at once, and a later step delivers one,
//! whatever happened between: its receiver takes it only if it leads in the answer's own term, as
//! the core drops a message of an older term than its own (`Raft::step_term`), and drops it
//! otherwise. A stale acknowledgement delivered late is so explored, and [`Mutant::StaleAck`], which
//! takes one whatever its term, is the defect the variant must find.

use std::fmt;

use hyper_check::explore::{Model, Step};

/// The voters.
pub const SERVERS: usize = 3;
/// The most indexes a scope models.
pub const MAX_LEN: usize = 3;
/// A majority of the voters.
const QUORUM: usize = SERVERS / 2 + 1;

/// A rule taken out, or none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mutant {
    /// The core.
    None,
    /// A leader counts its log in memory toward an entry's majority.
    LeaderMemory,
    /// A leader takes a durable acknowledgement whatever its term (with acknowledgements in flight).
    StaleAck,
}

/// What a scope searches.
#[derive(Clone, Copy, Debug)]
pub struct Scope {
    /// The highest term.
    pub max_term: u8,
    /// The longest log.
    pub max_len: usize,
    /// The rule taken out.
    pub mutant: Mutant,
    /// Durable acknowledgements in flight at once, at most [`BAG`]; none, each acts as it is given.
    pub in_flight: usize,
}

/// The most acknowledgements a scope keeps in flight.
pub const BAG: usize = 2;

/// A durable acknowledgement in flight: to whom and from whom (each plus one; zero, an empty
/// slot), its sender's term as it was sent, and the index the sender's disk matched its leader's
/// log through.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Ack {
    pub to: u8,
    pub from: u8,
    pub term: u8,
    pub index: u8,
}

/// The last entry of a log: `(term, index)`, ordered as Raft orders logs (thesis §3.6.1).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Point {
    pub term: u8,
    pub index: u8,
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
            return Point::default();
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
    pub issued: bool,
    pub ilog: Log,
    /// While it leads: what each member said its disk holds of its log.
    pub matched: [u8; SERVERS],
}

/// What was committed, or answered, at an index: the entry's term, and the term of the leader
/// that committed or answered it; zero for nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Chosen {
    pub entry: u8,
    pub by: u8,
}

/// The group, what was committed at each index, and what was answered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct State {
    pub servers: [Server; SERVERS],
    pub chosen: [Chosen; MAX_LEN],
    pub answered: [Chosen; MAX_LEN],
    /// The acknowledgements in flight, sorted, empty slots first.
    pub bag: [Ack; BAG],
}

/// One step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// `c` is elected in `term` by the voters of `grants` (a bitmask).
    Elect { c: u8, term: u8, grants: u8 },
    /// The leader `l` appends an entry of its term.
    Append { l: u8 },
    /// The follower `f` takes the leader `l`'s log through `k`, as Raft's append does.
    Replicate { l: u8, f: u8, k: u8 },
    /// The follower `f` answers its leader for what its disk holds of the leader's log: at once, or
    /// into the bag with acknowledgements in flight.
    DurableAnswer { f: u8 },
    /// The acknowledgement in the bag's slot is delivered.
    Deliver { slot: u8 },
    /// `s` issues a write of its log in memory.
    Issue { s: u8 },
    /// The write `s` issued is durable.
    Complete { s: u8 },
    /// `s` crashes and opens on what is durable.
    Crash { s: u8 },
    /// The leader `l` commits through `index`.
    Commit { l: u8, index: u8 },
    /// The leader `l` answers the committed entry at `index`, its own disk holding it or not.
    Answer { l: u8, index: u8 },
}

/// Why a step is a violation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// An entry answered that a majority's disks do not hold.
    OffMajority,
    /// An index committed to a second entry.
    Agreement,
    /// A leader of a later term lacks an entry committed or answered.
    LeaderHolds,
}

/// The paths a step may take, counted (the non-vacuity counters, `docs/sim.md` §4.4).
const PATHS: &[&str] = &[
    "answered ahead of its disk",
    "crashed after an answer",
    "elected without the leader before",
];
/// With acknowledgements in flight, one more: a stale one delivered.
const PATHS_IN_FLIGHT: &[&str] = &[
    "answered ahead of its disk",
    "crashed after an answer",
    "elected without the leader before",
    "a stale acknowledgement delivered",
];
const AHEAD: u64 = 1;
const CRASHED_AFTER: u64 = 1 << 1;
const WITHOUT: u64 = 1 << 2;
const STALE: u64 = 1 << 3;

/// The model of a scope.
pub struct Unpersisted {
    pub scope: Scope,
}

impl fmt::Debug for Unpersisted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Unpersisted({:?})", self.scope)
    }
}

impl Unpersisted {
    /// The highest index `s` matches its own log in memory through on its disk.
    fn durable_match(s: &Server) -> u8 {
        (0..=s.mlog.len.min(s.dlog.len))
            .rev()
            .find(|k| s.dlog.matches(&s.mlog, *k))
            .unwrap_or(0)
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
            || grants & (1 << ci) == 0
        {
            return None;
        }
        let mut counted = 0usize;
        for v in 0..SERVERS {
            if grants & (1 << v) == 0 {
                continue;
            }
            counted += 1;
            if v == ci {
                continue;
            }
            let voter = &state.servers[v];
            let free = voter.term < term
                || (voter.term == term && (voter.vote == 0 || voter.vote == c + 1));
            if !free || cand.mlog.last() < voter.mlog.last() {
                return None;
            }
        }
        if counted < QUORUM {
            return None;
        }
        // A leader still leading that granted nothing: the group moved on without it.
        let without = (0..SERVERS).any(|v| state.servers[v].leads && grants & (1 << v) == 0);
        let mut next = *state;
        for v in 0..SERVERS {
            if grants & (1 << v) == 0 {
                continue;
            }
            let voter = &mut next.servers[v];
            voter.term = term;
            voter.vote = c + 1;
            voter.leads = false;
            voter.leader = 0;
            voter.through = 0;
            voter.matched = [0; SERVERS];
        }
        let s = &mut next.servers[ci];
        s.leads = true;
        s.leader = c + 1;
        // Its first entry is of its term.
        let at = usize::from(s.mlog.len);
        s.mlog.terms[at] = term;
        s.mlog.len += 1;
        Some(self.checked(next, if without { WITHOUT } else { 0 }))
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
        s.matched = [0; SERVERS];
        s.leader = l + 1;
        s.mlog = mlog;
        s.through = through;
        Some(self.checked(next, 0))
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
        if self.scope.in_flight == 0 {
            next.servers[li].matched[fi] = j;
            return Some(self.checked(next, 0));
        }
        let ack = Ack {
            to: li as u8 + 1,
            from: f + 1,
            term: s.term,
            index: j,
        };
        let held = next.bag.iter().filter(|a| a.to != 0).count();
        if held >= self.scope.in_flight.min(BAG) || next.bag.contains(&ack) {
            return None;
        }
        let empty = next.bag.iter().position(|a| a.to == 0)?;
        next.bag[empty] = ack;
        next.bag.sort_unstable();
        Some(self.checked(next, 0))
    }
    /// The acknowledgement in `slot` delivered: its receiver takes it if it leads in its term (or,
    /// the mutant's, whatever its term), else drops it, as the core drops a message of an older
    /// term than its own.
    fn deliver(&self, state: &State, slot: u8) -> Option<Step<State, Fault>> {
        let ack = *state.bag.get(usize::from(slot))?;
        if ack.to == 0 {
            return None;
        }
        let mut next = *state;
        next.bag[usize::from(slot)] = Ack::default();
        next.bag.sort_unstable();
        let li = usize::from(ack.to - 1);
        let fi = usize::from(ack.from - 1);
        let receiver = state.servers[li];
        let stale = receiver.term > ack.term;
        let takes =
            receiver.leads && (receiver.term == ack.term || self.scope.mutant == Mutant::StaleAck);
        if takes {
            let matched = &mut next.servers[li].matched[fi];
            *matched = (*matched).max(ack.index);
        }
        Some(self.checked(next, if stale { STALE } else { 0 }))
    }
    fn issue(&self, state: &State, s: u8) -> Option<Step<State, Fault>> {
        let si = usize::from(s);
        let me = state.servers[si];
        // Nothing in memory its disk lacks: no write to make.
        if me.issued || me.dlog == me.mlog {
            return None;
        }
        let mut next = *state;
        let m = &mut next.servers[si];
        m.issued = true;
        m.ilog = m.mlog;
        Some(self.checked(next, 0))
    }
    fn complete(&self, state: &State, s: u8) -> Option<Step<State, Fault>> {
        let si = usize::from(s);
        if !state.servers[si].issued {
            return None;
        }
        let mut next = *state;
        let m = &mut next.servers[si];
        m.issued = false;
        m.dlog = m.ilog;
        Some(self.checked(next, 0))
    }
    fn crash(&self, state: &State, s: u8) -> Option<Step<State, Fault>> {
        let si = usize::from(s);
        let mut next = *state;
        let m = &mut next.servers[si];
        m.mlog = m.dlog;
        m.issued = false;
        m.ilog = Log::default();
        m.leads = false;
        m.leader = 0;
        m.through = 0;
        m.matched = [0; SERVERS];
        if next == *state {
            return None;
        }
        let after = state.answered.iter().any(|a| a.entry != 0);
        Some(self.checked(next, if after { CRASHED_AFTER } else { 0 }))
    }
    /// How many members' disks hold the leader `l`'s entry at `index`, the leader's own counted
    /// through its durable log, or, the mutant's, its log in memory.
    fn holders(&self, state: &State, l: usize, index: u8) -> usize {
        let leader = &state.servers[l];
        let own = if self.scope.mutant == Mutant::LeaderMemory {
            leader.mlog.len
        } else {
            Self::durable_match(leader)
        };
        (0..SERVERS)
            .filter(|v| {
                if *v == l {
                    own >= index
                } else {
                    leader.matched[*v] >= index
                }
            })
            .count()
    }
    fn commit(&self, state: &State, l: u8, index: u8) -> Option<Step<State, Fault>> {
        let leader = *Self::leader(state, l)?;
        let li = usize::from(l);
        if index == 0 || index > leader.mlog.len || leader.mlog.term(index) != leader.term {
            return None;
        }
        if self.holders(state, li, index) < QUORUM {
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
        let mut step = self.checked(next, 0);
        if step.fault.is_none() {
            step.fault = fault;
        }
        Some(step)
    }
    fn answer(&self, state: &State, l: u8, index: u8) -> Option<Step<State, Fault>> {
        let leader = *Self::leader(state, l)?;
        let at = usize::from(index).checked_sub(1)?;
        let entry = leader.mlog.term(index);
        let chosen = *state.chosen.get(at)?;
        // A committed entry of its own term, answered once.
        if entry != leader.term || chosen.entry != entry || state.answered[at].entry != 0 {
            return None;
        }
        let on_disks = state
            .servers
            .iter()
            .filter(|s| s.dlog.term(index) == entry)
            .count();
        let mut next = *state;
        next.answered[at] = Chosen {
            entry,
            by: leader.term,
        };
        let ahead = leader.dlog.term(index) != entry;
        let mut step = self.checked(next, if ahead { AHEAD } else { 0 });
        if on_disks < QUORUM {
            step.fault = Some(Fault::OffMajority);
        }
        Some(step)
    }
    /// `next` held to the invariants checked on every step.
    fn checked(&self, next: State, paths: u64) -> Step<State, Fault> {
        let mut fault = None;
        for s in &next.servers {
            if !s.leads {
                continue;
            }
            for (slot, chosen) in next.chosen.iter().chain(next.answered.iter()).enumerate() {
                if chosen.entry == 0 || s.term <= chosen.by {
                    continue;
                }
                let index = (slot % MAX_LEN) as u8 + 1;
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
            t.matched[*to] = s.matched[j];
        }
        out.servers[order[i]] = t;
    }
    for (slot, ack) in state.bag.iter().enumerate() {
        out.bag[slot] = Ack {
            to: name(ack.to),
            from: name(ack.from),
            ..*ack
        };
    }
    out.bag.sort_unstable();
    out
}

impl Model for Unpersisted {
    type State = State;
    type Action = Action;
    type Fault = Fault;
    type Key = State;

    fn paths(&self) -> &'static [&'static str] {
        if self.scope.in_flight == 0 {
            PATHS
        } else {
            PATHS_IN_FLIGHT
        }
    }
    fn initial(&self) -> State {
        State::default()
    }
    fn actions(&self, _state: &State, out: &mut Vec<Action>) {
        let n = SERVERS as u8;
        for c in 0..n {
            for term in 1..=self.scope.max_term {
                for grants in 1..(1u8 << SERVERS) {
                    out.push(Action::Elect { c, term, grants });
                }
            }
        }
        for s in 0..n {
            out.push(Action::Append { l: s });
            out.push(Action::DurableAnswer { f: s });
            if usize::from(s) < self.scope.in_flight.min(BAG) {
                out.push(Action::Deliver { slot: s });
            }
            out.push(Action::Issue { s });
            out.push(Action::Complete { s });
            out.push(Action::Crash { s });
            for index in 1..=self.scope.max_len as u8 {
                out.push(Action::Commit { l: s, index });
                out.push(Action::Answer { l: s, index });
            }
            for other in 0..n {
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
            Action::DurableAnswer { f } => self.durable_answer(state, f),
            Action::Deliver { slot } => self.deliver(state, slot),
            Action::Issue { s } => self.issue(state, s),
            Action::Complete { s } => self.complete(state, s),
            Action::Crash { s } => self.crash(state, s),
            Action::Commit { l, index } => self.commit(state, l, index),
            Action::Answer { l, index } => self.answer(state, l, index),
        }
    }
    fn canonical(&self, state: &State) -> State {
        // The members play alike: a class is its states under every renaming of them, and its key
        // the least.
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
