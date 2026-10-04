//! What the checkers cost (`docs/benchmarks.md`, "hyper-check's checkers"):
//! `cargo bench -p hyper-check --bench search -- [rounds]`.
//!
//! - **pass**: the search on an honest register history of 20,000 operations by three clients
//!   (mantle's three gateways), each read seeing the value at a point in its interval: the order
//!   found on the first path, a configuration an operation.
//! - **refuse**: the search on a history with no order, every configuration reachable explored:
//!   six clients each writing and reading eight times, and a last read of a value nobody wrote. The
//!   fingerprint search, then the confirmation on whole keys.
//! - **witness**: the witness checker on the honest history's stream of calls, publications and
//!   returns.
//!
//! Per configuration (per event for the witness): wall and CPU nanoseconds, instructions and cycles
//! (`docs/tails.md` §1a; where the OS counts them), allocations, and the most bytes held at once by
//! the whole run, each round printing the one-minute load average read just before it.
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::expect_used,
    clippy::disallowed_macros,
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::indexing_slicing,
    clippy::cognitive_complexity,
    missing_docs
)]
use std::time::Instant;

use hyper_check::search::{Budget, Verdict, search};
use hyper_check::witness::{self, Consistency, Event, Initial, Outcome, Request, Tracing};
use hyper_check::{Access, Answer, Operation, Register};
use hyper_measure::{alloc, cost, usage};
use hyper_sim::Seeded;

#[global_allocator]
static ALLOCATOR: alloc::Counting = alloc::Counting;

#[allow(
    clippy::disallowed_methods,
    reason = "a benchmark times itself on the host clock"
)]
fn host_now() -> Instant {
    Instant::now()
}

type Op = Operation<Access<u64>, Answer<u64>>;
type Stream = Vec<Event<u8, u64, Access<u64>, Answer<u64>>>;

/// An honest history of `len` operations by `clients` clients, each read seeing the value at its
/// point, as the witness's stream and as operations.
fn honest(seed: u64, len: usize, clients: usize) -> (Vec<Op>, Stream) {
    let mut draws = Seeded::new(seed);
    let mut free = vec![0u64; clients];
    // (time, kind: 0 call, 1 point, 2 return, op)
    let mut marks: Vec<(u64, u8, usize)> = Vec::new();
    let mut ops: Vec<Op> = Vec::new();
    for at in 0..len {
        let client = draws.below(clients as u64) as usize;
        let call = free[client] + draws.below(3);
        let point = call + 1 + draws.below(3);
        let ret = point + 1 + draws.below(3);
        free[client] = ret + 1;
        let write = draws.below(2) == 0;
        ops.push(Operation {
            call: call * 4,
            ret: Some(ret * 4 + 2),
            input: if write {
                Access::Write(at as u64 + 1)
            } else {
                Access::Read
            },
            output: None,
        });
        marks.push((call * 4, 0, at));
        marks.push((point * 4 + 1, 1, at));
        marks.push((ret * 4 + 2, 2, at));
    }
    marks.sort_unstable();
    let mut value = None;
    let mut sequence = 0u64;
    let mut events = Vec::new();
    let mut seen: Vec<(u64, Option<u64>)> = vec![(0, None); len];
    for (_, kind, at) in marks {
        let key = at as u64;
        match (kind, ops[at].input.clone()) {
            (0, Access::Write(v)) => events.push(Event::Invoke {
                call: key,
                request: Request::Mutation {
                    object: 0,
                    key,
                    input: Access::Write(v),
                },
            }),
            (0, Access::Read) => events.push(Event::Invoke {
                call: key,
                request: Request::Read {
                    object: 0,
                    input: Access::Read,
                    consistency: Consistency::Linearizable,
                },
            }),
            (1, Access::Write(v)) => {
                value = Some(v);
                sequence += 1;
                events.push(Event::Publish {
                    object: 0,
                    sequence,
                    key,
                    input: Access::Write(v),
                });
                ops[at].output = Some(Answer::Written);
            }
            (1, Access::Read) => {
                seen[at] = (sequence, value);
                ops[at].output = Some(Answer::Read(value));
            }
            (2, Access::Write(_)) => events.push(Event::Complete {
                call: key,
                outcome: Outcome::Committed {
                    output: Answer::Written,
                },
            }),
            (_, Access::Read) => events.push(Event::Complete {
                call: key,
                outcome: Outcome::Read {
                    sequence: seen[at].0,
                    output: Answer::Read(seen[at].1),
                },
            }),
            _ => {}
        }
    }
    (ops, events)
}

/// A history with no order, its whole space explored: `clients` clients each writing and reading
/// `each` times concurrently, and a last read of a value nobody wrote.
fn refused(clients: usize, each: usize) -> Vec<Op> {
    let mut ops = Vec::new();
    for client in 0..clients {
        for round in 0..each {
            let start = (round * 10) as u64;
            let value = (client * 1_000 + round) as u64 + 1;
            ops.push(Operation {
                call: start,
                ret: Some(start + 9),
                input: Access::Write(value),
                output: Some(Answer::Written),
            });
            ops.push(Operation {
                call: start,
                ret: Some(start + 9),
                input: Access::Read,
                output: Some(Answer::Read(Some(value))),
            });
        }
    }
    let end = (each * 10) as u64;
    ops.push(Operation {
        call: end,
        ret: Some(end + 1),
        input: Access::Read,
        output: Some(Answer::Read(Some(u64::MAX))),
    });
    ops
}

struct Run {
    units: u64,
    nanos: f64,
    cost: cost::Cost,
}

fn run(work: impl FnOnce() -> u64) -> Run {
    let start = host_now();
    let (units, cost) = cost::measure(work);
    let nanos = start.elapsed().as_nanos() as f64;
    Run { units, nanos, cost }
}

fn report(name: &str, run: &Run) {
    let units = run.units.max(1) as f64;
    let per = |count: Option<u64>| {
        count.map_or_else(
            || "unmeasured".to_string(),
            |count| format!("{:.1}", count as f64 / units),
        )
    };
    let usage = run.cost.usage;
    println!(
        "{name}: {} units; a unit: {:.1} ns wall, {} ns CPU, {} instructions, {} cycles, {:.3} \
         allocations; {} bytes held at most",
        run.units,
        run.nanos / units,
        per(usage.map(|usage| usage.cpu_ns())),
        per(usage.and_then(|usage| usage.instructions)),
        per(usage.and_then(|usage| usage.cycles)),
        run.cost.counts.calls() as f64 / units,
        run.cost.counts.peak
    );
}

fn main() {
    let rounds: u32 = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(3);
    let register = Register::<u64>::new();
    let (ops, events) = honest(1, 20_000, 3);
    let wide = refused(6, 8);
    let initial = [Initial {
        object: 0u8,
        sequence: 0,
        state: None,
    }];
    for round in 0..rounds {
        println!(
            "round {round}, load {:.2}",
            usage::load().unwrap_or(f64::NAN)
        );
        let pass = run(|| {
            let found = search(&register, &ops, &Budget::default()).unwrap();
            assert!(matches!(found.verdict, Verdict::Linearizable { .. }));
            found.configurations
        });
        report("pass (configurations)", &pass);
        let mut confirmed = 0;
        let refuse = run(|| {
            let found = search(&register, &wide, &Budget::default()).unwrap();
            assert!(matches!(found.verdict, Verdict::NotLinearizable(_)));
            confirmed = found.confirmed.unwrap();
            found.configurations + confirmed
        });
        report("refuse (configurations, both runs)", &refuse);
        println!("  of which on whole keys: {confirmed}");
        let witness = run(|| {
            let found = witness::check(
                &register,
                &initial,
                &events,
                events.len(),
                Tracing::Complete,
            )
            .unwrap();
            assert_eq!(found.report.pending, 0);
            events.len() as u64
        });
        report("witness (events)", &witness);
    }
}
