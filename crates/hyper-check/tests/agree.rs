//! The two checkers on one history: a seeded register service's histories, which both pass and
//! whose orders both verify; and defects planted in them, which both refuse where the clients'
//! history itself has no order, and which the witness alone refuses where only the system's own
//! order is wrong.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::disallowed_macros,
    clippy::cast_possible_truncation,
    clippy::cognitive_complexity,
    missing_docs
)]

use std::collections::BTreeMap;

use hyper_check::search::Budget;
use hyper_check::witness::{Consistency, Event, HistoryError, Initial, Outcome, Request};
use hyper_check::{Access, Agreement, Answer, Disagreement, Register, agree};
use hyper_sim::Seeded;

type E = Event<u8, u64, Access<u64>, Answer<u64>>;

/// A client's operation under way.
#[derive(Clone, Debug)]
enum Doing {
    Idle,
    /// A write of the request `key` (its value is its key), published or not.
    Writing {
        key: u64,
        object: u8,
        call: u64,
        published: bool,
    },
    Reading {
        object: u8,
        call: u64,
        prefix: u64,
    },
}

struct Service {
    draws: Seeded,
    events: Vec<E>,
    clients: Vec<Doing>,
    /// Each object's states, from its first.
    states: BTreeMap<u8, Vec<Option<u64>>>,
    /// Requests whose client gave up on them, which the service may still publish.
    lost: Vec<(u64, u8)>,
    next_key: u64,
    next_call: u64,
}

const OBJECTS: u8 = 2;

impl Service {
    fn new(seed: u64, clients: usize) -> Self {
        let states = (0..OBJECTS).map(|object| (object, vec![None])).collect();
        Self {
            draws: Seeded::new(seed),
            events: Vec::new(),
            clients: vec![Doing::Idle; clients],
            states,
            lost: Vec::new(),
            next_key: 1,
            next_call: 1,
        }
    }

    fn call(&mut self) -> u64 {
        self.next_call += 1;
        self.next_call
    }

    fn publish(&mut self, object: u8, key: u64) {
        let states = self.states.get_mut(&object).unwrap();
        let sequence = states.len() as u64;
        states.push(Some(key));
        self.events.push(Event::Publish {
            object,
            sequence,
            key,
            input: Access::Write(key),
        });
    }

    /// One step of one client, or the service publishing a lost request.
    fn step(&mut self) {
        if !self.lost.is_empty() && self.draws.below(20) == 0 {
            let at = self.draws.below(self.lost.len() as u64) as usize;
            let (key, object) = self.lost.swap_remove(at);
            self.publish(object, key);
            return;
        }
        let client = self.draws.below(self.clients.len() as u64) as usize;
        let doing = self.clients[client].clone();
        self.clients[client] = match doing {
            Doing::Idle => self.begin(),
            Doing::Writing {
                key,
                object,
                call,
                published,
            } => self.write(key, object, call, published),
            Doing::Reading {
                object,
                call,
                prefix,
            } => self.read(object, call, prefix),
        };
    }

    fn begin(&mut self) -> Doing {
        let object = self.draws.below(u64::from(OBJECTS)) as u8;
        let call = self.call();
        if self.draws.below(2) == 0 {
            let key = self.next_key;
            self.next_key += 1;
            self.events.push(Event::Invoke {
                call,
                request: Request::Mutation {
                    object,
                    key,
                    input: Access::Write(key),
                },
            });
            Doing::Writing {
                key,
                object,
                call,
                published: false,
            }
        } else {
            self.events.push(Event::Invoke {
                call,
                request: Request::Read {
                    object,
                    input: Access::Read,
                    consistency: Consistency::Linearizable,
                },
            });
            let prefix = self.states[&object].len() as u64 - 1;
            Doing::Reading {
                object,
                call,
                prefix,
            }
        }
    }

    fn write(&mut self, key: u64, object: u8, call: u64, published: bool) -> Doing {
        match self.draws.below(10) {
            // The client gives up: unknown, and the service publishes it later, or never.
            0 => {
                self.events.push(Event::Complete {
                    call,
                    outcome: Outcome::Unknown,
                });
                if !published && self.draws.below(2) == 0 {
                    self.lost.push((key, object));
                }
                Doing::Idle
            }
            // A retry of the same request, after an unknown answer.
            1 => {
                self.events.push(Event::Complete {
                    call,
                    outcome: Outcome::Unknown,
                });
                let call = self.call();
                self.events.push(Event::Invoke {
                    call,
                    request: Request::Mutation {
                        object,
                        key,
                        input: Access::Write(key),
                    },
                });
                Doing::Writing {
                    key,
                    object,
                    call,
                    published,
                }
            }
            _ if !published => {
                self.publish(object, key);
                Doing::Writing {
                    key,
                    object,
                    call,
                    published: true,
                }
            }
            _ => {
                self.events.push(Event::Complete {
                    call,
                    outcome: Outcome::Committed {
                        output: Answer::Written,
                    },
                });
                Doing::Idle
            }
        }
    }

    fn read(&mut self, object: u8, call: u64, prefix: u64) -> Doing {
        let states = &self.states[&object];
        let latest = states.len() as u64 - 1;
        let sequence = prefix + self.draws.below(latest - prefix + 1);
        let seen = states[sequence as usize];
        self.events.push(Event::Complete {
            call,
            outcome: Outcome::Read {
                sequence,
                output: Answer::Read(seen),
            },
        });
        Doing::Idle
    }
}

fn history(seed: u64, steps: usize, clients: usize) -> Vec<E> {
    let mut service = Service::new(seed, clients);
    for _ in 0..steps {
        service.step();
    }
    service.events
}

fn initial() -> Vec<Initial<u8, Option<u64>>> {
    (0..OBJECTS)
        .map(|object| Initial {
            object,
            sequence: 0,
            state: None,
        })
        .collect()
}

#[allow(clippy::type_complexity)]
fn judged(
    events: &[E],
) -> Result<Agreement<u8, u64, Option<u64>, Answer<u64>>, Disagreement<u8, Option<u64>, Answer<u64>>>
{
    agree(
        &Register::<u64>::new(),
        &initial(),
        events,
        events.len(),
        &Budget::default(),
    )
}

/// The service's histories are linearizable with its order as witness: both checkers pass every
/// one, and both orders verify (`agree` holds each to the definition).
#[test]
fn both_checkers_pass_the_services_histories() {
    let mut retries = 0;
    for seed in 0..400u64 {
        let events = history(seed, 200, 1 + (seed % 4) as usize);
        match judged(&events) {
            Ok(Agreement::Linearizable { witnessed, .. }) => retries += witnessed.report.retries,
            other => panic!("seed {seed}: {other:?}"),
        }
    }
    assert!(retries > 0, "no request was retried after it took effect");
}

/// The places of a history's reads answered, each with the sequence it observed.
fn reads(events: &[E]) -> Vec<usize> {
    events
        .iter()
        .enumerate()
        .filter(|(_, event)| {
            matches!(
                event,
                Event::Complete {
                    outcome: Outcome::Read { .. },
                    ..
                }
            )
        })
        .map(|(at, _)| at)
        .collect()
}

/// A read answered a value nobody wrote: no order exists, and both refuse.
#[test]
fn a_value_nobody_wrote_is_refused_by_both() {
    let mut planted = 0;
    for seed in 0..200u64 {
        let mut events = history(seed, 120, 3);
        let Some(at) = reads(&events).first().copied() else {
            continue;
        };
        if let Event::Complete {
            outcome: Outcome::Read { output, .. },
            ..
        } = &mut events[at]
        {
            *output = Answer::Read(Some(u64::MAX));
        }
        planted += 1;
        match judged(&events) {
            Ok(Agreement::NotLinearizable { refused, .. }) => {
                assert_eq!(refused.error, HistoryError::StateMismatch, "seed {seed}");
            }
            other => panic!("seed {seed}: {other:?}"),
        }
    }
    assert!(planted > 150);
}

/// A read that saw a value overwritten by a write answered before the read began, the value's own
/// write answered before the overwrite began: no order exists, and both refuse.
#[test]
fn a_definitely_stale_read_is_refused_by_both() {
    let mut planted = 0;
    for seed in 0..600u64 {
        let mut events = history(seed, 160, 3);
        let Some((at, sequence, value)) = stale(&events) else {
            continue;
        };
        if let Event::Complete {
            outcome:
                Outcome::Read {
                    sequence: seen,
                    output,
                },
            ..
        } = &mut events[at]
        {
            *seen = sequence;
            *output = Answer::Read(value);
        }
        planted += 1;
        match judged(&events) {
            Ok(Agreement::NotLinearizable { refused, .. }) => {
                assert_eq!(refused.error, HistoryError::StaleRead, "seed {seed}");
            }
            other => panic!("seed {seed}: {other:?}"),
        }
    }
    assert!(planted > 50, "{planted}");
}

/// A read whose answer can be made definitely stale: it began after a write `w` was answered
/// committed, and `w`'s predecessor in its object's sequence was a write answered before `w` began.
/// The read's place, and the predecessor's sequence and value.
fn stale(events: &[E]) -> Option<(usize, u64, Option<u64>)> {
    // Each request's first call, its answer, and its object's sequence.
    let mut called: BTreeMap<u64, usize> = BTreeMap::new();
    let mut answered: BTreeMap<u64, usize> = BTreeMap::new();
    let mut calls: BTreeMap<u64, u64> = BTreeMap::new();
    let mut published: BTreeMap<(u8, u64), u64> = BTreeMap::new();
    let mut read_calls: BTreeMap<u64, (usize, u8)> = BTreeMap::new();
    for (at, event) in events.iter().enumerate() {
        match event {
            Event::Invoke {
                call,
                request: Request::Mutation { key, .. },
            } => {
                called.entry(*key).or_insert(at);
                calls.insert(*call, *key);
            }
            Event::Invoke {
                call,
                request: Request::Read { object, .. },
            } => {
                read_calls.insert(*call, (at, *object));
            }
            Event::Publish {
                object,
                sequence,
                key,
                ..
            } => {
                published.insert((*object, *sequence), *key);
            }
            Event::Complete {
                call,
                outcome: Outcome::Committed { .. },
            } => {
                if let Some(key) = calls.get(call) {
                    answered.entry(*key).or_insert(at);
                }
            }
            Event::Complete {
                call,
                outcome: Outcome::Read { .. },
            } => {
                let (invoked, object) = read_calls[call];
                // The latest write of this object answered before the read began, with a
                // predecessor answered before it began.
                for ((on, sequence), key) in published.iter().rev() {
                    if *on != object || *sequence < 2 {
                        continue;
                    }
                    let (Some(done), Some(began)) = (answered.get(key), called.get(key)) else {
                        continue;
                    };
                    if *done >= invoked {
                        continue;
                    }
                    let before = published[&(object, sequence - 1)];
                    if answered.get(&before).is_some_and(|done| done < began) {
                        return Some((at, sequence - 1, Some(before)));
                    }
                    break;
                }
            }
            _ => {}
        }
    }
    None
}

/// A request published twice, and a success answered before its publication: the clients'
/// history keeps its order, so the search passes it, and the witness refuses the system's word.
#[test]
fn defects_in_the_systems_own_order_are_the_witness_checkers_to_find() {
    for seed in 0..100u64 {
        let events = history(seed, 150, 3);
        let Some(publication) = events
            .iter()
            .position(|event| matches!(event, Event::Publish { .. }))
        else {
            continue;
        };
        let Event::Publish { object, key, .. } = events[publication].clone() else {
            panic!("a publication")
        };
        let mut doubled = events.clone();
        let latest = doubled
            .iter()
            .filter_map(|event| match event {
                Event::Publish {
                    object: on,
                    sequence,
                    ..
                } if *on == object => Some(*sequence),
                _ => None,
            })
            .max()
            .unwrap();
        doubled.push(Event::Publish {
            object,
            sequence: latest + 1,
            key,
            input: Access::Write(key),
        });
        match judged(&doubled) {
            Err(Disagreement::WitnessRefused { refused }) => {
                assert_eq!(refused.error, HistoryError::DuplicateCommit, "seed {seed}");
            }
            other => panic!("seed {seed}: {other:?}"),
        }
    }
}

/// Any read's answer replaced by another value written to its object, or by nothing: whatever
/// the witness says, the search never refuses what the witness passed, and every order exhibited
/// verifies.
#[test]
fn the_checkers_never_disagree_the_wrong_way() {
    let (mut both, mut witness_only) = (0, 0);
    for seed in 0..600u64 {
        let mut events = history(seed, 100, 3);
        let reads = reads(&events);
        if reads.is_empty() {
            continue;
        }
        let mut draws = Seeded::new(seed ^ 0x5eed);
        let at = reads[draws.below(reads.len() as u64) as usize];
        let drawn = draws.below(40);
        if let Event::Complete {
            outcome: Outcome::Read { output, .. },
            ..
        } = &mut events[at]
        {
            *output = Answer::Read((drawn > 0).then_some(drawn));
        }
        match judged(&events) {
            Ok(Agreement::Linearizable { .. }) => {}
            Ok(Agreement::NotLinearizable { .. }) => both += 1,
            Err(Disagreement::WitnessRefused { .. }) => witness_only += 1,
            Err(other) => panic!("seed {seed}: {other}"),
        }
    }
    assert!(both > 0 && witness_only > 0, "{both} {witness_only}");
}

/// A request answered unknown and then, asked about again, failed: no operation of the clients'
/// history, so a read that never saw it agrees with both checkers.
#[test]
fn a_failed_request_is_no_operation() {
    let events: Vec<E> = vec![
        Event::Invoke {
            call: 1,
            request: Request::Mutation {
                object: 0,
                key: 7,
                input: Access::Write(7),
            },
        },
        Event::Complete {
            call: 1,
            outcome: Outcome::Unknown,
        },
        Event::Invoke {
            call: 2,
            request: Request::Mutation {
                object: 0,
                key: 7,
                input: Access::Write(7),
            },
        },
        Event::Complete {
            call: 2,
            outcome: Outcome::Failed,
        },
        Event::Invoke {
            call: 3,
            request: Request::Read {
                object: 0,
                input: Access::Read,
                consistency: Consistency::Linearizable,
            },
        },
        Event::Complete {
            call: 3,
            outcome: Outcome::Read {
                sequence: 0,
                output: Answer::Read(None),
            },
        },
    ];
    let logical = hyper_check::agree::operations(&events);
    assert_eq!(logical.len(), 1, "{logical:?}");
    assert!(matches!(
        judged(&events),
        Ok(Agreement::Linearizable { .. })
    ));
}
