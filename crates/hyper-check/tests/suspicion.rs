//! `docs/models/Suspicion.tla` searched by `hyper_check::explore` (`tests/models/suspicion.rs`): the
//! rules that decide when a member that elects by suspicion acts on its detectors' word, with one
//! voter of three stopped (where acting on a mistake can produce no leader) and with none.
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

#[path = "models/suspicion.rs"]
mod suspicion;

use std::num::NonZeroUsize;

use hyper_check::explore::{Outcome, explore};
use hyper_check::search::{Budget, MEMORY_CEILING};
use suspicion::{Fault, Rule, Scope, Suspicion};

/// The third of three voters stopped for good.
const THIRD: u8 = 1 << 2;

fn scope(stopped: u8, max_term: u8, rule: Rule) -> Scope {
    Scope {
        servers: 3,
        stopped,
        max_term,
        rule,
        stable: stopped != 0,
        reached: false,
        hand_over: false,
    }
}

/// How a search ended: every class visited, or a fault met.
#[derive(Debug, PartialEq, Eq)]
enum End {
    Exhausted(u64),
    Fault(Fault),
}

fn ends(scope: Scope) -> End {
    let model = Suspicion::at(scope);
    match explore(
        &model,
        NonZeroUsize::new(4).unwrap(),
        Budget {
            memory: MEMORY_CEILING,
        },
    )
    .unwrap()
    {
        Outcome::Exhausted(report) => End::Exhausted(report.classes),
        Outcome::Fault { fault, .. } => End::Fault(fault),
        Outcome::Unknown { report, .. } => panic!("unknown at {} classes", report.classes),
    }
}

/// The core's rule, one voter of three stopped: Election Safety, a pre-candidate asks only where it
/// could be elected without the members it suspects, and a group settled on a leader keeps it and
/// its term whatever the detectors say of the live voters, at the states TLC counts
/// (`Suspicion.cfg`).
#[test]
fn with_one_voter_of_three_stopped_a_settled_group_keeps_its_leader() {
    assert_eq!(
        ends(scope(THIRD, 3, Rule::Core)),
        End::Exhausted(11_376),
        "Suspicion.cfg"
    );
}

/// The core's rule with every voter up: Election Safety, at the states TLC counts
/// (`SuspicionFull.cfg`).
#[test]
fn with_every_voter_up_the_rule_keeps_one_leader_a_term() {
    assert_eq!(
        ends(scope(0, 2, Rule::Core)),
        End::Exhausted(1_306_432),
        "SuspicionFull.cfg"
    );
}

/// Each part of the rule taken out, a group settled on a leader loses it, or a pre-candidate asks
/// with no quorum to win by (`SuspicionStepDown.cfg`, `SuspicionUnconditional.cfg`,
/// `SuspicionAsking.cfg`).
#[test]
fn each_part_of_the_rule_taken_out_is_refused() {
    assert_eq!(
        ends(scope(THIRD, 2, Rule::StepDown)),
        End::Fault(Fault::Stable),
        "a leader that steps down"
    );
    assert_eq!(
        ends(scope(THIRD, 2, Rule::Unconditional)),
        End::Fault(Fault::Stable),
        "a leader that orders unconditionally"
    );
    assert_eq!(
        ends(scope(THIRD, 2, Rule::Asking)),
        End::Fault(Fault::AskingCanWin),
        "a pre-candidate that asks on"
    );
}

/// The scopes reach what they are for: a settled leader its detectors leave no quorum, where one
/// voter is stopped (`SuspicionReached.cfg`); and, with every voter up, a candidate a conditional
/// order made granted a quorum, the hand-over a leader that hears none of its voters still makes
/// (`SuspicionHandOver.cfg`).
#[test]
fn the_scopes_reach_what_they_are_for() {
    let mut left = scope(THIRD, 3, Rule::Core);
    left.reached = true;
    assert_eq!(ends(left), End::Fault(Fault::NoLeaderLeftNoQuorum));
    let mut handed = scope(0, 2, Rule::Core);
    handed.hand_over = true;
    assert_eq!(ends(handed), End::Fault(Fault::NoHandOver));
}
