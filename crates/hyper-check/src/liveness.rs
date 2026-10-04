//! Liveness (`docs/sim.md` §4.2): every run ends in its liveness phase and must converge, and the
//! bound it is held to is neither a literal nor a confidence someone picks.
//!
//! Raft does not bound the elections a group needs: split votes repeat with nonzero probability
//! (Ongaro's thesis §9.3, elections geometric in `Pr(split)`), so any fixed count of elections is a
//! false-failure rate chosen by hand. The phase waits on progress instead: it goes on while any
//! member's term, commit, applied index or last index moves ([`Progress`]), and fails once a
//! [`Quiet`] period passes in which none moved — a period within which a live group elects or
//! starts a new term, from the members' own settings, so a period without movement is a stuck
//! group, not a split vote. A group that keeps moving without converging is ended by the run's own
//! step budget, unconverged.
//!
//! A liveness property that is not convergence is a [`Monitor`] with hot and cold states
//! (Deligiannis et al., FAST 2016, §2.5): each obligation raised makes it hot until it is met, and
//! a run that ends its bound with one open fails ("we consider an execution longer than a large
//! user-supplied bound as an 'infinite' execution"; here the bound is the run's own).

use std::collections::BTreeMap;
use std::fmt;

/// A sum or product past `u64`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Overflow;

impl fmt::Display for Overflow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a quiet period past u64")
    }
}

impl std::error::Error for Overflow {}

/// What a member's settings say of how long its group takes to move, in the ordered discipline's
/// nanoseconds (`docs/timing.md` §2.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Laws {
    /// The detector's stated detection time: within it a member suspects a leader that stopped
    /// (zero where the harness itself says what the detectors suspect).
    pub detection_ns: u64,
    /// The election law's span: the longest randomized delay before a member campaigns.
    pub span_ns: u64,
    /// The election's vote rounds, each a round trip and a flush (a pre-vote, a vote, and the
    /// leader's first append in hyper-raft's elections by suspicion).
    pub vote_rounds_ns: u64,
    /// A replication round: within it an entry a leader took is held by a quorum and committed.
    pub replication_ns: u64,
}

/// How long a group may move nothing before it is stuck: in nanoseconds under the ordered
/// discipline, in rounds of one tick of each member under the free one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Quiet {
    /// The period.
    pub period: u64,
}

impl Quiet {
    /// Under the ordered discipline: the detection time, one election round (the longest
    /// randomized delay of the election law's span and its vote rounds) and a replication round.
    pub fn ordered(laws: &Laws) -> Result<Self, Overflow> {
        let period = laws
            .detection_ns
            .checked_add(laws.span_ns)
            .and_then(|sum| sum.checked_add(laws.vote_rounds_ns))
            .and_then(|sum| sum.checked_add(laws.replication_ns))
            .ok_or(Overflow)?;
        Ok(Self { period })
    }

    /// [`Quiet::ordered`] in rounds of `round_ns`, a round being one tick of each member's clock:
    /// the period rounded up to whole rounds.
    pub fn in_rounds(laws: &Laws, round_ns: u64) -> Result<Self, Overflow> {
        let period = Self::ordered(laws)?.period.div_ceil(round_ns.max(1));
        Ok(Self { period })
    }

    /// Under the free discipline, where a tick stands for time: twice the longest timeout a member
    /// draws (`2 · election_tick − 1` ticks, its timeouts drawn from `[election_tick,
    /// 2 · election_tick)`) with its `patience` past it, and the round its messages are delivered
    /// in. Within one such timeout every member's timer fires and its campaign ends its lease on a
    /// leader; within the second every member has campaigned with no lease left; a pre-vote's
    /// answers then turn on the voters' terms, logs and priorities, which a quiet group does not
    /// move, so a group no member won by then wins none later (hyper-raft's `Cluster::settles`).
    pub fn ticks(election_tick: u64, patience: u64) -> Result<Self, Overflow> {
        let longest = election_tick
            .checked_mul(2)
            .and_then(|ticks| ticks.checked_sub(1))
            .and_then(|ticks| ticks.checked_add(patience))
            .ok_or(Overflow)?;
        let period = longest
            .checked_mul(2)
            .and_then(|ticks| ticks.checked_add(1))
            .ok_or(Overflow)?;
        Ok(Self { period })
    }
}

/// Where a member is: what [`Progress`] watches move.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Position {
    /// Its term.
    pub term: u64,
    /// Its commit.
    pub commit: u64,
    /// The index it applied through.
    pub applied: u64,
    /// Its log's last index.
    pub last: u64,
}

/// The members' positions and when any last moved.
#[derive(Clone, Debug)]
pub struct Progress {
    seen: BTreeMap<u64, Position>,
    moved: u64,
    quiet: Quiet,
}

impl Progress {
    /// Watching from `now`, stuck after `quiet` with nothing moving.
    pub fn new(quiet: Quiet, now: u64) -> Self {
        Self {
            seen: BTreeMap::new(),
            moved: now,
            quiet,
        }
    }

    /// `member` is at `position` at `now`: true when it moved (a member first seen moved).
    pub fn observe(&mut self, now: u64, member: u64, position: Position) -> bool {
        let moved = self.seen.insert(member, position) != Some(position);
        if moved {
            self.moved = self.moved.max(now);
        }
        moved
    }

    /// Whether a quiet period passed at `now` with nothing moving.
    pub fn stuck(&self, now: u64) -> bool {
        now.saturating_sub(self.moved) > self.quiet.period
    }

    /// When anything last moved.
    pub fn moved(&self) -> u64 {
        self.moved
    }
}

/// The obligations a monitor still holds open when its run ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hot<K> {
    /// Each open obligation, with when it was raised.
    pub open: Vec<(K, u64)>,
}

impl<K: fmt::Debug> fmt::Display for Hot<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "the run ended hot: {} obligations open", self.open.len())?;
        if let Some((key, since)) = self.open.first() {
            write!(f, ", the first {key:?} since {since}")?;
        }
        Ok(())
    }
}

impl<K: fmt::Debug> std::error::Error for Hot<K> {}

/// A monitor already holds as many obligations as its bound.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Full {
    /// The bound.
    pub bound: usize,
}

/// A liveness monitor (P#'s): hot while an obligation is open, cold when none is.
#[derive(Clone, Debug)]
pub struct Monitor<K> {
    open: BTreeMap<K, u64>,
    met: u64,
    bound: usize,
}

impl<K: Ord + Clone> Monitor<K> {
    /// A monitor holding at most `bound` obligations open at once.
    pub fn new(bound: usize) -> Self {
        Self {
            open: BTreeMap::new(),
            met: 0,
            bound,
        }
    }

    /// The obligation `key` arose at `now`; one raised again keeps when it first arose.
    pub fn raise(&mut self, key: K, now: u64) -> Result<(), Full> {
        if self.open.contains_key(&key) {
            return Ok(());
        }
        if self.open.len() >= self.bound {
            return Err(Full { bound: self.bound });
        }
        self.open.insert(key, now);
        Ok(())
    }

    /// The obligation `key` was met; true when it was open.
    pub fn meet(&mut self, key: &K) -> bool {
        let was = self.open.remove(key).is_some();
        if was {
            self.met = self.met.saturating_add(1);
        }
        was
    }

    /// Whether an obligation is open.
    pub fn is_hot(&self) -> bool {
        !self.open.is_empty()
    }

    /// Obligations met so far.
    pub fn met(&self) -> u64 {
        self.met
    }

    /// The run ended its bound: cold, or the obligations still open.
    pub fn end(&self) -> Result<(), Hot<K>> {
        if self.open.is_empty() {
            return Ok(());
        }
        Err(Hot {
            open: self
                .open
                .iter()
                .map(|(key, since)| (key.clone(), *since))
                .collect(),
        })
    }
}
