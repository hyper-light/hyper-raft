//! Each oracle passes what its specification allows and catches the defects planted against it
//! (`docs/sim.md` §4.6): one test an oracle, the allowed observations first and then each planted
//! defect, which must be refused with the violation that names it.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::disallowed_macros,
    clippy::cast_possible_truncation,
    clippy::cognitive_complexity,
    missing_docs
)]

use std::collections::BTreeMap;

use hyper_check::oracle::{
    Durability, DurableView, ElectionSafety, ExactlyOnce, FastAgreement, LeaderCompleteness,
    LogMatching, LogView, ReadSafety, Released, Rule, SameHistory, Says, StateMachineSafety, Terms,
    Violation,
};

#[test]
fn election_safety_refuses_a_second_leader_of_a_term_ever() {
    let mut oracle = ElectionSafety::new(16);
    oracle.leader(1, 1).unwrap();
    oracle.leader(1, 1).unwrap();
    oracle.leader(2, 2).unwrap();
    oracle.leader(3, 4).unwrap();
    assert_eq!(oracle.terms(), 3);
    // Planted: member 3 leads term 2, which member 2 led, though 2 has long stepped down.
    assert_eq!(
        oracle.leader(3, 2),
        Err(Violation::TwoLeaders {
            term: 2,
            first: 2,
            second: 3
        })
    );
    // Its bound refuses.
    let mut small = ElectionSafety::new(1);
    small.leader(1, 1).unwrap();
    assert!(matches!(small.leader(1, 2), Err(Violation::Full { .. })));
}

#[test]
fn log_matching_holds_every_entry_of_a_term_at_an_index_to_the_first() {
    let mut oracle = LogMatching::<u8>::new(Terms::Compared, 64);
    // Two logs that agree, one longer, one past a snapshot of term 1 at index 2.
    for (member, entries) in [
        (1u64, vec![(1u64, 1u64, 10u8), (2, 1, 11), (3, 2, 12)]),
        (2, vec![(1, 1, 10), (2, 1, 11)]),
    ] {
        let mut before = 0;
        for (index, term, value) in entries {
            oracle.holds(member, index, term, &value, before).unwrap();
            before = term;
        }
    }
    oracle.holds(3, 3, 2, &12, 1).unwrap();
    // Planted: an entry of term 2 at 3 stating something else.
    assert!(matches!(
        oracle.holds(4, 3, 2, &99, 1),
        Err(Violation::LogsDisagree {
            index: 3,
            term: 2,
            ..
        })
    ));
    // Planted: the same entry after another term than the first log's.
    assert!(matches!(
        oracle.holds(4, 3, 2, &12, 2),
        Err(Violation::LogsDisagree { .. })
    ));
    // Planted: a log whose terms go down.
    assert!(matches!(
        oracle.holds(5, 4, 1, &13, 2),
        Err(Violation::TermsDecrease { .. })
    ));
    // With the fast track, an entry may follow one value under either of its terms; it still
    // states one value an index and term.
    let mut fast = LogMatching::<u8>::new(Terms::Ignored, 64);
    fast.holds(1, 5, 5, &50, 5).unwrap();
    fast.holds(2, 5, 5, &50, 2).unwrap();
    assert!(matches!(
        fast.holds(3, 5, 5, &51, 5),
        Err(Violation::LogsDisagree { .. })
    ));
}

/// A log of entries above a snapshot.
struct Log {
    start: u64,
    entries: BTreeMap<u64, (u64, u8)>,
}

impl LogView<u8> for Log {
    fn start(&self) -> u64 {
        self.start
    }
    fn entry(&self, index: u64) -> Option<(u64, &u8)> {
        self.entries.get(&index).map(|(term, value)| (*term, value))
    }
}

#[test]
fn state_machine_safety_and_leader_completeness_hold_the_committed_entries() {
    let mut committed = StateMachineSafety::<u8>::new(Terms::Compared, 64);
    committed.committed(1, 1, 1, &10).unwrap();
    committed.committed(2, 1, 1, &10).unwrap();
    committed.committed(1, 2, 2, &11).unwrap();
    assert_eq!(committed.highest(), 2);
    // Planted: another entry committed at 2, by what it states and by its term.
    assert_eq!(
        committed.committed(3, 2, 2, &99),
        Err(Violation::TwoCommitted {
            member: 3,
            index: 2
        })
    );
    assert_eq!(
        committed.committed(3, 2, 3, &11),
        Err(Violation::TwoCommitted {
            member: 3,
            index: 2
        })
    );
    // A group with the fast track compares what an entry states alone.
    let mut fast = StateMachineSafety::<u8>::new(Terms::Ignored, 64);
    fast.committed(1, 2, 2, &11).unwrap();
    fast.committed(2, 2, 3, &11).unwrap();
    assert!(fast.committed(2, 2, 3, &12).is_err());

    let mut complete = LeaderCompleteness::new(16);
    let holds = Log {
        start: 1,
        entries: BTreeMap::from([(2, (2, 11)), (3, (3, 12))]),
    };
    complete.leader(3, 3, &holds, &committed).unwrap();
    // Checked once a leadership: a later look at the same leadership passes whatever it holds.
    let empty = Log {
        start: 0,
        entries: BTreeMap::new(),
    };
    complete.leader(3, 3, &empty, &committed).unwrap();
    // Planted: a new leader without the entry committed at 2.
    assert_eq!(
        complete.leader(4, 4, &empty, &committed),
        Err(Violation::LeaderLacks {
            member: 4,
            term: 4,
            index: 1
        })
    );
    let other = Log {
        start: 1,
        entries: BTreeMap::from([(2, (1, 11))]),
    };
    assert_eq!(
        complete.leader(5, 5, &other, &committed),
        Err(Violation::LeaderLacks {
            member: 5,
            term: 5,
            index: 2
        })
    );
    // A leader of a term elected after a later term committed holds what earlier terms committed,
    // not what the later term did: index 2 was committed in term 7.
    let mut late = LeaderCompleteness::new(16);
    late.committed_in(1, 1).unwrap();
    late.committed_in(2, 7).unwrap();
    let first = Log {
        start: 0,
        entries: BTreeMap::from([(1, (1, 10))]),
    };
    late.leader(6, 6, &first, &committed).unwrap();
    assert_eq!(
        late.leader(8, 8, &first, &committed),
        Err(Violation::LeaderLacks {
            member: 8,
            term: 8,
            index: 2
        })
    );
}

#[test]
fn fast_agreement_refuses_two_values_chosen_at_an_index() {
    // Fast Raft's fast quorum, ⌈3M/4⌉ of M.
    let fast = |voters: usize| (3 * voters).div_ceil(4);
    let mut oracle = FastAgreement::<u8>::new(64);
    // Votes before the term's quorum is known count once it is.
    oracle.vote(2, 5, 1, &7, None).unwrap();
    oracle.vote(2, 5, 2, &7, None).unwrap();
    oracle.vote(2, 5, 3, &7, None).unwrap();
    assert_eq!(oracle.chosen(5), None);
    oracle.term(2, &[1, 2, 3, 4], &[], fast).unwrap();
    assert_eq!(oracle.chosen(5), Some(&7));
    // Fewer than a fast quorum for another value chooses nothing.
    oracle.term(3, &[1, 2, 3, 4], &[], fast).unwrap();
    oracle.vote(3, 5, 1, &8, None).unwrap();
    oracle.vote(3, 5, 4, &8, None).unwrap();
    assert_eq!(oracle.chosen(5), Some(&7));
    // Planted: a fast quorum of a later term chooses another value.
    assert_eq!(
        oracle.vote(3, 5, 2, &8, None),
        Err(Violation::TwoChosen { index: 5, term: 3 })
    );
    // Votes of members that are no voters of the term choose nothing.
    let mut narrow = FastAgreement::<u8>::new(64);
    narrow.term(5, &[2, 4], &[], fast).unwrap();
    narrow.vote(5, 35, 1, &1, None).unwrap();
    narrow.vote(5, 35, 5, &1, None).unwrap();
    narrow.vote(5, 35, 3, &2, None).unwrap();
    narrow.vote(5, 35, 2, &2, None).unwrap();
    assert_eq!(narrow.chosen(35), None);
    narrow.vote(5, 35, 4, &2, None).unwrap();
    assert_eq!(narrow.chosen(35), Some(&2));
    // Planted: a voter votes two values in one round.
    assert_eq!(
        oracle.vote(2, 5, 1, &9, None),
        Err(Violation::VoteChanged {
            member: 1,
            term: 2,
            index: 5
        })
    );
}

/// A round's quorums are the protocol's: a fast quorum of every set of voters its leader counted by,
/// none under a joint election or a third set; and a vote at an index its term's leader holds, or
/// at one committed, is no acceptor's.
#[test]
fn fast_agreement_counts_by_the_rounds_own_quorums_and_open_indexes() {
    let fast = |voters: usize| (3 * voters).div_ceil(4);
    // A change in the term: a value is chosen only by a fast quorum of both sets.
    let mut changed = FastAgreement::<u8>::new(64);
    changed.term(4, &[1, 2, 3], &[], fast).unwrap();
    changed.term(4, &[3, 4, 5], &[], fast).unwrap();
    for voter in [1, 2, 3] {
        changed.vote(4, 9, voter, &1, None).unwrap();
    }
    assert_eq!(changed.chosen(9), None);
    for voter in [4, 5] {
        changed.vote(4, 9, voter, &1, None).unwrap();
    }
    assert_eq!(changed.chosen(9), Some(&1));
    // A third set: the term chooses nothing more by votes.
    changed.term(4, &[6], &[], fast).unwrap();
    for voter in [1, 2, 3, 4, 5, 6] {
        changed.vote(4, 10, voter, &2, None).unwrap();
    }
    assert_eq!(changed.chosen(10), None);
    // Elected under a joint configuration: nothing chosen by votes in the term.
    let mut joint = FastAgreement::<u8>::new(64);
    joint.term(2, &[1, 2], &[1, 2, 3], fast).unwrap();
    for voter in [1, 2, 3] {
        joint.vote(2, 4, voter, &3, None).unwrap();
    }
    assert_eq!(joint.chosen(4), None);
    // Votes where the index is not open: against the entry the term's leader holds there, and at
    // one committed. A member behind its group sends again, in a later term, what it held beside its
    // log, and by a lone voter's quorum chooses nothing; a vote for the leader's own entry counts.
    let mut stale = FastAgreement::<u8>::new(64);
    stale.term(6, &[1], &[], fast).unwrap();
    stale.vote(6, 10, 1, &4, Some(&9)).unwrap();
    assert_eq!(stale.chosen(10), None);
    stale.vote(6, 11, 1, &4, Some(&4)).unwrap();
    assert_eq!(stale.chosen(11), Some(&4));
    stale.committed(13);
    stale.term(7, &[5], &[], fast).unwrap();
    stale.vote(7, 13, 5, &5, None).unwrap();
    assert_eq!(stale.chosen(13), None);
    stale.vote(7, 14, 5, &5, None).unwrap();
    assert_eq!(stale.chosen(14), Some(&5));
}

/// A device: term, vote, commit, a snapshot through `start` of term `start_term`, and the log's
/// entries' terms and values above it.
struct Device {
    term: u64,
    vote: u64,
    commit: u64,
    start: u64,
    start_term: u64,
    entries: Vec<(u64, u8)>,
}

impl DurableView<u8> for Device {
    fn term(&self) -> u64 {
        self.term
    }
    fn vote(&self) -> u64 {
        self.vote
    }
    fn commit(&self) -> u64 {
        self.commit
    }
    fn start(&self) -> u64 {
        self.start
    }
    fn last(&self) -> u64 {
        self.start + self.entries.len() as u64
    }
    fn term_at(&self, index: u64) -> Option<u64> {
        if index == self.start {
            return Some(self.start_term);
        }
        let at = index.checked_sub(self.start + 1)?;
        self.entries.get(at as usize).map(|(term, _)| *term)
    }
    fn holds(&self, index: u64, value: &u8) -> bool {
        index <= self.start
            || index
                .checked_sub(self.start + 1)
                .and_then(|at| self.entries.get(at as usize))
                .is_some_and(|(_, held)| held == value)
    }
}

fn device(term: u64, vote: u64, commit: u64, entries: &[(u64, u8)]) -> Device {
    Device {
        term,
        vote,
        commit,
        start: 0,
        start_term: 0,
        entries: entries.to_vec(),
    }
}

fn message(from: u64, to: u64, term: Option<u64>, says: Says<'_, u8>) -> Released<'_, u8> {
    Released {
        from,
        to,
        term,
        says,
        commit: None,
    }
}

fn rule(violation: Result<(), Violation>) -> Option<Rule> {
    match violation {
        Err(Violation::Durability { rule, .. }) => Some(rule),
        Ok(()) => None,
        Err(other) => panic!("{other}"),
    }
}

#[test]
fn durability_holds_every_output_to_the_senders_device() {
    let mut oracle = Durability::default();
    let disk = device(3, 1, 2, &[(1, 10), (2, 11), (3, 12)]);
    // Allowed: a vote request of the device's term and vote naming a last entry it holds; a vote
    // given as the device records it; an acknowledgement it holds; a pre-vote; an older term.
    let request = Says::VoteRequest {
        last_index: 3,
        last_term: 3,
    };
    assert_eq!(
        rule(oracle.released(&disk, &message(1, 2, Some(3), request))),
        None
    );
    assert_eq!(
        rule(oracle.released(
            &device(3, 2, 0, &[]),
            &message(1, 2, Some(3), Says::Vote { granted: true })
        )),
        None
    );
    let ack = Says::Acknowledges { index: 3 };
    assert_eq!(
        rule(oracle.released(&disk, &message(1, 2, Some(3), ack))),
        None
    );
    assert_eq!(
        rule(oracle.released(&disk, &message(1, 2, None, Says::Nothing))),
        None
    );
    assert_eq!(
        rule(oracle.released(
            &disk,
            &message(1, 2, Some(2), Says::Acknowledges { index: 9 })
        )),
        None
    );
    // Planted (I1): a message of a term the device does not hold.
    assert_eq!(
        rule(oracle.released(&disk, &message(1, 2, Some(4), Says::Nothing))),
        Some(Rule::I1)
    );
    // Planted (I1): a vote request before the device holds the member's own vote.
    let unvoted = device(3, 0, 2, &[(1, 10)]);
    let request = Says::VoteRequest {
        last_index: 1,
        last_term: 1,
    };
    assert_eq!(
        rule(oracle.released(&unvoted, &message(1, 2, Some(3), request))),
        Some(Rule::I1)
    );
    // Planted (I1): a request naming a last entry the device does not hold.
    let request = Says::VoteRequest {
        last_index: 4,
        last_term: 3,
    };
    assert_eq!(
        rule(oracle.released(&disk, &message(1, 2, Some(3), request))),
        Some(Rule::I1)
    );
    // Planted (I1): a vote given before it is durable.
    assert_eq!(
        rule(oracle.released(&disk, &message(1, 2, Some(3), Says::Vote { granted: true }))),
        Some(Rule::I1)
    );
    // Planted (I2): an acknowledgement past what the device holds.
    let ack = Says::Acknowledges { index: 4 };
    assert_eq!(
        rule(oracle.released(&disk, &message(1, 2, Some(3), ack))),
        Some(Rule::I2)
    );
    // Planted (I2): the fast track's word of an entry not held.
    let held = [(5u64, 7u8)];
    assert_eq!(
        rule(oracle.released(
            &disk,
            &message(1, 2, Some(3), Says::Holds { entries: &held })
        )),
        Some(Rule::I2)
    );
    // Planted (R-6): an answer stating a commit past the durable one.
    let mut answer = message(1, 2, Some(3), Says::Acknowledges { index: 3 });
    answer.commit = Some(3);
    assert_eq!(rule(oracle.released(&disk, &answer)), Some(Rule::R6));
}

#[test]
fn durability_holds_commits_applies_writes_and_starts() {
    let mut oracle = Durability::default();
    let disks = BTreeMap::from([
        (1u64, device(2, 1, 0, &[(1, 10), (2, 11)])),
        (2, device(2, 1, 0, &[(1, 10), (2, 11)])),
        (3, device(2, 1, 0, &[(1, 10)])),
        (4, device(2, 1, 0, &[])),
        (5, device(2, 1, 0, &[])),
    ]);
    let view = |member: u64| disks.get(&member);
    let three: &[&[u64]] = &[&[1, 2, 3]];
    let five: &[&[u64]] = &[&[1, 2, 3, 4, 5]];
    oracle.committed(1, 2, &11, &[three], view).unwrap();
    // Either configuration that decided it will do; a joint one needs both halves.
    oracle.committed(1, 2, &11, &[five, three], view).unwrap();
    let joint: &[&[u64]] = &[&[1, 2, 3], &[3, 4, 5]];
    // Planted (I3): committed where no majority of the configuration holds it.
    assert_eq!(
        rule(oracle.committed(1, 2, &11, &[five], view)),
        Some(Rule::I3)
    );
    assert_eq!(
        rule(oracle.committed(1, 2, &11, &[joint], view)),
        Some(Rule::I3)
    );
    // Planted (I3): a leader counting itself for what its device lacks.
    assert_eq!(
        rule(oracle.counted_self(3, &disks[&3], 2, &11)),
        Some(Rule::I3)
    );
    oracle.counted_self(1, &disks[&1], 2, &11).unwrap();

    // I4: applied once committed, and durable here, but a leader's own ahead of its write.
    oracle.applied(1, &disks[&1], 2, &11, 2, false).unwrap();
    oracle.applied(3, &disks[&3], 2, &11, 2, true).unwrap();
    assert_eq!(
        rule(oracle.applied(3, &disks[&3], 2, &11, 2, false)),
        Some(Rule::I4)
    );
    assert_eq!(
        rule(oracle.applied(1, &disks[&1], 2, &11, 1, false)),
        Some(Rule::I4)
    );

    // I5: a change applied only under a durable commit, or what the state machine holds.
    let committed = device(2, 1, 2, &[(1, 10), (2, 11)]);
    oracle.fenced(1, &committed, 2, 0).unwrap();
    oracle.fenced(1, &disks[&1], 2, 2).unwrap();
    assert_eq!(rule(oracle.fenced(1, &disks[&1], 2, 1)), Some(Rule::I5));

    // I7: a durable commit of entries held.
    oracle.written(1, &committed).unwrap();
    assert_eq!(
        rule(oracle.written(1, &device(2, 1, 3, &[(1, 10)]))),
        Some(Rule::I7)
    );

    // I8: the log starts no later than the state machine's durable point.
    let mut compacted = device(2, 1, 2, &[]);
    compacted.start = 2;
    compacted.start_term = 1;
    oracle.start(1, &compacted, 2).unwrap();
    assert_eq!(rule(oracle.start(1, &compacted, 1)), Some(Rule::I8));
}

#[test]
fn read_safety_holds_a_read_to_the_commit_known_when_it_was_asked() {
    let mut oracle = ReadSafety::new(16);
    oracle.committed(4);
    oracle.asked(1).unwrap();
    oracle.committed(6);
    oracle.asked(2).unwrap();
    oracle.answered(1, 1, 4).unwrap();
    oracle.answered(1, 2, 7).unwrap();
    assert_eq!(oracle.answers(), 2);
    // Planted: answered below the commit known when asked.
    oracle.asked(3).unwrap();
    assert_eq!(
        oracle.answered(2, 3, 5),
        Err(Violation::StaleRead {
            member: 2,
            read: 3,
            index: 5,
            floor: 6
        })
    );
    // Planted: an answer to a read nobody asked.
    assert!(matches!(
        oracle.answered(2, 9, 9),
        Err(Violation::UnaskedRead { .. })
    ));
}

#[test]
fn exactly_once_holds_a_write_to_one_index_and_every_member() {
    let mut oracle = ExactlyOnce::new(16);
    oracle.applied(1, 5, 100).unwrap();
    oracle.applied(2, 5, 100).unwrap();
    // Again at the same index, as a member restarted from an older snapshot applies it.
    oracle.applied(1, 5, 100).unwrap();
    oracle.acknowledged(100).unwrap();
    oracle.installed(3, 6).unwrap();
    oracle.settled(&[1, 2, 3]).unwrap();
    // Planted: a write applied at a second index (a retry taking effect twice).
    assert_eq!(
        oracle.applied(2, 9, 100),
        Err(Violation::AppliedTwice {
            member: 2,
            write: 100,
            first: 5,
            second: 9
        })
    );
    // Planted: an acknowledged write a member of the settled group never applied.
    assert_eq!(
        oracle.settled(&[1, 2, 3, 4]),
        Err(Violation::NeverApplied {
            member: 4,
            write: 100
        })
    );
    oracle.acknowledged(101).unwrap();
    assert!(oracle.settled(&[1]).is_err());
}

#[test]
fn same_history_holds_each_index_to_one_state() {
    let mut oracle = SameHistory::new(16);
    oracle.reached(1, 1, 0xa).unwrap();
    oracle.reached(2, 1, 0xa).unwrap();
    oracle.reached(1, 2, 0xb).unwrap();
    // Planted: a member whose state machine reached another state from the same entries.
    assert_eq!(
        oracle.reached(2, 2, 0xc),
        Err(Violation::HistoriesDiffer {
            member: 2,
            index: 2
        })
    );
}

#[test]
fn every_violation_names_its_oracle() {
    let named = [
        (
            Violation::TwoLeaders {
                term: 1,
                first: 1,
                second: 2,
            },
            "Election Safety",
        ),
        (
            Violation::TermsDecrease {
                member: 1,
                index: 1,
                before: 2,
                term: 1,
            },
            "Log Matching",
        ),
        (
            Violation::LeaderLacks {
                member: 1,
                term: 1,
                index: 1,
            },
            "Leader Completeness",
        ),
        (
            Violation::TwoCommitted {
                member: 1,
                index: 1,
            },
            "State Machine Safety",
        ),
        (Violation::TwoChosen { index: 1, term: 1 }, "Fast agreement"),
        (
            Violation::Durability {
                member: 1,
                rule: Rule::I1,
                at: 1,
                held: 0,
            },
            "Durability",
        ),
        (
            Violation::StaleRead {
                member: 1,
                read: 1,
                index: 1,
                floor: 2,
            },
            "Read safety",
        ),
        (
            Violation::AppliedTwice {
                member: 1,
                write: 1,
                first: 1,
                second: 2,
            },
            "Exactly once",
        ),
        (
            Violation::HistoriesDiffer {
                member: 1,
                index: 1,
            },
            "Same history",
        ),
    ];
    for (violation, oracle) in named {
        assert_eq!(violation.oracle(), oracle);
        assert!(violation.to_string().starts_with(oracle));
    }
}
