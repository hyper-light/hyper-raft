//! A sequential specification: what one object does, one operation at a time
//! (`docs/sim.md` §4.3).
//!
//! Both checkers judge a history against the same specification. It is deterministic and its
//! states are values (Lowe §3.1: "we require the sequential object to be immutable, and for
//! operations on it to return the resulting sequential object in addition to the natural result of
//! the operation"), so a configuration of the search is a state and a set of operations, and two
//! configurations that agree on both are one (Lowe's memo). A deterministic specification is what
//! Lowe's and Horn and Kroening's algorithms take (Lowe §9: "At present, we restrict the framework
//! to deterministic sequential specifications").
//!
//! An operation whose output nobody saw is judged by the state it leaves alone: [`Model::apply`] is
//! total, so every state takes every input, and an unknown output is accepted wherever the operation
//! is placed (Herlihy and Wing §2.2's extension of a history; Porcupine's and Knossos's crashed
//! operation).

use std::fmt::Debug;
use std::hash::Hash;
use std::marker::PhantomData;

/// A deterministic sequential specification of one object.
pub trait Model {
    /// The object's state: a value, compared whole and hashed for the search's fingerprints.
    type State: Clone + Eq + Hash + Debug;
    /// What an operation asks.
    type Input: Clone + Eq + Debug;
    /// What an operation answers.
    type Output: Clone + Eq + Debug;

    /// The state every history starts from.
    fn init(&self) -> Self::State;

    /// What `input` answers in `state`, and the state it leaves. Total: every state takes every
    /// input.
    fn apply(&self, state: &Self::State, input: &Self::Input) -> (Self::Output, Self::State);

    /// Whether `output` can answer `input` at all, whatever the state: a response that names
    /// another request or command is no answer to this one (focal's `ResponseMismatch`). Every
    /// output can by default.
    fn answers(&self, input: &Self::Input, output: &Self::Output) -> bool {
        let _ = (input, output);
        true
    }

    /// The bytes a state holds, its heap included: what a state costs the search's whole-key
    /// memo (`docs/sim.md` §7). Its inline size by default; a state with heap data says more.
    fn bytes(&self, state: &Self::State) -> usize {
        let _ = state;
        size_of::<Self::State>()
    }
}

/// A register of `V`s: a write makes its value current, a read answers the current value. With
/// every value written once, a read names the write it saw (mantle's `Register`, an object key's
/// current version).
pub struct Register<V>(PhantomData<fn() -> V>);

impl<V> Register<V> {
    /// The register.
    pub const fn new() -> Self {
        Self(PhantomData)
    }
}

impl<V> Default for Register<V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<V> Clone for Register<V> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<V> Copy for Register<V> {}

impl<V> Debug for Register<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Register")
    }
}

/// What a register is asked.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Access<V> {
    /// Make `V` the current value.
    Write(V),
    /// Answer the current value.
    Read,
}

/// What a register answers.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Answer<V> {
    /// The write took effect.
    Written,
    /// The value read: none before the first write.
    Read(Option<V>),
}

impl<V: Clone + Eq + Hash + Debug> Model for Register<V> {
    type State = Option<V>;
    type Input = Access<V>;
    type Output = Answer<V>;

    fn init(&self) -> Self::State {
        None
    }

    fn apply(&self, state: &Self::State, input: &Self::Input) -> (Self::Output, Self::State) {
        match input {
            Access::Write(value) => (Answer::Written, Some(value.clone())),
            Access::Read => (Answer::Read(state.clone()), state.clone()),
        }
    }

    fn answers(&self, input: &Self::Input, output: &Self::Output) -> bool {
        matches!(
            (input, output),
            (Access::Write(_), Answer::Written) | (Access::Read, Answer::Read(_))
        )
    }
}
