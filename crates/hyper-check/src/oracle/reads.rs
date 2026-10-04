//! Read safety (Ongaro's thesis §6.4, the read-only queries' rule): a read is answered at an index
//! no lower than every index committed when it was asked, so it reflects every write that completed
//! before it began. The commit known when a read is asked is the highest any member had heard
//! committed by then (a commit index is only ever an index committed); hyper-raft's `Cluster::report`
//! holds its reads to the highest index any member had applied, which this floor is never below.

use std::collections::BTreeMap;

use super::{Violation, room};

/// The reads asked, each with the commit known when it was asked: every answer to one is held to it,
/// a read answered twice included.
#[derive(Clone, Debug)]
pub struct ReadSafety {
    known: u64,
    asked: BTreeMap<u64, u64>,
    answered: u64,
    bound: usize,
}

impl ReadSafety {
    /// The oracle, holding at most `bound` reads.
    pub fn new(bound: usize) -> Self {
        Self {
            known: 0,
            asked: BTreeMap::new(),
            answered: 0,
            bound,
        }
    }

    /// Some member heard `index` committed.
    pub fn committed(&mut self, index: u64) {
        self.known = self.known.max(index);
    }

    /// The read `read` was asked now.
    pub fn asked(&mut self, read: u64) -> Result<(), Violation> {
        room(self.asked.len(), self.bound, "Read safety")?;
        self.asked.insert(read, self.known);
        Ok(())
    }

    /// `member` answered `read` at `index`.
    pub fn answered(&mut self, member: u64, read: u64, index: u64) -> Result<(), Violation> {
        let floor = self
            .asked
            .get(&read)
            .copied()
            .ok_or(Violation::UnaskedRead { member, read })?;
        if index < floor {
            return Err(Violation::StaleRead {
                member,
                read,
                index,
                floor,
            });
        }
        self.answered = self.answered.saturating_add(1);
        Ok(())
    }

    /// `read` will never be answered: its asker was told so, or gave up.
    pub fn dropped(&mut self, read: u64) {
        self.asked.remove(&read);
    }

    /// Reads answered so far.
    pub fn answers(&self) -> u64 {
        self.answered
    }

    /// The commit known now.
    pub fn known(&self) -> u64 {
        self.known
    }
}
