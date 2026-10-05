//! Durability (`docs/durable.md` §3, I1–I8 and R-6): no output leaves before the sender's durable
//! state supports it. Stated over a member's durable state `D` — what a restart would read — at the
//! moment an output leaves, which the harness lends as a [`DurableView`]: hyper-raft's `Lagged`
//! members, hyper-durable's shell and mantle's replica are held to the one statement.
//!
//! - **I1 (promises).** A message that carries a term leaves only once `D` holds that term (a
//!   pre-vote asks of a term no member takes). In `D`'s term, a vote request only with `D`'s vote
//!   its own and naming a last entry `D` holds or `D`'s log at least as up to date as the one it
//!   names, and a vote given only with `D`'s vote the candidate's (thesis §3.8, §3.6.1).
//! - **I2 (acknowledgements).** An acknowledgement of index `i` in `D`'s term leaves only once `D`
//!   holds an entry at `i` of no later term (what that term's leader sent) or a snapshot past it; the
//!   fast track's word of what a member holds only once `D` holds it, beside its log or in it, or
//!   its log reaches the index (a leader's entry supersedes what was held beside it).
//! - **R-6 (answers).** An answer states no commit beyond the commit `D` states.
//! - **I3 (self-count).** A leader commits an index only once a majority of each half of a
//!   configuration that decided it holds the entry durably (thesis §10.2.1: it may count itself only
//!   for what its own `D` holds, and may commit by a majority of followers alone).
//! - **I4 (apply).** An entry is applied only once committed, and durable at the member, but for a
//!   leader's own entries of its term where it applies before its write is durable
//!   (`docs/durable.md` §4.2).
//! - **I5 (restart-acted state).** A change of configuration, or an entry the state machine acts on
//!   at its next start, is applied only once `D` states a commit covering it, or the state machine
//!   holds it durably (§4.1).
//! - **I7 (order).** A commit a write states names only entries that write or an earlier one holds.
//! - **I8 (start).** The log's start never passes what the state machine holds durably.

use super::Violation;

/// Which rule of `docs/durable.md` §3 an output broke.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rule {
    /// Promises: a term or vote not durable.
    I1,
    /// Acknowledgements: an entry not durable.
    I2,
    /// Self-count: a commit no majority holds durably.
    I3,
    /// Apply: an entry not committed, or not durable here.
    I4,
    /// Restart-acted state: applied before a durable commit covered it.
    I5,
    /// Order: a durable commit past what the device holds.
    I7,
    /// Start: the log's start past the state machine's durable point.
    I8,
    /// Answers: a commit stated past the durable one.
    R6,
}

/// A member's durable state, as its device holds it: what a restart would read.
pub trait DurableView<V> {
    /// The term.
    fn term(&self) -> u64;
    /// The vote of that term, zero for none.
    fn vote(&self) -> u64;
    /// The commit stated.
    fn commit(&self) -> u64;
    /// The index through which a snapshot alone is held.
    fn start(&self) -> u64;
    /// The last index held, in the log or by the snapshot.
    fn last(&self) -> u64;
    /// The term of the entry held at `index`: the snapshot's at the start.
    fn term_at(&self, index: u64) -> Option<u64>;
    /// Whether the entry stating `value` at `index` is held: in the log, beside it (approved by
    /// itself), under the snapshot, or, where a fault at rest took it after it was acknowledged,
    /// marked as held (hyper-log's uncertainty mark).
    fn holds(&self, index: u64, value: &V) -> bool;
}

/// What a message says, as I1 and I2 judge it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Says<'a, V> {
    /// A candidate asks for votes, naming its last entry.
    VoteRequest {
        /// The last entry's index.
        last_index: u64,
        /// Its term.
        last_term: u64,
    },
    /// A vote, given or refused.
    Vote {
        /// Whether it is given.
        granted: bool,
    },
    /// An acknowledgement that the log holds the leader's entries through `index`.
    Acknowledges {
        /// The index.
        index: u64,
    },
    /// The fast track's word of what the member holds: entries by index.
    Holds {
        /// The entries held.
        entries: &'a [(u64, V)],
    },
    /// Nothing more than its term.
    Nothing,
}

/// A message as it leaves its sender.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Released<'a, V> {
    /// The sender.
    pub from: u64,
    /// The receiver.
    pub to: u64,
    /// The term it carries; `None` for a pre-vote's, which no member takes.
    pub term: Option<u64>,
    /// What it says.
    pub says: Says<'a, V>,
    /// The commit it states, where it is an answer that states one (R-6).
    pub commit: Option<u64>,
}

/// The durability oracle. It keeps no table: each check is against the views the harness lends at
/// the moment it observes, and counts what it held.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Durability {
    /// Messages held to their senders' devices.
    pub released: u64,
    /// Leaders' commits held to their voters' devices.
    pub commits: u64,
    /// Entries applied held to their members' devices.
    pub applied: u64,
    /// Restart-acted entries held to the durable commit.
    pub fenced: u64,
}

fn broke(member: u64, rule: Rule, at: u64, held: u64) -> Violation {
    Violation::Durability {
        member,
        rule,
        at,
        held,
    }
}

impl Durability {
    /// I1, I2 and R-6 on `message`, leaving its sender with `view` durable.
    pub fn released<V, D: DurableView<V>>(
        &mut self,
        view: &D,
        message: &Released<'_, V>,
    ) -> Result<(), Violation> {
        self.released = self.released.saturating_add(1);
        let from = message.from;
        if let Some(commit) = message.commit
            && commit > view.commit()
        {
            return Err(broke(from, Rule::R6, commit, view.commit()));
        }
        let Some(term) = message.term else {
            return Ok(());
        };
        if term > view.term() {
            return Err(broke(from, Rule::I1, term, view.term()));
        }
        // A message of an older term than the device's was superseded by what the member did
        // since: it is as late as the network may make any message (thesis §3.3).
        if term < view.term() {
            return Ok(());
        }
        said(view, message)
    }

    /// I3: `leader` committed the entry stating `value` at `index`; it is held durably by a
    /// majority of each half of one of `configurations` (the one it decided by: before the step,
    /// or one a change applied in it), each a list of halves of voters, with `view` lending each
    /// voter's device.
    pub fn committed<'v, V, D: DurableView<V> + 'v>(
        &mut self,
        leader: u64,
        index: u64,
        value: &V,
        configurations: &[&[&[u64]]],
        view: impl Fn(u64) -> Option<&'v D>,
    ) -> Result<(), Violation> {
        self.commits = self.commits.saturating_add(1);
        let held_by = |half: &[u64]| {
            half.iter()
                .filter(|voter| view(**voter).is_some_and(|device| device.holds(index, value)))
                .count()
        };
        let decided = configurations.iter().any(|halves| {
            halves
                .iter()
                .all(|half| half.is_empty() || held_by(half).saturating_mul(2) > half.len())
        });
        if decided {
            return Ok(());
        }
        Err(broke(leader, Rule::I3, index, 0))
    }

    /// I3's self-count: a leader counts itself toward the entry at `index` only once `view`, its
    /// own device, holds it.
    pub fn counted_self<V, D: DurableView<V>>(
        &mut self,
        leader: u64,
        view: &D,
        index: u64,
        value: &V,
    ) -> Result<(), Violation> {
        if view.holds(index, value) {
            return Ok(());
        }
        Err(broke(leader, Rule::I3, index, view.last()))
    }

    /// I4: `member` applied the entry stating `value` at `index`, with `committed` the commit it
    /// knew and `view` its device; `own_ahead` where it leads, the entry is of its term, and it
    /// applies its own entries before its write of them is durable.
    pub fn applied<V, D: DurableView<V>>(
        &mut self,
        member: u64,
        view: &D,
        index: u64,
        value: &V,
        committed: u64,
        own_ahead: bool,
    ) -> Result<(), Violation> {
        self.applied = self.applied.saturating_add(1);
        if index > committed {
            return Err(broke(member, Rule::I4, index, committed));
        }
        if view.holds(index, value) || own_ahead {
            return Ok(());
        }
        Err(broke(member, Rule::I4, index, view.last()))
    }

    /// I5: `member` applied, at `index`, a change of configuration or an entry its state machine
    /// acts on at start, with `view` its device and `machine` the index its state machine holds
    /// durably.
    pub fn fenced<V, D: DurableView<V>>(
        &mut self,
        member: u64,
        view: &D,
        index: u64,
        machine: u64,
    ) -> Result<(), Violation> {
        self.fenced = self.fenced.saturating_add(1);
        let durable = view.commit().max(machine);
        if index <= durable {
            return Ok(());
        }
        Err(broke(member, Rule::I5, index, durable))
    }

    /// I7: a write of `member`'s became durable, leaving `view`: the commit it states is of
    /// entries it holds.
    pub fn written<V, D: DurableView<V>>(
        &mut self,
        member: u64,
        view: &D,
    ) -> Result<(), Violation> {
        if view.commit() <= view.last() {
            return Ok(());
        }
        Err(broke(member, Rule::I7, view.commit(), view.last()))
    }

    /// I8: `member`'s log, `view`, starts no later than `machine`, the index its state machine
    /// holds durably.
    pub fn start<V, D: DurableView<V>>(
        &mut self,
        member: u64,
        view: &D,
        machine: u64,
    ) -> Result<(), Violation> {
        if view.start() <= machine {
            return Ok(());
        }
        Err(broke(member, Rule::I8, view.start(), machine))
    }
}

/// I1 and I2 on what `message`, of `view`'s term, says.
fn said<V, D: DurableView<V>>(view: &D, message: &Released<'_, V>) -> Result<(), Violation> {
    let from = message.from;
    match message.says {
        Says::VoteRequest {
            last_index,
            last_term,
        } => {
            if view.vote() != from {
                return Err(broke(from, Rule::I1, view.term(), view.vote()));
            }
            // The request names a last entry the device holds, or the device's log has since
            // become at least as up to date as the one named (a later write of the term, the
            // term's leader's append, cut the log the request was made from): what voters judge
            // a request by is that claim (thesis §3.6.1), and a device at least as current holds
            // every entry a voter granting it could require.
            let named = last_index == 0 || view.term_at(last_index) == Some(last_term);
            let last = view.last();
            let current = (view.term_at(last).unwrap_or(0), last) >= (last_term, last_index);
            if !named && !current {
                return Err(broke(from, Rule::I1, last_index, view.last()));
            }
            Ok(())
        }
        Says::Vote { granted: true } if view.vote() != message.to => {
            Err(broke(from, Rule::I1, view.term(), view.vote()))
        }
        Says::Acknowledges { index } => {
            let term = view.term();
            let held = index <= view.start() || view.term_at(index).is_some_and(|at| at <= term);
            if held {
                return Ok(());
            }
            Err(broke(from, Rule::I2, index, view.last()))
        }
        Says::Holds { entries } => {
            for (index, value) in entries {
                if !view.holds(*index, value) && view.last() < *index {
                    return Err(broke(from, Rule::I2, *index, view.last()));
                }
            }
            Ok(())
        }
        Says::Vote { .. } | Says::Nothing => Ok(()),
    }
}
