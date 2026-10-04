//! The liveness phase's quiet period, its progress watch and its monitors (`docs/sim.md` §4.2), and
//! the non-vacuity floors (§4.4).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::disallowed_macros,
    missing_docs
)]

use hyper_check::coverage::{Counters, Fallen, Floor, Measured, Per, hold};
use hyper_check::liveness::{Laws, Monitor, Position, Progress, Quiet};

/// The two rules give hyper-raft's `Cluster::settles` its numbers: on ticks, at an election tick of
/// ten and a patience of two, twice the longest timeout with its patience and a round; by
/// suspicion, the schedule's span, three vote rounds and a replication round in ticks of 500 ns.
#[test]
fn the_quiet_period_comes_from_the_members_settings() {
    assert_eq!(
        Quiet::ticks(10, 2).unwrap().period,
        2 * (2 * 10 - 1 + 2) + 1
    );
    let laws = Laws {
        detection_ns: 0,
        span_ns: 5_000,
        vote_rounds_ns: 3 * 5_000,
        replication_ns: 5_000,
    };
    assert_eq!(Quiet::ordered(&laws).unwrap().period, 25_000);
    assert_eq!(
        Quiet::in_rounds(&laws, 500).unwrap().period,
        (5_000u64 + 4 * 5_000).div_ceil(500)
    );
    assert!(Quiet::ticks(u64::MAX, 0).is_err());
}

#[test]
fn a_group_is_stuck_only_once_a_quiet_period_passes_with_nothing_moving() {
    let mut progress = Progress::new(Quiet { period: 3 }, 0);
    let at = |term, commit| Position {
        term,
        commit,
        applied: commit,
        last: commit,
    };
    assert!(progress.observe(0, 1, at(1, 0)));
    assert!(!progress.observe(1, 1, at(1, 0)));
    assert!(!progress.stuck(3));
    // A new term moves it, whatever else does not: a split vote is movement.
    assert!(progress.observe(3, 1, at(2, 0)));
    assert!(!progress.stuck(6));
    assert!(progress.stuck(7));
    assert_eq!(progress.moved(), 3);
}

#[test]
fn a_monitor_that_ends_hot_fails_and_one_that_ends_cold_passes() {
    let mut monitor = Monitor::<u64>::new(2);
    monitor.raise(1, 10).unwrap();
    monitor.raise(1, 20).unwrap();
    monitor.raise(2, 11).unwrap();
    assert!(monitor.raise(3, 12).is_err());
    assert!(monitor.is_hot());
    assert!(monitor.meet(&2));
    assert!(!monitor.meet(&2));
    let hot = monitor.end().unwrap_err();
    assert_eq!(hot.open, vec![(1, 10)]);
    assert!(monitor.meet(&1));
    monitor.end().unwrap();
    assert_eq!(monitor.met(), 2);
}

const PATHS: &[&str] = &["elections", "compactions"];

#[test]
fn floors_hold_their_paths_and_their_own_measurements() {
    let mut counters = Counters::new(PATHS);
    assert!(counters.add("elections", 30));
    assert!(counters.hit("compactions"));
    assert!(!counters.hit("nothing"));
    let floors = [
        Floor {
            path: "elections",
            per: Per::Seed,
            measured: Measured {
                count: 30,
                seeds: 10,
            },
        },
        Floor {
            path: "compactions",
            per: Per::Campaign,
            measured: Measured {
                count: 1,
                seeds: 10,
            },
        },
    ];
    hold(&floors, &counters, 10).unwrap();
    // A path that fell below its floor fails.
    assert_eq!(
        hold(&floors, &counters, 30),
        Err(Fallen::Below {
            path: "elections",
            count: 30,
            seeds: 30
        })
    );
    // A floor set above what it says was measured fails.
    let unmeasured = [Floor {
        path: "elections",
        per: Per::Seed,
        measured: Measured {
            count: 5,
            seeds: 10,
        },
    }];
    assert_eq!(
        hold(&unmeasured, &counters, 10),
        Err(Fallen::Unmeasured { path: "elections" })
    );
    let undeclared = [Floor {
        path: "nothing",
        per: Per::Campaign,
        measured: Measured { count: 1, seeds: 1 },
    }];
    assert!(matches!(
        hold(&undeclared, &counters, 10),
        Err(Fallen::Undeclared { .. })
    ));
    let mut more = Counters::new(PATHS);
    more.hit("elections");
    assert!(counters.merge(&more));
    assert_eq!(counters.count("elections"), 31);
}
