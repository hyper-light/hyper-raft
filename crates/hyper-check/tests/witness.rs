//! The witness checker against focal's cases (`crates/focal-sim/src/history.rs` at focal
//! `origin/slates-port` `8d4f322`, and the cases `focal-ledger`'s `native_session_tests.rs` and
//! `focal-node`'s `history_black_box.rs` build on it), each carried with its meaning onto the
//! generic checker over a model of focal's ledger; and against the checks the generalization adds.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::disallowed_macros,
    missing_docs
)]

use hyper_check::witness::{
    Consistency, Event, HistoryError, Initial, Outcome, Placed, Report, Request, Tracing, check,
};
use hyper_check::{Access, Answer, Model, Register};

/// focal's ledger, as much of it as its checker's cases use: a ledger's sequence and state hash; a
/// command names its ledger, its request and its command hash, and is answered with a receipt
/// (focal's `MutationReceipt`); a read answers the sequence and hash it observed.
struct Ledger;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct State {
    sequence: u64,
    hash: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Input {
    Command {
        ledger: u8,
        key: u8,
        command_hash: [u8; 32],
    },
    Read,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Output {
    Receipt {
        ledger: u8,
        key: u8,
        sequence: u64,
        command_hash: [u8; 32],
    },
    Read {
        sequence: u64,
        hash: [u8; 32],
    },
}

impl Model for Ledger {
    type State = State;
    type Input = Input;
    type Output = Output;

    fn init(&self) -> State {
        State {
            sequence: 0,
            hash: [0; 32],
        }
    }

    fn apply(&self, state: &State, input: &Input) -> (Output, State) {
        match input {
            Input::Command {
                ledger,
                key,
                command_hash,
            } => {
                let sequence = state.sequence + 1;
                let mut hash = state.hash;
                for (byte, command) in hash.iter_mut().zip(command_hash) {
                    *byte = byte.wrapping_mul(31).wrapping_add(*command);
                }
                hash[0] = hash[0].wrapping_add(sequence.to_le_bytes()[0]);
                let receipt = Output::Receipt {
                    ledger: *ledger,
                    key: *key,
                    sequence,
                    command_hash: *command_hash,
                };
                (receipt, State { sequence, hash })
            }
            Input::Read => (
                Output::Read {
                    sequence: state.sequence,
                    hash: state.hash,
                },
                state.clone(),
            ),
        }
    }

    /// A receipt answers only the command it names (focal's `ResponseMismatch`: "response belongs
    /// to another request or command").
    fn answers(&self, input: &Input, output: &Output) -> bool {
        match (input, output) {
            (
                Input::Command {
                    ledger,
                    key,
                    command_hash,
                },
                Output::Receipt {
                    ledger: l,
                    key: k,
                    command_hash: c,
                    ..
                },
            ) => ledger == l && key == k && command_hash == c,
            (Input::Read, Output::Read { .. }) => true,
            _ => false,
        }
    }
}

type E = Event<u8, u8, Input, Output>;

const LEDGER: u8 = 1;
const KEY: u8 = 1;

fn initial() -> Vec<Initial<u8, State>> {
    vec![Initial {
        object: LEDGER,
        sequence: 0,
        state: Ledger.init(),
    }]
}

fn command(key: u8) -> Input {
    Input::Command {
        ledger: LEDGER,
        key,
        command_hash: [key; 32],
    }
}

/// The receipt the model gives `key`'s command as the `sequence`-th of the ledger.
fn receipt(key: u8, sequence: u64) -> Output {
    Output::Receipt {
        ledger: LEDGER,
        key,
        sequence,
        command_hash: [key; 32],
    }
}

fn invocation(call: u64, key: u8) -> E {
    Event::Invoke {
        call,
        request: Request::Mutation {
            object: LEDGER,
            key,
            input: command(key),
        },
    }
}

fn publish(key: u8, sequence: u64) -> E {
    Event::Publish {
        object: LEDGER,
        sequence,
        key,
        input: command(key),
    }
}

fn committed(call: u64, key: u8, sequence: u64) -> E {
    Event::Complete {
        call,
        outcome: Outcome::Committed {
            output: receipt(key, sequence),
        },
    }
}

fn read(call: u64, consistency: Consistency) -> E {
    Event::Invoke {
        call,
        request: Request::Read {
            object: LEDGER,
            input: Input::Read,
            consistency,
        },
    }
}

fn read_back(call: u64, sequence: u64, hash: [u8; 32]) -> E {
    Event::Complete {
        call,
        outcome: Outcome::Read {
            sequence,
            output: Output::Read { sequence, hash },
        },
    }
}

/// focal's checker accepted any publication: its histories are partially traced.
fn focal(events: &[E], max_events: usize) -> Result<Report, HistoryError> {
    check(&Ledger, &initial(), events, max_events, Tracing::Partial)
        .map(|witnessed| witnessed.report)
        .map_err(|refused| refused.error)
}

#[test]
fn lost_reply_retry_is_one_commit_and_concurrent_read_can_observe_earlier_prefix() {
    let events = vec![
        invocation(1, KEY),
        read(2, Consistency::Linearizable),
        publish(KEY, 1),
        Event::Complete {
            call: 1,
            outcome: Outcome::Unknown,
        },
        read_back(2, 0, [0; 32]),
        invocation(3, KEY),
        committed(3, KEY, 1),
    ];
    assert_eq!(
        focal(&events, 20).unwrap(),
        Report {
            publications: 1,
            reads: 1,
            retries: 1,
            unknown: 1,
            pending: 0,
            untraced: 0,
        }
    );
    // Every attempt traced: the publication fell within the first attempt.
    assert!(check(&Ledger, &initial(), &events, 20, Tracing::Complete).is_ok());
}

#[test]
fn speculative_success_and_stale_linearizable_read_are_rejected() {
    assert_eq!(
        focal(&[invocation(1, KEY), committed(1, KEY, 1)], 10),
        Err(HistoryError::PrematureSuccess)
    );
    let events = [
        publish(KEY, 1),
        read(1, Consistency::Linearizable),
        read_back(1, 0, [0; 32]),
    ];
    assert_eq!(focal(&events, 10), Err(HistoryError::StaleRead));
}

#[test]
fn a_history_over_its_event_budget_is_capacity() {
    assert_eq!(focal(&[invocation(1, KEY)], 0), Err(HistoryError::Capacity));
}

#[test]
fn a_reused_call_id_is_identity() {
    assert_eq!(
        focal(&[invocation(1, KEY), invocation(1, KEY)], 20),
        Err(HistoryError::Identity)
    );
}

#[test]
fn completing_a_call_that_never_invoked_is_identity() {
    let events = [Event::Complete {
        call: 7,
        outcome: Outcome::Refused,
    }];
    assert_eq!(focal(&events, 20), Err(HistoryError::Identity));
}

#[test]
fn an_invocation_on_an_unknown_ledger_is_identity() {
    let events = [Event::Invoke {
        call: 1,
        request: Request::Read {
            object: 9,
            input: Input::Read,
            consistency: Consistency::Linearizable,
        },
    }];
    assert_eq!(focal(&events, 20), Err(HistoryError::Identity));
}

#[test]
fn a_publication_that_skips_a_sequence_is_prefix() {
    assert_eq!(focal(&[publish(KEY, 2)], 20), Err(HistoryError::Prefix));
}

#[test]
fn two_publications_of_one_request_are_a_duplicate_commit() {
    assert_eq!(
        focal(&[publish(KEY, 1), publish(KEY, 2)], 20),
        Err(HistoryError::DuplicateCommit)
    );
}

#[test]
fn a_receipt_that_contradicts_its_request_is_a_response_mismatch() {
    let wrong = Output::Receipt {
        ledger: LEDGER,
        key: KEY,
        sequence: 1,
        command_hash: [9; 32],
    };
    let events = [
        invocation(1, KEY),
        Event::Complete {
            call: 1,
            outcome: Outcome::Committed { output: wrong },
        },
    ];
    assert_eq!(focal(&events, 20), Err(HistoryError::ResponseMismatch));
}

#[test]
fn a_committed_outcome_for_a_read_is_a_response_mismatch() {
    let events = [read(1, Consistency::Linearizable), committed(1, KEY, 1)];
    assert_eq!(focal(&events, 20), Err(HistoryError::ResponseMismatch));
}

#[test]
fn a_read_whose_hash_does_not_match_its_prefix_is_a_state_mismatch() {
    let events = [read(1, Consistency::Exact(0)), read_back(1, 0, [42; 32])];
    assert_eq!(focal(&events, 20), Err(HistoryError::StateMismatch));
}

/// focal-ledger's `recovered_prefix_is_a_linearizable_publication_history`: two requests in flight,
/// both published, both answered; one request published at two sequences is a duplicate commit;
/// an answer before the publication is premature.
#[test]
fn a_recovered_prefix_is_a_linearizable_publication_history() {
    let events = vec![
        invocation(0, 1),
        invocation(1, 2),
        publish(1, 1),
        publish(2, 2),
        committed(0, 1, 1),
        committed(1, 2, 2),
    ];
    let report = focal(&events, 64).unwrap();
    assert_eq!(
        (report.publications, report.reads, report.retries),
        (2, 0, 0)
    );
    assert_eq!((report.unknown, report.pending), (0, 0));
    assert_eq!(
        focal(&[publish(1, 1), publish(1, 2)], 64),
        Err(HistoryError::DuplicateCommit)
    );
    assert_eq!(
        focal(&[invocation(0, 1), committed(0, 1, 1)], 64),
        Err(HistoryError::PrematureSuccess)
    );
}

/// focal-node's black-box histories: clients' creates interleaved, each published before the
/// answer that observes it; with a publication dropped, the answer that needed it is refused; a
/// lost reply retried commits once, and a second publication of its request is a duplicate.
#[test]
fn black_box_histories_hold_and_their_tamperings_are_refused() {
    let mut events = Vec::new();
    for (call, key) in [(0u64, 1u8), (1, 3), (2, 2), (3, 4)] {
        let sequence = call + 1;
        events.push(invocation(call, key));
        events.push(publish(key, sequence));
        events.push(committed(call, key, sequence));
    }
    let report = focal(&events, 256).unwrap();
    assert_eq!(
        (report.publications, report.unknown, report.pending),
        (4, 0, 0)
    );
    let tampered: Vec<E> = events
        .iter()
        .filter(|event| !matches!(event, Event::Publish { sequence: 4, .. }))
        .cloned()
        .collect();
    assert!(focal(&tampered, 256).is_err());

    let mut retried = vec![
        invocation(0, 1),
        publish(1, 1),
        Event::Complete {
            call: 0,
            outcome: Outcome::Unknown,
        },
        invocation(1, 1),
        committed(1, 1, 1),
        invocation(2, 2),
        publish(2, 2),
        committed(2, 2, 2),
    ];
    let report = focal(&retried, 256).unwrap();
    assert_eq!(
        (report.publications, report.unknown, report.retries),
        (2, 1, 1)
    );
    retried.push(publish(1, 3));
    assert_eq!(focal(&retried, 256), Err(HistoryError::DuplicateCommit));
}

// --- What the generalization adds ---------------------------------------------------------------

fn complete(events: &[E]) -> Result<Report, HistoryError> {
    check(&Ledger, &initial(), events, 64, Tracing::Complete)
        .map(|witnessed| witnessed.report)
        .map_err(|refused| refused.error)
}

/// Every client traced: a request nobody asked is not published.
#[test]
fn under_complete_tracing_a_publication_nobody_asked_is_refused() {
    assert_eq!(complete(&[publish(KEY, 1)]), Err(HistoryError::Unasked));
}

/// A publication falls while an attempt of its request may take effect.
#[test]
fn a_publication_with_no_attempt_under_way_is_refused() {
    let refused_first = [
        invocation(1, KEY),
        Event::Complete {
            call: 1,
            outcome: Outcome::Refused,
        },
        publish(KEY, 1),
    ];
    assert_eq!(complete(&refused_first), Err(HistoryError::Unattributed));
    let answered_first = [invocation(1, KEY), committed(1, KEY, 1), publish(KEY, 1)];
    assert_eq!(
        complete(&answered_first),
        Err(HistoryError::PrematureSuccess)
    );
    // An attempt answered unknown may take effect any time after.
    let unknown_first = [
        invocation(1, KEY),
        Event::Complete {
            call: 1,
            outcome: Outcome::Unknown,
        },
        publish(KEY, 1),
    ];
    assert!(complete(&unknown_first).is_ok());
}

/// An attempt answered refused took no effect: the publication that fell while it alone ran is
/// a refusal that lied.
#[test]
fn a_refused_attempt_that_took_effect_is_refused() {
    let lied = [
        invocation(1, KEY),
        publish(KEY, 1),
        Event::Complete {
            call: 1,
            outcome: Outcome::Refused,
        },
    ];
    assert_eq!(complete(&lied), Err(HistoryError::RefusedButPublished));
    // With an earlier attempt pending for good, the publication is that attempt's.
    let earlier = [
        invocation(1, KEY),
        Event::Complete {
            call: 1,
            outcome: Outcome::Unknown,
        },
        invocation(2, KEY),
        publish(KEY, 1),
        Event::Complete {
            call: 2,
            outcome: Outcome::Refused,
        },
    ];
    assert!(complete(&earlier).is_ok());
}

/// A retry carries its request's command, and the system publishes the command asked.
#[test]
fn a_request_is_one_command() {
    let other = [
        invocation(1, KEY),
        Event::Publish {
            object: LEDGER,
            sequence: 1,
            key: KEY,
            input: command(2),
        },
    ];
    assert_eq!(complete(&other), Err(HistoryError::ResponseMismatch));
    let retry_other = [
        invocation(1, KEY),
        Event::Invoke {
            call: 2,
            request: Request::Mutation {
                object: LEDGER,
                key: KEY,
                input: command(2),
            },
        },
    ];
    assert_eq!(complete(&retry_other), Err(HistoryError::ResponseMismatch));
}

/// A read changes nothing: a "read" whose input the model says writes is refused.
#[test]
fn a_read_that_writes_is_refused() {
    let register = Register::<u8>::new();
    let initial = [Initial {
        object: 0u8,
        sequence: 0,
        state: None,
    }];
    let events: Vec<Event<u8, u8, Access<u8>, Answer<u8>>> = vec![
        Event::Invoke {
            call: 1,
            request: Request::Read {
                object: 0,
                input: Access::Write(5),
                consistency: Consistency::Linearizable,
            },
        },
        Event::Complete {
            call: 1,
            outcome: Outcome::Read {
                sequence: 0,
                output: Answer::Written,
            },
        },
    ];
    let refused = check(&register, &initial, &events, 8, Tracing::Complete).unwrap_err();
    assert_eq!(refused.error, HistoryError::MutatingRead);
}

/// A history passed exhibits its order: each ledger's publications in sequence, each read after
/// the publication it observed.
#[test]
fn a_passed_history_exhibits_its_order() {
    let hash_after_one = Ledger.apply(&Ledger.init(), &command(1)).1.hash;
    let events = vec![
        read(10, Consistency::Linearizable),
        invocation(1, 1),
        read_back(10, 0, [0; 32]),
        read(11, Consistency::Linearizable),
        publish(1, 1),
        committed(1, 1, 1),
        read_back(11, 1, hash_after_one),
        invocation(2, 2),
        publish(2, 2),
        committed(2, 2, 2),
    ];
    let witnessed = check(&Ledger, &initial(), &events, 64, Tracing::Complete).unwrap();
    assert_eq!(
        witnessed.order[&LEDGER],
        vec![
            Placed::Read(10),
            Placed::Mutation(1),
            Placed::Read(11),
            Placed::Mutation(2)
        ]
    );
    // A read answered before the commit it must see is refused, and one past the latest too.
    let stale = vec![
        invocation(1, 1),
        publish(1, 1),
        committed(1, 1, 1),
        read(10, Consistency::Linearizable),
        read_back(10, 0, [0; 32]),
    ];
    assert_eq!(complete(&stale), Err(HistoryError::StaleRead));
    let future = vec![
        read(10, Consistency::Linearizable),
        read_back(10, 1, [0; 32]),
    ];
    assert_eq!(complete(&future), Err(HistoryError::StaleRead));
}

/// A request its client was told failed for good is never published, before or after.
#[test]
fn a_request_told_failed_is_never_published() {
    let failed = |call| Event::Complete {
        call,
        outcome: Outcome::Failed,
    };
    let after = [invocation(1, KEY), publish(KEY, 1), failed(1)];
    assert_eq!(complete(&after), Err(HistoryError::FailedButPublished));
    let before = [invocation(1, KEY), failed(1), publish(KEY, 1)];
    assert_eq!(complete(&before), Err(HistoryError::FailedButPublished));
    let never = [
        invocation(1, KEY),
        failed(1),
        invocation(2, 2),
        publish(2, 1),
    ];
    assert!(complete(&never).is_ok());
}
