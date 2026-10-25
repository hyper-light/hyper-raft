//! The persisted class's commits with a leader answering ahead of its own disk (core step R-6,
//! `Config::apply_unpersisted`; kiln's CP-T2.2, Focal's condition 2), searched exhaustively
//! (`tests/models/unpersisted.rs`): every entry answered is on a majority's disks at its answer,
//! the leader's memory not counted, and every later leader holds it; the rule that keeps the
//! leader's memory out of the count, taken out, is refused by its shortest history.
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

#[path = "models/unpersisted.rs"]
mod unpersisted;

use std::num::NonZeroUsize;

use hyper_check::explore::{Model, Outcome, explore, shortest};
use hyper_check::search::{Budget, MEMORY_CEILING};
use unpersisted::{Fault, Mutant, Scope, Unpersisted};

fn workers() -> NonZeroUsize {
    NonZeroUsize::new(4).unwrap()
}

#[test]
#[ignore = "exhaustive at full scope, in release; CI's explore job runs it"]
fn every_answered_entry_is_on_a_majoritys_disks() {
    for (max_term, max_len) in [(2u8, 2usize), (2, 3), (3, 2)] {
        let model = Unpersisted {
            scope: Scope {
                max_term,
                max_len,
                mutant: Mutant::None,
            },
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
            panic!("{max_term} terms, {max_len} indexes: {found:?}");
        };
        println!(
            "{max_term} terms, {max_len} indexes: {} classes, {} levels, peak {} MB; paths {:?}",
            report.classes,
            report.levels,
            report.peak >> 20,
            model.paths().iter().zip(&report.paths).collect::<Vec<_>>()
        );
        for path in model.paths() {
            assert!(
                report.taken(&model, path) > 0,
                "{max_term} terms, {max_len} indexes: {path} never reached"
            );
        }
    }
}

#[test]
#[ignore = "a search to the rule's defect, in release; CI's explore job runs it"]
fn the_leaders_memory_counted_is_caught() {
    let model = Unpersisted {
        scope: Scope {
            max_term: 2,
            max_len: 2,
            mutant: Mutant::LeaderMemory,
        },
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
        Outcome::Fault {
            history,
            fault,
            report,
            ..
        } => {
            println!(
                "LeaderMemory: {fault:?} after {} steps, {} classes: {history:?}",
                history.len(),
                report.classes
            );
            assert_eq!(fault, Fault::OffMajority);
        }
        other => panic!("LeaderMemory not caught: {other:?}"),
    }
}
