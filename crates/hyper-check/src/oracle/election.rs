//! Election Safety (Ongaro's thesis, Figure 3.2): "at most one leader can be elected in a given
//! term". Held over the whole history: every leader ever seen of a term is the first seen of it,
//! whatever has happened to it since (slates' explorer; hyper-raft's `Cluster::report`).

use std::collections::BTreeMap;

use super::{Violation, room};

/// Every term's leader, over the whole history.
#[derive(Clone, Debug)]
pub struct ElectionSafety {
    leaders: BTreeMap<u64, u64>,
    bound: usize,
}

impl ElectionSafety {
    /// The oracle, holding the leaders of at most `bound` terms.
    pub fn new(bound: usize) -> Self {
        Self {
            leaders: BTreeMap::new(),
            bound,
        }
    }

    /// `member` was seen leading `term`.
    pub fn leader(&mut self, member: u64, term: u64) -> Result<(), Violation> {
        match self.leaders.get(&term) {
            Some(first) if *first == member => Ok(()),
            Some(first) => Err(Violation::TwoLeaders {
                term,
                first: *first,
                second: member,
            }),
            None => {
                room(self.leaders.len(), self.bound, "Election Safety")?;
                self.leaders.insert(term, member);
                Ok(())
            }
        }
    }

    /// The terms led so far.
    pub fn terms(&self) -> usize {
        self.leaders.len()
    }

    /// The leader of `term`, if one was seen.
    pub fn leader_of(&self, term: u64) -> Option<u64> {
        self.leaders.get(&term).copied()
    }
}
