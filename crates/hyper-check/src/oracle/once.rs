//! Exactly once (Ongaro's thesis §6.3: a state machine "would execute each command exactly once";
//! mantle's "every put a gateway was answered exists exactly once on every member"; focal's
//! `DuplicateCommit`): a write takes effect at one index, whichever member applies it and however
//! often its client retried it; and once the group settles, every write a client was told took
//! effect is applied by every member of the configuration, or lies under a snapshot it installed. A
//! member that applies a write again at the same index, as one does when it restarts from an older
//! snapshot, applies it once.

use std::collections::{BTreeMap, BTreeSet};

use super::{Violation, room};

/// Each write's index, every member that applied it, the writes acknowledged, and each member's
/// latest snapshot.
#[derive(Clone, Debug)]
pub struct ExactlyOnce {
    applied: BTreeMap<u64, (u64, BTreeSet<u64>)>,
    acknowledged: BTreeSet<u64>,
    snapshots: BTreeMap<u64, u64>,
    bound: usize,
}

impl ExactlyOnce {
    /// The oracle, holding at most `bound` writes.
    pub fn new(bound: usize) -> Self {
        Self {
            applied: BTreeMap::new(),
            acknowledged: BTreeSet::new(),
            snapshots: BTreeMap::new(),
            bound,
        }
    }

    /// `member` applied `write` at `index`.
    pub fn applied(&mut self, member: u64, index: u64, write: u64) -> Result<(), Violation> {
        match self.applied.get_mut(&write) {
            Some((first, _)) if *first != index => Err(Violation::AppliedTwice {
                member,
                write,
                first: *first,
                second: index,
            }),
            Some((_, members)) => {
                members.insert(member);
                Ok(())
            }
            None => {
                room(self.applied.len(), self.bound, "Exactly once")?;
                self.applied
                    .insert(write, (index, BTreeSet::from([member])));
                Ok(())
            }
        }
    }

    /// `member` installed a snapshot through `index`: every write applied at or below it is applied
    /// there.
    pub fn installed(&mut self, member: u64, index: u64) -> Result<(), Violation> {
        if !self.snapshots.contains_key(&member) {
            room(self.snapshots.len(), self.bound, "Exactly once")?;
        }
        let through = self.snapshots.entry(member).or_insert(0);
        *through = (*through).max(index);
        Ok(())
    }

    /// `write`'s client was told it took effect.
    pub fn acknowledged(&mut self, write: u64) -> Result<(), Violation> {
        if !self.acknowledged.contains(&write) {
            room(self.acknowledged.len(), self.bound, "Exactly once")?;
            self.acknowledged.insert(write);
        }
        Ok(())
    }

    /// Once the group settled: every write acknowledged was applied by every one of `members`.
    pub fn settled(&self, members: &[u64]) -> Result<(), Violation> {
        for write in &self.acknowledged {
            let applied = self.applied.get(write);
            for member in members {
                let under = |index: u64| {
                    self.snapshots
                        .get(member)
                        .is_some_and(|through| index <= *through)
                };
                let held = applied.is_some_and(|(index, by)| by.contains(member) || under(*index));
                if !held {
                    return Err(Violation::NeverApplied {
                        member: *member,
                        write: *write,
                    });
                }
            }
        }
        Ok(())
    }

    /// Writes applied so far.
    pub fn writes(&self) -> usize {
        self.applied.len()
    }

    /// Writes acknowledged so far.
    pub fn acknowledgements(&self) -> usize {
        self.acknowledged.len()
    }
}
