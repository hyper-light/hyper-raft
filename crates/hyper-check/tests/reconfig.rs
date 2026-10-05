//! `docs/models/Reconfig.tla` searched by `hyper_check::explore` (`tests/models/reconfig.rs`): the
//! configuration an election counts by, across two changes of the voters.
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

#[path = "models/reconfig.rs"]
mod reconfig;

use std::num::NonZeroUsize;

use hyper_check::explore::{Outcome, explore};
use hyper_check::search::{Budget, MEMORY_CEILING};
use reconfig::{Elections, Precedence, Reconfig, Scenario, Scope, Stand};

fn scope(
    scenario: Scenario,
    max_term: u8,
    max_len: usize,
    elections: Elections,
    commits: Elections,
    stand: Stand,
) -> Scope {
    let servers = match scenario {
        Scenario::Single | Scenario::SingleJoint => 2,
        Scenario::Remove => 3,
        Scenario::Promote | Scenario::Joint => 4,
    };
    Scope {
        servers,
        values: 0,
        max_term,
        max_len,
        elections,
        commits,
        stand,
        scenario,
        priority: [0; reconfig::MAX_SERVERS],
        precedence: Precedence::Log,
        elects: false,
        reached: false,
        stood: false,
        reduce: false,
        symmetry: false,
    }
}

/// How a search ended: every class visited, or a fault met.
#[derive(Debug, PartialEq, Eq)]
enum End {
    Exhausted(u64),
    Fault(reconfig::Fault),
}

fn ends(scope: Scope) -> End {
    let model = Reconfig::at(scope);
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

/// The scope with `Elects` checked.
fn electing(scope: Scope) -> Scope {
    Scope {
        elects: true,
        ..scope
    }
}

fn reduced(scope: Scope) -> Scope {
    Scope {
        reduce: true,
        symmetry: true,
        ..scope
    }
}

use Elections::{Applied, Newest};

/// raft-rs's rule, elections and commitment by the configuration applied, elects two leaders of
/// a term through a joint configuration (fast seed 135,923 of the core's schedules).
#[test]
fn the_applied_rule_elects_two_leaders_of_a_term() {
    let scope = reduced(scope(Scenario::Joint, 2, 2, Applied, Applied, Stand::Voter));
    assert_eq!(ends(scope), End::Fault(reconfig::Fault::OneLeader));
}

/// A sole voter adding a second, by one entry and through a joint configuration, under the core's
/// rule: every invariant, at the states TLC counts (`ReconfigSingle.cfg`, `ReconfigSingleJoint.cfg`).
#[test]
fn the_core_rule_holds_where_a_sole_voter_adds_a_second() {
    let single = electing(scope(Scenario::Single, 3, 3, Newest, Newest, Stand::Needed));
    assert_eq!(ends(single), End::Exhausted(31_203));
    let joint = electing(scope(
        Scenario::SingleJoint,
        3,
        3,
        Newest,
        Newest,
        Stand::Needed,
    ));
    assert_eq!(ends(joint), End::Exhausted(24_460));
}

/// Through two changes of four servers, under the core's rule: every invariant at the states TLC
/// counts (`ReconfigJoint.cfg`, `ReconfigPromote.cfg`), and past TLC's bounds with the reductions
/// of `docs/research/reconfiguration.md` §5.
#[test]
#[ignore = "the full scopes, in release; CI's explore job runs them"]
fn the_core_rule_holds_through_two_changes() {
    let joint = electing(scope(Scenario::Joint, 2, 2, Newest, Newest, Stand::Needed));
    assert_eq!(ends(joint), End::Exhausted(4_945_526));
    let promote = electing(scope(
        Scenario::Promote,
        2,
        2,
        Newest,
        Newest,
        Stand::Needed,
    ));
    assert_eq!(ends(promote), End::Exhausted(6_496_567));
    for (scenario, terms, length, classes) in REDUCED {
        let scope = reduced(electing(scope(
            *scenario,
            *terms,
            *length,
            Newest,
            Newest,
            Stand::Needed,
        )));
        assert_eq!(
            ends(scope),
            End::Exhausted(*classes),
            "{scenario:?} {terms} {length}"
        );
    }
}

/// The reduced scopes past TLC's bounds and the classes each reaches (2026-10-05).
const REDUCED: &[(Scenario, u8, usize, u64)] = &[
    (Scenario::Joint, 3, 2, 11_959_538),
    (Scenario::Promote, 3, 2, 15_081_832),
    (Scenario::Joint, 2, 3, 11_140_163),
    (Scenario::Promote, 2, 3, 13_558_834),
];

/// The thesis's rule for elections alone, commitment counted by the configuration applied, loses
/// a committed entry at three terms (`docs/research/reconfiguration.md` §3, option (a)).
#[test]
#[ignore = "the full scopes, in release; CI's explore job runs them"]
fn elections_by_the_newest_and_commits_by_the_applied_lose_a_committed_entry() {
    for scenario in [Scenario::Joint, Scenario::Promote] {
        let scope = reduced(scope(scenario, 3, 2, Newest, Applied, Stand::Voter));
        assert_eq!(
            ends(scope),
            End::Fault(reconfig::Fault::LeaderHolds),
            "{scenario:?}"
        );
    }
}

/// What the scopes are for is reached: a member elected while its log holds a change past its
/// commit, and one elected that its newest configuration names no voter.
#[test]
fn the_scopes_reach_what_they_are_for() {
    let mut on_pending = reduced(scope(Scenario::Joint, 2, 2, Newest, Newest, Stand::Needed));
    on_pending.reached = true;
    assert_eq!(
        ends(on_pending),
        End::Fault(reconfig::Fault::NoElectedOnPending)
    );
    let mut stood = reduced(scope(Scenario::Joint, 2, 2, Newest, Newest, Stand::Needed));
    stood.stood = true;
    assert_eq!(ends(stood), End::Fault(reconfig::Fault::NoElectedUnnamed));
}

/// Swarm group seed 9,657's shape: A, of the highest priority, holds the longest log, of an older
/// term; C, which counts by the joint configuration that removes B, needs A's vote. By raft-rs's
/// precedence of length A refuses both candidates of the later term and can never be elected itself,
/// and with every member up the group elects no one; by the log's precedence, the core's, some
/// member can always be elected (`docs/raft.md` §3.3, `ReconfigRemove.cfg`,
/// `ReconfigRemoveLength.cfg`).
#[test]
fn by_the_logs_precedence_a_group_with_every_member_up_can_always_elect() {
    let ranked = |precedence| Scope {
        priority: [2, 0, 1, 0],
        precedence,
        elects: true,
        ..scope(Scenario::Remove, 2, 3, Newest, Newest, Stand::Needed)
    };
    assert_eq!(ends(ranked(Precedence::Log)), End::Exhausted(667_123));
    assert_eq!(
        ends(ranked(Precedence::Length)),
        End::Fault(reconfig::Fault::Elects)
    );
}
