//! Both checkers on every history of mantle's range simulation's default seeds (`docs/sim.md`
//! §14.7): each key a register (mantle's `Register`, an object key's current version), its puts
//! published in mantle's commit order. They must agree on every one, and refuse each with a value no
//! put wrote planted in its first read.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::disallowed_macros,
    missing_docs
)]

use std::collections::BTreeMap;

use hyper_check::search::Budget;
use hyper_check::witness::{Consistency, Event, Initial, Outcome, Request};
use hyper_check::{Access, Agreement, Answer, Register, agree};
use hyper_measure::alloc::Counting;
use hyper_measure::cost::{Costs, measure};

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// The recorded histories (`tests/data/mantle-range-histories.txt`, made with
/// `tests/data/mantle-range-recorder.patch` applied to mantle `21ae613`): compiled in, so no test
/// opens a file.
const HISTORIES: &str = include_str!("data/mantle-range-histories.txt");

type E = Event<u8, u64, Access<String>, Answer<String>>;

/// Each seed's history as the witness checker's stream.
fn histories() -> Vec<(u64, Vec<E>)> {
    let mut seeds: Vec<(u64, Vec<E>)> = Vec::new();
    let mut keys: BTreeMap<String, u8> = BTreeMap::new();
    let mut puts: BTreeMap<u64, String> = BTreeMap::new();
    for line in HISTORIES.lines().filter(|line| !line.starts_with('#')) {
        let words: Vec<&str> = line.split_whitespace().collect();
        let mut object = |name: &str| {
            let next = u8::try_from(keys.len()).unwrap();
            *keys.entry(name.to_string()).or_insert(next)
        };
        let event = match words.as_slice() {
            ["s", seed] => {
                seeds.push((seed.parse().unwrap(), Vec::new()));
                continue;
            }
            ["i", "w", op, key, value] => {
                let op: u64 = op.parse().unwrap();
                puts.insert(op, value.to_string());
                Event::Invoke {
                    call: op,
                    request: Request::Mutation {
                        object: object(key),
                        key: op,
                        input: Access::Write(value.to_string()),
                    },
                }
            }
            ["i", "r", op, key] => Event::Invoke {
                call: op.parse().unwrap(),
                request: Request::Read {
                    object: object(key),
                    input: Access::Read,
                    consistency: Consistency::Linearizable,
                },
            },
            ["p", op, key, sequence] => {
                let op: u64 = op.parse().unwrap();
                Event::Publish {
                    object: object(key),
                    sequence: sequence.parse().unwrap(),
                    key: op,
                    input: Access::Write(puts[&op].clone()),
                }
            }
            ["c", "w", op] => Event::Complete {
                call: op.parse().unwrap(),
                outcome: Outcome::Committed {
                    output: Answer::Written,
                },
            },
            ["c", "r", op, sequence, seen] => Event::Complete {
                call: op.parse().unwrap(),
                outcome: Outcome::Read {
                    sequence: sequence.parse().unwrap(),
                    output: Answer::Read((*seen != "-").then(|| seen.to_string())),
                },
            },
            other => panic!("an unknown line: {other:?}"),
        };
        seeds.last_mut().unwrap().1.push(event);
    }
    seeds
}

fn initial() -> Vec<Initial<u8, Option<String>>> {
    (0..2)
        .map(|object| Initial {
            object,
            sequence: 0,
            state: None,
        })
        .collect()
}

#[test]
fn both_checkers_pass_every_history_of_mantles_seeds() {
    let histories = histories();
    assert_eq!(histories.len(), 48);
    let (mut operations, mut configurations) = (0, 0);
    let mut each = Vec::new();
    let mut costs = Costs::new();
    for (seed, events) in &histories {
        let (found, cost) = measure(|| {
            agree(
                &Register::<String>::new(),
                &initial(),
                events,
                events.len(),
                &Budget::default(),
            )
        });
        costs.add(&cost);
        match found {
            Ok(Agreement::Linearizable {
                witnessed,
                searched,
            }) => {
                assert_eq!(witnessed.report.pending, 0, "seed {seed}");
                operations += hyper_check::agree::operations(events).len();
                let searched = searched.values().map(|s| s.configurations).sum::<u64>();
                configurations += searched;
                each.push(searched);
            }
            other => panic!("seed {seed}: {other:?}"),
        }
    }
    let tails = hyper_measure::cost::Tails::of(&mut each).unwrap();
    // The search's memory is bounded by the memo's ceiling (`docs/sim.md` §7); what a history held
    // at most, by the allocator's count, is held against it.
    assert!(costs.peak_bytes() < hyper_check::search::MEMORY_CEILING as u64);
    println!(
        "48 histories, {operations} operations, {configurations} configurations searched; a \
         history's configurations {tails}; the two checkers' cost a history, at a load of {:.2}:\n{}",
        hyper_measure::usage::load().unwrap_or(f64::NAN),
        costs.report()
    );
    assert_eq!(operations, 2_880);
}

#[test]
fn both_checkers_refuse_each_history_with_a_value_nobody_put() {
    for (seed, mut events) in histories() {
        let read = events
            .iter()
            .position(|event| {
                matches!(
                    event,
                    Event::Complete {
                        outcome: Outcome::Read { .. },
                        ..
                    }
                )
            })
            .unwrap();
        if let Event::Complete {
            outcome: Outcome::Read { output, .. },
            ..
        } = &mut events[read]
        {
            *output = Answer::Read(Some("nobody".to_string()));
        }
        let found = agree(
            &Register::<String>::new(),
            &initial(),
            &events,
            events.len(),
            &Budget::default(),
        );
        assert!(
            matches!(found, Ok(Agreement::NotLinearizable { .. })),
            "seed {seed}: {found:?}"
        );
    }
}
