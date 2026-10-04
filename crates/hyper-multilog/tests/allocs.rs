//! What the layer allocates on its hot paths, beside what the core allocates for the same work
//! (`CLAUDE.md` §1a; `docs/multilog.md` §11 step 5), held exactly: counts are a function of the
//! work, not of the machine. The owner's storage and network are the harness's and are not counted.
//! - the merge: applying a command copies nothing and allocates nothing;
//! - a proposal through the layer allocates what the core's proposal of the same bytes does, once
//!   its owner reserved the layer's room ([`entry::SUFFIX_BYTES`]);
//! - a barrier: what the core's proposal of its nine bytes does;
//! - handing a `Ready`'s committed entries over, and stepping a message through the layer's
//!   screen: nothing beyond the core.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::disallowed_macros,
    clippy::cognitive_complexity,
    clippy::cast_possible_truncation,
    missing_docs,
    unreachable_pub
)]

mod support;

use hyper_measure::alloc::{self, Counting};
use hyper_multilog::{Applied, Flow, Limits, Route, entry, log_of};
use hyper_raft::RawNode;
use hyper_raft::proto::ConfState;
use support::{Disk, Group, Member, Store, config};

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const ROOMY: Limits = Limits { unmerged: 1 << 20 };

/// A command of `bytes` with the layer's room reserved, as an owner that knows the layer makes it.
fn command(i: u64, bytes: usize) -> Vec<u8> {
    let mut data = Vec::with_capacity(bytes + entry::SUFFIX_BYTES);
    data.extend_from_slice(&i.to_le_bytes());
    data.resize(bytes, 0x5a);
    data
}

/// A one-voter member of one log, leading it.
fn alone(logs: usize) -> Member {
    let mut member = Member::new(1, &[1], logs, 3, ROOMY);
    let mut out = Vec::new();
    for log in 0..logs {
        member.multi.node_mut(log).unwrap().campaign().unwrap();
        member.drive(log, &mut out);
    }
    member.settle(&mut out);
    member
}

#[test]
fn applying_a_command_allocates_nothing() {
    assert!(
        alloc::installed(),
        "the counting allocator is not installed"
    );
    let mut group = Group::new(3, 3, 5, ROOMY);
    for (log, leader) in [(0, 1), (1, 2), (2, 3)] {
        group.elect(log, leader);
    }
    group.member(3).hold_apply = true;
    let mut proposed = 0;
    for i in 0..300u64 {
        let route = if i % 10 == 0 {
            Route::Global
        } else {
            Route::Key(i % 17)
        };
        let log = group.member(1).multi.route(route);
        let leader = (1..=3).find(|id| group.member(*id).leads(log)).unwrap();
        assert!(group.member(leader).propose(route, command(i, 64)));
        proposed += 1;
        group.quiet();
    }
    let member = group.member(3);
    let mut applied = 0u64;
    alloc::begin();
    let advance = member
        .multi
        .apply(u64::MAX, &mut |applied_now| {
            if let Applied::Command(_) = applied_now {
                applied += 1;
            }
            Flow::Continue
        })
        .unwrap();
    let counts = alloc::end();
    assert_eq!(applied, proposed, "every command merged in one call");
    assert!(
        advance.consumed > applied,
        "barriers and leaders' entries passed too"
    );
    assert_eq!(
        (counts.allocations, counts.reallocations),
        (0, 0),
        "{applied} commands merged: {counts:?}"
    );
}

#[test]
fn a_proposal_through_the_layer_allocates_what_the_core_does() {
    let mut member = alone(1);
    let mut bare = RawNode::new(
        &config(1, 1, 3),
        Store(Disk::new(ConfState {
            voters: vec![1],
            ..ConfState::default()
        })),
    )
    .unwrap();
    bare.campaign().unwrap();
    while bare.has_ready() {
        let ready = bare.ready().unwrap();
        let _ = bare.advance_append(ready).unwrap();
    }
    let key = (0..).find(|k| log_of(*k, 1) == 0).unwrap();
    for (bytes, i) in [(8usize, 0u64), (64, 1), (4096, 2)] {
        let layer_data = command(i, bytes);
        alloc::begin();
        member.multi.propose(Route::Key(key), layer_data).unwrap();
        let layer = alloc::end();
        let bare_data = entry::keyed_command(command(i, bytes), key).unwrap();
        alloc::begin();
        bare.propose(Vec::new(), bare_data).unwrap();
        let core = alloc::end();
        assert_eq!(
            (layer.allocations, layer.reallocations, layer.bytes),
            (core.allocations, core.reallocations, core.bytes),
            "a proposal of {bytes} bytes"
        );
    }
}

/// Rounds each comparison repeats, the first ones warming the member's queues for both sides.
const ROUNDS: u64 = 64;

#[test]
fn a_barrier_allocates_what_the_cores_proposal_of_its_bytes_does() {
    let mut group = Group::new(3, 2, 7, ROOMY);
    group.elect(0, 1);
    group.elect(1, 2);
    for id in 1..=3 {
        group.member(id).auto_barriers = false;
    }
    let (mut layer, mut core) = (alloc::Counts::ZERO, alloc::Counts::ZERO);
    for round in 0..ROUNDS {
        // A global, committed: the leader of log 1 owes a barrier; the layer proposes it.
        assert!(group.member(1).propose(Route::Global, command(round, 8)));
        group.quiet();
        let leader = group.member(2);
        alloc::begin();
        assert_eq!(leader.multi.barriers().unwrap(), 1);
        let counted = alloc::end();
        group.quiet();
        // The same leader proposes the same bytes through the core alone.
        let named = group.member(2).multi.latest_global();
        let leader = group.member(2);
        alloc::begin();
        let data = entry::barrier_naming(named).unwrap();
        leader
            .multi
            .node_mut(1)
            .unwrap()
            .propose(Vec::new(), data)
            .unwrap();
        let bare = alloc::end();
        group.quiet();
        if round >= ROUNDS / 2 {
            add(&mut layer, &counted);
            add(&mut core, &bare);
        }
    }
    assert_eq!(
        (layer.allocations, layer.reallocations, layer.bytes),
        (core.allocations, core.reallocations, core.bytes),
        "barriers: {layer:?} against the core's {core:?}"
    );
}

fn add(sum: &mut alloc::Counts, more: &alloc::Counts) {
    sum.allocations += more.allocations;
    sum.reallocations += more.reallocations;
    sum.bytes += more.bytes;
}

#[test]
fn handing_entries_over_and_screening_a_message_allocate_nothing_of_the_layers() {
    let mut group = Group::new(3, 2, 11, ROOMY);
    group.elect(0, 1);
    group.elect(1, 2);
    alloc::begin();
    group.member(3).multi.hand_over(0, &[]).unwrap();
    let counts = alloc::end();
    assert_eq!((counts.allocations, counts.reallocations), (0, 0));
    // A forwarded keyed proposal, screened then stepped, against the core's step of the same, at
    // one leader in turn.
    let key = (0..).find(|k| log_of(*k, 2) == 0).unwrap();
    let make = |i: u64| {
        let mut message = hyper_raft::proto::message(1, hyper_raft::proto::MessageType::MsgPropose);
        message.from = 3;
        message.entries.push(hyper_raft::proto::Entry {
            data: entry::keyed_command(command(i, 32), key).unwrap(),
            ..hyper_raft::proto::Entry::default()
        });
        message
    };
    let (mut layer, mut core) = (alloc::Counts::ZERO, alloc::Counts::ZERO);
    for round in 0..ROUNDS {
        let screened = make(2 * round);
        alloc::begin();
        group.member(1).multi.step(0, screened).unwrap();
        let counted = alloc::end();
        group.quiet();
        let direct = make(2 * round + 1);
        alloc::begin();
        group
            .member(1)
            .multi
            .node_mut(0)
            .unwrap()
            .step(direct)
            .unwrap();
        let bare = alloc::end();
        group.quiet();
        if round >= ROUNDS / 2 {
            add(&mut layer, &counted);
            add(&mut core, &bare);
        }
    }
    assert_eq!(
        (layer.allocations, layer.reallocations, layer.bytes),
        (core.allocations, core.reallocations, core.bytes),
        "forwarded proposals screened: {layer:?} against the core's {core:?}"
    );
}
