//! `docs/models/FastTrack.tla` searched by `hyper_check::explore` (`tests/models/fasttrack.rs`).
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

#[path = "models/fasttrack.rs"]
mod fasttrack;

use std::num::NonZeroUsize;

use fasttrack::{
    ANY, Action, Configs, Counts, EVERY, FastTrack, Fault, Marks, Reach, Releases, Reports, Rule,
    Scope, Votes,
};
use hyper_check::explore::{Model, Outcome, explore, run_script};
use hyper_check::search::{Budget, MEMORY_CEILING};

fn workers() -> NonZeroUsize {
    NonZeroUsize::new(4).unwrap()
}

fn budget() -> Budget {
    Budget {
        memory: MEMORY_CEILING,
    }
}

fn three(max_term: u8, max_len: usize, held_at: u8, values: u8) -> Scope {
    Scope {
        servers: 3,
        initial: 0b111,
        target: 0b111,
        joint: false,
        values,
        max_term,
        max_len,
        held_at,
        losers: 0,
        rule: Rule::Most,
        marks: Marks::Core,
        counts: Counts::Round,
        configs: Configs::Term,
        releases: Releases::Classic,
        votes: Votes::Held,
        reports: Reports::Held,
        reach: Reach::None,
        rename_servers: true,
        rename_values: true,
        leads: [ANY; 5],
        proposed: [EVERY; 5],
        reduce: false,
        legacy: false,
    }
}

/// The four members of the scenario.
const A: u8 = 0;
const B: u8 = 1;
const C: u8 = 2;
const D: u8 = 3;
const NOOP: u8 = fasttrack::NOOP;
const V1: u8 = fasttrack::V1;
const V2: u8 = fasttrack::V1 + 1;

/// The scenario of `docs/models/FastTrackScenario.tla` (swarm fast seed 41,345's shape): A leads
/// term 1, B term 2, D term 3; v1 is proposed in term 1, v2 in term 2.
fn scenario(releases: Releases, votes: Votes) -> Scope {
    Scope {
        servers: 4,
        initial: 0b1111,
        target: 0b1111,
        releases,
        votes,
        rename_servers: false,
        rename_values: false,
        leads: [ANY, A, B, D, ANY],
        proposed: [0, 0b01, 0b10, 0, 0],
        reduce: true,
        ..three(3, 3, 0b1100, 2)
    }
}

/// Term 1 of the scenario: A leads (elected by B and C), writes its no-op and commits it by the
/// classic quorum, B and C hold v1 at indexes 2 and 3 (and A, if `leader_holds`), A takes v1 at
/// both and commits them by the fast quorum of A, B and C.
fn term_one(leader_holds: bool) -> Vec<Action> {
    let mut steps = vec![
        Action::Elect {
            c: A,
            q: (1 << B) | (1 << C),
            v: (1 << A) | (1 << B) | (1 << C),
            choice: 0,
        },
        Action::Take { l: A, v: NOOP },
        Action::Replicate {
            l: A,
            m: B,
            p: 0,
            k: 1,
        },
        Action::Replicate {
            l: A,
            m: C,
            p: 0,
            k: 1,
        },
        Action::ClassicCommit { l: A, i: 1 },
    ];
    let holders: &[u8] = if leader_holds { &[A, B, C] } else { &[B, C] };
    for &m in holders {
        for i in [2, 3] {
            steps.push(Action::Hold { m, i, v: V1 });
        }
    }
    steps.extend([
        Action::Take { l: A, v: V1 },
        Action::Take { l: A, v: V1 },
        Action::FastCommit { l: A },
        Action::FastCommit { l: A },
    ]);
    steps
}

/// The rest: B is elected in term 2 by C and D, whose logs end at the no-op, and takes v1 at 2 and
/// 3 again under term 2; D takes B's log through index 2 only, and holds v2 at 3; D is elected in
/// term 3 by A and C, whose logs end in term 1, and recovers index 3 taking `taken`.
fn terms_two_and_three(taken: u8) -> Vec<Action> {
    // The values an election takes, in base four from the first index above the log: index 0 the
    // no-op, 1 v1, 2 v2.
    let digit = |v: u8| v - NOOP;
    vec![
        Action::Elect {
            c: B,
            q: (1 << C) | (1 << D),
            v: (1 << B) | (1 << C) | (1 << D),
            choice: digit(V1) + 4 * digit(V1),
        },
        Action::Replicate {
            l: B,
            m: D,
            p: 0,
            k: 2,
        },
        Action::Hold { m: D, i: 3, v: V2 },
        Action::Elect {
            c: D,
            q: (1 << A) | (1 << C),
            v: (1 << A) | (1 << C) | (1 << D),
            choice: digit(taken),
        },
    ]
}

/// Before the fix, a run of the model loses the entry committed at index 3: with both rules the
/// core had, and with either alone. The release on log coverage alone (A, B and C held v1, A's and
/// B's holdings went with their logs reaching the index, and C's log never did) and the counting
/// of logs alone (A counted for its log, holding nothing) each leave D's election one report of v1
/// against D's own v2.
#[test]
fn the_entry_the_swarm_lost_is_lost_by_each_rule_before_the_fix() {
    for (releases, votes, leader_holds) in [
        (Releases::Log, Votes::Logs, false),
        (Releases::Log, Votes::Held, true),
        (Releases::Classic, Votes::Logs, false),
    ] {
        let model = FastTrack::at(scenario(releases, votes));
        let mut steps = term_one(leader_holds);
        steps.extend(terms_two_and_three(V2));
        let reached = run_script(&model, model.initial(), &steps);
        assert!(
            matches!(reached, Ok((_, Some(Fault::LeaderHolds)))),
            "{releases:?} {votes:?}: {reached:?}"
        );
    }
}

/// With the fix the same run keeps the entry: A and C still hold v1 at index 3, two reports
/// against D's one, so D's election may not take v2 there, and takes v1.
#[test]
fn with_the_fix_the_same_run_keeps_the_entry() {
    let model = FastTrack::at(scenario(Releases::Classic, Votes::Held));
    // Without A's holding the fast quorum is not one.
    assert_eq!(
        run_script(&model, model.initial(), &term_one(false)),
        Err(term_one(false).len() - 2)
    );
    let mut steps = term_one(true);
    steps.extend(terms_two_and_three(V2));
    let last = steps.len() - 1;
    assert_eq!(run_script(&model, model.initial(), &steps), Err(last));
    let mut steps = term_one(true);
    steps.extend(terms_two_and_three(V1));
    let (state, fault) = run_script(&model, model.initial(), &steps).unwrap();
    assert_eq!(fault, None);
    assert_eq!(state.log[usize::from(D)][2].value, V1);
}

/// The configurations of `docs/models/*.cfg`, by name: each one's constants.
fn configurations() -> Vec<(&'static str, Scope)> {
    let base =
        |servers: usize, initial: u8, target: u8, values: u8, t: u8, l: usize, held: u8| Scope {
            servers,
            initial,
            target,
            ..three(t, l, held, values)
        };
    let one = base(3, 0b111, 0b111, 2, 3, 1, 0b10);
    let round = base(3, 0b111, 0b111, 1, 2, 2, 0b110);
    let four = base(4, 0b1111, 0b1111, 1, 3, 1, 0b10);
    let change = base(3, 0b111, 0b011, 2, 2, 2, 0b100);
    let grow = base(4, 0b0111, 0b1111, 1, 2, 2, 0b100);
    let classic = base(3, 0b011, 0b111, 1, 3, 2, 0);
    let joint = Scope {
        joint: true,
        ..base(3, 0b011, 0b110, 1, 3, 3, 0)
    };
    let marked = Scope {
        losers: 0b111,
        ..base(3, 0b111, 0b111, 1, 2, 2, 0)
    };
    let marked_change = Scope {
        losers: 0b111,
        ..base(3, 0b011, 0b111, 1, 2, 2, 0)
    };
    let marked_joint = Scope {
        losers: 0b111,
        joint: true,
        ..base(3, 0b111, 0b011, 1, 2, 2, 0)
    };
    let restamp = base(3, 0b111, 0b111, 1, 2, 3, 0b100);
    let marked_one = Scope {
        losers: 0b111,
        ..base(3, 0b111, 0b111, 1, 2, 1, 0)
    };
    vec![
        ("one", one),
        ("round", round),
        ("four", four),
        ("change", change),
        ("grow", grow),
        ("classic", classic),
        ("joint", joint),
        (
            "reached",
            Scope {
                reach: Reach::NoFastByHeld,
                ..round
            },
        ),
        (
            "anyround",
            Scope {
                counts: Counts::Any,
                ..four
            },
        ),
        (
            "least",
            Scope {
                rule: Rule::Least,
                counts: Counts::Any,
                ..base(5, 0b11111, 0b11111, 2, 2, 1, 0b10)
            },
        ),
        (
            "growreached",
            Scope {
                reach: Reach::NoFastByHeldAfterChange,
                ..grow
            },
        ),
        ("restamp", restamp),
        (
            "restampreach",
            Scope {
                reach: Reach::NoRestamp,
                ..restamp
            },
        ),
        ("marked", marked),
        ("markedchange", marked_change),
        ("markedjoint", marked_joint),
        (
            "markedself",
            Scope {
                marks: Marks::SelfVote,
                ..marked_one
            },
        ),
        (
            "markedwhole",
            Scope {
                marks: Marks::Whole,
                ..marked_one
            },
        ),
        (
            "markedreach",
            Scope {
                reach: Reach::NoMarkedLeader,
                ..marked
            },
        ),
        (
            "changereach",
            Scope {
                reach: Reach::NoMarkedLeader,
                ..marked_change
            },
        ),
        (
            "jointreach",
            Scope {
                reach: Reach::NoMarkedLeader,
                ..marked_joint
            },
        ),
    ]
}

/// The model mirrors the specification: before the fix, at every configuration of one value
/// (where `SYMMETRY Alike` is a group, so TLC's count is the number of orbits, as this search's
/// is), it reaches exactly the distinct states TLC counted (`docs/models/README.md`, 2026-10-01
/// and 2026-10-02, and with the configuration a member counts by the newest in its log, CI runs
/// 37296371662 and 37305116263, 2026-10-05).
#[test]
#[ignore = "the full configurations; CI's explore job runs it"]
fn the_model_counts_what_tlc_counted() {
    let counted = [
        ("round", 2_462_010u64),
        ("four", 3_207_204),
        ("grow", 5_228_729),
        ("classic", 5_013_585),
        ("joint", 798_339),
        ("marked", 196_484),
        ("markedchange", 224_402),
        ("markedjoint", 784_034),
    ];
    for (name, scope) in configurations() {
        let Some((_, states)) = counted.iter().find(|(known, _)| *known == name) else {
            continue;
        };
        // `Classic.cfg` had three indexes before the release series took it to two.
        let max_len = if name == "classic" { 3 } else { scope.max_len };
        let legacy = Scope {
            legacy: true,
            releases: Releases::Log,
            votes: Votes::Logs,
            max_len,
            ..scope
        };
        match explore(&FastTrack::at(legacy), workers(), budget()).unwrap() {
            Outcome::Exhausted(report) => assert_eq!(report.classes, *states, "{name}"),
            other => panic!("{name}: {other:?}"),
        }
    }
}

/// The model is the specification with the fix: at every configuration that passes and that CI's
/// model job and this search both count, the classes are TLC's distinct states (CI run
/// 37336523719, 2026-10-05).
#[test]
#[ignore = "the full configurations; CI's explore job runs it"]
fn the_model_counts_what_tlc_counts() {
    let counted = [
        ("one", 573_160u64),
        ("round", 3_974_278),
        ("four", 3_602_968),
        ("change", 4_067_274),
        ("grow", 9_551_710),
        ("classic", 841_050),
        ("joint", 2_626_941),
        ("marked", 303_762),
        ("markedchange", 431_168),
        ("markedjoint", 1_182_158),
    ];
    for (name, scope) in configurations() {
        let Some((_, states)) = counted.iter().find(|(known, _)| *known == name) else {
            continue;
        };
        match explore(&FastTrack::at(scope), workers(), budget()).unwrap() {
            Outcome::Exhausted(report) => assert_eq!(report.classes, *states, "{name}"),
            other => panic!("{name}: {other:?}"),
        }
    }
}

/// `LogMatching` compares terms only where neither member has committed, for a member keeps the
/// stamp of an entry it committed that a later leader's election took again under its own term
/// (`docs/models/FastTrack.tla`, `Restamped`). The restamp scope reaches such a pair, which the
/// whole-entry comparison refused, and keeps every invariant with the comparison the core holds to
/// (`FastTrackRestamp.cfg`, `FastTrackRestampReached.cfg`).
#[test]
#[ignore = "a whole scope; CI's explore job runs it"]
fn the_restamp_scope_reaches_a_restamped_prefix_and_keeps_log_matching() {
    let scope = |name: &str| {
        configurations()
            .into_iter()
            .find(|(known, _)| *known == name)
            .map(|(_, scope)| scope)
            .unwrap()
    };
    match explore(&FastTrack::at(scope("restampreach")), workers(), budget()).unwrap() {
        Outcome::Fault { fault, .. } => assert_eq!(fault, Fault::NoRestamp),
        other => panic!("the restamp scope reached no restamped prefix: {other:?}"),
    }
    match explore(&FastTrack::at(scope("restamp")), workers(), budget()).unwrap() {
        Outcome::Exhausted(report) => assert_eq!(report.classes, 3_808_625),
        other => panic!("{other:?}"),
    }
}
