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

/// `docs/multilog.md` §3.1: log 0's leader relays no barrier to a log it does not lead, for its
/// own replication of log 0 tells that log's leader the global on every path the relay could
/// take; each other log's leader appends its own, and the member that leads neither log 0 nor the
/// log relays. Three logs led apart: one global costs each log one barrier and one relay.
#[test]
fn log_0s_leader_relays_no_barrier() {
    let mut group = Group::new(3, 3, 11, ROOMY);
    group.elect(0, 1);
    group.elect(1, 2);
    group.elect(2, 3);
    assert!(group.member(1).propose(Route::Global, b"g".to_vec()));
    group.quiet();
    let proposed: Vec<u64> = group
        .members
        .iter()
        .map(|member| member.counts.barriers_proposed)
        .collect();
    assert_eq!(
        proposed,
        vec![0, 2, 2],
        "log 0's leader none; each other member its own log's barrier and one relay"
    );
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

/// Three members of two logs whose elections run by suspicion, as an owner's detectors drive them.
fn by_suspicion(seed: u64) -> Group {
    let ids = [1, 2, 3];
    Group {
        members: ids
            .iter()
            .map(|id| support::Member::open(*id, &ids, 2, seed + id, ROOMY, true))
            .collect(),
        flight: Vec::new(),
        blocked: std::collections::BTreeSet::new(),
    }
}

/// `docs/multilog.md` §7.1: two logs' leaders cut from each other (a partial partition: the third
/// member hears both). Log 1's leader is cut from log 0's, so its merge stops at log 0's next
/// global: it stands at priority zero in log 1 and yields it to the member it does not suspect;
/// log 0's leader, cut only from a later log's, keeps log 0. The new leader of log 1 does not hand
/// it back while its preferred voter's messages state it cut; once the cut heals, it does.
#[test]
fn a_leader_cut_from_a_lower_logs_leader_yields_and_is_not_handed_back() {
    let mut group = by_suspicion(23);
    group.elect(0, 1);
    group.elect(1, 2);
    for member in &mut group.members {
        member.multi.spread(&[1, 2, 3]).unwrap();
    }
    assert_eq!(
        group.member(2).multi.hand_off(1),
        None,
        "2 is log 1's preferred voter"
    );
    group.cut(1, 2, true);
    for (me, them) in [(1, 2), (2, 1)] {
        for log in 0..2 {
            group
                .member(me)
                .multi
                .node_mut(log)
                .unwrap()
                .suspect(them)
                .unwrap();
        }
    }
    assert!(group.member(2).multi.cut_below(1));
    assert!(
        !group.member(1).multi.cut_below(1),
        "1 leads log 0 and hears log 0"
    );
    assert!(!group.member(3).multi.cut_below(1));
    // A command in each log, so each leader hears member 3 hold its whole log.
    for (leader, log) in [(1, 0), (2, 1)] {
        let key = (0..).find(|key| log_of(*key, 2) == log).unwrap();
        assert!(group.member(leader).propose(Route::Key(key), b"k".to_vec()));
        group.quiet();
    }
    assert_eq!(group.member(1).multi.hand_off(0), None, "1 keeps log 0");
    assert_eq!(group.member(2).multi.hand_off(1), Some(3), "2 yields log 1");
    group
        .member(2)
        .multi
        .node_mut(1)
        .unwrap()
        .transfer_leader(3)
        .unwrap();
    group.quiet();
    assert!(group.member(3).leads(1), "3 leads log 1");
    // 2's messages state it cut below log 1: 3 hands it nothing back, though 2 is preferred.
    assert_eq!(group.member(3).multi.hand_off(1), None);
    assert!(group.member(1).propose(Route::Global, b"g".to_vec()));
    group.quiet();
    for id in [1, 3] {
        assert_eq!(
            group.member(id).counts.globals_applied,
            1,
            "member {id} hears both leaders, and applies"
        );
    }
    group.cut(1, 2, false);
    for (me, them) in [(1, 2), (2, 1)] {
        for log in 0..2 {
            group
                .member(me)
                .multi
                .node_mut(log)
                .unwrap()
                .trust(them)
                .unwrap();
        }
    }
    assert!(!group.member(2).multi.cut_below(1));
    let key = (0..).find(|key| log_of(*key, 2) == 1).unwrap();
    assert!(group.member(3).propose(Route::Key(key), b"k".to_vec()));
    group.quiet();
    assert_eq!(
        group.member(3).multi.hand_off(1),
        Some(2),
        "log 1 goes back to 2"
    );
}

/// `docs/multilog.md` §9: a batch for one log is one proposal, carried to each follower by one
/// append; a batch with a command routed elsewhere is refused whole, and the bound on what a log
/// holds past its merge counts the batch's commands.
#[test]
fn a_batch_is_one_proposal_refused_whole() {
    let mut group = Group::new(3, 2, 29, Limits { unmerged: 8 });
    group.elect(0, 1);
    group.elect(1, 1);
    let keys: Vec<u64> = (0..).filter(|key| log_of(*key, 2) == 1).take(5).collect();
    let batch: Vec<(Route, Vec<u8>)> = keys
        .iter()
        .map(|key| (Route::Key(*key), key.to_le_bytes().to_vec()))
        .collect();
    group.member(1).multi.propose_in(1, batch.clone()).unwrap();
    let mut out = Vec::new();
    group.member(1).settle(&mut out);
    let appends: Vec<&Message> = out
        .iter()
        .filter(|(log, m)| *log == 1 && m.msg_type == MessageType::MsgAppend)
        .map(|(_, m)| m)
        .collect();
    assert_eq!(appends.len(), 2, "one append to each follower");
    assert!(appends.iter().all(|m| m.entries.len() == 5));
    group
        .flight
        .extend(out.into_iter().map(|(log, m)| (1, log, m)));
    group.quiet();
    let mut misrouted = batch.clone();
    misrouted.push((Route::Global, b"g".to_vec()));
    assert_eq!(
        group.member(1).multi.propose_in(1, misrouted),
        Err(Error::Violation("a batch's command routed to another log"))
    );
    // Log 1 holds its leader's entry and five commands, all merged; nine more would pass eight.
    let nine: Vec<(Route, Vec<u8>)> = (0..9)
        .map(|i| (Route::Key(keys[i % 5]), vec![i as u8]))
        .collect();
    assert_eq!(
        group.member(1).multi.propose_in(1, nine),
        Err(Error::Capacity("unmerged"))
    );
    group.member(1).multi.propose_in(1, batch).unwrap();
    group.quiet();
    same_everywhere(&mut group);
}

/// The fast track's proposal of a command, made by a member that does not lead its log, held by
/// every voter and taken by the leader, and committed by the fast quorum: every member applies it
/// in the merged order (`docs/multilog.md` §9.1).
#[test]
fn a_command_proposed_by_the_fast_track_is_applied_alike_everywhere() {
    let mut group = Group::open(3, 2, 37, ROOMY, true);
    group.elect(0, 1);
    group.elect(1, 1);
    let key = (0..).find(|key| log_of(*key, 2) == 1).unwrap();
    assert!(group.member(2).propose_fast(Route::Key(key), b"k".to_vec()));
    assert!(group.member(3).propose_fast(Route::Global, b"g".to_vec()));
    group.quiet();
    for member in &group.members {
        assert_eq!(member.counts.keyed_applied, 1, "member {}", member.id);
        assert_eq!(member.counts.globals_applied, 1, "member {}", member.id);
        assert!(member.displaced.is_empty());
    }
    let committed: u64 = (0..2)
        .map(|log| {
            group.members[0]
                .multi
                .node(log)
                .unwrap()
                .raft
                .fast_stats()
                .committed
        })
        .sum();
    assert_eq!(committed, 2, "each by the fast quorum");
    same_everywhere(&mut group);
}

/// No member holds what the fast track may not carry (`docs/multilog.md` §9.1): a command in
/// another log, and a barrier, are refused before the core sees them, and nothing changes.
#[test]
fn a_fast_proposal_out_of_place_is_refused_and_no_member_holds_it() {
    let mut group = Group::open(3, 2, 41, ROOMY, true);
    group.elect(0, 1);
    group.elect(1, 1);
    let elsewhere = (0..).find(|key| log_of(*key, 2) == 0).unwrap();
    for data in [
        entry::keyed_command(b"k".to_vec(), elsewhere).unwrap(),
        entry::barrier_naming(1).unwrap(),
        entry::global(b"g".to_vec()).unwrap(),
    ] {
        let message = Message {
            msg_type: hyper_raft::fast::FAST_PROPOSE,
            from: 2,
            to: 3,
            entries: vec![Entry {
                index: 2,
                term: 1,
                data,
                ..Entry::default()
            }],
            ..Message::default()
        };
        assert!(matches!(
            group.member(3).multi.step(1, message),
            Err(Error::Violation(_))
        ));
        assert_eq!(
            group
                .member(3)
                .multi
                .node(1)
                .unwrap()
                .raft
                .fast_stats()
                .held,
            0,
            "nothing is held"
        );
    }
}

/// `docs/multilog.md` §6 by the fast track: a leader whose log holds `unmerged` entries past its
/// merge takes nothing more from the fast track, however much the other members propose, until
/// the merge moves; then it takes what their votes decided, and a proposal made after.
#[test]
fn a_leader_at_its_unmerged_bound_takes_nothing_by_the_fast_track() {
    // The global waiting past the merge takes one entry of the bound; the test needs room for one
    // command the fast track takes and none for another, so the least bound is two, and one
    // proposal past the room shows the cap.
    let unmerged = 2;
    let proposed = unmerged;
    let mut group = Group::open(3, 2, 43, Limits { unmerged }, true);
    group.elect(0, 1);
    assert!(group.member(1).propose(Route::Global, b"g".to_vec()));
    group.quiet();
    let key = (0..).find(|key| log_of(*key, 2) == 0).unwrap();
    for i in 0..proposed {
        group
            .member(2)
            .propose_fast(Route::Key(key), i.to_le_bytes().to_vec());
        group.quiet();
    }
    let leader = group.member(1);
    let last = leader
        .multi
        .node(0)
        .unwrap()
        .raft
        .log()
        .last_index()
        .unwrap();
    let merged = leader.multi.merged_through(0).unwrap();
    assert!(
        last - merged <= unmerged,
        "the leader holds {} past its merge",
        last - merged
    );
    assert_eq!(
        leader.counts.globals_applied, 0,
        "the global waits for log 1"
    );
    group.elect(1, 2);
    for member in &group.members {
        assert_eq!(member.counts.globals_applied, 1, "member {}", member.id);
    }
    let before = group.member(2).counts.keyed_applied;
    assert!(
        group
            .member(2)
            .propose_fast(Route::Key(key), b"after".to_vec())
    );
    group.quiet();
    assert_eq!(group.member(2).counts.keyed_applied, before + 1);
    same_everywhere(&mut group);
}

/// `docs/multilog.md` §3.5: a group of two logs grows to three and shrinks to one while it takes
/// commands; every member applies the resize at one point, opens and ends the same logs, and
/// applies the same history, a member restarted from an image taken before the resizes included.
#[test]
fn a_group_grows_and_shrinks_its_logs_and_every_member_applies_alike() {
    let mut group = Group::new(3, 2, 47, ROOMY);
    group.elect(0, 1);
    group.elect(1, 1);
    let write = |group: &mut Group, round: u8| {
        for key in 0..8u64 {
            let route = Route::Key(key);
            assert!(group.member(1).propose(route, vec![round, key as u8]));
            group.quiet();
        }
    };
    write(&mut group, 0);
    group.member(3).image_at_next_global = true;
    assert!(group.member(1).propose(Route::Global, b"g".to_vec()));
    group.quiet();
    assert!(group.member(3).image.is_some(), "member 3 took an image");
    group.member(1).multi.propose_resize(3).unwrap();
    group.quiet();
    for member in &group.members {
        assert_eq!(member.multi.count(), 3, "member {}", member.id);
        assert_eq!(member.counts.resizes, 1, "member {}", member.id);
    }
    group.elect(2, 1);
    write(&mut group, 1);
    same_everywhere(&mut group);
    group.member(1).multi.propose_resize(1).unwrap();
    group.quiet();
    for member in &group.members {
        assert_eq!(member.multi.count(), 1, "member {}", member.id);
        assert_eq!(member.counts.resizes, 2, "member {}", member.id);
    }
    write(&mut group, 2);
    same_everywhere(&mut group);
    let applied = group.member(1).counts.keyed_applied;
    assert_eq!(applied, 24, "every write applied");
    // Member 3 reopens from its image, two resizes back, and reaches the same history.
    group.member(3).restart();
    group.quiet();
    assert_eq!(group.member(3).multi.count(), 1);
    same_everywhere(&mut group);
}

/// A message of another generation of a log (`docs/multilog.md` §3.5) is dropped: one an earlier
/// generation's leader sent, with a term past this generation's, moves nothing here.
#[test]
fn a_message_of_another_generation_of_a_log_is_dropped() {
    let mut group = Group::new(3, 2, 53, ROOMY);
    group.elect(1, 1);
    let term = group.member(2).multi.node(1).unwrap().raft.term();
    let stale = |generation: i64| Message {
        msg_type: MessageType::MsgHeartbeat,
        from: 3,
        to: 2,
        term: term + 5,
        priority: generation << 32,
        ..Message::default()
    };
    group.member(2).multi.step(1, stale(1)).unwrap();
    assert_eq!(group.member(2).multi.node(1).unwrap().raft.term(), term);
    group.member(2).multi.step(1, stale(0)).unwrap();
    assert_eq!(
        group.member(2).multi.node(1).unwrap().raft.term(),
        term + 5,
        "the same generation's message is taken"
    );
}
