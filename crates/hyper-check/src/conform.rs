//! The TLA+ abstraction (`docs/sim.md` §4.5, "Coverage, defined"): a group's members read as the
//! variables of `docs/models/FastTrack.tla` — each member's `term`, `vote`, `role`, `log`, `held`,
//! `commit` and `classic` — so that every step of a simulated run can be held to the model, and the
//! abstract states a campaign reaches are its coverage.
//!
//! **The conformance check** ([`conforms`]). Every action of the model's `Next` keeps a set of
//! relations between a member's variables before and after it, and so does any sequence of its
//! actions: a step of the implementation that breaks one is a step no run of the model takes
//! (SandTable's and Mocket's idea, run on the implementation's own schedules). The relations, each
//! with the actions that keep it:
//!
//! - a member's term never falls (`Elect` and `Replicate` raise it; nothing lowers it);
//! - within a term a vote, once cast, stays (`Elect` votes only for a member at a lower term or at
//!   `t` with no vote; `Replicate` keeps the vote within the term);
//! - its commit never falls, and what it committed never changes (`Replicate` takes from
//!   `Max(p, commit[m]) + 1`; only `Lose`, a fault at rest, cuts it, which the harness names);
//! - no entry of its log bears a term above its own (`Take` and `Elect` write the member's term;
//!   `Replicate` sets the member's term to the leader's);
//! - what it knows committed by a classic quorum never falls (`ClassicCommit` and `Replicate`
//!   raise it to what the leader knows; only `Lose` cuts it). The model's `classic <= commit` is
//!   of a member's own state, and is not checked here: the abstraction reads the durable commit,
//!   which lags the member's own (a commit the member gives its owner outside a hard state need
//!   not be durable, `LightReady::commit_index`), and what it released may reach storage first;
//! - what it holds by itself is above what it knows committed by a classic quorum, and it lets a
//!   holding go only once it knows its index so committed (`Release`, `ReleaseBy`);
//! - a member that becomes leader voted for itself in its term (`Elect`);
//! - a leader that stays leader in its term keeps its log and only adds to it, with entries of its
//!   term (`Take`; no action rewrites a leader's log in its term).
//!
//! A member that crashed and restarted has lost only what was not durable: the abstraction reads
//! durable state (its device's term, vote, log, commit, holdings and what it released), so a
//! restart changes only its role.
//! Entries below a member's snapshot are a committed prefix and are not compared.
//!
//! **Coverage** ([`Abstract::point`]). The abstract state's shape, bounded so that a campaign's
//! points are finitely many: per member whether it runs, its role, its vote's kind, and its term,
//! log length, last log term and commit each as an offset below the group's greatest (each capped
//! at [`SPREAD`]), and how many entries it holds beside its log (capped at [`HELD`]).

use crate::explore::fingerprint;

/// The widest offset below the group's greatest that coverage tells apart: three, the terms and
/// indexes FastTrack.tla's largest configurations check (`MaxTerm` and `MaxLen` up to 3,
/// `docs/models/README.md`), so a point distinguishes what the model's scopes do.
pub const SPREAD: u64 = 3;

/// The most entries beside a log that coverage tells apart: two, the indexes at which a
/// configuration holds proposals (`HeldAt`, at most two in FastTrack.tla's configurations).
pub const HELD: usize = 2;

/// A member as the model's variables read it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Member {
    /// Whether it runs (a stopped member is one that does nothing for a while).
    pub up: bool,
    /// `term`.
    pub term: u64,
    /// `vote`: zero for none.
    pub vote: u64,
    /// `role = "leader"`.
    pub leader: bool,
    /// The index of the last entry below the log: its snapshot.
    pub start: u64,
    /// `log` above `start`: each entry's term and a digest of what it states.
    pub log: Vec<(u64, u64)>,
    /// `held`: the indexes it holds proposals at by itself.
    pub held: Vec<u64>,
    /// `commit`.
    pub commit: u64,
    /// `classic`: the index through which it knows its log committed by a classic quorum, as it
    /// released what it held through it.
    pub classic: u64,
}

impl Member {
    /// Its last index.
    pub fn last(&self) -> u64 {
        self.start
            .saturating_add(u64::try_from(self.log.len()).unwrap_or(u64::MAX))
    }

    /// Its entry at `index`, if its log holds one above its start.
    pub fn entry(&self, index: u64) -> Option<(u64, u64)> {
        let at = index.checked_sub(self.start)?.checked_sub(1)?;
        self.log.get(usize::try_from(at).ok()?).copied()
    }

    /// The term of its last entry, or zero.
    pub fn last_term(&self) -> u64 {
        self.log.last().map_or(0, |(term, _)| *term)
    }
}

/// A group as the model's variables read it, member `k` the `k`-th.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Abstract {
    /// The members.
    pub members: Vec<Member>,
}

/// A step no run of the model takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Nonconformance {
    /// A member's term fell.
    TermFell {
        /// The member's place.
        member: usize,
        /// Before.
        from: u64,
        /// After.
        to: u64,
    },
    /// A member's vote changed within a term.
    VoteChanged {
        /// The member's place.
        member: usize,
        /// The term.
        term: u64,
    },
    /// A member's commit fell.
    CommitFell {
        /// The member's place.
        member: usize,
        /// Before.
        from: u64,
        /// After.
        to: u64,
    },
    /// An entry a member had committed changed.
    CommittedChanged {
        /// The member's place.
        member: usize,
        /// The index.
        index: u64,
    },
    /// An entry bears a term above its member's.
    EntryAboveTerm {
        /// The member's place.
        member: usize,
        /// The index.
        index: u64,
    },
    /// What a member knows committed by a classic quorum fell.
    ClassicFell {
        /// The member's place.
        member: usize,
        /// Before.
        from: u64,
        /// After.
        to: u64,
    },
    /// A member holds a proposal at an index it knows committed by a classic quorum.
    HeldBelowClassic {
        /// The member's place.
        member: usize,
        /// The index.
        index: u64,
    },
    /// A member let a holding go at an index it did not know committed by a classic quorum.
    ReleasedEarly {
        /// The member's place.
        member: usize,
        /// The index.
        index: u64,
    },
    /// A member became leader without its own vote in its term.
    LeaderWithoutItsVote {
        /// The member's place.
        member: usize,
        /// The term.
        term: u64,
    },
    /// A leader rewrote its log in its term, or added an entry of another term.
    LeaderRewrote {
        /// The member's place.
        member: usize,
        /// The index.
        index: u64,
    },
}

impl std::fmt::Display for Nonconformance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "no step of FastTrack.tla: {self:?}")
    }
}

/// `before → after` held to the relations every action of the model keeps. `lost` names the
/// members that suffered a fault at rest in the step (the model's `Lose`), whose commit and log
/// may fall.
pub fn conforms(before: &Abstract, after: &Abstract, lost: &[usize]) -> Result<(), Nonconformance> {
    for (member, (old, new)) in before.members.iter().zip(&after.members).enumerate() {
        let faulted = lost.contains(&member);
        if new.term < old.term {
            return Err(Nonconformance::TermFell {
                member,
                from: old.term,
                to: new.term,
            });
        }
        if new.term == old.term && old.vote != 0 && new.vote != old.vote {
            return Err(Nonconformance::VoteChanged {
                member,
                term: new.term,
            });
        }
        if !faulted {
            if new.commit < old.commit {
                return Err(Nonconformance::CommitFell {
                    member,
                    from: old.commit,
                    to: new.commit,
                });
            }
            let from = old.start.max(new.start).saturating_add(1);
            for index in from..=old.commit {
                if let (Some(was), Some(is)) = (old.entry(index), new.entry(index))
                    && was != is
                {
                    return Err(Nonconformance::CommittedChanged { member, index });
                }
            }
        }
        for (at, (term, _)) in new.log.iter().enumerate() {
            if *term > new.term {
                let index = new
                    .start
                    .saturating_add(u64::try_from(at).unwrap_or(u64::MAX))
                    .saturating_add(1);
                return Err(Nonconformance::EntryAboveTerm { member, index });
            }
        }
        if !faulted && new.classic < old.classic {
            return Err(Nonconformance::ClassicFell {
                member,
                from: old.classic,
                to: new.classic,
            });
        }
        if let Some(index) = new.held.iter().find(|index| **index <= new.classic) {
            return Err(Nonconformance::HeldBelowClassic {
                member,
                index: *index,
            });
        }
        if !faulted
            && let Some(index) = old
                .held
                .iter()
                .find(|index| **index > new.classic && !new.held.contains(index))
        {
            return Err(Nonconformance::ReleasedEarly {
                member,
                index: *index,
            });
        }
        let became = new.leader && (!old.leader || old.term != new.term);
        if became && new.vote != u64::try_from(member).unwrap_or(u64::MAX).saturating_add(1) {
            return Err(Nonconformance::LeaderWithoutItsVote {
                member,
                term: new.term,
            });
        }
        if old.leader && new.leader && old.term == new.term && !faulted {
            let from = old.start.max(new.start).saturating_add(1);
            for index in from..=old.last() {
                if old.entry(index) != new.entry(index) {
                    return Err(Nonconformance::LeaderRewrote { member, index });
                }
            }
            for index in old.last().saturating_add(1)..=new.last() {
                if new.entry(index).is_some_and(|(term, _)| term != new.term) {
                    return Err(Nonconformance::LeaderRewrote { member, index });
                }
            }
        }
    }
    Ok(())
}

/// A member's part of a coverage point.
#[derive(Hash)]
struct Shape {
    up: bool,
    leader: bool,
    vote: u8,
    term: u64,
    last: u64,
    last_term: u64,
    commit: u64,
    classic: u64,
    held: usize,
}

impl Abstract {
    /// The coverage point of this state: its shape, bounded (the module's doc), fingerprinted.
    pub fn point(&self) -> u128 {
        let most = |of: &dyn Fn(&Member) -> u64| self.members.iter().map(of).max().unwrap_or(0);
        let (term, last, last_term, commit) = (
            most(&|m| m.term),
            most(&Member::last),
            most(&Member::last_term),
            most(&|m| m.commit),
        );
        let below = |top: u64, value: u64| top.saturating_sub(value).min(SPREAD);
        let shape: Vec<Shape> = self
            .members
            .iter()
            .enumerate()
            .map(|(place, m)| {
                let own = u64::try_from(place).unwrap_or(u64::MAX).saturating_add(1);
                let vote = match m.vote {
                    0 => 0,
                    v if v == own => 1,
                    _ => 2,
                };
                Shape {
                    up: m.up,
                    leader: m.leader,
                    vote,
                    term: below(term, m.term),
                    last: below(last, m.last()),
                    last_term: below(last_term, m.last_term()),
                    commit: below(commit, m.commit),
                    classic: below(m.commit, m.classic),
                    held: m.held.len().min(HELD),
                }
            })
            .collect();
        fingerprint(&shape)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(term: u64, vote: u64, log: &[(u64, u64)], commit: u64) -> Member {
        Member {
            up: true,
            term,
            vote,
            leader: false,
            start: 0,
            log: log.to_vec(),
            held: Vec::new(),
            commit,
            classic: 0,
        }
    }

    #[test]
    fn a_follower_taking_a_leaders_entries_conforms() {
        let before = Abstract {
            members: vec![
                member(1, 1, &[(1, 7)], 1),
                member(1, 1, &[(1, 7), (1, 8)], 1),
            ],
        };
        let mut after = before.clone();
        after.members[0] = member(2, 0, &[(1, 7), (2, 9)], 1);
        assert_eq!(conforms(&before, &after, &[]), Ok(()));
    }

    #[test]
    #[allow(clippy::cognitive_complexity, reason = "one assertion a relation")]
    fn each_relation_refuses_its_step() {
        let before = Abstract {
            members: vec![member(2, 1, &[(1, 7), (2, 8)], 1)],
        };
        let mut after = before.clone();
        after.members[0].term = 1;
        assert!(matches!(
            conforms(&before, &after, &[]),
            Err(Nonconformance::TermFell { .. })
        ));
        let mut after = before.clone();
        after.members[0].vote = 3;
        assert!(matches!(
            conforms(&before, &after, &[]),
            Err(Nonconformance::VoteChanged { .. })
        ));
        let mut after = before.clone();
        after.members[0].log[0] = (1, 6);
        assert!(matches!(
            conforms(&before, &after, &[]),
            Err(Nonconformance::CommittedChanged { index: 1, .. })
        ));
        assert_eq!(conforms(&before, &after, &[0]), Ok(()));
        let mut after = before.clone();
        after.members[0].log.push((3, 9));
        assert!(matches!(
            conforms(&before, &after, &[]),
            Err(Nonconformance::EntryAboveTerm { .. })
        ));
        // A holding at an index the log holds conforms; at one known committed by a classic
        // quorum it does not, nor letting a holding go before.
        let mut after = before.clone();
        after.members[0].held.push(2);
        assert_eq!(conforms(&before, &after, &[]), Ok(()));
        after.members[0].classic = 2;
        assert!(matches!(
            conforms(&before, &after, &[]),
            Err(Nonconformance::HeldBelowClassic { index: 2, .. })
        ));
        let mut known = before.clone();
        known.members[0].classic = 1;
        assert!(matches!(
            conforms(&known, &before, &[]),
            Err(Nonconformance::ClassicFell { from: 1, to: 0, .. })
        ));
        assert_eq!(conforms(&known, &before, &[0]), Ok(()));
        let mut held = before.clone();
        held.members[0].held.push(2);
        let mut after = held.clone();
        after.members[0].held.clear();
        assert!(matches!(
            conforms(&held, &after, &[]),
            Err(Nonconformance::ReleasedEarly { index: 2, .. })
        ));
        after.members[0].classic = 1;
        assert!(matches!(
            conforms(&held, &after, &[]),
            Err(Nonconformance::ReleasedEarly { index: 2, .. })
        ));
        let mut after = held.clone();
        after.members[0].held.clear();
        after.members[0].commit = 2;
        after.members[0].classic = 2;
        assert_eq!(conforms(&held, &after, &[]), Ok(()));
        after.members[0].classic = 0;
        assert!(matches!(
            conforms(&held, &after, &[]),
            Err(Nonconformance::ReleasedEarly { index: 2, .. })
        ));
        let mut after = before.clone();
        after.members[0].leader = true;
        after.members[0].term = 3;
        after.members[0].vote = 2;
        assert!(matches!(
            conforms(&before, &after, &[]),
            Err(Nonconformance::LeaderWithoutItsVote { .. })
        ));
    }

    #[test]
    fn the_point_is_bounded_and_ignores_absolute_terms() {
        let at = |base: u64| Abstract {
            members: vec![
                member(base, 0, &[(base, 1)], base),
                member(base + 1, 0, &[], 0),
            ],
        };
        assert_eq!(at(5).point(), at(50).point());
    }
}
