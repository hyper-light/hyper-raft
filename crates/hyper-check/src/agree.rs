//! The two checkers on one history (`docs/sim.md` §4.3: "Where both checkers apply (every
//! simulated history with a commit order), they must agree; a disagreement is a bug in one of
//! them or in the witness").
//!
//! The witness checker reads the stream of calls, publications and returns; the search checker
//! reads the operations the clients saw, with the stream's places as their times, and no
//! publication. A request is one operation however often it was retried: called when its first
//! attempt that was not refused began, returned when an attempt was first answered committed, or
//! never, and left out when its client was told it failed; a read is called and returned by its one attempt, and one never answered, or refused, took
//! no effect and is left out (a read changes nothing). Each checker that passes exhibits its order,
//! and each order is held by [`verify`], so a pass is taken on no checker's word.
//!
//! A witness's order is a linearization, so a history the witness checker passes the search checker
//! must pass; a history the search checker refuses the witness checker must refuse. The other
//! disagreement, the witness refused and the search passing, is a system whose own order is wrong
//! though its clients' history has another: a bug in the system's witness, which this reports as
//! such.

use std::collections::BTreeMap;
use std::fmt;

use crate::history::{Operation, Unverified, verify};
use crate::model::Model;
use crate::search::{Budget, Counterexample, SearchError, Spent, Verdict, search};
use crate::witness::{self, Event, Initial, Outcome, Placed, Refused, Request, Tracing, Witnessed};

/// Who an operation is: a request by its key, or a read by its attempt.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Who<K> {
    /// A request.
    Request(K),
    /// A read.
    Read(u64),
}

/// An operation of the clients' history, as the search checker reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Logical<P, K, I, O> {
    /// The object.
    pub object: P,
    /// Who it is.
    pub who: Who<K>,
    /// Its call, return, input and output, the stream's places as times.
    pub op: Operation<I, O>,
}

/// One request as the stream showed it: its operation's place, each attempt's call and whether it
/// was refused, and the first answer committed.
struct Attempts<O> {
    place: usize,
    /// Whether its client was told it failed for good.
    failed: bool,
    calls: Vec<(u64, bool)>,
    committed: Option<(u64, O)>,
}

/// An attempt, by what it is an attempt of.
enum Call<K> {
    /// The `attempt`-th attempt of the request `key`.
    Request { key: K, attempt: usize },
    /// The read at `place`.
    Read { place: usize },
}

/// The operations being gathered from a stream.
struct Gather<P, K, I, O> {
    found: Vec<Option<Logical<P, K, I, O>>>,
    requests: BTreeMap<K, Attempts<O>>,
    calls: BTreeMap<u64, Call<K>>,
}

/// The clients' operations in `events`, in the order each first appeared.
pub fn operations<P: Clone, K: Ord + Clone, I: Clone, O: Clone>(
    events: &[Event<P, K, I, O>],
) -> Vec<Logical<P, K, I, O>> {
    let mut gather = Gather {
        found: Vec::new(),
        requests: BTreeMap::new(),
        calls: BTreeMap::new(),
    };
    for (at, event) in events.iter().enumerate() {
        let at = u64::try_from(at).unwrap_or(u64::MAX);
        match event {
            Event::Invoke { call, request } => gather.invoked(at, *call, request),
            Event::Complete { call, outcome } => gather.completed(at, *call, outcome),
            Event::Publish { .. } => {}
        }
    }
    gather.finish()
}

impl<P: Clone, K: Ord + Clone, I: Clone, O: Clone> Gather<P, K, I, O> {
    fn push(&mut self, object: &P, who: Who<K>, at: u64, input: &I) -> usize {
        self.found.push(Some(Logical {
            object: object.clone(),
            who,
            op: Operation {
                call: at,
                ret: None,
                input: input.clone(),
                output: None,
            },
        }));
        self.found.len().saturating_sub(1)
    }

    fn invoked(&mut self, at: u64, call: u64, request: &Request<P, K, I>) {
        match request {
            Request::Mutation { object, key, input } => {
                if !self.requests.contains_key(key) {
                    let place = self.push(object, Who::Request(key.clone()), at, input);
                    let attempts = Attempts {
                        place,
                        failed: false,
                        calls: Vec::new(),
                        committed: None,
                    };
                    self.requests.insert(key.clone(), attempts);
                }
                if let Some(attempts) = self.requests.get_mut(key) {
                    let attempt = attempts.calls.len();
                    attempts.calls.push((at, false));
                    let key = key.clone();
                    self.calls.insert(call, Call::Request { key, attempt });
                }
            }
            Request::Read { object, input, .. } => {
                let place = self.push(object, Who::Read(call), at, input);
                self.calls.insert(call, Call::Read { place });
            }
        }
    }

    fn completed(&mut self, at: u64, call: u64, outcome: &Outcome<O>) {
        match (self.calls.get(&call), outcome) {
            (Some(Call::Request { key, attempt }), _) => {
                let Some(attempts) = self.requests.get_mut(key) else {
                    return;
                };
                match outcome {
                    Outcome::Committed { output } if attempts.committed.is_none() => {
                        attempts.committed = Some((at, output.clone()));
                    }
                    Outcome::Refused => {
                        if let Some(refused) = attempts.calls.get_mut(*attempt) {
                            refused.1 = true;
                        }
                    }
                    Outcome::Failed => attempts.failed = true,
                    _ => {}
                }
            }
            (Some(Call::Read { place }), Outcome::Read { output, .. }) => {
                if let Some(Some(logical)) = self.found.get_mut(*place) {
                    logical.op.ret = Some(at);
                    logical.op.output = Some(output.clone());
                }
            }
            (Some(Call::Read { place }), _) => {
                if let Some(slot) = self.found.get_mut(*place) {
                    *slot = None;
                }
            }
            (None, _) => {}
        }
    }

    /// Each request called when its first attempt not refused began, and returned at its first
    /// committed answer; one whose every attempt was refused, or whose client was told it failed,
    /// took no effect, and a read never answered none either.
    fn finish(mut self) -> Vec<Logical<P, K, I, O>> {
        for attempts in self.requests.into_values() {
            let Some(slot) = self.found.get_mut(attempts.place) else {
                continue;
            };
            let first = attempts
                .calls
                .iter()
                .filter(|(_, refused)| !refused)
                .map(|(call, _)| *call)
                .min();
            match (slot.as_mut(), first) {
                (Some(logical), Some(call)) if !attempts.failed => {
                    logical.op.call = call;
                    if let Some((ret, output)) = attempts.committed {
                        logical.op.ret = Some(ret.max(call));
                        logical.op.output = Some(output);
                    }
                }
                _ => *slot = None,
            }
        }
        self.found
            .into_iter()
            .flatten()
            .filter(|logical| matches!(logical.who, Who::Request(_)) || logical.op.ret.is_some())
            .collect()
    }
}

/// A specification started from a state of the caller's: an object that had a history before
/// the one checked.
struct Started<'m, M: Model> {
    model: &'m M,
    state: M::State,
}

impl<M: Model> Model for Started<'_, M> {
    type State = M::State;
    type Input = M::Input;
    type Output = M::Output;

    fn init(&self) -> Self::State {
        self.state.clone()
    }

    fn apply(&self, state: &Self::State, input: &Self::Input) -> (Self::Output, Self::State) {
        self.model.apply(state, input)
    }

    fn answers(&self, input: &Self::Input, output: &Self::Output) -> bool {
        self.model.answers(input, output)
    }

    fn bytes(&self, state: &Self::State) -> usize {
        self.model.bytes(state)
    }
}

/// Which checker exhibited an order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Checker {
    /// The witness checker.
    Witness,
    /// The search checker.
    Search,
}

/// The two checkers agreed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Agreement<P, K, S, O> {
    /// Both passed, and both orders verified.
    Linearizable {
        /// The witness checker's report and order.
        witnessed: Witnessed<P, K>,
        /// Each object's search.
        searched: BTreeMap<P, Searched<S, O>>,
    },
    /// Both refused.
    NotLinearizable {
        /// The witness checker's refusal.
        refused: Refused,
        /// The object the search refused, and why.
        object: P,
        /// The search's counterexample.
        counterexample: Counterexample<S, O>,
    },
}

/// One object's search, its verdict's places those of the clients' operations.
pub type Searched<S, O> = crate::search::Searched<S, O>;

/// The two checkers did not agree, or a history could not be judged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Disagreement<P, S, O> {
    /// The witness checker passed a history the search refused: one of them is wrong.
    SearchRefused {
        /// The object.
        object: P,
        /// The search's counterexample.
        counterexample: Counterexample<S, O>,
    },
    /// The search found an order where the system's own order was refused: the witness is wrong.
    WitnessRefused {
        /// The witness checker's refusal.
        refused: Refused,
    },
    /// The search ran out of its budget.
    Unknown {
        /// The object.
        object: P,
        /// What ran out.
        spent: Spent,
    },
    /// An exhibited order does not linearize the history.
    Unverified {
        /// Whose order.
        checker: Checker,
        /// The object.
        object: P,
        /// What is wrong with it.
        error: Unverified<O>,
    },
    /// An operation the search was given is not one.
    Search(SearchError),
}

impl<P: fmt::Debug, S: fmt::Debug, O: fmt::Debug> fmt::Display for Disagreement<P, S, O> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SearchRefused {
                object,
                counterexample,
            } => write!(
                f,
                "the witness passed {object:?} and the search refused it: {counterexample:?}"
            ),
            Self::WitnessRefused { refused } => {
                write!(
                    f,
                    "the search found an order the system's own refuses: {refused}"
                )
            }
            Self::Unknown { object, spent } => {
                write!(f, "the search of {object:?} ran out: {spent:?}")
            }
            Self::Unverified {
                checker,
                object,
                error,
            } => write!(
                f,
                "{checker:?}'s order of {object:?} is no linearization: {error}"
            ),
            Self::Search(error) => write!(f, "{error}"),
        }
    }
}

impl<P: fmt::Debug, S: fmt::Debug, O: fmt::Debug> std::error::Error for Disagreement<P, S, O> {}

/// What [`agree`] finds of a history of `M`'s objects named by `P` and requests by `K`.
pub type Agreed<P, K, M> = Result<
    Agreement<P, K, <M as Model>::State, <M as Model>::Output>,
    Disagreement<P, <M as Model>::State, <M as Model>::Output>,
>;

/// `events`, every client's attempts traced, judged by both checkers within `max_events` and
/// `budget`: their agreement, or what kept them from it.
pub fn agree<M: Model, P: Ord + Clone, K: Ord + Clone>(
    model: &M,
    initial: &[Initial<P, M::State>],
    events: &[Event<P, K, M::Input, M::Output>],
    max_events: usize,
    budget: &Budget,
) -> Agreed<P, K, M> {
    let witnessed = witness::check(model, initial, events, max_events, Tracing::Complete);
    let logical = operations(events);
    let mut searched = BTreeMap::new();
    let mut refusal = None;
    for base in initial {
        let (ops, places): (Vec<_>, Vec<_>) = logical
            .iter()
            .enumerate()
            .filter(|(_, logical)| logical.object == base.object)
            .map(|(place, logical)| (logical.op.clone(), place))
            .unzip();
        let started = Started {
            model,
            state: base.state.clone(),
        };
        let found = search(&started, &ops, budget).map_err(Disagreement::Search)?;
        match &found.verdict {
            Verdict::Linearizable { order } => {
                verify(&started, &ops, order).map_err(|error| Disagreement::Unverified {
                    checker: Checker::Search,
                    object: base.object.clone(),
                    error,
                })?
            }
            Verdict::NotLinearizable(counterexample) => {
                if refusal.is_none() {
                    refusal = Some((base.object.clone(), counterexample.clone()));
                }
            }
            Verdict::Unknown(spent) => {
                return Err(Disagreement::Unknown {
                    object: base.object.clone(),
                    spent: *spent,
                });
            }
        }
        if let Ok(witnessed) = &witnessed {
            verify_witness(&started, base, witnessed, &logical, &ops, &places)?;
        }
        searched.insert(base.object.clone(), found);
    }
    match (witnessed, refusal) {
        (Ok(witnessed), None) => Ok(Agreement::Linearizable {
            witnessed,
            searched,
        }),
        (Err(refused), Some((object, counterexample))) => Ok(Agreement::NotLinearizable {
            refused,
            object,
            counterexample,
        }),
        (Ok(_), Some((object, counterexample))) => Err(Disagreement::SearchRefused {
            object,
            counterexample,
        }),
        (Err(refused), None) => Err(Disagreement::WitnessRefused { refused }),
    }
}

/// The witness's order of one object, as places among that object's operations, verified.
fn verify_witness<M: Model, P: Ord + Clone, K: Ord + Clone>(
    model: &M,
    base: &Initial<P, M::State>,
    witnessed: &Witnessed<P, K>,
    logical: &[Logical<P, K, M::Input, M::Output>],
    ops: &[Operation<M::Input, M::Output>],
    places: &[usize],
) -> Result<(), Disagreement<P, M::State, M::Output>> {
    let unverified = |error| Disagreement::Unverified {
        checker: Checker::Witness,
        object: base.object.clone(),
        error,
    };
    let local: BTreeMap<&Who<K>, usize> = places
        .iter()
        .enumerate()
        .filter_map(|(local, place)| logical.get(*place).map(|op| (&op.who, local)))
        .collect();
    let mut order = Vec::new();
    for placed in witnessed.order.get(&base.object).into_iter().flatten() {
        let who = match placed {
            Placed::Mutation(key) => Who::Request(key.clone()),
            Placed::Read(call) => Who::Read(*call),
        };
        let at = local
            .get(&who)
            .copied()
            .ok_or(unverified(Unverified::Unknown { op: usize::MAX }))?;
        order.push(at);
    }
    verify(model, ops, &order).map_err(unverified)
}
