//! slates' models of the fast track (`docs/sim.md` §4.5, §8: `support/exhaustive.rs`,
//! `slot_model.rs` and `prefix_model.rs` moved into hyper-check), built again on
//! `hyper_check::explore` and held to the class counts and shortest histories slates recorded
//! (slates `docs/wip/BENCHMARKS.md`, "The slot model" of 2026-09-28 and "The prefix model,
//! corrected" of 2026-09-29): a class count is exact, so the same model under an exact
//! representative per orbit reaches the same count, whatever the search's order or code.
//!
//! The scopes that fit the default suite run in it; the full scopes run in release in CI's
//! `explore` job (`--ignored`), and every one of them on the owner's machine with its time and
//! peak in `docs/benchmarks.md`, "hyper-check's searches (S-5)".
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::disallowed_macros,
    clippy::cast_possible_truncation,
    clippy::cognitive_complexity,
    clippy::too_many_lines,
    missing_docs,
    unreachable_pub,
    dead_code
)]

mod models;

use std::num::NonZeroUsize;

use hyper_check::explore::{Found, Model, Outcome, Report, explore, run_script, shortest};
use hyper_check::search::{Budget, MEMORY_CEILING};
use hyper_measure::alloc::Counting;
use models::prefix::{self, PrefixModel, Tail, Variant};
use models::slot::{self, Action, Reading, Rule, Scope, SlotModel};

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// The workers a parallel search runs on: four, GitHub's standard Linux runner's vCPUs (the
/// runner `docs/models/README.md` measured TLC's suite on), so a count measured here is measured
/// as CI runs it. The classes a search counts do not depend on it
/// (`the_workers_change_no_count`).
fn workers() -> NonZeroUsize {
    NonZeroUsize::new(4).unwrap()
}

fn budget() -> Budget {
    Budget {
        memory: MEMORY_CEILING,
    }
}

fn exhausted<M: Model>(model: &M, found: Found<M>, label: &str) -> Report {
    match found {
        Outcome::Exhausted(report) => {
            println!(
                "{label}: {} classes, {} levels, {} bytes at the peak; {}",
                report.classes,
                report.levels,
                report.peak,
                model
                    .paths()
                    .iter()
                    .zip(&report.paths)
                    .map(|(name, count)| format!("{name} {count}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            report
        }
        Outcome::Fault {
            from,
            action,
            fault,
            report,
            ..
        } => panic!(
            "{label}: {fault:?} by {action:?} from {from:?} after {} classes",
            report.classes
        ),
        Outcome::Unknown { report, spent } => panic!(
            "{label}: the budget was reached ({spent:?}) at {} classes",
            report.classes
        ),
    }
}

// ---------------------------------------------------------------------------------------------
// The slot model.

fn slot_scope(nodes: usize, indices: usize, values: u8, terms: u8, rule: Rule) -> Scope {
    Scope {
        nodes,
        indices,
        values,
        terms,
        rule,
    }
}

/// The ballot rule at `scope`: no fault, every path taken that the scope has (a commit above an
/// uncommitted index needs two indexes), and the classes counted.
fn ballots_hold(scope: Scope) -> Report {
    let model = SlotModel::at(scope);
    let report = exhausted(
        &model,
        explore(&model, workers(), budget()).unwrap(),
        &format!("slot model, {scope:?}"),
    );
    for (bit, (name, count)) in slot::PATHS.iter().zip(&report.paths).enumerate() {
        let reachable = scope.indices > 1 || 1u64 << bit != slot::OUT_OF_ORDER_COMMIT;
        assert!(!reachable || *count > 0, "{name} never ran at {scope:?}");
    }
    report
}

/// The ballot rule keeps agreement and P2c at four members, one index, two values and four terms:
/// 463,715 classes, the count slates' search reached (`slot_model.rs`, 2026-09-28) and the count
/// `docs/sim.md` §9 holds S-5 to.
#[test]
fn the_ballot_recovery_reaches_slates_count_of_classes() {
    let report = ballots_hold(slot_scope(4, 1, 2, 4, Rule::Ballots));
    assert_eq!(report.classes, 463_715);
}

/// The classes a search counts do not depend on how many workers share it, nor on whether it is
/// the serial search.
#[test]
fn the_workers_change_no_count() {
    let scope = slot_scope(4, 1, 2, 3, Rule::Ballots);
    let model = SlotModel::at(scope);
    let counts: Vec<u64> = [1, 2, 3, 7]
        .into_iter()
        .map(|workers| {
            let found = explore(&model, NonZeroUsize::new(workers).unwrap(), budget()).unwrap();
            exhausted(&model, found, &format!("{workers} workers")).classes
        })
        .collect();
    let serial = exhausted(
        &model,
        shortest(&model, model.initial(), &|_| true, budget()).unwrap(),
        "serial",
    );
    assert!(
        counts.iter().all(|count| *count == serial.classes),
        "{counts:?} {}",
        serial.classes
    );
}

/// The ballot rule at slates' other measured scopes: five members (a fast quorum larger than a
/// classic one), three values (a fast round split three ways), and two indexes.
#[test]
#[ignore = "exhaustive at full scope; CI's explore job runs it in release"]
fn the_ballot_recovery_reaches_slates_counts_at_full_scope() {
    for (nodes, indices, values, terms, classes) in [
        (5, 1, 2, 3, 1_586_398),
        (4, 1, 3, 4, 642_654),
        (5, 1, 2, 4, 23_552_907),
    ] {
        let report = ballots_hold(slot_scope(nodes, indices, values, terms, Rule::Ballots));
        assert_eq!(
            report.classes, classes,
            "{nodes}, {indices}, {values}, {terms}"
        );
    }
    ballots_hold(slot_scope(3, 2, 2, 2, Rule::Ballots));
}

/// Fast Raft's published recovery commits two values at one index under the two readings that let
/// a leader decide by votes over an entry it holds, and the search gives the shortest history of
/// each; the third reading has no fault in the scope, its whole space 999,583 classes (slates'
/// count), and fails liveness instead
/// ([`keeping_leader_approved_entries_stalls_the_log_after_one_crash`]).
#[test]
#[ignore = "three serial searches of a million classes each: CI's explore job runs them in release"]
fn the_published_recovery_loses_agreement_when_its_leader_decides_by_votes() {
    // slates' serial search met each disagreement at these counts and depths; the same order of
    // actions meets it at the same count.
    for (reading, steps, classes) in [
        (Reading::Literal, 15, 1_049_232),
        (Reading::OncePerTerm, 18, 1_199_113),
    ] {
        let model = SlotModel::at(slot_scope(4, 1, 2, 4, Rule::Published(reading)));
        let found = shortest(
            &model,
            model.initial(),
            &|fault| matches!(fault, slot::Fault::Disagreement { .. }),
            budget(),
        )
        .unwrap();
        let Outcome::Fault {
            history,
            fault,
            report,
            ..
        } = found
        else {
            panic!("{reading:?}: no disagreement found");
        };
        println!(
            "{reading:?}: {fault:?} after {} steps, {} classes:\n  {}",
            history.len(),
            report.classes,
            history
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n  ")
        );
        assert!(
            report.taken(&model, slot::PATHS[0]) > 0,
            "{reading:?}: no fast commit"
        );
        assert_eq!(
            (history.len(), report.classes),
            (steps, classes),
            "{reading:?}"
        );
        // The history is concrete and the fault was met from a representative, which renames
        // values: the replay raises a fault of the same kind.
        let (_, replayed) = run_script(&model, model.initial(), &history).unwrap();
        assert!(
            matches!(replayed, Some(slot::Fault::Disagreement { .. })),
            "{replayed:?}"
        );
    }
    let model = SlotModel::at(slot_scope(
        4,
        1,
        2,
        4,
        Rule::Published(Reading::KeepLeaderApproved),
    ));
    let report = exhausted(
        &model,
        shortest(
            &model,
            model.initial(),
            &|fault| matches!(fault, slot::Fault::Disagreement { .. }),
            budget(),
        )
        .unwrap(),
        "keeping leader-approved entries",
    );
    assert_eq!(report.classes, 999_583);
}

const A: usize = 0;
const B: usize = 1;
const C: usize = 2;
const D: usize = 3;
const E: usize = 4;
const W: u8 = 0;
const V: u8 = 1;

fn of(members: &[usize]) -> u8 {
    members.iter().fold(0, |mask, m| mask | (1 << m))
}

/// Slates' §3.7 by hand, with five members, where a fast quorum (four) is larger than a classic one
/// (three), under the reading that decides once a term: A decides w in term 1; B decides v in term
/// 2, which reaches C; C leads term 3 and B, C, D and E hold v, a fast quorum, so v commits; A,
/// whose last leader-approved entry is w of term 1, wins term 4 by D and E, who hold only
/// self-approved v, and its w overwrites theirs and commits.
#[test]
fn five_members_lose_a_fast_committed_entry_under_the_published_recovery() {
    let model = SlotModel::at(slot_scope(
        5,
        1,
        2,
        4,
        Rule::Published(Reading::OncePerTerm),
    ));
    let history = [
        Action::Timeout { node: A },
        Action::Elect {
            node: A,
            quorum: of(&[A, B, C]),
        },
        Action::Insert {
            node: A,
            index: 0,
            value: W,
        },
        Action::Insert {
            node: B,
            index: 0,
            value: W,
        },
        Action::Insert {
            node: C,
            index: 0,
            value: V,
        },
        Action::Decide {
            leader: A,
            index: 0,
            value: W,
            voters: of(&[A, B, C]),
        },
        Action::Timeout { node: B },
        Action::Elect {
            node: B,
            quorum: of(&[B, C, D]),
        },
        Action::Insert {
            node: D,
            index: 0,
            value: V,
        },
        Action::Learn { node: E, term: 2 },
        Action::Insert {
            node: E,
            index: 0,
            value: V,
        },
        Action::Decide {
            leader: B,
            index: 0,
            value: V,
            voters: of(&[B, C, D, E]),
        },
        Action::Replicate {
            leader: B,
            node: C,
            index: 0,
        },
        Action::Timeout { node: C },
        Action::Elect {
            node: C,
            quorum: of(&[C, D, E]),
        },
        Action::Learn { node: B, term: 3 },
        Action::Decide {
            leader: C,
            index: 0,
            value: V,
            voters: of(&[B, C, D, E]),
        },
        Action::Timeout { node: A },
        Action::Timeout { node: A },
        Action::Timeout { node: A },
        Action::Elect {
            node: A,
            quorum: of(&[A, D, E]),
        },
    ];
    let overwrite = [Action::Replicate {
        leader: A,
        node: D,
        index: 0,
    }];
    let commit = [
        Action::Replicate {
            leader: A,
            node: E,
            index: 0,
        },
        Action::Decide {
            leader: A,
            index: 0,
            value: W,
            voters: of(&[A, D, E]),
        },
        Action::Replicate {
            leader: A,
            node: D,
            index: 0,
        },
        Action::Replicate {
            leader: A,
            node: E,
            index: 0,
        },
        Action::Commit {
            leader: A,
            index: 0,
        },
    ];
    let start = model.initial();
    assert_eq!(run_script(&model, start, &history).unwrap().1, None);
    assert_eq!(
        run_script(&model, start, &[&history[..], &overwrite].concat())
            .unwrap()
            .1,
        Some(slot::Fault::OverwroteChosen {
            index: 0,
            chosen: V,
            sent: W,
            term: 4
        })
    );
    assert_eq!(
        run_script(&model, start, &[&history[..], &overwrite, &commit].concat())
            .unwrap()
            .1,
        Some(slot::Fault::Disagreement {
            index: 0,
            first: V,
            second: W
        })
    );
}

/// Under the reading that keeps leader-approved entries, one crash between a decision and its
/// commit stalls the log: A decides w and crashes with only B holding it; B leads term 2 and
/// replicates w to all, but may not decide over it by votes nor commit it, not being of its term.
/// Every future of that state is searched, and none commits.
#[test]
fn keeping_leader_approved_entries_stalls_the_log_after_one_crash() {
    let model = SlotModel::at(slot_scope(
        4,
        1,
        2,
        4,
        Rule::Published(Reading::KeepLeaderApproved),
    ));
    let history = [
        Action::Timeout { node: A },
        Action::Elect {
            node: A,
            quorum: of(&[A, B, C]),
        },
        Action::Insert {
            node: A,
            index: 0,
            value: W,
        },
        Action::Insert {
            node: B,
            index: 0,
            value: W,
        },
        Action::Insert {
            node: C,
            index: 0,
            value: V,
        },
        Action::Decide {
            leader: A,
            index: 0,
            value: W,
            voters: of(&[A, B, C]),
        },
        Action::Replicate {
            leader: A,
            node: B,
            index: 0,
        },
        Action::Timeout { node: B },
        Action::Elect {
            node: B,
            quorum: of(&[B, C, D]),
        },
        Action::Replicate {
            leader: B,
            node: A,
            index: 0,
        },
        Action::Replicate {
            leader: B,
            node: C,
            index: 0,
        },
        Action::Replicate {
            leader: B,
            node: D,
            index: 0,
        },
    ];
    let (stuck, fault) = run_script(&model, model.initial(), &history).unwrap();
    assert_eq!(fault, None);
    let futures = exhausted(
        &model,
        shortest(&model, stuck, &|_| true, budget()).unwrap(),
        "the stalled state's futures",
    );
    assert!(futures.classes > 1);
    assert_eq!(
        futures.taken(&model, slot::PATHS[0]) + futures.taken(&model, slot::PATHS[1]),
        0,
        "some future of the stalled state commits"
    );
}

// ---------------------------------------------------------------------------------------------
// The prefix model.

fn prefix_scope(
    nodes: usize,
    indices: usize,
    values: u8,
    terms: u8,
    variant: Variant,
) -> prefix::Scope {
    prefix::Scope {
        nodes,
        indices,
        values,
        terms,
        variant,
        tail: Tail::Cleared,
    }
}

fn stale(scope: prefix::Scope) -> prefix::Scope {
    prefix::Scope {
        tail: Tail::Stale,
        ..scope
    }
}

/// The design at `scope`: no fault, every path the design has taken (a hole needs three indexes),
/// and no committed entry ever kept under an older term than a leader's.
fn design_holds(scope: prefix::Scope) -> Report {
    let model = PrefixModel::at(scope);
    let report = exhausted(
        &model,
        explore(&model, workers(), budget()).unwrap(),
        &format!("prefix model, {scope:?}"),
    );
    for name in prefix::DESIGN_PATHS {
        assert!(
            report.taken(&model, name) > 0,
            "{name} never ran at {scope:?}"
        );
    }
    assert!(
        scope.indices < 3 || report.taken(&model, "holes filled with no-ops") > 0,
        "no hole filled at {scope:?}"
    );
    assert_eq!(
        report.taken(&model, "committed entries kept under an older term"),
        0,
        "a committed entry kept under an older term at {scope:?}"
    );
    report
}

/// The design at three members, three indexes, one value and two terms: 1,951,672 classes, slates'
/// count for the corrected design (2026-09-29), with a cut log's tail cleared or kept stale as
/// slates' model keeps it (at two terms no stale tail splits a class).
#[test]
#[ignore = "two searches of two million classes: CI's explore job runs them in release"]
fn the_prefix_design_reaches_slates_count_of_classes() {
    let scope = prefix_scope(3, 3, 1, 2, Variant::Design);
    assert_eq!(design_holds(scope).classes, 1_951_672);
    assert_eq!(design_holds(stale(scope)).classes, 1_951_672);
}

/// The design at slates' full scopes. With a cut log's tail kept stale, as slates' model keeps it,
/// each reaches slates' recorded count exactly; with the tail cleared, each reaches fewer, by the
/// keys a stale tail split off one orbit (slates' signature reads a log's places past its length):
/// these are the scopes' classes, one key an orbit.
#[test]
#[ignore = "exhaustive at full scope; CI's explore job runs it in release"]
fn the_prefix_design_reaches_slates_counts_at_full_scope() {
    for (nodes, indices, values, terms, slates, orbits) in [
        (3, 3, 1, 3, 21_776_022, 21_771_580),
        (4, 2, 2, 3, 12_559_351, 12_551_719),
        (3, 2, 2, 4, 3_401_082, 3_400_472),
    ] {
        let scope = prefix_scope(nodes, indices, values, terms, Variant::Design);
        assert_eq!(
            design_holds(stale(scope)).classes,
            slates,
            "{scope:?}, stale"
        );
        assert_eq!(design_holds(scope).classes, orbits, "{scope:?}");
    }
}

/// Every `Variant` slates' search rejected, refused: each of the four that loses a committed entry
/// or breaks log matching is refused by its shortest history, of the length slates' serial search
/// found (12, 16, 12 and 18 steps); reporting log entries too keeps every property but recovers a
/// deposed leader's entry from a log, which the design never does, and is refused for it.
#[test]
#[ignore = "the serial searches to the rejected variants' faults: CI's explore job runs them in release"]
fn every_rejected_variant_is_refused() {
    refuse_each_variant();
}

fn refuse_each_variant() {
    // Each variant's shortest history, and the classes the serial search reached when it met it:
    // with the stale tail slates' counts (its `BENCHMARKS.md`, 2026-09-29), with it cleared the
    // orbits'. The search's order of actions is slates', so it meets the fault where slates' did.
    for (variant, terms, steps, slates, orbits) in [
        (Variant::CommitFromWindows, 3, 12, 382_952, 382_705),
        (Variant::DropCovered, 3, 16, 2_015_576, 2_014_571),
        (Variant::PruneAtFastCommit, 3, 12, 860_982, 860_670),
        (Variant::DropAtSync, 4, 18, 15_379_817, 15_374_180),
    ] {
        for (tail, classes) in [(Tail::Stale, slates), (Tail::Cleared, orbits)] {
            let scope = prefix::Scope {
                tail,
                ..prefix_scope(3, 3, 1, terms, variant)
            };
            let model = PrefixModel::at(scope);
            let found = shortest(&model, model.initial(), &|_| true, budget()).unwrap();
            let Outcome::Fault {
                history,
                fault,
                report,
                ..
            } = found
            else {
                panic!("{variant:?}, {tail:?}: refused nothing ({found:?})");
            };
            println!(
                "{variant:?}, {tail:?}: {fault:?} after {} steps, {} classes reached:\n  {}",
                history.len(),
                report.classes,
                history
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("\n  ")
            );
            assert!(
                matches!(
                    fault,
                    prefix::Fault::LeaderIncomplete { .. }
                        | prefix::Fault::Disagreement { .. }
                        | prefix::Fault::LogsDiverge { .. }
                ),
                "{variant:?}: {fault:?}"
            );
            assert_eq!(
                (history.len(), report.classes),
                (steps, classes),
                "{variant:?}, {tail:?}"
            );
            let (_, replayed) = run_script(&model, model.initial(), &history).unwrap();
            assert_eq!(
                replayed.map(|f| std::mem::discriminant(&f)),
                Some(std::mem::discriminant(&fault))
            );
        }
    }
    // Reporting logs too: slates' count with the stale tail, the orbits' without, and in both the
    // 49,654 recoveries from a log slates counted.
    for (tail, classes) in [(Tail::Stale, 21_789_824), (Tail::Cleared, 21_785_636)] {
        let scope = prefix::Scope {
            tail,
            ..prefix_scope(3, 3, 1, 3, Variant::ReportLogsToo)
        };
        let model = PrefixModel::at(scope);
        let report = exhausted(
            &model,
            explore(&model, workers(), budget()).unwrap(),
            "reporting logs too",
        );
        assert_eq!(report.classes, classes, "{tail:?}");
        assert_eq!(
            report.taken(&model, "recoveries from a log above the candidate's"),
            49_654
        );
    }
}

/// The variants a default suite affords, refused at the same scope as above where their faults lie
/// within it: the two whose shortest histories are 12 steps.
#[test]
#[ignore = "serial searches of up to a million classes: CI's explore job runs them in release"]
fn the_variants_that_fail_in_twelve_steps_are_refused() {
    for variant in [Variant::CommitFromWindows, Variant::PruneAtFastCommit] {
        let model = PrefixModel::at(prefix_scope(3, 3, 1, 3, variant));
        let found = shortest(&model, model.initial(), &|_| true, budget()).unwrap();
        let Outcome::Fault { history, .. } = found else {
            panic!("{variant:?} refused nothing");
        };
        assert_eq!(history.len(), 12, "{variant:?}");
    }
}

/// A search held to a budget it cannot fit says Unknown, never a pass, and holds no more than the
/// budget: the parallel search by its own count, the serial one (on this thread) by the counting
/// allocator's.
#[test]
fn a_search_past_its_budget_says_unknown_and_holds_no_more() {
    let model = SlotModel::at(slot_scope(4, 1, 2, 4, Rule::Ballots));
    let small = Budget { memory: 1 << 20 };
    let found = explore(&model, workers(), small).unwrap();
    let Outcome::Unknown { report, .. } = found else {
        panic!("a search of 463,715 classes fit a mebibyte: {found:?}");
    };
    assert!(
        report.classes < 463_715 && report.peak <= small.memory,
        "{report:?}"
    );
    hyper_measure::alloc::begin();
    let found = shortest(&model, model.initial(), &|_| true, small).unwrap();
    let counts = hyper_measure::alloc::end();
    let Outcome::Unknown { report, .. } = found else {
        panic!("a serial search of 463,715 classes fit a mebibyte");
    };
    println!(
        "within a MiB: {} classes, counted {} bytes, the allocator's peak {}",
        report.classes, report.peak, counts.peak
    );
    assert!(report.peak <= small.memory, "{report:?}");
    assert!(
        u64::try_from(counts.peak).unwrap() <= small.memory as u64,
        "{counts:?}"
    );
}
