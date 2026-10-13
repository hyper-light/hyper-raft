//! The clients' history (`docs/sim.md` §6, step S-7): every write and read an end-to-end test asks
//! of its group, each stamped on the test's one clock, its call before it is sent and its return
//! when its answer is read (Lowe §7.1), and the history held linearizable as a map of registers by
//! hyper-check's search checker, key by key (Herlihy and Wing Theorem 1). Reading back each answered
//! write checks that nothing answered was lost; this checks that no read saw what the group's order
//! could not have given it, a stale read among them.
//!
//! What goes in: a write answered committed, a write never answered (it may take effect at any time
//! after its call, or never), and a read answered with a value. A refusal took no effect and a read
//! never answered observed nothing, so neither is in it (Herlihy and Wing §2.2). The history holds
//! what its test asks: each ask is a call the test makes until it is answered or its group stops
//! moving.

use std::time::Instant;

use hyper_check::search::{Budget, SearchError, Verdict, search_partitions};
use hyper_check::{Access, Answer, Map, Operation};

use crate::wire::{Op, Outcome};

/// A client's operation as the history check reads it: the key, a write of a value or a read, and
/// what was answered.
pub type Recorded = Operation<(Vec<u8>, Access<Vec<u8>>), Answer<Vec<u8>>>;

/// The clients' history of one test's group.
#[derive(Debug)]
pub struct Recorder {
    epoch: Instant,
    operations: Vec<Recorded>,
}

impl Recorder {
    /// An empty history, its clock starting now.
    #[allow(
        clippy::disallowed_methods,
        reason = "real processes on the host's clock (CLAUDE.md §1a, end to end)"
    )]
    pub fn new() -> Self {
        Self {
            epoch: Instant::now(),
            operations: Vec::new(),
        }
    }

    /// The history's clock: nanoseconds since it began.
    pub fn clock(&self) -> u64 {
        u64::try_from(self.epoch.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }

    /// `op`, called at `call`, as it was answered (`outcome`, none when no answer came).
    pub fn record(&mut self, op: &Op<'_>, call: u64, outcome: Option<&Outcome>) {
        let (input, output) = match (op, outcome) {
            (Op::Put { key, value }, Some(Outcome::Put(_))) => (
                (key.to_vec(), Access::Write(value.to_vec())),
                Some(Answer::Written),
            ),
            (Op::Put { key, value }, None) => ((key.to_vec(), Access::Write(value.to_vec())), None),
            (Op::Get { key }, Some(Outcome::Value(value))) => (
                (key.to_vec(), Access::Read),
                Some(Answer::Read(value.clone())),
            ),
            _ => return,
        };
        let ret = output.is_some().then(|| self.clock());
        self.operations.push(Operation {
            call,
            ret,
            input,
            output,
        });
    }

    /// A write of `value` to `key` called at `call`: answered committed now, or never answered.
    pub fn write(&mut self, key: Vec<u8>, value: Vec<u8>, call: u64, answered: bool) {
        let ret = answered.then(|| self.clock());
        self.operations.push(Operation {
            call,
            ret,
            input: (key, Access::Write(value)),
            output: answered.then_some(Answer::Written),
        });
    }

    /// The operations recorded.
    pub fn operations(&self) -> &[Recorded] {
        &self.operations
    }

    /// The keys whose histories have no order, or none the search's budget found, each with what
    /// the search said of it; empty when every key's has one.
    pub fn unlinearizable(&self) -> Result<Vec<String>, SearchError> {
        let parts = search_partitions(
            &Map::<Vec<u8>, Vec<u8>>::new(),
            &self.operations,
            Map::key,
            &Budget::default(),
        )?;
        Ok(parts
            .into_iter()
            .filter(|part| !matches!(part.searched.verdict, Verdict::Linearizable { .. }))
            .map(|part| {
                format!(
                    "{}: {:?}",
                    String::from_utf8_lossy(&part.key),
                    part.searched.verdict
                )
            })
            .collect())
    }

    /// How many keys the history holds.
    pub fn keys(&self) -> usize {
        let mut keys: Vec<&[u8]> = self
            .operations
            .iter()
            .map(|op| op.input.0.as_slice())
            .collect();
        keys.sort_unstable();
        keys.dedup();
        keys.len()
    }
}

impl Default for Recorder {
    fn default() -> Self {
        Self::new()
    }
}
