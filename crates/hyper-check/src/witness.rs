//! The witness checker (`docs/sim.md` §4.3): focal's `focal-sim` `history.rs` (focal
//! `origin/slates-port`, `docs/research/sim.md` §5), generalized from `focal_model`'s types to a
//! [`Model`] and the caller's names for objects and requests.
//!
//! The system under test states its own commit order: each object's publications, in sequence. The
//! checker reads the clients' calls and returns and those publications as one stream, in the order
//! the driver observed them, and verifies in one pass that the publication order is a legal
//! sequential history and that each operation took effect between its call and its return:
//!
//! - **Contiguous publication**: each object's publications come in sequence, none skipped
//!   (`Prefix`).
//! - **One commit per request**: a request, however often retried, is published once
//!   (`DuplicateCommit`), as the command it asked (`ResponseMismatch`).
//! - **Within its call and return**: a request is published while an attempt of it may still take
//!   effect: invoked and not answered, or answered as unknown, which leaves it pending for good
//!   (Herlihy and Wing §2.2). A publication with no such attempt is `Unattributed`; a request no
//!   client asked, under complete tracing, `Unasked`; one whose only attempt was refused, though
//!   it was published while that attempt ran, `RefusedButPublished`; one whose client was told it
//!   failed for good, `FailedButPublished`. No success is answered before
//!   its publication (`PrematureSuccess`), and an answer is the specification's at the publication
//!   (`ResponseMismatch`).
//! - **Reads**: a read observes a published prefix within its consistency: a linearizable read one
//!   no older than the prefix when it was asked and no newer than the latest when it was answered
//!   (`StaleRead`), and its answer is the specification's in that prefix's state (`StateMismatch`).
//!
//! It is linear in the history, within a declared event budget (`Capacity`), and it proves a
//! history linearizable by exhibiting the order: each object's publications, with each read after
//! the publication it observed. It needs the system's word for the order; the search checker
//! (`crate::search`) needs none, and where both apply they must agree (`crate::agree`).
//!
//! Generalized from focal's in four ways, each a check focal's types made unnecessary or that
//! focal did not make: the state is the specification's, not a hash the owner reports (focal: "Per-
//! prefix hashes must also be checked against Core's serial oracle by the driving workload"); a
//! publication is held to an attempt that could have made it, where focal accepted a publication of
//! any request; a refused attempt is held to having had no effect; and a read is held to changing
//! nothing.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::model::Model;

/// How old a read may be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Consistency {
    /// No older than the prefix when it was asked: linearizable.
    Linearizable,
    /// No older than this sequence.
    AtLeast(u64),
    /// Exactly this sequence.
    Exact(u64),
}

/// What a client asked of an object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request<P, K, I> {
    /// A command, under the request's key: every attempt of it carries the key and the command.
    Mutation {
        /// The object.
        object: P,
        /// The request's identity, the same on every retry.
        key: K,
        /// The command.
        input: I,
    },
    /// A read.
    Read {
        /// The object.
        object: P,
        /// What it reads.
        input: I,
        /// How old it may be.
        consistency: Consistency,
    },
}

/// What an attempt was answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome<O> {
    /// The command took effect, and answered `output`.
    Committed {
        /// The answer.
        output: O,
    },
    /// The read observed the prefix through `sequence`, and answered `output`.
    Read {
        /// The prefix observed.
        sequence: u64,
        /// The answer.
        output: O,
    },
    /// This attempt took no effect.
    Refused,
    /// The request took no effect and never will: its client was told so, as a session's question
    /// can tell it once the request's place is decided (Knossos's failed operation).
    Failed,
    /// Its client does not know: the attempt may take effect at any time, or never.
    Unknown,
}

/// One event, in the order the driver observed it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event<P, K, I, O> {
    /// A client's attempt began.
    Invoke {
        /// The attempt's identity, unique in the history.
        call: u64,
        /// What it asked.
        request: Request<P, K, I>,
    },
    /// The system made a command durable and visible as the next of its object's sequence.
    Publish {
        /// The object.
        object: P,
        /// Its place in the object's sequence.
        sequence: u64,
        /// The request it is.
        key: K,
        /// The command.
        input: I,
    },
    /// An attempt was answered.
    Complete {
        /// The attempt.
        call: u64,
        /// Its answer.
        outcome: Outcome<O>,
    },
}

/// An object as the history begins: the sequence it has reached and its state there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Initial<P, S> {
    /// The object.
    pub object: P,
    /// The sequence of its last publication before the history.
    pub sequence: u64,
    /// Its state there.
    pub state: S,
}

/// Whether every client's attempts are in the history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tracing {
    /// Every request is some client's in the history: a publication of any other is `Unasked`.
    Complete,
    /// Some clients are not traced: a publication of a request no traced client asked is that of
    /// an untraced one, counted.
    Partial,
}

/// What a history held.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// Publications.
    pub publications: usize,
    /// Reads answered.
    pub reads: usize,
    /// Commands answered as committed by an attempt that began after their publication: a retry
    /// of a request that had taken effect.
    pub retries: usize,
    /// Attempts answered as unknown.
    pub unknown: usize,
    /// Attempts never answered.
    pub pending: usize,
    /// Publications of requests no traced client asked (partial tracing).
    pub untraced: usize,
}

/// Why a history is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryError {
    /// More events than the declared budget, or more objects.
    Capacity,
    /// An attempt's identity used twice, or unknown; an object unknown or declared twice.
    Identity,
    /// A publication is not the next of its object's sequence.
    Prefix,
    /// A request published twice.
    DuplicateCommit,
    /// A command answered as committed before its publication.
    PrematureSuccess,
    /// An answer that is not its request's: of another kind, naming another command, or not the
    /// specification's at the publication; or a request published, or retried, as another command.
    ResponseMismatch,
    /// A read observed a prefix its consistency does not allow.
    StaleRead,
    /// A read's answer is not the specification's in the prefix it observed.
    StateMismatch,
    /// A request no client asked was published, under complete tracing.
    Unasked,
    /// A request was published while none of its attempts could have taken effect.
    Unattributed,
    /// A request was published while its one attempt ran, and that attempt was answered refused.
    RefusedButPublished,
    /// A read changed its object's state.
    MutatingRead,
    /// A request was published though its client was told it never would be.
    FailedButPublished,
}

impl fmt::Display for HistoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let what = match self {
            Self::Capacity => "the history exceeds its declared event budget",
            Self::Identity => "a duplicate or missing invocation or object",
            Self::Prefix => "a publication is not the next of its object's sequence",
            Self::DuplicateCommit => "a request committed more than once",
            Self::PrematureSuccess => "success was answered before its publication",
            Self::ResponseMismatch => "an answer belongs to another request or command",
            Self::StaleRead => "a read violates its consistency",
            Self::StateMismatch => "a read's answer is not its prefix's",
            Self::Unasked => "a request no client asked was published",
            Self::Unattributed => "a request was published with no attempt of it under way",
            Self::RefusedButPublished => "an attempt answered refused took effect",
            Self::MutatingRead => "a read changed its object",
            Self::FailedButPublished => "a request its client was told failed took effect",
        };
        f.write_str(what)
    }
}

impl std::error::Error for HistoryError {}

/// A refusal, and the event at which it was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Refused {
    /// What is wrong.
    pub error: HistoryError,
    /// The event's place in the history.
    pub at: usize,
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "event {}: {}", self.at, self.error)
    }
}

impl std::error::Error for Refused {}

/// An operation in an exhibited order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Placed<K> {
    /// A request, at its publication.
    Mutation(K),
    /// A read, by its attempt, after the publication it observed.
    Read(u64),
}

/// A history the checker passed: what it held, and the order it exhibits for each object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Witnessed<P, K> {
    /// What it held.
    pub report: Report,
    /// Each object's operations, in the order they take effect.
    pub order: BTreeMap<P, Vec<Placed<K>>>,
}

struct Object<S, K> {
    base: u64,
    sequence: u64,
    /// The state after each sequence from `base` on.
    states: Vec<S>,
    mutations: Vec<K>,
    /// Each read answered: the sequence it observed, its call's place, its call.
    reads: Vec<(u64, usize, u64)>,
}

struct Published<O> {
    at: usize,
    output: O,
    /// Attempts under way at the publication and not since refused.
    eligible: u32,
    /// Whether an attempt answered unknown before the publication, pending for good.
    unknown_before: bool,
}

struct Req<P, I, O> {
    object: P,
    input: I,
    open: u32,
    unknown: bool,
    failed: bool,
    published: Option<Published<O>>,
}

enum Attempt<P, K, I> {
    Mutation {
        key: K,
        at: usize,
    },
    Read {
        object: P,
        input: I,
        consistency: Consistency,
        prefix: u64,
        at: usize,
    },
}

struct Check<'m, M: Model, P, K> {
    model: &'m M,
    tracing: Tracing,
    objects: BTreeMap<P, Object<M::State, K>>,
    requests: BTreeMap<K, Req<P, M::Input, M::Output>>,
    active: BTreeMap<u64, Attempt<P, K, M::Input>>,
    calls: BTreeSet<u64>,
    report: Report,
}

/// Whether `events` is a linearizable history of `model` with the system's order as its witness,
/// starting from `initial`, within `max_events` events.
pub fn check<M: Model, P: Ord + Clone, K: Ord + Clone>(
    model: &M,
    initial: &[Initial<P, M::State>],
    events: &[Event<P, K, M::Input, M::Output>],
    max_events: usize,
    tracing: Tracing,
) -> Result<Witnessed<P, K>, Refused> {
    let refuse = |error, at| Refused { error, at };
    if events.len() > max_events || initial.len() > max_events {
        return Err(refuse(HistoryError::Capacity, 0));
    }
    let mut check = Check {
        model,
        tracing,
        objects: BTreeMap::new(),
        requests: BTreeMap::new(),
        active: BTreeMap::new(),
        calls: BTreeSet::new(),
        report: Report::default(),
    };
    for base in initial {
        check.declare(base).map_err(|error| refuse(error, 0))?;
    }
    for (at, event) in events.iter().enumerate() {
        check.event(at, event).map_err(|error| refuse(error, at))?;
    }
    check.report.pending = check.active.len();
    Ok(Witnessed {
        report: check.report,
        order: check.order(),
    })
}

/// One more of a count, which a history within its budget never takes past `usize`.
fn more(count: &mut usize) {
    *count = count.saturating_add(1);
}

impl<M: Model, P: Ord + Clone, K: Ord + Clone> Check<'_, M, P, K> {
    fn declare(&mut self, base: &Initial<P, M::State>) -> Result<(), HistoryError> {
        if self.objects.contains_key(&base.object) {
            return Err(HistoryError::Identity);
        }
        let states = vec![base.state.clone()];
        self.objects.insert(
            base.object.clone(),
            Object {
                base: base.sequence,
                sequence: base.sequence,
                states,
                mutations: Vec::new(),
                reads: Vec::new(),
            },
        );
        Ok(())
    }

    fn event(
        &mut self,
        at: usize,
        event: &Event<P, K, M::Input, M::Output>,
    ) -> Result<(), HistoryError> {
        match event {
            Event::Invoke { call, request } => self.invoke(at, *call, request),
            Event::Publish {
                object,
                sequence,
                key,
                input,
            } => self.publish(at, object, *sequence, key, input),
            Event::Complete { call, outcome } => self.complete(*call, outcome),
        }
    }

    fn invoke(
        &mut self,
        at: usize,
        call: u64,
        request: &Request<P, K, M::Input>,
    ) -> Result<(), HistoryError> {
        if !self.calls.insert(call) {
            return Err(HistoryError::Identity);
        }
        let attempt = match request {
            Request::Mutation { object, key, input } => {
                self.object(object)?;
                self.attempt(object, key, input)?;
                Attempt::Mutation {
                    key: key.clone(),
                    at,
                }
            }
            Request::Read {
                object,
                input,
                consistency,
            } => Attempt::Read {
                object: object.clone(),
                input: input.clone(),
                consistency: *consistency,
                prefix: self.object(object)?.sequence,
                at,
            },
        };
        self.active.insert(call, attempt);
        Ok(())
    }

    fn object(&self, object: &P) -> Result<&Object<M::State, K>, HistoryError> {
        self.objects.get(object).ok_or(HistoryError::Identity)
    }

    /// An attempt of the request `key` began: a first, or a retry carrying the same command.
    fn attempt(&mut self, object: &P, key: &K, input: &M::Input) -> Result<(), HistoryError> {
        match self.requests.get_mut(key) {
            Some(req) => {
                if req.object != *object || req.input != *input {
                    return Err(HistoryError::ResponseMismatch);
                }
                req.open = req.open.saturating_add(1);
            }
            None => {
                self.requests.insert(
                    key.clone(),
                    Req {
                        object: object.clone(),
                        input: input.clone(),
                        open: 1,
                        unknown: false,
                        failed: false,
                        published: None,
                    },
                );
            }
        }
        Ok(())
    }

    fn publish(
        &mut self,
        at: usize,
        object: &P,
        sequence: u64,
        key: &K,
        input: &M::Input,
    ) -> Result<(), HistoryError> {
        let current = self.object(object)?.sequence;
        if current.checked_add(1) != Some(sequence) {
            return Err(HistoryError::Prefix);
        }
        let (eligible, unknown_before) = self.attributed(object, key, input)?;
        let model = self.model;
        let place = self.objects.get_mut(object).ok_or(HistoryError::Identity)?;
        let state = place.states.last().ok_or(HistoryError::Identity)?;
        let (output, next) = model.apply(state, input);
        place.states.push(next);
        place.sequence = sequence;
        place.mutations.push(key.clone());
        let req = self.requests.get_mut(key).ok_or(HistoryError::Identity)?;
        req.published = Some(Published {
            at,
            output,
            eligible,
            unknown_before,
        });
        more(&mut self.report.publications);
        Ok(())
    }

    /// The attempts that could have made a publication of `key`: those under way, and whether one
    /// was answered unknown before; a request no traced client asked is an untraced one's under
    /// partial tracing.
    fn attributed(
        &mut self,
        object: &P,
        key: &K,
        input: &M::Input,
    ) -> Result<(u32, bool), HistoryError> {
        let Some(req) = self.requests.get(key) else {
            if self.tracing == Tracing::Complete {
                return Err(HistoryError::Unasked);
            }
            more(&mut self.report.untraced);
            self.requests.insert(
                key.clone(),
                Req {
                    object: object.clone(),
                    input: input.clone(),
                    open: 0,
                    unknown: true,
                    failed: false,
                    published: None,
                },
            );
            return Ok((0, true));
        };
        if req.published.is_some() {
            return Err(HistoryError::DuplicateCommit);
        }
        if req.failed {
            return Err(HistoryError::FailedButPublished);
        }
        if req.object != *object || req.input != *input {
            return Err(HistoryError::ResponseMismatch);
        }
        if req.open == 0 && !req.unknown {
            return Err(HistoryError::Unattributed);
        }
        Ok((req.open, req.unknown))
    }

    fn complete(&mut self, call: u64, outcome: &Outcome<M::Output>) -> Result<(), HistoryError> {
        let attempt = self.active.remove(&call).ok_or(HistoryError::Identity)?;
        match (attempt, outcome) {
            (Attempt::Mutation { key, .. }, Outcome::Unknown) => {
                more(&mut self.report.unknown);
                let req = self.requests.get_mut(&key).ok_or(HistoryError::Identity)?;
                req.open = req.open.saturating_sub(1);
                req.unknown = true;
                Ok(())
            }
            (Attempt::Read { .. }, Outcome::Unknown) => {
                more(&mut self.report.unknown);
                Ok(())
            }
            (Attempt::Mutation { key, at }, Outcome::Refused) => self.refused(&key, at),
            (Attempt::Read { .. }, Outcome::Refused | Outcome::Failed) => Ok(()),
            (Attempt::Mutation { key, .. }, Outcome::Failed) => self.failed(&key),
            (Attempt::Mutation { key, at }, Outcome::Committed { output }) => {
                self.committed(&key, at, output)
            }
            (
                Attempt::Read {
                    object,
                    input,
                    consistency,
                    prefix,
                    at,
                },
                Outcome::Read { sequence, output },
            ) => {
                let read = Read {
                    input: &input,
                    consistency,
                    prefix,
                    sequence: *sequence,
                    output,
                };
                self.read(&object, &read)?;
                let place = self
                    .objects
                    .get_mut(&object)
                    .ok_or(HistoryError::Identity)?;
                place.reads.push((*sequence, at, call));
                more(&mut self.report.reads);
                Ok(())
            }
            (Attempt::Read { .. }, Outcome::Committed { .. })
            | (Attempt::Mutation { .. }, Outcome::Read { .. }) => {
                Err(HistoryError::ResponseMismatch)
            }
        }
    }

    /// An attempt begun at `at` took no effect: if the publication fell while it ran and it was the
    /// last attempt that could have made it, the refusal is false.
    fn refused(&mut self, key: &K, at: usize) -> Result<(), HistoryError> {
        let req = self.requests.get_mut(key).ok_or(HistoryError::Identity)?;
        req.open = req.open.saturating_sub(1);
        if let Some(published) = req.published.as_mut()
            && at < published.at
        {
            published.eligible = published.eligible.saturating_sub(1);
            if published.eligible == 0 && !published.unknown_before {
                return Err(HistoryError::RefusedButPublished);
            }
        }
        Ok(())
    }

    /// The request `key` failed for good: it was never published, and never will be.
    fn failed(&mut self, key: &K) -> Result<(), HistoryError> {
        let req = self.requests.get_mut(key).ok_or(HistoryError::Identity)?;
        req.open = req.open.saturating_sub(1);
        if req.published.is_some() {
            return Err(HistoryError::FailedButPublished);
        }
        req.failed = true;
        Ok(())
    }

    fn committed(&mut self, key: &K, at: usize, output: &M::Output) -> Result<(), HistoryError> {
        let model = self.model;
        let req = self.requests.get_mut(key).ok_or(HistoryError::Identity)?;
        if !model.answers(&req.input, output) {
            return Err(HistoryError::ResponseMismatch);
        }
        let published = req
            .published
            .as_ref()
            .ok_or(HistoryError::PrematureSuccess)?;
        if published.output != *output {
            return Err(HistoryError::ResponseMismatch);
        }
        let retry = at > published.at;
        req.open = req.open.saturating_sub(1);
        if retry {
            more(&mut self.report.retries);
        }
        Ok(())
    }

    fn read(&self, object: &P, read: &Read<'_, M::Input, M::Output>) -> Result<(), HistoryError> {
        let place = self.object(object)?;
        let floor = match read.consistency {
            Consistency::Linearizable => read.prefix,
            Consistency::AtLeast(least) => least,
            Consistency::Exact(exact) => exact,
        };
        let exact = match read.consistency {
            Consistency::Exact(exact) => read.sequence == exact,
            _ => true,
        };
        if read.sequence < floor || read.sequence > place.sequence || !exact {
            return Err(HistoryError::StaleRead);
        }
        let state = read
            .sequence
            .checked_sub(place.base)
            .and_then(|offset| usize::try_from(offset).ok())
            .and_then(|offset| place.states.get(offset))
            .ok_or(HistoryError::StaleRead)?;
        if !self.model.answers(read.input, read.output) {
            return Err(HistoryError::ResponseMismatch);
        }
        let (expected, after) = self.model.apply(state, read.input);
        if after != *state {
            return Err(HistoryError::MutatingRead);
        }
        if expected != *read.output {
            return Err(HistoryError::StateMismatch);
        }
        Ok(())
    }

    /// Each object's order: its publications in sequence, each read after the publication it
    /// observed, reads of one prefix in the order they were asked.
    fn order(&mut self) -> BTreeMap<P, Vec<Placed<K>>> {
        let mut orders = BTreeMap::new();
        for (key, object) in &mut self.objects {
            object.reads.sort_unstable();
            let mut order = Vec::new();
            let mut reads = object.reads.iter().peekable();
            let mut sequence = object.base;
            loop {
                while let Some((_, _, call)) = reads.next_if(|(seen, _, _)| *seen == sequence) {
                    order.push(Placed::Read(*call));
                }
                let next = sequence
                    .checked_sub(object.base)
                    .and_then(|offset| usize::try_from(offset).ok())
                    .and_then(|offset| object.mutations.get(offset));
                let Some(mutation) = next else {
                    break;
                };
                order.push(Placed::Mutation(mutation.clone()));
                sequence = sequence.saturating_add(1);
            }
            orders.insert(key.clone(), order);
        }
        orders
    }
}

/// A read's answer, as the checker holds it to its object.
struct Read<'a, I, O> {
    input: &'a I,
    consistency: Consistency,
    prefix: u64,
    sequence: u64,
    output: &'a O,
}
