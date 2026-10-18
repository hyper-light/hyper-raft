//! Core step R-8's held commits searched exhaustively (`tests/models/held.rs`, `docs/raft.md`
//! §3.7): the core's rules hold Leader Completeness and agreement over every run of the scope, and
//! each rule taken out is caught.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::disallowed_macros,
    clippy::cast_possible_truncation,
    clippy::too_many_lines,
    clippy::cognitive_complexity,
    missing_docs,
    unreachable_pub,
    dead_code
)]

#[path = "models/held.rs"]
mod held;

use std::num::NonZeroUsize;

use held::{Fault, Held, Mutant, Scope};
use hyper_check::explore::{Model, Outcome, explore, shortest};
use hyper_check::search::{Budget, MEMORY_CEILING};

fn workers() -> NonZeroUsize {
    NonZeroUsize::new(4).unwrap()
}

fn scope(max_term: u8, max_len: usize, spanning: usize, mutant: Mutant) -> Scope {
    Scope {
        max_term,
        max_len,
        spanning,
        mutant,
    }
}

/// The core's rules at two terms and two indexes, and at three terms and two: every run holds
/// Leader Completeness and agreement, and each mechanism the argument rests on is reached.
#[test]
#[ignore = "exhaustive at full scope, in release; CI's explore job runs it"]
fn the_cores_rules_hold_every_committed_entry() {
    for (max_term, max_len, spanning) in [(2u8, 2usize, 2usize), (2, 2, 3), (3, 2, 2)] {
        let model = Held {
            scope: scope(max_term, max_len, spanning, Mutant::None),
        };
        let found = explore(
            &model,
            workers(),
            Budget {
                memory: MEMORY_CEILING,
            },
        )
        .unwrap();
        let Outcome::Exhausted(report) = found else {
            panic!("{max_term} terms, {max_len} indexes, spanning {spanning}: {found:?}");
        };
        println!(
            "{max_term} terms, {max_len} indexes, spanning {spanning}: {} classes, {} levels, peak {} MB; paths {:?}",
            report.classes,
            report.levels,
            report.peak >> 20,
            model.paths().iter().zip(&report.paths).collect::<Vec<_>>()
        );
        for path in [
            "held answer",
            "held commit",
            "marked at open",
            "narrowed",
            "mark ended",
        ] {
            assert!(report.taken(&model, path) > 0, "{path} never reached");
        }
    }
}

/// Each rule taken out: the search finds the defect it is for.
#[test]
#[ignore = "serial searches to each rule's defect, in release; CI's explore job runs them"]
fn each_rule_taken_out_is_caught() {
    for mutant in [
        Mutant::LeaderMemory,
        Mutant::AnswerPastPromise,
        Mutant::DurableCoverOnly,
        Mutant::LowerPromise,
        Mutant::NarrowToOwnLog,
    ] {
        let model = Held {
            scope: scope(2, 2, 2, mutant),
        };
        let start = model.initial();
        let found = shortest(
            &model,
            start,
            &|_: &Fault| true,
            Budget {
                memory: MEMORY_CEILING,
            },
        )
        .unwrap();
        match found {
            Outcome::Fault { history, fault, .. } => println!(
                "{mutant:?}: {fault:?} after {} steps: {history:?}",
                history.len()
            ),
            other => panic!("{mutant:?} not caught: {other:?}"),
        }
    }
}
