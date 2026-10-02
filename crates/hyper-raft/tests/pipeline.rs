//! Readies taken ahead of their persistence (core step R-4, `docs/durable.md`
//! §2.1). Every member of the group takes `Ready`s while earlier writes are
//! out, its writes become durable on its disk in the order they were issued
//! but when the schedule says, and its owner hears of them later still, a
//! notice at a time for one write or several. Those steps are drawn at
//! random among proposals, ticks, deliveries, elections, changes,
//! compactions and crashes, and a crash loses every write that was not
//! durable.
//!
//! Whatever the interleaving, the members are held to the invariants of
//! `docs/durable.md` §3 the core keeps, against their disks as they are at
//! each step (`support::lagged`, `Cluster::check_durable`):
//! - I1: no term, vote or vote request leaves before the disk holds it, and
//!   a leader sends at once only while its term and vote are durable;
//! - I2: no acknowledgement leaves before the disk holds what it
//!   acknowledges, nor the fast track's word that a member holds an entry;
//! - I3: a leader counts itself for no entry its disk does not hold, and
//!   commits nothing a majority of each half of its configuration does not
//!   hold durably;
//! - I4: nothing is given to apply that is not committed and durable here,
//!   but a leader's own entries where it applies before its write is durable
//!   (core step R-6, `docs/durable.md` §4.2);
//! - I5: a change of configuration applies only once the disk states a
//!   commit covering it, the member holding it and asked for nothing more
//!   to apply meanwhile (R-6's apply pause, §4.1);
//! - I7: once a write is durable the disk holds what the member held when
//!   it took the `Ready`;
//! - R-6: an answer to an append or a heartbeat states no commit its disk
//!   does not state when it leaves;
//!
//! and to what every schedule is held to (`Cluster::report`): no two members
//! commit different entries at one index, no term has two leaders, every
//! answered read saw what was committed when it was asked. Once the network
//! is whole and the members up, the group elects and commits.
#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::disallowed_macros,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cognitive_complexity,
    unreachable_pub
)]
mod support;

use support::{Cluster, Coverage, Lagged, Mix, Op, Seeded, Settings, Step};

fn count(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

/// Of a hundred steps, how many are persistence steps: enough that writes
/// pile up to the bound and notices cover several, few enough that the
/// group still elects and commits between them.
const LAG: u64 = 35;

/// Of a hundred chances to make a leader's write durable, how many are
/// taken where its disk is the slowest of its group. Measured: at a hundred
/// (a disk like the others') 24 schedules of the setting that applies before
/// durability never once committed a leader's entry by its followers before
/// its own write (no entry applied ahead); at a quarter they do at the
/// default size (10 entries) and at 1,000 schedules (303).
const SLOW_LEADER: u64 = 25;

fn mix(fast: u64) -> Mix {
    Mix {
        leader_leaves: true,
        bursts: true,
        windows: true,
        fast,
        lag: LAG,
        ..Mix::everything()
    }
}

/// The schedules of `settings`: where a leader applies before its own write
/// is durable, its disk is the slowest of its group.
fn mix_for(settings: &Settings, fast: u64) -> Mix {
    Mix {
        leader_durable: if settings.apply_unpersisted {
            SLOW_LEADER
        } else {
            100
        },
        ..mix(fast)
    }
}

/// One group of members driven ahead of their persistence under the
/// schedule of `seed`; `crash` stops and reopens the member of the
/// `crash.0`-th persistence step that did something, right after it.
/// Returns the group and how many persistence steps did something.
fn schedule(
    settings: Settings,
    voters: &[u64],
    seed: u64,
    steps: u64,
    mix: &Mix,
    crash: Option<u64>,
) -> (Cluster<Lagged>, u64) {
    let mut group: Cluster<Lagged> = Cluster::new(5, voters, settings, seed);
    group.stop_who_left = true;
    let mut rng = Seeded(seed);
    let mut persisted = 0u64;
    for _ in 0..steps {
        let op = group.choose(&mut rng, mix);
        let reports = group.act(&op);
        if let Op::Persist(member, _) = op
            && reports.iter().any(|report| report.accepted == Some(true))
        {
            if crash == Some(persisted) {
                group.act(&Op::Restart(member));
            }
            persisted += 1;
        }
    }
    assert!(
        group.settles(400),
        "seed {seed}, crash {crash:?}: the group did not settle"
    );
    assert_eq!(
        group.deposed, 0,
        "seed {seed}: a member led a group it left"
    );
    (group, persisted)
}

/// Schedules of `settings` at the depth it states; what they reached.
fn schedules(name: &str, settings: Settings, voters: &[u64], fast: u64) -> (Coverage, usize) {
    let seeds = count("HYPER_RAFT_SEEDS", 24);
    let steps = count("HYPER_RAFT_STEPS", 2_000);
    let first = count("HYPER_RAFT_SEED", 0);
    let mut coverage = Coverage::default();
    let mut committed = 0;
    for seed in first..first + seeds {
        let (group, _) = schedule(
            settings,
            voters,
            seed,
            steps,
            &mix_for(&settings, fast),
            None,
        );
        coverage.add(group.coverage());
        committed += group.chosen.len();
    }
    println!(
        "{name}: {seeds} schedules of {steps} steps committed {committed} entries; {coverage:?}"
    );
    // A schedule that never had two writes out, never heard of several at
    // once and never lost one proves nothing of R-4.
    assert!(committed as u64 > seeds * 8, "{name}: {committed}");
    assert!(
        coverage.behind > seeds && coverage.several > seeds,
        "{name}: {coverage:?}"
    );
    assert!(
        coverage.refused > 0 && coverage.lost > 0,
        "{name}: {coverage:?}"
    );
    assert!(coverage.held_back > 0, "{name}: {coverage:?}");
    // R-6 reached: answers held to the disk's commit, changes held behind
    // the fence, and the commit stated for them.
    assert!(
        coverage.answers > seeds && coverage.fenced > 0 && coverage.stated > 0,
        "{name}: {coverage:?}"
    );
    if settings.apply_unpersisted {
        assert!(coverage.unpersisted > 0, "{name}: {coverage:?}");
    }
    (coverage, committed)
}

#[test]
fn random_interleavings_keep_every_invariant() {
    for (name, settings) in [
        (
            "shell, three writes out",
            Settings {
                depth: 3,
                ..Settings::shell()
            },
        ),
        (
            "focal, two writes out, in place",
            Settings {
                depth: 2,
                in_place: true,
                ..Settings::focal()
            },
        ),
        (
            "focal, three writes out, narrow",
            Settings {
                depth: 3,
                max_inflight_msgs: 2,
                max_size_per_msg: 1,
                max_committed_size_per_ready: 64,
                ..Settings::focal()
            },
        ),
        (
            "focal, three writes out, in place, a leader applying before its write",
            Settings {
                depth: 3,
                in_place: true,
                apply_unpersisted: true,
                ..Settings::focal()
            },
        ),
    ] {
        schedules(name, settings, &[1, 2, 3], 0);
    }
}

/// `docs/raft.md`'s fast-track schedules with readies persisted at random
/// lags: what a member says it holds beside its log it says once its disk
/// holds it (I2), and the fast quorum commits only what is durable.
#[test]
fn the_fast_track_with_readies_persisted_at_random_lags_is_safe_and_settles() {
    for in_place in [false, true] {
        schedules(
            "fast",
            Settings {
                depth: 3,
                in_place,
                ..Settings::fast()
            },
            &[1, 2, 3, 4, 5],
            60,
        );
    }
}

/// A crash at every persistence step of a schedule in turn: after a `Ready`
/// was taken and its write issued, after a write became durable and before
/// its owner heard, and after the owner heard and released what waited.
/// Each crash loses the writes out and nothing durable, and the group stays
/// safe and settles.
#[test]
fn a_crash_at_every_persistence_step_loses_nothing_durable() {
    let seeds = count("HYPER_RAFT_CRASH_SEEDS", 3);
    let steps = count("HYPER_RAFT_CRASH_STEPS", 400);
    // The second has a leader apply its own entries before its write of them
    // is durable: a crash between the two (`docs/durable.md` §12).
    for apply_unpersisted in [false, true] {
        let settings = Settings {
            depth: 3,
            apply_unpersisted,
            ..Settings::focal()
        };
        let mix = mix_for(&settings, 0);
        let mut crashes = 0u64;
        let mut lost = 0u64;
        let mut reached = Coverage::default();
        for seed in 0..seeds {
            let (_, events) = schedule(settings, &[1, 2, 3], seed, steps, &mix, None);
            for at in 0..events {
                let (group, _) = schedule(settings, &[1, 2, 3], seed, steps, &mix, Some(at));
                crashes += 1;
                let coverage = group.coverage();
                lost += coverage.lost;
                reached.add(coverage);
            }
        }
        println!(
            "applying before durable {apply_unpersisted}: {crashes} crashes, one at each persistence step, lost {lost} writes out; {reached:?}"
        );
        assert!(crashes > seeds * 20 && lost > 0, "{crashes} {lost}");
        assert!(reached.answers > 0 && reached.fenced > 0, "{reached:?}");
        assert!(!apply_unpersisted || reached.unpersisted > 0, "{reached:?}");
    }
}

/// A notice of a write heard after a crash is never given: the member opens
/// on what its disk holds, and a write durable before the crash and never
/// heard of is held all the same.
#[test]
fn a_write_durable_and_never_heard_of_is_held_after_a_crash() {
    let settings = Settings {
        depth: 3,
        ..Settings::focal()
    };
    let mut group: Cluster<Lagged> = Cluster::new(3, &[1, 2, 3], settings, 7);
    group.act(&Op::Campaign(1));
    for _ in 0..64 {
        for id in group.up() {
            group.act(&Op::Persist(id, Step::Flush));
        }
        if group.net.is_empty() {
            break;
        }
        while !group.net.is_empty() {
            group.act(&Op::Deliver {
                at: 0,
                keep: false,
                lose: false,
            });
        }
    }
    assert_eq!(group.leaders_now(), vec![1]);
    let before = group.disk(1).last_index();
    group.act(&Op::Propose(1, b"kept".to_vec()));
    group.act(&Op::Persist(1, Step::Take));
    group.act(&Op::Propose(1, b"lost".to_vec()));
    group.act(&Op::Persist(1, Step::Take));
    // The first is durable and never heard of; the second is out.
    group.act(&Op::Persist(1, Step::Durable));
    assert_eq!(group.disk(1).last_index(), before + 1);
    group.act(&Op::Restart(1));
    let disk = group.disk(1);
    assert_eq!(disk.last_index(), before + 1);
    assert_eq!(disk.entries.last().unwrap().data, b"kept");
    assert_eq!(group.coverage().lost, 1);
    assert!(group.settles(400));
}
