//! `docs/models/Suspicion.tla` as a [`Model`] of `hyper_check::explore`: the same variables, actions
//! and invariants, with no symmetry and no reduction, so that a search's class count is the number
//! of distinct states TLC finds for the same configuration (`docs/models/Suspicion*.cfg` state the
//! counts this search reaches, and CI's model job holds TLC to them).
//!
//! Each action is the specification's of the same name. A member is its place, 0 to 2; `NOBODY` is
//! 3. `Stable` is checked on the step, from a state where a leader is settled to one where it is not
//! (or its term moved): the specification's history variable `settled` holds that leader and term
//! from the first state that settles, and since a settled group stays settled wherever `Stable`
//! holds, the variable is a function of the state there and adds no state to TLC's count.

use hyper_check::explore::{Model, Packer, Step};

pub const MAX_SERVERS: usize = 3;
/// A term packs in two bits: `0..=MAX_TERM`.
pub const MAX_TERM: u8 = 3;
const WORDS: usize = 2;
const NOBODY: u8 = 3;

const FOLLOWER: u8 = 0;
const PRECANDIDATE: u8 = 1;
const CANDIDATE: u8 = 2;
const LEADER: u8 = 3;

/// `Kinds`: how a candidate's campaign began.
const ASKED: u8 = 0;
const ORDER: u8 = 1;
const CONDITION: u8 = 2;

/// `Rule`: what a member does when its detectors leave it no quorum.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rule {
    /// A leader leads on and orders heirs only where each can be elected without it; a pre-candidate
    /// stops asking.
    Core,
    /// A leader steps down, and having led its term orders heirs unconditionally.
    StepDown,
    /// A leader leads on and orders unconditionally.
    Unconditional,
    /// As `Core`, but a pre-candidate asks on.
    Asking,
}

#[derive(Clone, Copy, Debug)]
pub struct Scope {
    pub servers: usize,
    /// The members stopped for good, a bit each.
    pub stopped: u8,
    pub max_term: u8,
    pub rule: Rule,
    /// Whether `Stable` is checked (`CheckStable`).
    pub stable: bool,
    /// Whether `NoLeaderLeftNoQuorum` is checked (the scope must reach it).
    pub reached: bool,
    /// Whether `NoHandOver` is checked (the scope must reach it).
    pub hand_over: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct State {
    pub term: [u8; MAX_SERVERS],
    pub vote: [u8; MAX_SERVERS],
    pub role: [u8; MAX_SERVERS],
    pub leader: [u8; MAX_SERVERS],
    pub left: [u8; MAX_SERVERS],
    /// A bit a member.
    pub suspects: [u8; MAX_SERVERS],
    /// A bit a member.
    pub granted: [u8; MAX_SERVERS],
    pub kind: [u8; MAX_SERVERS],
    /// A bit an order (`order_bit`).
    pub orders: u64,
    /// The leader of terms 1 to `MAX_TERM`, `NOBODY` for none.
    pub leader_of: [u8; MAX_TERM as usize],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Suspect { s: u8, x: u8 },
    Trust { s: u8, x: u8 },
    PreVote { s: u8 },
    GrantPre { m: u8, c: u8 },
    ElectPre { c: u8 },
    GrantVote { m: u8, c: u8 },
    Win { c: u8 },
    Hear { m: u8, t: u8 },
    Learn { s: u8, m: u8 },
    Order { l: u8, h: u8 },
    TakeOrder { h: u8, o: u8 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    OneLeader,
    Stable,
    AskingCanWin,
    NoLeaderLeftNoQuorum,
    NoHandOver,
}

pub type Key = [u64; WORDS];

pub struct Suspicion {
    scope: Scope,
}

const PATHS: &[&str] = &[
    "leaders elected",
    "orders given",
    "orders taken, a campaign made",
    "orders declined: the heir could not be elected without the orderer",
    "pre-candidates that stopped asking",
    "settled leaders left no quorum",
];
const ELECTED: u64 = 1;
const ORDERED: u64 = 1 << 1;
const TAKEN: u64 = 1 << 2;
const DECLINED: u64 = 1 << 3;
const STOPPED_ASKING: u64 = 1 << 4;
const LEFT_NO_QUORUM: u64 = 1 << 5;

fn bit(s: usize) -> u8 {
    1 << s
}
fn has(mask: u8, s: usize) -> bool {
    s < 8 && mask & bit(s) != 0
}

/// An order: from, to, term (1-based), condition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Order {
    from: usize,
    to: usize,
    term: u8,
    cond: bool,
}

impl Order {
    fn bit(self) -> u64 {
        let index = ((self.from * MAX_SERVERS + self.to) * usize::from(MAX_TERM)
            + usize::from(self.term - 1))
            * 2
            + usize::from(self.cond);
        1 << index
    }
    fn at(index: usize) -> Self {
        let cond = index % 2 == 1;
        let rest = index / 2;
        let term = u8::try_from(rest % usize::from(MAX_TERM)).unwrap() + 1;
        let rest = rest / usize::from(MAX_TERM);
        Self {
            from: rest / MAX_SERVERS,
            to: rest % MAX_SERVERS,
            term,
            cond,
        }
    }
}
const ORDER_BITS: usize = MAX_SERVERS * MAX_SERVERS * MAX_TERM as usize * 2;

impl Suspicion {
    pub fn at(scope: Scope) -> Self {
        assert!(scope.servers <= MAX_SERVERS && scope.max_term <= MAX_TERM);
        Self { scope }
    }

    fn all(&self) -> u8 {
        u8::try_from((1usize << self.scope.servers) - 1).unwrap()
    }
    fn live(&self, s: usize) -> bool {
        s < self.scope.servers && !has(self.scope.stopped, s)
    }
    /// `Quorum(S)`: a majority of every member.
    fn quorum(&self, set: u8) -> bool {
        2 * (set & self.all()).count_ones() > self.all().count_ones()
    }
    fn tq(&self, state: &State, s: usize) -> bool {
        self.quorum(bit(s) | (self.all() & !state.suspects[s]))
    }
    fn elected_without(&self, state: &State, h: usize, o: usize) -> bool {
        self.quorum((bit(h) | (self.all() & !state.suspects[h])) & !bit(o))
    }
    fn trusts_leader(state: &State, s: usize) -> bool {
        state.leader[s] != NOBODY && !has(state.suspects[s], usize::from(state.leader[s]))
    }
    fn settled(&self, state: &State, l: usize) -> bool {
        self.live(l)
            && state.role[l] == LEADER
            && (0..self.scope.servers).filter(|s| self.live(*s)).all(|s| {
                state.suspects[s] & self.scope.stopped == self.scope.stopped
                    && (s == l
                        || (state.term[s] == state.term[l]
                            && state.role[s] == FOLLOWER
                            && usize::from(state.leader[s]) == l))
            })
    }
    /// `Lease(m, c, t)`.
    fn lease(state: &State, m: usize, c: usize, t: u8) -> bool {
        let forgets = state.role[m] != LEADER && usize::from(state.leader[m]) == c;
        let ldr = if forgets { NOBODY } else { state.leader[m] };
        state.role[m] == LEADER
            || (ldr != NOBODY
                && usize::from(ldr) != c
                && t <= state.term[m] + 1
                && !has(state.suspects[m], usize::from(ldr)))
    }
    /// `Follow(s, t, l)`.
    fn follow(next: &mut State, s: usize, t: u8, l: u8) {
        if t > next.term[s] {
            next.vote[s] = NOBODY;
        }
        next.term[s] = t;
        next.role[s] = FOLLOWER;
        next.leader[s] = l;
        next.left[s] = NOBODY;
        next.granted[s] = 0;
        next.kind[s] = ASKED;
    }

    fn suspect(&self, state: &State, s: usize, x: usize) -> Option<(State, u64)> {
        if !self.live(s) || x >= self.scope.servers || x == s || has(state.suspects[s], x) {
            return None;
        }
        let mut next = *state;
        next.suspects[s] |= bit(x);
        let tq = self.tq(&next, s);
        let mut paths = 0;
        if state.role[s] == LEADER && !tq && self.scope.rule == Rule::StepDown {
            next.role[s] = FOLLOWER;
            next.leader[s] = NOBODY;
        } else if state.role[s] == PRECANDIDATE && !tq && self.scope.rule != Rule::Asking {
            next.role[s] = FOLLOWER;
            next.leader[s] = state.left[s];
            next.left[s] = NOBODY;
            next.granted[s] = 0;
            paths |= STOPPED_ASKING;
        }
        Some((next, paths))
    }

    fn trust(&self, state: &State, s: usize, x: usize) -> Option<(State, u64)> {
        if !self.live(s) || !has(state.suspects[s], x) || !self.live(x) {
            return None;
        }
        let mut next = *state;
        next.suspects[s] &= !bit(x);
        Some((next, 0))
    }

    fn pre_vote(&self, state: &State, s: usize) -> Option<(State, u64)> {
        if !self.live(s)
            || state.role[s] == LEADER
            || Self::trusts_leader(state, s)
            || !self.tq(state, s)
            || state.term[s] >= self.scope.max_term
        {
            return None;
        }
        let mut next = *state;
        next.left[s] = match state.role[s] {
            FOLLOWER => state.leader[s],
            PRECANDIDATE => state.left[s],
            _ => NOBODY,
        };
        next.role[s] = PRECANDIDATE;
        next.leader[s] = NOBODY;
        next.granted[s] = bit(s);
        next.kind[s] = ASKED;
        Some((next, 0))
    }

    fn grant_pre(&self, state: &State, m: usize, c: usize) -> Option<(State, u64)> {
        if !self.live(c)
            || state.role[c] != PRECANDIDATE
            || !self.live(m)
            || m == c
            || has(state.granted[c], m)
        {
            return None;
        }
        let t = state.term[c] + 1;
        let mut next = *state;
        if t > state.term[m] {
            if Self::lease(state, m, c, t) {
                return None;
            }
            if state.role[m] != LEADER && usize::from(state.leader[m]) == c {
                next.leader[m] = NOBODY;
            }
        } else if t == state.term[m] {
            let can = usize::from(state.vote[m]) == c
                || (state.vote[m] == NOBODY && state.leader[m] == NOBODY);
            if !can {
                return None;
            }
        } else {
            return None;
        }
        next.granted[c] |= bit(m);
        Some((next, 0))
    }

    fn elect_pre(&self, state: &State, c: usize) -> Option<(State, u64)> {
        if !self.live(c) || state.role[c] != PRECANDIDATE || !self.quorum(state.granted[c]) {
            return None;
        }
        let mut next = *state;
        next.term[c] += 1;
        next.vote[c] = u8::try_from(c).unwrap();
        next.role[c] = CANDIDATE;
        next.leader[c] = NOBODY;
        next.left[c] = NOBODY;
        next.granted[c] = bit(c);
        next.kind[c] = ASKED;
        Some((next, 0))
    }

    fn grant_vote(&self, state: &State, m: usize, c: usize) -> Option<(State, u64)> {
        if !self.live(c)
            || state.role[c] != CANDIDATE
            || !self.live(m)
            || m == c
            || has(state.granted[c], m)
        {
            return None;
        }
        let t = state.term[c];
        let mut next = *state;
        if t > state.term[m] {
            if state.kind[c] == ASKED && Self::lease(state, m, c, t) {
                return None;
            }
            next.term[m] = t;
            next.vote[m] = u8::try_from(c).unwrap();
            next.role[m] = FOLLOWER;
            next.leader[m] = NOBODY;
            next.left[m] = NOBODY;
            next.granted[c] |= bit(m);
            next.granted[m] = 0;
            next.kind[m] = ASKED;
        } else if t == state.term[m] {
            let can = usize::from(state.vote[m]) == c
                || (state.vote[m] == NOBODY && state.leader[m] == NOBODY);
            if !can {
                return None;
            }
            next.vote[m] = u8::try_from(c).unwrap();
            next.granted[c] |= bit(m);
        } else {
            return None;
        }
        Some((next, 0))
    }

    fn win(&self, state: &State, c: usize) -> Option<(State, u64)> {
        if !self.live(c) || state.role[c] != CANDIDATE || !self.quorum(state.granted[c]) {
            return None;
        }
        let mut next = *state;
        next.role[c] = LEADER;
        next.leader[c] = u8::try_from(c).unwrap();
        next.granted[c] = 0;
        next.kind[c] = ASKED;
        let t = usize::from(state.term[c]) - 1;
        if next.leader_of[t] == NOBODY {
            next.leader_of[t] = u8::try_from(c).unwrap();
        }
        Some((next, ELECTED))
    }

    fn hear(&self, state: &State, m: usize, t: u8) -> Option<(State, u64)> {
        if t == 0 || t > self.scope.max_term {
            return None;
        }
        let l = state.leader_of[usize::from(t) - 1];
        if l == NOBODY || !self.live(usize::from(l)) || !self.live(m) || usize::from(l) == m {
            return None;
        }
        if t < state.term[m]
            || (state.role[m] == PRECANDIDATE
                && t == state.term[m]
                && has(state.suspects[m], usize::from(l)))
            || (state.role[m] == FOLLOWER && t == state.term[m] && state.leader[m] == l)
        {
            return None;
        }
        let mut next = *state;
        Self::follow(&mut next, m, t, l);
        Some((next, 0))
    }

    fn learn(&self, state: &State, s: usize, m: usize) -> Option<(State, u64)> {
        if !self.live(s)
            || !self.live(m)
            || s == m
            || state.role[s] == FOLLOWER
            || state.term[m] <= state.term[s]
        {
            return None;
        }
        let mut next = *state;
        Self::follow(&mut next, s, state.term[m], NOBODY);
        Some((next, 0))
    }

    fn order(&self, state: &State, l: usize, h: usize) -> Option<(State, u64)> {
        if !self.live(l) || h >= self.scope.servers || h == l || self.tq(state, l) {
            return None;
        }
        let allowed = if self.scope.rule == Rule::StepDown {
            state.role[l] == FOLLOWER
                && state.term[l] >= 1
                && usize::from(state.leader_of[usize::from(state.term[l]) - 1]) == l
        } else {
            state.role[l] == LEADER
        };
        if !allowed {
            return None;
        }
        let o = Order {
            from: l,
            to: h,
            term: state.term[l],
            cond: matches!(self.scope.rule, Rule::Core | Rule::Asking),
        };
        if state.orders & o.bit() != 0 {
            return None;
        }
        let mut next = *state;
        next.orders |= o.bit();
        Some((next, ORDERED))
    }

    fn take_order(&self, state: &State, h: usize, index: usize) -> Option<(State, u64)> {
        if index >= ORDER_BITS || state.orders & (1 << index) == 0 {
            return None;
        }
        let o = Order::at(index);
        if o.to != h || !self.live(h) {
            return None;
        }
        let ripe = o.term > state.term[h]
            || (o.term == state.term[h] && matches!(state.role[h], FOLLOWER | PRECANDIDATE));
        if !ripe {
            return None;
        }
        let takes = !o.cond || self.elected_without(state, h, o.from);
        let mut next = *state;
        if takes && o.term < self.scope.max_term {
            next.term[h] = o.term + 1;
            next.vote[h] = u8::try_from(h).unwrap();
            next.role[h] = CANDIDATE;
            next.leader[h] = NOBODY;
            next.left[h] = NOBODY;
            next.granted[h] = bit(h);
            next.kind[h] = if o.cond { CONDITION } else { ORDER };
            return Some((next, TAKEN));
        }
        if o.term <= state.term[h] {
            return None;
        }
        Self::follow(&mut next, h, o.term, NOBODY);
        Some((next, if takes { 0 } else { DECLINED }))
    }

    fn violation(&self, before: &State, state: &State) -> Option<Fault> {
        let n = self.scope.servers;
        for l in 0..n {
            for m in 0..n {
                if l != m
                    && state.role[l] == LEADER
                    && state.role[m] == LEADER
                    && state.term[l] == state.term[m]
                {
                    return Some(Fault::OneLeader);
                }
            }
        }
        if self.scope.stable {
            for l in 0..n {
                if self.settled(before, l)
                    && (!self.settled(state, l) || state.term[l] != before.term[l])
                {
                    return Some(Fault::Stable);
                }
            }
        }
        if (0..n).any(|s| self.live(s) && state.role[s] == PRECANDIDATE && !self.tq(state, s)) {
            return Some(Fault::AskingCanWin);
        }
        if self.scope.reached && (0..n).any(|l| self.settled(state, l) && !self.tq(state, l)) {
            return Some(Fault::NoLeaderLeftNoQuorum);
        }
        if self.scope.hand_over
            && (0..n).any(|c| {
                self.live(c)
                    && state.role[c] == CANDIDATE
                    && state.kind[c] == CONDITION
                    && self.quorum(state.granted[c])
            })
        {
            return Some(Fault::NoHandOver);
        }
        None
    }

    fn pack(&self, state: &State) -> Key {
        let mut p = Packer::<WORDS>::new();
        let mut put = |width: usize, value: u64| p.put(width, value).unwrap();
        for s in 0..MAX_SERVERS {
            put(2, u64::from(state.term[s]));
            put(2, u64::from(state.vote[s]));
            put(2, u64::from(state.role[s]));
            put(2, u64::from(state.leader[s]));
            put(2, u64::from(state.left[s]));
            put(3, u64::from(state.suspects[s]));
            put(3, u64::from(state.granted[s]));
            put(2, u64::from(state.kind[s]));
        }
        put(ORDER_BITS, state.orders);
        for t in 0..usize::from(MAX_TERM) {
            put(2, u64::from(state.leader_of[t]));
        }
        p.words()
    }

    fn unpacked(&self, key: &Key) -> State {
        let mut p = Packer::<WORDS>::over(*key);
        let mut take = |width: usize| p.take(width).unwrap();
        let small = |value: u64| u8::try_from(value).unwrap();
        let mut state = self.initial();
        for s in 0..MAX_SERVERS {
            state.term[s] = small(take(2));
            state.vote[s] = small(take(2));
            state.role[s] = small(take(2));
            state.leader[s] = small(take(2));
            state.left[s] = small(take(2));
            state.suspects[s] = small(take(3));
            state.granted[s] = small(take(3));
            state.kind[s] = small(take(2));
        }
        state.orders = take(ORDER_BITS);
        for t in 0..usize::from(MAX_TERM) {
            state.leader_of[t] = small(take(2));
        }
        state
    }
}

impl Model for Suspicion {
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
            role: [FOLLOWER; MAX_SERVERS],
            leader: [NOBODY; MAX_SERVERS],
            left: [NOBODY; MAX_SERVERS],
            suspects: [0; MAX_SERVERS],
            granted: [0; MAX_SERVERS],
            kind: [ASKED; MAX_SERVERS],
            orders: 0,
            leader_of: [NOBODY; MAX_TERM as usize],
        }
    }

    fn actions(&self, state: &State, out: &mut Vec<Action>) {
        let n = self.scope.servers;
        let small = |x: usize| u8::try_from(x).unwrap();
        for s in 0..n {
            for x in 0..n {
                out.push(Action::Suspect {
                    s: small(s),
                    x: small(x),
                });
                out.push(Action::Trust {
                    s: small(s),
                    x: small(x),
                });
                out.push(Action::GrantPre {
                    m: small(s),
                    c: small(x),
                });
                out.push(Action::GrantVote {
                    m: small(s),
                    c: small(x),
                });
                out.push(Action::Learn {
                    s: small(s),
                    m: small(x),
                });
                out.push(Action::Order {
                    l: small(s),
                    h: small(x),
                });
            }
            out.push(Action::PreVote { s: small(s) });
            out.push(Action::ElectPre { c: small(s) });
            out.push(Action::Win { c: small(s) });
            for t in 1..=self.scope.max_term {
                out.push(Action::Hear { m: small(s), t });
            }
        }
        let mut orders = state.orders;
        while orders != 0 {
            let index = orders.trailing_zeros() as usize;
            orders &= orders - 1;
            let o = Order::at(index);
            out.push(Action::TakeOrder {
                h: small(o.to),
                o: small(index),
            });
        }
    }

    fn apply(&self, state: &State, action: Action) -> Option<Step<State, Fault>> {
        let u = usize::from;
        let (next, mut paths) = match action {
            Action::Suspect { s, x } => self.suspect(state, u(s), u(x)),
            Action::Trust { s, x } => self.trust(state, u(s), u(x)),
            Action::PreVote { s } => self.pre_vote(state, u(s)),
            Action::GrantPre { m, c } => self.grant_pre(state, u(m), u(c)),
            Action::ElectPre { c } => self.elect_pre(state, u(c)),
            Action::GrantVote { m, c } => self.grant_vote(state, u(m), u(c)),
            Action::Win { c } => self.win(state, u(c)),
            Action::Hear { m, t } => self.hear(state, u(m), t),
            Action::Learn { s, m } => self.learn(state, u(s), u(m)),
            Action::Order { l, h } => self.order(state, u(l), u(h)),
            Action::TakeOrder { h, o } => self.take_order(state, u(h), u(o)),
        }?;
        if next == *state {
            return None;
        }
        if (0..self.scope.servers).any(|l| self.settled(&next, l) && !self.tq(&next, l)) {
            paths |= LEFT_NO_QUORUM;
        }
        Some(Step {
            state: next,
            fault: self.violation(state, &next),
            paths,
        })
    }

    fn canonical(&self, state: &State) -> Key {
        self.pack(state)
    }

    fn unpack(&self, key: &Key) -> State {
        self.unpacked(key)
    }
}
