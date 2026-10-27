//! The persisted class's commits with a leader answering ahead of its own disk (core step R-6,
//! `Config::apply_unpersisted`; kiln's CP-T2.2, Focal's condition 2), searched exhaustively
//! (`tests/models/unpersisted.rs`): every entry answered is on a majority's disks at its answer,
//! the leader's memory not counted, and every later leader holds it; the rule that keeps the
//! leader's memory out of the count, taken out, is refused by its shortest history. With durable
//! acknowledgements in flight (Focal's condition 2's advisory), a stale one delivered late is
//! explored at a small scope, and the core's term check, taken out, is refused likewise.
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
                in_flight: 0,
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
            in_flight: 0,
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

/// Focal's condition 2's advisory (kiln CP-T2.2): with durable acknowledgements in flight, a bag of
/// two, any of them delivered at any later step, a stale one from an earlier term is delivered late,
/// and the persisted class still answers only what a majority's disks hold. Three terms and two
/// indexes, the scope the stale acknowledgement needs to matter (its receiver leading again, its
/// log rewritten between): the search exhausts with no fault, every path among its steps.
#[test]
#[ignore = "exhaustive at a small scope, in release; CI's explore job runs it"]
fn every_answered_entry_is_on_a_majoritys_disks_with_acknowledgements_in_flight() {
    let model = Unpersisted {
        scope: Scope {
            max_term: 3,
            max_len: 2,
            mutant: Mutant::None,
            in_flight: 2,
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
        panic!("acknowledgements in flight: {found:?}");
    };
    println!(
        "acknowledgements in flight: {} classes, {} levels, peak {} MB; paths {:?}",
        report.classes,
        report.levels,
        report.peak >> 20,
        model.paths().iter().zip(&report.paths).collect::<Vec<_>>()
    );
    for path in model.paths() {
        assert!(
            report.taken(&model, path) > 0,
            "acknowledgements in flight: {path} never reached"
        );
    }
}

/// The core's term check taken out (`Mutant::StaleAck`): a leader takes a durable acknowledgement
/// whatever its term, and its shortest history to a fault is found (a stale acknowledgement of its
/// earlier lead, its log rewritten since, counted toward an entry of its new term).
#[test]
#[ignore = "a search to the rule's defect, in release; CI's explore job runs it"]
fn a_stale_acknowledgement_taken_is_caught() {
    let model = Unpersisted {
        scope: Scope {
            max_term: 3,
            max_len: 2,
            mutant: Mutant::StaleAck,
            in_flight: 2,
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
                "StaleAck: {fault:?} after {} steps, {} classes: {history:?}",
                history.len(),
                report.classes
            );
        }
        other => panic!("StaleAck not caught: {other:?}"),
    }
}
