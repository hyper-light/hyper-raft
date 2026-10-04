//! The oracles (`docs/sim.md` §4.1): what a run must keep after every step, over its whole history
//! and not only at its end (slates' rule: "ever, not only among the current roles").
//!
//! Each oracle is written against its specification and not an implementation (focal's 06 §1:
//! "keep the oracle ... simple and independent"): its statement and its source are in its module.
//! A harness feeds each the observations it makes — a leader of a term, an entry a log holds, an
//! entry committed or applied, a vote cast, a message released with what its sender's device held,
//! a read asked and answered, a write acknowledged and applied — and an oracle answers each with
//! `Ok` or the [`Violation`] it found. Every table an oracle keeps has the bound its harness states
//! for the run, and refuses past it ([`Violation::Full`]).
//!
//! | Oracle | Statement | Source |
//! |---|---|---|
//! | [`ElectionSafety`] | at most one leader per term, ever | Raft thesis Fig. 3.2 |
//! | [`LogMatching`] | two logs with an entry of one term at an index agree through it; terms never decrease along a log | thesis Fig. 3.2; slates' explorer |
//! | [`LeaderCompleteness`] | a leader holds every entry committed before it, at its index | thesis Fig. 3.2; TLA+ `LeaderHolds` |
//! | [`StateMachineSafety`] | one entry committed per index, compared by what it states | thesis Fig. 3.2; hyper-raft `chosen` |
//! | [`FastAgreement`] | no two values chosen at one index, from a ghost of every vote cast | Fast Paxos §3.3; slates' explorer |
//! | [`Durability`] | no output leaves before the sender's durable state supports it | `docs/durable.md` §3, I1–I8 and R-6 |
//! | [`ReadSafety`] | a read is answered at or above the commit known when it was asked | thesis §6.4 |
//! | [`ExactlyOnce`] | every acknowledged write applied once, and by every member | thesis §6.3; mantle, focal `DuplicateCommit` |
//! | [`SameHistory`] | every member that applied an index reached the same state there | mantle's rows; TigerBeetle's identical replicas |

mod durable;
mod election;
mod fast;
mod matching;
mod once;
mod reads;
mod same;

pub use durable::{Durability, DurableView, Released, Rule, Says};
pub use election::ElectionSafety;
pub use fast::FastAgreement;
pub use matching::{LeaderCompleteness, LogMatching, LogView, StateMachineSafety, Terms};
pub use once::ExactlyOnce;
pub use reads::ReadSafety;
pub use same::SameHistory;

use std::fmt;

/// What a run broke.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Violation {
    /// Election Safety: two members led one term.
    TwoLeaders {
        /// The term.
        term: u64,
        /// The member seen leading it first.
        first: u64,
        /// The member seen leading it since.
        second: u64,
    },
    /// Log Matching: a log holds an entry of a term at an index that states otherwise, or follows
    /// another term, than the entry of that term at that index another log held.
    LogsDisagree {
        /// The member whose log disagrees.
        member: u64,
        /// The index.
        index: u64,
        /// The term.
        term: u64,
    },
    /// Log Matching: a log's terms decrease.
    TermsDecrease {
        /// The member.
        member: u64,
        /// The index whose term is less than the one before it.
        index: u64,
        /// The term before it.
        before: u64,
        /// Its term.
        term: u64,
    },
    /// Leader Completeness: a leader lacks a committed entry.
    LeaderLacks {
        /// The leader.
        member: u64,
        /// Its term.
        term: u64,
        /// The committed index it does not hold as committed.
        index: u64,
    },
    /// State Machine Safety: a member committed another entry at an index.
    TwoCommitted {
        /// The member.
        member: u64,
        /// The index.
        index: u64,
    },
    /// Fast agreement: two values chosen at one index.
    TwoChosen {
        /// The index.
        index: u64,
        /// The term whose votes chose the second.
        term: u64,
    },
    /// Fast agreement: a voter voted two values at one index in one term.
    VoteChanged {
        /// The voter.
        member: u64,
        /// The term.
        term: u64,
        /// The index.
        index: u64,
    },
    /// Durability: an output left before the sender's durable state supported it.
    Durability {
        /// The sender, or the member that decided.
        member: u64,
        /// The rule broken.
        rule: Rule,
        /// The index or term the output named.
        at: u64,
        /// What the device held instead.
        held: u64,
    },
    /// Read safety: a read answered below the commit known when it was asked.
    StaleRead {
        /// The member that answered.
        member: u64,
        /// The read.
        read: u64,
        /// The index it was answered at.
        index: u64,
        /// The commit known when it was asked.
        floor: u64,
    },
    /// Read safety: a read answered that was never asked.
    UnaskedRead {
        /// The member that answered.
        member: u64,
        /// The read.
        read: u64,
    },
    /// Exactly once: a write applied at two indexes.
    AppliedTwice {
        /// The member that applied it the second time.
        member: u64,
        /// The write.
        write: u64,
        /// The index it was applied at first.
        first: u64,
        /// The other.
        second: u64,
    },
    /// Exactly once: an acknowledged write a member of the settled group never applied.
    NeverApplied {
        /// The member.
        member: u64,
        /// The write.
        write: u64,
    },
    /// Same history: a member reached another state at an index than the first member that
    /// applied it.
    HistoriesDiffer {
        /// The member.
        member: u64,
        /// The index.
        index: u64,
    },
    /// An oracle's table reached the bound its harness stated.
    Full {
        /// The oracle.
        oracle: &'static str,
        /// The bound.
        bound: usize,
    },
}

impl Violation {
    /// The oracle that found it.
    pub fn oracle(&self) -> &'static str {
        match self {
            Self::TwoLeaders { .. } => "Election Safety",
            Self::LogsDisagree { .. } | Self::TermsDecrease { .. } => "Log Matching",
            Self::LeaderLacks { .. } => "Leader Completeness",
            Self::TwoCommitted { .. } => "State Machine Safety",
            Self::TwoChosen { .. } | Self::VoteChanged { .. } => "Fast agreement",
            Self::Durability { .. } => "Durability",
            Self::StaleRead { .. } | Self::UnaskedRead { .. } => "Read safety",
            Self::AppliedTwice { .. } | Self::NeverApplied { .. } => "Exactly once",
            Self::HistoriesDiffer { .. } => "Same history",
            Self::Full { oracle, .. } => oracle,
        }
    }
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: ", self.oracle())?;
        match self {
            Self::TwoLeaders {
                term,
                first,
                second,
            } => write!(f, "members {first} and {second} both led term {term}"),
            Self::LogsDisagree {
                member,
                index,
                term,
            } => write!(
                f,
                "member {member}'s entry of term {term} at {index} is not the one another log held"
            ),
            Self::TermsDecrease {
                member,
                index,
                before,
                term,
            } => write!(
                f,
                "member {member}'s log holds term {term} at {index} after term {before}"
            ),
            Self::LeaderLacks {
                member,
                term,
                index,
            } => write!(
                f,
                "member {member}, leading term {term}, lacks the entry committed at {index}"
            ),
            Self::TwoCommitted { member, index } => {
                write!(f, "member {member} committed another entry at {index}")
            }
            Self::TwoChosen { index, term } => {
                write!(f, "votes of term {term} chose a second value at {index}")
            }
            Self::VoteChanged {
                member,
                term,
                index,
            } => write!(
                f,
                "member {member} voted two values at {index} in term {term}"
            ),
            Self::Durability {
                member,
                rule,
                at,
                held,
            } => write!(
                f,
                "member {member} broke {rule:?} at {at} with {held} durable"
            ),
            Self::StaleRead {
                member,
                read,
                index,
                floor,
            } => write!(
                f,
                "member {member} answered read {read} at {index}, asked when {floor} was committed"
            ),
            Self::UnaskedRead { member, read } => {
                write!(
                    f,
                    "member {member} answered read {read}, which was never asked"
                )
            }
            Self::AppliedTwice {
                member,
                write,
                first,
                second,
            } => write!(
                f,
                "member {member} applied write {write} at {second}, applied at {first} already"
            ),
            Self::NeverApplied { member, write } => {
                write!(
                    f,
                    "member {member} never applied acknowledged write {write}"
                )
            }
            Self::HistoriesDiffer { member, index } => {
                write!(f, "member {member} reached another state at {index}")
            }
            Self::Full { bound, .. } => write!(f, "its table reached its bound of {bound}"),
        }
    }
}

impl std::error::Error for Violation {}

/// `table` has room for one more within `bound`, or the oracle refuses.
fn room(len: usize, bound: usize, oracle: &'static str) -> Result<(), Violation> {
    if len >= bound {
        return Err(Violation::Full { oracle, bound });
    }
    Ok(())
}
