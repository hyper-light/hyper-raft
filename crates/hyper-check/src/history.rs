//! Histories as clients saw them, and the check of an order either checker exhibits.
//!
//! An [`Operation`] is a call and its return on one clock (Lowe §7.1: a call stamped strictly
//! before any return that observes it), its input, and its output when a client saw one. An
//! operation that never returned has no return and no output (Herlihy and Wing §2.2's pending
//! invocation).
//!
//! [`verify`] is the definition of linearizability (Herlihy and Wing §2.2, Lowe's Definition 1)
//! applied to an exhibited order, independent of both checkers: every completed operation is in
//! it once, an operation that never returned is in it at most once, the order keeps every
//! operation that returned before another was called ahead of it, and the specification run in
//! that order gives every output a client saw. A checker's pass is never taken on its word: each
//! exhibits its order, and this holds it.

use std::fmt;

use crate::model::Model;

/// One operation as a client saw it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Operation<I, O> {
    /// When it was called.
    pub call: u64,
    /// When it returned; `None` if it never did: it may have taken effect at any point after its
    /// call, or never.
    pub ret: Option<u64>,
    /// What it asked.
    pub input: I,
    /// What it answered, if a client saw it.
    pub output: Option<O>,
}

/// Why an operation is not one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Malformed {
    /// It returned before it was called.
    ReturnsBeforeCall {
        /// Its place in the history.
        op: usize,
    },
    /// It has an output but never returned.
    OutputWithoutReturn {
        /// Its place in the history.
        op: usize,
    },
}

impl fmt::Display for Malformed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReturnsBeforeCall { op } => write!(f, "operation {op} returns before its call"),
            Self::OutputWithoutReturn { op } => {
                write!(f, "operation {op} has an output and no return")
            }
        }
    }
}

impl std::error::Error for Malformed {}

/// Every operation of `ops` is one.
pub fn check_operations<I, O>(ops: &[Operation<I, O>]) -> Result<(), Malformed> {
    for (at, op) in ops.iter().enumerate() {
        match op.ret {
            Some(ret) if ret < op.call => return Err(Malformed::ReturnsBeforeCall { op: at }),
            None if op.output.is_some() => {
                return Err(Malformed::OutputWithoutReturn { op: at });
            }
            _ => {}
        }
    }
    Ok(())
}

/// Why an exhibited order is not a linearization.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unverified<O> {
    /// An operation the history does not have.
    Unknown {
        /// The place named.
        op: usize,
    },
    /// An operation named twice.
    Twice {
        /// Its place.
        op: usize,
    },
    /// A completed operation left out.
    Missing {
        /// Its place.
        op: usize,
    },
    /// `after` is ordered after `before`, though it returned before `before` was called.
    RealTime {
        /// The operation ordered first.
        before: usize,
        /// The operation ordered after it.
        after: usize,
    },
    /// The specification, run in the order, answers `op` with `expected`, not what a client saw.
    Output {
        /// The operation.
        op: usize,
        /// What the specification answers there.
        expected: O,
    },
    /// The history itself is not one.
    Malformed(Malformed),
}

impl<O: fmt::Debug> fmt::Display for Unverified<O> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown { op } => write!(f, "the order names operation {op}, not in the history"),
            Self::Twice { op } => write!(f, "the order names operation {op} twice"),
            Self::Missing { op } => write!(f, "the order leaves out completed operation {op}"),
            Self::RealTime { before, after } => write!(
                f,
                "the order puts {after} after {before}, though {after} returned before {before} was called"
            ),
            Self::Output { op, expected } => write!(
                f,
                "in the order, operation {op} answers {expected:?}, not what its client saw"
            ),
            Self::Malformed(malformed) => write!(f, "{malformed}"),
        }
    }
}

impl<O: fmt::Debug> std::error::Error for Unverified<O> {}

/// Whether `order` linearizes `ops` with respect to `model` (Herlihy and Wing's definition).
pub fn verify<M: Model>(
    model: &M,
    ops: &[Operation<M::Input, M::Output>],
    order: &[usize],
) -> Result<(), Unverified<M::Output>> {
    check_operations(ops).map_err(Unverified::Malformed)?;
    placed_once(ops, order)?;
    in_real_time(ops, order)?;
    let mut state = model.init();
    for at in order {
        let op = ops.get(*at).ok_or(Unverified::Unknown { op: *at })?;
        let (output, next) = model.apply(&state, &op.input);
        if op.output.as_ref().is_some_and(|seen| *seen != output) {
            return Err(Unverified::Output {
                op: *at,
                expected: output,
            });
        }
        state = next;
    }
    Ok(())
}

/// Every place in `order` names an operation once, and every completed operation is named.
fn placed_once<I, O>(ops: &[Operation<I, O>], order: &[usize]) -> Result<(), Unverified<O>> {
    let mut seen = vec![false; ops.len()];
    for at in order {
        let slot = seen.get_mut(*at).ok_or(Unverified::Unknown { op: *at })?;
        if *slot {
            return Err(Unverified::Twice { op: *at });
        }
        *slot = true;
    }
    match ops
        .iter()
        .zip(&seen)
        .position(|(op, placed)| op.ret.is_some() && !placed)
    {
        Some(op) => Err(Unverified::Missing { op }),
        None => Ok(()),
    }
}

/// No operation in `order` returned before an operation ahead of it was called: with the latest
/// call ahead of each kept, one comparison an operation.
fn in_real_time<I, O>(ops: &[Operation<I, O>], order: &[usize]) -> Result<(), Unverified<O>> {
    let mut latest: Option<(u64, usize)> = None;
    for at in order {
        let op = ops.get(*at).ok_or(Unverified::Unknown { op: *at })?;
        if let (Some((called, before)), Some(ret)) = (latest, op.ret)
            && ret < called
        {
            return Err(Unverified::RealTime { before, after: *at });
        }
        if latest.is_none_or(|(called, _)| op.call > called) {
            latest = Some((op.call, *at));
        }
    }
    Ok(())
}
