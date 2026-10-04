//! The layer over real members of the core (`docs/multilog.md` §3, §5, §6, §11 step 2): one log
//! through the layer against the bare core; the leader's one barrier a global and its refusal of
//! what is out of place; a barrier relayed past a cut; the bound on what a log holds beyond its
//! merge; a restart from an image; an image installed from a leader's snapshot.
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

use hyper_multilog::{Error, Limits, Route, entry, log_of};
use hyper_raft::RawNode;
use hyper_raft::proto::{ConfState, Entry, Message, MessageType};
use support::{Disk, Group, Store, config};

/// A bound on what a log holds past its merge that none of these tests reaches but the one that
/// tests it.
const ROOMY: Limits = Limits { unmerged: 1 << 20 };

/// The commands of `log` in `member`'s disk, as stated: how many barriers it holds.
fn barriers_in(group: &mut Group, member: u64, log: usize) -> usize {
    group
        .member(member)
        .multi
        .node(log)
        .unwrap()
        .store()
        .0
        .entries
        .iter()
        .filter(|entry| matches!(entry::read(entry), entry::Stated::Barrier(_)))
        .count()
}

/// Every member's state machine, compared: the same history everywhere.
fn same_everywhere(group: &mut Group) {
    let first = group.members[0].app.clone();
    for member in &group.members {
        assert_eq!(member.app.keys, first.keys, "member {}", member.id);
        assert_eq!(member.app.logs, first.logs, "member {}", member.id);
    }
}

/// The bare core, three members, driven as the layer's harness drives each log: what each applies,
/// by index, with its bytes.
struct Bare {
    nodes: Vec<RawNode<Store>>,
    applied: Vec<Vec<(u64, Vec<u8>)>>,
    flight: Vec<Message>,
}

impl Bare {
    fn new(voters: u64, seed: u64) -> Self {
        let ids: Vec<u64> = (1..=voters).collect();
        let boot = ConfState {
            voters: ids.clone(),
            ..ConfState::default()
        };
        let nodes = ids
            .iter()
            .map(|id| {
                // The seed the layer gives log 0's member: the member's, mixed with the log's.
                let member_seed = seed.wrapping_mul(1_000_003).wrapping_add(*id);
                let mut cfg = config(*id, ids.len(), member_seed);
                cfg.seed = hyper_multilog::route::mix(member_seed ^ hyper_multilog::route::mix(0));
                RawNode::new(&cfg, Store(Disk::new(boot.clone()))).unwrap()
            })
            .collect();
        Self {
            nodes,
            applied: vec![Vec::new(); voters as usize],
            flight: Vec::new(),
        }
    }
    fn drive(&mut self, at: usize) {
        let node = &mut self.nodes[at];
        while node.has_ready() {
            let mut ready = node.ready().unwrap();
            node.store_mut().0.append(ready.entries());
            if let Some(hard) = ready.hard_state() {
                node.store_mut().0.hard_state = *hard;
            }
            self.flight.extend(ready.take_messages());
            self.flight.extend(ready.take_persisted_messages());
            let mut committed = ready.take_committed_entries();
            let mut light = node.advance_append(ready).unwrap();
            if let Some(commit) = light.commit_index() {
                node.store_mut().0.hard_state.commit = commit;
                node.commit_durable(commit).unwrap();
            }
            self.flight.extend(light.take_messages());
            committed.extend(light.take_committed_entries());
            for entry in &committed {
                if !entry.data.is_empty() {
                    self.applied[at].push((entry.index, entry.data.clone()));
                }
            }
            if let Some(last) = committed.last() {
                node.advance_apply_to(last.index).unwrap();
            }
        }
    }
    fn quiet(&mut self) {
        for at in 0..self.nodes.len() {
            self.drive(at);
        }
        while !self.flight.is_empty() {
            let flight = std::mem::take(&mut self.flight);
            for message in flight {
                let at = (message.to - 1) as usize;
                let _ = self.nodes[at].step(message);
            }
            for at in 0..self.nodes.len() {
                self.drive(at);
            }
        }
    }
}

/// `docs/multilog.md` §1: one log through the layer applies what the bare core applies, on one
/// schedule: the same entries at the same indexes, in the same order, each command's bytes the
/// owner's (the bare core holds them with the layer's tag, which the layer reads off).
#[test]
fn one_log_through_the_layer_applies_what_the_bare_core_applies() {
    let mut group = Group::new(3, 1, 7, ROOMY);
    let mut bare = Bare::new(3, 7);
    group.elect(0, 1);
    bare.nodes[0].campaign().unwrap();
    bare.quiet();
    for i in 0..40u64 {
        let command = i.to_le_bytes().to_vec();
        let (route, data) = if i % 5 == 0 {
            (Route::Global, entry::global(command.clone()).unwrap())
        } else {
            (
                Route::Key(i % 7),
                entry::keyed_command(command.clone(), i % 7).unwrap(),
            )
        };
        assert!(group.member(1).propose(route, command));
        bare.nodes[0].propose(Vec::new(), data).unwrap();
        group.quiet();
        bare.quiet();
    }
    for at in 0..3usize {
        let layer = &group.members[at].app.logs[&0];
        let core: Vec<(u64, Vec<u8>)> = bare.applied[at]
            .iter()
            .map(|(index, data)| {
                let command = match entry::read(&Entry {
                    data: data.clone(),
                    ..Entry::default()
                }) {
                    entry::Stated::Global(command) => command.to_vec(),
                    entry::Stated::Keyed { command, .. } => command.to_vec(),
                    other => panic!("the bare core applied {other:?}"),
                };
                (*index, command)
            })
            .collect();
        assert_eq!(layer, &core, "member {}", at + 1);
        assert_eq!(layer.len(), 40);
    }
}

/// `docs/multilog.md` §3.1–§3.2: a global command is owed a barrier by every member, in every
/// other log; the leader of each appends one and drops the members' forwarded copies, and every
/// member applies the global. A proposal out of place, as a peer would forward it, is refused at
/// the leader and changes nothing; a forwarded barrier the leader covers is dropped.
#[test]
fn a_leader_keeps_one_barrier_a_global_and_refuses_what_is_out_of_place() {
    let mut group = Group::new(3, 3, 11, ROOMY);
    group.elect(0, 1);
    group.elect(1, 2);
    group.elect(2, 3);
    assert!(group.member(3).propose(Route::Global, b"g".to_vec()));
    group.quiet();
    for log in 1..3 {
        for member in 1..=3 {
            assert_eq!(
                barriers_in(&mut group, member, log),
                1,
                "log {log} at member {member}"
            );
        }
    }
    for member in &group.members {
        assert_eq!(member.counts.globals_applied, 1, "member {}", member.id);
    }
    same_everywhere(&mut group);
    let leader = group.member(2);
    let last = leader
        .multi
        .node(1)
        .unwrap()
        .raft
        .log()
        .last_index()
        .unwrap();
    let forward = |data: Vec<u8>| {
        let mut message = hyper_raft::proto::message(2, MessageType::MsgPropose);
        message.from = 3;
        message.entries.push(Entry {
            data,
            ..Entry::default()
        });
        message
    };
    let elsewhere = (0..).find(|key| log_of(*key, 3) != 1).unwrap();
    for (data, why) in [
        (
            entry::global(b"x".to_vec()).unwrap(),
            "a global command outside log 0",
        ),
        (
            entry::keyed_command(b"x".to_vec(), elsewhere).unwrap(),
            "a keyed command in another log",
        ),
        (vec![0xff], "bytes no member writes"),
    ] {
        assert_eq!(
            leader.multi.step(1, forward(data)),
            Err(Error::Violation(why))
        );
    }
    let covered = entry::barrier_naming(leader.multi.latest_global()).unwrap();
    assert_eq!(leader.multi.step(1, forward(covered)), Ok(()));
    assert_eq!(
        leader
            .multi
            .node(1)
            .unwrap()
            .raft
            .log()
            .last_index()
            .unwrap(),
        last,
        "nothing appended"
    );
    let log0 = group.member(1);
    let mut barrier = hyper_raft::proto::message(1, MessageType::MsgPropose);
    barrier.from = 3;
    barrier.entries.push(Entry {
        data: entry::barrier_naming(1).unwrap(),
        ..Entry::default()
    });
    assert_eq!(
        log0.multi.step(0, barrier),
        Err(Error::Violation("a barrier in log 0"))
    );
}

/// `docs/multilog.md` §3.1: log 0's leader and log 1's leader cannot hear each other; log 1's
/// leader never learns the global committed, and the member that hears both relays its barrier:
/// it applies the global, and once the cut heals every member does, alike.
#[test]
fn a_barrier_is_relayed_past_a_cut_between_two_leaders() {
    let mut group = Group::new(3, 2, 13, ROOMY);
    group.elect(0, 1);
    group.elect(1, 2);
    group.cut(1, 2, true);
    assert!(group.member(1).propose(Route::Global, b"g".to_vec()));
    group.quiet();
    assert_eq!(
        group.member(2).multi.latest_global(),
        0,
        "log 1's leader never heard the global"
    );
    assert_eq!(
        barriers_in(&mut group, 2, 1),
        1,
        "the barrier member 3 relayed"
    );
    assert_eq!(
        group.member(3).counts.globals_applied,
        1,
        "member 3 hears both, and applies it"
    );
    group.cut(1, 2, false);
    for _ in 0..3 {
        group.tick();
    }
    for member in &group.members {
        assert_eq!(member.counts.globals_applied, 1, "member {}", member.id);
    }
    same_everywhere(&mut group);
}

/// `docs/multilog.md` §6: a log whose merge waits (its global waits for a log with no leader)
/// holds at most `unmerged` entries past its merge: its leader refuses a client's command there,
/// and takes them again once the waiting log elects and its barrier lets the merge move.
#[test]
fn what_a_log_holds_past_its_merge_is_bounded_at_its_leader() {
    let unmerged = 6;
    let mut group = Group::new(3, 2, 17, Limits { unmerged });
    group.elect(0, 1);
    assert!(group.member(1).propose(Route::Global, b"g".to_vec()));
    group.quiet();
    let key = (0..).find(|key| log_of(*key, 2) == 0).unwrap();
    let mut taken = 0;
    let mut refused = None;
    for i in 0..20u64 {
        match group
            .member(1)
            .multi
            .propose(Route::Key(key), i.to_le_bytes().to_vec())
        {
            Ok(_) => taken += 1,
            Err(error) => {
                refused = Some(error);
                break;
            }
        }
        group.quiet();
    }
    assert_eq!(refused, Some(Error::Capacity("unmerged")));
    assert!(
        taken < unmerged,
        "the global and the leader's first entry count too: {taken}"
    );
    assert_eq!(
        group.member(1).counts.globals_applied,
        0,
        "the global waits for log 1"
    );
    group.elect(1, 2);
    for member in &group.members {
        assert_eq!(member.counts.globals_applied, 1, "member {}", member.id);
    }
    assert!(
        group
            .member(1)
            .multi
            .propose(Route::Key(key), b"after".to_vec())
            .is_ok()
    );
    group.quiet();
    same_everywhere(&mut group);
}

/// `docs/multilog.md` §5.5: a member restarts from its image at a global's canonical cut, applies
/// again what its logs hold past the cut, and reaches every other member's history.
#[test]
fn a_member_restarts_from_its_image_and_reaches_the_same_history() {
    let mut group = Group::new(3, 3, 19, ROOMY);
    group.elect(0, 1);
    group.elect(1, 2);
    group.elect(2, 3);
    let mut i = 0u64;
    let mut command = |group: &mut Group, route: Route| {
        i += 1;
        let at = i % 3 + 1;
        let log = group.member(at).multi.route(route);
        let leader = (1..=3).find(|id| group.member(*id).leads(log)).unwrap();
        assert!(
            group
                .member(leader)
                .propose(route, i.to_le_bytes().to_vec())
        );
        group.quiet();
    };
    for k in 0..12 {
        command(&mut group, Route::Key(k));
    }
    group.member(3).image_at_next_global = true;
    command(&mut group, Route::Global);
    assert!(
        group.member(3).image.is_some(),
        "an image at the global's cut"
    );
    for k in 0..12 {
        command(&mut group, Route::Key(k));
    }
    command(&mut group, Route::Global);
    let before = group.member(3).app.clone();
    group.member(3).restart();
    assert_ne!(group.member(3).app, before, "restarted to the image");
    group.quiet();
    for _ in 0..3 {
        group.tick();
    }
    assert_eq!(
        group.member(3).app.keys,
        before.keys,
        "applied again to the same history"
    );
    same_everywhere(&mut group);
}

/// `docs/multilog.md` §5.3: a member cut off while the others take an image and compact every log
/// to it is sent a log's snapshot when it returns; it installs the image, which is ahead of its
/// whole state, and goes on to the others' history.
#[test]
fn a_member_behind_a_compaction_installs_the_image_and_catches_up() {
    let mut group = Group::new(3, 2, 23, ROOMY);
    group.elect(0, 1);
    group.elect(1, 2);
    group.isolate(3, true);
    for i in 0..10u64 {
        let route = Route::Key(i);
        let log = group.member(1).multi.route(route);
        let leader = if log == 0 { 1 } else { 2 };
        assert!(
            group
                .member(leader)
                .propose(route, i.to_le_bytes().to_vec())
        );
        group.quiet();
    }
    group.member(1).image_at_next_global = true;
    group.member(2).image_at_next_global = true;
    assert!(group.member(1).propose(Route::Global, b"cut".to_vec()));
    group.quiet();
    assert!(group.member(1).image.is_some() && group.member(2).image.is_some());
    assert!(
        group
            .member(1)
            .multi
            .node(0)
            .unwrap()
            .store()
            .0
            .snapshot_index()
            > 0
    );
    group.isolate(3, false);
    for _ in 0..4 {
        group.tick();
    }
    assert!(
        group.member(3).counts.installed >= 1,
        "member 3 installed an image"
    );
    same_everywhere(&mut group);
}
