//! Same history: every member that applied an index reached the same state there (mantle's "every
//! member holds the same rows"; the E2E crates' equal digests of applied history; TigerBeetle's
//! replicas, "designed to be byte-for-byte identical across caught-up nodes"). State Machine Safety
//! holds the log's entries equal; this holds the state machines that applied them equal, which a
//! nondeterministic state machine breaks with the log whole. A state is held by its digest, the
//! harness's to compute, after each index applied or installed by a snapshot.

use std::collections::BTreeMap;

use super::{Violation, room};

/// The digest of the state each index left, as the first member to reach it reported it.
#[derive(Clone, Debug)]
pub struct SameHistory {
    states: BTreeMap<u64, u64>,
    bound: usize,
}

impl SameHistory {
    /// The oracle, holding at most `bound` indexes.
    pub fn new(bound: usize) -> Self {
        Self {
            states: BTreeMap::new(),
            bound,
        }
    }

    /// `member`'s state after applying, or installing, through `index` has digest `digest`.
    pub fn reached(&mut self, member: u64, index: u64, digest: u64) -> Result<(), Violation> {
        match self.states.get(&index) {
            Some(first) if *first == digest => Ok(()),
            Some(_) => Err(Violation::HistoriesDiffer { member, index }),
            None => {
                room(self.states.len(), self.bound, "Same history")?;
                self.states.insert(index, digest);
                Ok(())
            }
        }
    }

    /// The digest of the state `index` left, if a member reached it.
    pub fn at(&self, index: u64) -> Option<u64> {
        self.states.get(&index).copied()
    }

    /// Indexes reached so far.
    pub fn indexes(&self) -> usize {
        self.states.len()
    }
}
