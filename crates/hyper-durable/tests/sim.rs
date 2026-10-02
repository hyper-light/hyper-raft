//! The shell under deterministic simulation (`docs/durable.md` §12): groups driven by seeded
//! schedules of detectors' words, deliveries, losses, drives, writes made durable at the
//! schedule's choice,
//! refusals for room, failed writes, proposals, entries acted on at start, changes of
//! configuration, reads, compactions and crashes, with readies ahead of their persistence up to
//! the store's depth; every output held to the oracle of `support::cluster` against the
//! sender's durable state, and every group settling once whole.
//!
//! Then the durability windows, enumerated: a schedule's events are counted, and the schedule
//! is run again once for each, crashing the member whose event it was right after it.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::disallowed_macros,
    clippy::cast_possible_truncation,
    clippy::cognitive_complexity,
    clippy::unreachable
)]

mod support;

use support::Seeded;
use support::cluster::{Cluster, Op, Reached, Shape};

/// One schedule of `steps` from `seed`; `crash` crashes the member of the `crash`-th step that
/// did something, right after it. The group, and how many steps did something.
fn schedule(shape: Shape, seed: u64, steps: u64, crash: Option<u64>) -> (Cluster, u64) {
    let mut group = Cluster::new(shape, seed);
    let mut rng = Seeded(seed);
    let mut events = 0u64;
    for _ in 0..steps {
        let op = group.choose(&mut rng, true);
        if group.act(op) {
            if crash == Some(events) {
                let member = match op {
                    Op::Suspect(id, _)
                    | Op::Trust(id, _)
                    | Op::Drive(id)
                    | Op::Durable(id)
                    | Op::Propose(id)
                    | Op::Fence(id)
                    | Op::Change(id, ..)
                    | Op::Read(id)
                    | Op::Compact(id)
                    | Op::Crash(id)
                    | Op::Refuse(id)
                    | Op::Fail(id)
                    | Op::Resume(id) => id,
                    Op::Deliver(_) | Op::Lose(_) => 1,
                };
                group.crash(member);
            }
            events += 1;
        }
    }
    assert!(
        group.settles(4_000),
        "seed {seed}, crash {crash:?}: the group did not settle: {:?}",
        group.reached
    );
    (group, events)
}

const SHAPES: [(&str, Shape); 5] = [
    (
        "three voters, depth three, durable state machine",
        Shape {
            members: 4,
            voters: 3,
            depth: 3,
            volatile: false,
            control: false,
            ahead: false,
        },
    ),
    (
        "three voters, depth two, a replayed state machine",
        Shape {
            members: 4,
            voters: 3,
            depth: 2,
            volatile: true,
            control: false,
            ahead: false,
        },
    ),
    (
        "a control group, every entry acted on at start, depth three",
        Shape {
            members: 3,
            voters: 3,
            depth: 3,
            volatile: true,
            control: true,
            ahead: false,
        },
    ),
    (
        "three voters, depth three, a leader applying before its write is durable",
        Shape {
            members: 4,
            voters: 3,
            depth: 3,
            volatile: false,
            control: false,
            ahead: true,
        },
    ),
    (
        "five voters, depth one (slates' synchronous store)",
        Shape {
            members: 5,
            voters: 5,
            depth: 1,
            volatile: false,
            control: false,
            ahead: false,
        },
    ),
];

#[test]
fn random_schedules_keep_every_invariant_and_settle() {
    let seeds = support::count("HYPER_DURABLE_SEEDS", 128);
    let steps = support::count("HYPER_DURABLE_STEPS", 5_000);
    let first = support::count("HYPER_DURABLE_SEED", 0);
    for (name, shape) in SHAPES {
        let mut reached = Reached::default();
        for seed in first..first + seeds {
            let (group, _) = schedule(shape, seed, steps, None);
            let r = group.reached;
            reached.committed += r.committed;
            reached.crashes += r.crashes;
            reached.refused += r.refused;
            reached.stalls += r.stalls;
            reached.fenced += r.fenced;
            reached.behind_fence += r.behind_fence;
            reached.deep += r.deep;
            reached.changes += r.changes;
            reached.compactions += r.compactions;
            reached.ahead += r.ahead;
            reached.acted += group.acted();
        }
        println!("{name}: {seeds} schedules of {steps} steps: {reached:?}");
        // A schedule that never pipelined, never crashed, never held an entry behind the fence
        // and never stalled proves nothing of the shell.
        // Writes are made durable at random and a stall lasts until the owner resumes: a few
        // entries a schedule, every one of them through every rule above.
        assert!(reached.committed > seeds * 5, "{name}: {reached:?}");
        assert!(
            reached.crashes > 0 && reached.fenced > 0,
            "{name}: {reached:?}"
        );
        assert!(reached.refused > 0, "{name}: {reached:?}");
        assert!(
            reached.behind_fence > 0 && reached.acted > 0,
            "{name}: {reached:?}"
        );
        if shape.depth > 1 {
            assert!(reached.deep > 0, "{name}: {reached:?}");
        }
    }
}

/// A crash right after every step of a schedule that did something, in turn.
#[test]
fn a_crash_after_every_durability_event_loses_nothing_durable() {
    let seeds = support::count("HYPER_DURABLE_CRASH_SEEDS", 4);
    let steps = support::count("HYPER_DURABLE_CRASH_STEPS", 800);
    let mut crashes = 0u64;
    for (name, shape) in [SHAPES[0], SHAPES[2]] {
        for seed in 0..seeds {
            let (_, events) = schedule(shape, seed, steps, None);
            for at in 0..events {
                schedule(shape, seed, steps, Some(at));
                crashes += 1;
            }
            println!("{name}, seed {seed}: crashed after each of {events} events");
        }
    }
    assert!(crashes > seeds * 50, "{crashes}");
}
