//! A member whose log lost at rest entries it acknowledged (`docs/durable.md`
//! §5): it opens marked (`hyper_raft::Lost`), says so to a leader that counts
//! the entries (a refusal flagged `lost`), and the leader takes its progress
//! back to the last entry the member holds and sends the lost entries again:
//! entries, not a snapshot (core step R-5, CTRL's follower repair, Alagappan
//! et al., FAST 2018, §3.4). A snapshot goes only where the leader no longer
//! holds them.
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

use hyper_raft::proto::{Message, MessageType};
use support::{Cluster, Fault, New, Op, Settings};

/// Delivers everything on the network, in order, through `look` first, which
/// may change it as it passes; what is delivered is returned.
fn quiet(group: &mut Cluster<New>, look: &mut impl FnMut(&mut Message)) -> Vec<Message> {
    let mut seen = Vec::new();
    for _ in 0..10_000 {
        if group.net.is_empty() {
            return seen;
        }
        look(&mut group.net[0]);
        seen.push(group.net[0].clone());
        group.act(&Op::Deliver {
            at: 0,
            keep: false,
            lose: false,
        });
    }
    panic!("the network does not fall quiet");
}

fn pass(_: &mut Message) {}

/// A group of three led by 1, which committed `count` entries every member
/// holds.
fn group_with(count: usize) -> Cluster<New> {
    let mut group: Cluster<New> = Cluster::new(3, &[1, 2, 3], Settings::focal(), 1);
    group.act(&Op::Campaign(1));
    quiet(&mut group, &mut pass);
    assert_eq!(group.leaders_now(), vec![1]);
    for at in 0..count {
        group.act(&Op::Propose(1, format!("entry {at}").into_bytes()));
        quiet(&mut group, &mut pass);
    }
    group
}

/// A beat of the leader's heartbeats, and everything that follows from it.
fn beat(group: &mut Cluster<New>, look: &mut impl FnMut(&mut Message)) -> Vec<Message> {
    let mut seen = Vec::new();
    for _ in 0..Settings::focal().heartbeat_tick {
        group.act(&Op::Tick(1));
        seen.extend(quiet(group, look));
    }
    seen
}

/// The leader's appends to `member` among `messages`, and the indexes they carry.
fn sent_to(messages: &[Message], member: u64) -> (Vec<u64>, usize) {
    let mut indexes = Vec::new();
    let mut snapshots = 0;
    for message in messages.iter().filter(|m| m.from == 1 && m.to == member) {
        match message.msg_type {
            MessageType::MsgAppend => indexes.extend(message.entries.iter().map(|e| e.index)),
            MessageType::MsgSnapshot => snapshots += 1,
            _ => {}
        }
    }
    (indexes, snapshots)
}

#[test]
fn lost_entries_are_repaired_by_their_exact_resend() {
    let mut group = group_with(20);
    let last = group.disk(1).last_index();
    assert_eq!(group.disk(3).last_index(), last);
    // The last ten entries of member 3 are lost at rest, their persist
    // record kept: it opens marked through what it acknowledged.
    group.act(&Op::Corrupt(3, Fault::Lose(10)));
    let held = last - 10;
    assert_eq!(group.disk(3).last_index(), held);
    let lost = group.node(3).unwrap().raw.raft.lost().unwrap();
    assert_eq!((lost.index, lost.term), (last, 1));
    let mut seen = Vec::new();
    for _ in 0..4 {
        seen.extend(beat(&mut group, &mut pass));
    }
    // It said so once the leader's heartbeat counted what it lost.
    assert!(
        seen.iter()
            .any(|m| m.from == 3 && m.lost && m.reject_hint == held)
    );
    // Exactly the lost entries were sent again, each once, and no snapshot.
    let (indexes, snapshots) = sent_to(&seen, 3);
    assert_eq!(indexes, (held + 1..=last).collect::<Vec<_>>());
    assert_eq!(snapshots, 0);
    assert_eq!(group.disk(3).entries, group.disk(1).entries);
    assert_eq!(group.node(3).unwrap().raw.raft.lost(), None);
    // The repaired member counts toward what follows.
    group.act(&Op::Propose(1, b"after".to_vec()));
    quiet(&mut group, &mut pass);
    assert_eq!(group.disk(3).last_index(), last + 1);
}

/// The defect R-5 closes: without the flag the refusal is an ordinary one for
/// an index the leader counts as held, which it takes for a stale answer, and
/// the group reaches a fixed point in which the member never holds again what
/// it lost.
#[test]
fn without_the_flag_a_leader_never_sends_the_lost_entries_again() {
    let mut group = group_with(20);
    let last = group.disk(1).last_index();
    group.act(&Op::Corrupt(3, Fault::Lose(10)));
    let mut strip = |message: &mut Message| message.lost = false;
    let mut before = None;
    for _ in 0..4 {
        let seen = beat(&mut group, &mut strip);
        assert_eq!(sent_to(&seen, 3), (Vec::new(), 0));
        let now = (
            group.node(1).unwrap().raw.raft.tracker().get(3).cloned(),
            group.disk(3).last_index(),
        );
        if let Some(before) = &before {
            assert_eq!(before, &now, "a fixed point");
        }
        before = Some(now);
    }
    assert_eq!(group.disk(3).last_index(), last - 10);
}

/// A leader that compacted past what the member lost sends a snapshot: only
/// there.
#[test]
fn a_snapshot_goes_only_where_the_leader_no_longer_holds_the_entries() {
    let mut group = group_with(20);
    let last = group.disk(1).last_index();
    group.act(&Op::Compact(1));
    assert_eq!(group.disk(1).snapshot_index(), last);
    group.act(&Op::Corrupt(3, Fault::Lose(10)));
    let mut seen = Vec::new();
    for _ in 0..4 {
        seen.extend(beat(&mut group, &mut pass));
    }
    let (indexes, snapshots) = sent_to(&seen, 3);
    assert!(indexes.is_empty());
    assert_eq!(snapshots, 1);
    assert_eq!(group.disk(3).last_index(), last);
    assert_eq!(group.node(3).unwrap().raw.raft.lost(), None);
}

/// A bit flipped in an entry before the last write's is found when the member
/// opens: the log keeps nothing from it on and marks through what it held. The
/// leader sends the entries again from there, and the damaged one is never
/// applied.
#[test]
fn a_damaged_entry_before_the_last_is_cut_marked_and_repaired() {
    let mut group = group_with(20);
    let last = group.disk(1).last_index();
    let damaged = last - 15;
    group.act(&Op::Corrupt(3, Fault::Flip(damaged)));
    assert_eq!(group.disk(3).last_index(), damaged - 1);
    let mut seen = Vec::new();
    for _ in 0..4 {
        seen.extend(beat(&mut group, &mut pass));
    }
    let (indexes, snapshots) = sent_to(&seen, 3);
    assert_eq!(indexes, (damaged..=last).collect::<Vec<_>>());
    assert_eq!(snapshots, 0);
    assert_eq!(group.disk(3).entries, group.disk(1).entries);
}
