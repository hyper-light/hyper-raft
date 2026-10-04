//! Fast agreement: no two values are ever chosen at one index (Lamport, Fast Paxos, Distributed
//! Computing 19(2), 2006, §3.3: a value is chosen in a round when a quorum of that round votes for
//! it, and no two values are chosen; Paxos's P2 at the acceptors). Counted from a ghost of every vote
//! cast — the acceptors' state, a vote counting from the moment it is cast, whatever becomes of its
//! message — as slates' explorer counts it (`votes_cast`). A round is a term, and its quorums are the
//! protocol's (hyper-raft `docs/raft.md` §3, the second rule): a value is chosen at an index in a
//! term once, for every set of voters the term's leader counted by, a fast quorum of that set voted
//! it there in that term. The sets are the voters it was elected under and the one other set a
//! change in the term named; a leader elected under a joint configuration, or whose term named a
//! third set, counts by no fast quorum, and its term chooses nothing by votes. A vote from a member
//! that is no voter of a set counts for nothing in that set.
//!
//! A vote is an acceptor's in its term's round only where the round leaves the index open. Not at
//! an index committed: every later leader holds that entry (Leader Completeness), so no leader
//! counts another value there. Nor, while the term's leader leads it, against an entry that leader
//! holds at the index: Fast Raft's leader is the round's one learner, and counts a vote only for
//! what it took there. A vote that fails either — a member behind its group sending again what it
//! held beside a log that has since moved on — is counted by no leader, and the classic oracles hold
//! those indexes.
//!
//! Every value chosen at an index, in any term, is the first chosen there. An acceptor votes once in
//! a round (Paxos), so a voter that votes two values at one index in one term is refused too.

use std::collections::BTreeMap;

use super::{Violation, room};

/// A term's round: the sets of voters its leader counted by, each with its fast quorum.
#[derive(Clone, Debug)]
struct Round {
    sets: Vec<(Vec<u64>, usize)>,
    /// Whether its leader counts by fast quorums at all: elected under no joint configuration, and
    /// no third set named.
    fast: bool,
}

/// The ghost of every fast vote cast, and the value chosen at each index.
#[derive(Clone, Debug)]
pub struct FastAgreement<V> {
    votes: BTreeMap<(u64, u64), BTreeMap<u64, V>>,
    rounds: BTreeMap<u64, Round>,
    committed: u64,
    chosen: BTreeMap<u64, V>,
    bound: usize,
}

impl<V: Clone + Eq> FastAgreement<V> {
    /// The oracle, holding at most `bound` indexes' votes and terms.
    pub fn new(bound: usize) -> Self {
        Self {
            votes: BTreeMap::new(),
            rounds: BTreeMap::new(),
            committed: 0,
            chosen: BTreeMap::new(),
            bound,
        }
    }

    /// `term`'s leader counts by `voters` and, in a joint configuration, `outgoing`, each with the
    /// fast quorum `quorum` gives of its size (Fast Raft's ⌈3M/4⌉ of `M` in hyper-raft's). Its
    /// first call is the configuration it was elected under; each later one names what it applied
    /// since. Votes of the term cast before are counted now.
    pub fn term(
        &mut self,
        term: u64,
        voters: &[u64],
        outgoing: &[u64],
        quorum: impl Fn(usize) -> usize,
    ) -> Result<(), Violation> {
        let first = !self.rounds.contains_key(&term);
        if first {
            room(self.rounds.len(), self.bound, "Fast agreement")?;
            self.rounds.insert(
                term,
                Round {
                    sets: Vec::new(),
                    fast: outgoing.is_empty(),
                },
            );
        }
        let Some(round) = self.rounds.get_mut(&term) else {
            return Ok(());
        };
        for set in [voters, outgoing] {
            if set.is_empty() || round.sets.iter().any(|(held, _)| held == set) {
                continue;
            }
            if round.sets.len() >= 2 {
                round.fast = false;
                continue;
            }
            round.sets.push((set.to_vec(), quorum(set.len()).max(1)));
        }
        if first {
            let indexes: Vec<u64> = self
                .votes
                .range((term, 0)..=(term, u64::MAX))
                .map(|((_, index), _)| *index)
                .collect();
            for index in indexes {
                self.tally(term, index)?;
            }
        }
        Ok(())
    }

    /// Some member heard `index` committed: no vote at or below it is an acceptor's.
    pub fn committed(&mut self, index: u64) {
        self.committed = self.committed.max(index);
    }

    /// `voter` voted, in `term`, for `value` at `index`; `held` is the entry the term's leader holds
    /// there, if it leads the term still and its log reaches the index.
    pub fn vote(
        &mut self,
        term: u64,
        index: u64,
        voter: u64,
        value: &V,
        held: Option<&V>,
    ) -> Result<(), Violation> {
        if index <= self.committed || held.is_some_and(|held| held != value) {
            return Ok(());
        }
        if !self.votes.contains_key(&(term, index)) {
            room(self.votes.len(), self.bound, "Fast agreement")?;
        }
        let cast = self.votes.entry((term, index)).or_default();
        match cast.get(&voter) {
            Some(held) if held != value => Err(Violation::VoteChanged {
                member: voter,
                term,
                index,
            }),
            Some(_) => Ok(()),
            None => {
                cast.insert(voter, value.clone());
                self.tally(term, index)
            }
        }
    }

    /// The votes of `term` at `index` counted: a value a fast quorum of every set cast is chosen,
    /// and must be the first chosen there.
    fn tally(&mut self, term: u64, index: u64) -> Result<(), Violation> {
        let (Some(round), Some(cast)) = (self.rounds.get(&term), self.votes.get(&(term, index)))
        else {
            return Ok(());
        };
        if !round.fast || round.sets.is_empty() {
            return Ok(());
        }
        let won_in = |value: &V, (voters, quorum): &(Vec<u64>, usize)| {
            cast.iter()
                .filter(|(voter, held)| voters.contains(voter) && *held == value)
                .count()
                >= *quorum
        };
        let won = cast
            .values()
            .find(|value| round.sets.iter().all(|set| won_in(value, set)));
        let Some(won) = won else {
            return Ok(());
        };
        match self.chosen.get(&index) {
            Some(first) if first != won => Err(Violation::TwoChosen { index, term }),
            Some(_) => Ok(()),
            None => {
                self.chosen.insert(index, won.clone());
                Ok(())
            }
        }
    }

    /// The value chosen at `index`, if a fast quorum chose one.
    pub fn chosen(&self, index: u64) -> Option<&V> {
        self.chosen.get(&index)
    }

    /// Indexes a fast quorum chose.
    pub fn choices(&self) -> usize {
        self.chosen.len()
    }

    /// Votes cast.
    pub fn votes(&self) -> usize {
        self.votes.values().map(BTreeMap::len).sum()
    }
}
