//! The search checker against histories whose verdict is known: mantle's cases
//! (`crates/range/tests/linear.rs` at mantle `origin/dev` `21ae613`), every history of up to three
//! operations on a small domain, and seeded histories of up to eight, each held to an independent
//! search (Wing and Gong's tree search, as Lowe's Figure 1 states it, over sets) and every order it
//! exhibits held to the definition.
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

use std::hash::Hash;

use hyper_check::search::{Budget, Print, Searched, Spent, Verdict, search, search_with};
use hyper_check::{Access, Answer, Model, Operation, Register, search_partitions, verify};
use hyper_sim::Seeded;

type Op<V> = Operation<Access<V>, Answer<V>>;

fn put(call: u64, ret: Option<u64>, value: &str) -> Op<String> {
    Operation {
        call,
        ret,
        input: Access::Write(value.into()),
        output: ret.map(|_| Answer::Written),
    }
}

fn get(call: u64, ret: u64, seen: Option<&str>) -> Op<String> {
    Operation {
        call,
        ret: Some(ret),
        input: Access::Read,
        output: Some(Answer::Read(seen.map(Into::into))),
    }
}

/// mantle's budget for its cases, a million steps, is a memory budget here: these histories hold a
/// handful of configurations.
fn verdict(ops: &[Op<String>]) -> Verdict<Option<String>, Answer<String>> {
    let register = Register::<String>::new();
    let found = search(&register, ops, &Budget::default()).unwrap();
    if let Verdict::Linearizable { order } = &found.verdict {
        verify(&register, ops, order).unwrap();
    }
    found.verdict
}

fn linearizable(ops: &[Op<String>]) -> bool {
    matches!(verdict(ops), Verdict::Linearizable { .. })
}

fn refused(ops: &[Op<String>]) -> bool {
    matches!(verdict(ops), Verdict::NotLinearizable(_))
}

/// mantle's `sequential_and_overlapping_histories_that_fit`.
#[test]
fn sequential_and_overlapping_histories_that_fit() {
    assert!(linearizable(&[]));
    let sequential = [
        put(1, Some(2), "a"),
        get(3, 4, Some("a")),
        put(5, Some(6), "b"),
        get(7, 8, Some("b")),
    ];
    assert!(linearizable(&sequential));
    // A get overlapping a put may see it or not.
    for seen in [None, Some("a")] {
        let overlapping = [put(1, Some(5), "a"), get(2, 3, seen)];
        assert!(linearizable(&overlapping), "{seen:?}");
    }
    // Two overlapping puts in either order, as the gets that follow show.
    let racing = [
        put(1, Some(4), "a"),
        put(2, Some(3), "b"),
        get(5, 6, Some("a")),
        get(7, 8, Some("a")),
    ];
    assert!(linearizable(&racing));
    // A put that never returned may have taken effect.
    let pending = [put(1, None, "c"), get(5, 6, Some("c"))];
    assert!(linearizable(&pending));
    let pending_unseen = [put(1, None, "c"), get(5, 6, None)];
    assert!(linearizable(&pending_unseen));
}

/// mantle's `histories_that_do_not_fit_are_refused`.
#[test]
fn histories_that_do_not_fit_are_refused() {
    // A get after a finished put must see it.
    let stale = [put(1, Some(2), "a"), get(3, 4, None)];
    assert!(refused(&stale));
    // A later put finished before the get began, so it must see the later value.
    let lost = [
        put(1, Some(2), "a"),
        put(3, Some(4), "b"),
        get(5, 6, Some("a")),
    ];
    assert!(refused(&lost));
    // Two gets in real-time order cannot see the writes in opposite orders.
    let flipped = [
        put(1, Some(10), "a"),
        put(1, Some(10), "b"),
        get(2, 3, Some("a")),
        get(4, 5, Some("b")),
        get(6, 7, Some("a")),
    ];
    assert!(refused(&flipped));
    // A value nobody wrote.
    let invented = [get(1, 2, Some("z"))];
    assert!(refused(&invented));
}

/// Lowe §3's counterexample: the longest prefix, the operation that could not follow it, the state
/// there and what it could have answered.
#[test]
fn a_refusal_says_where_no_order_could_go_on() {
    let lost = [
        put(1, Some(2), "a"),
        put(3, Some(4), "b"),
        get(5, 6, Some("a")),
    ];
    let Verdict::NotLinearizable(counterexample) = verdict(&lost) else {
        panic!("refused");
    };
    assert_eq!(counterexample.prefix, vec![0, 1]);
    assert_eq!(counterexample.stuck, 2);
    assert_eq!(counterexample.state, Some("b".to_string()));
    assert_eq!(
        counterexample.legal,
        vec![Answer::Read(Some("b".to_string()))]
    );
    assert!(counterexample.pending.is_empty());
    // At the deepest point every operation that could take effect has: what is left pending
    // there is what no order could take either, a read that saw a value never current.
    let pending = [
        put(1, Some(2), "a"),
        put(3, Some(4), "c"),
        get(3, 9, Some("y")),
        get(5, 6, Some("z")),
    ];
    let Verdict::NotLinearizable(counterexample) = verdict(&pending) else {
        panic!("refused");
    };
    assert_eq!(counterexample.prefix, vec![0, 1]);
    assert_eq!(counterexample.stuck, 3);
    assert_eq!(counterexample.pending, vec![2]);
    assert_eq!(
        counterexample.legal,
        vec![Answer::Read(Some("c".to_string()))]
    );
}

/// An order exhibited is checked against the definition, and a wrong one is refused by it.
#[test]
fn verify_refuses_what_is_not_a_linearization() {
    let register = Register::<String>::new();
    let ops = [put(1, Some(2), "a"), get(3, 4, Some("a"))];
    verify(&register, &ops, &[0, 1]).unwrap();
    assert!(verify(&register, &ops, &[1, 0]).is_err());
    assert!(verify(&register, &ops, &[0]).is_err());
    assert!(verify(&register, &ops, &[0, 0, 1]).is_err());
    assert!(verify(&register, &ops, &[0, 1, 2]).is_err());
    // A pending put may be left out or put anywhere after its call.
    let pending = [put(1, None, "c"), get(5, 6, None)];
    verify(&register, &pending, &[1]).unwrap();
    verify(&register, &pending, &[1, 0]).unwrap();
    assert!(verify(&register, &pending, &[0, 1]).is_err());
}

// --- The independent search -------------------------------------------------------------------

/// Wing and Gong's tree search as Lowe's Figure 1 states it, over sets and with no list, memo or
/// order of trying: whether some order of the operations not yet placed, each one minimal (no
/// completed operation left returned before its call), gives every output seen. A pending
/// operation may stay out.
fn brute<M: Model>(model: &M, ops: &[Operation<M::Input, M::Output>]) -> bool {
    fn go<M: Model>(
        model: &M,
        ops: &[Operation<M::Input, M::Output>],
        placed: &mut Vec<bool>,
        state: &M::State,
    ) -> bool {
        let done = ops
            .iter()
            .zip(placed.iter())
            .all(|(op, placed)| *placed || op.ret.is_none());
        if done {
            return true;
        }
        for at in 0..ops.len() {
            if placed[at] {
                continue;
            }
            let op = &ops[at];
            let blocked = ops.iter().enumerate().any(|(other, earlier)| {
                other != at && !placed[other] && earlier.ret.is_some_and(|ret| ret < op.call)
            });
            if blocked {
                continue;
            }
            let (output, next) = model.apply(state, &op.input);
            if op.output.as_ref().is_some_and(|seen| *seen != output) {
                continue;
            }
            placed[at] = true;
            if go(model, ops, placed, &next) {
                return true;
            }
            placed[at] = false;
        }
        false
    }
    let mut placed = vec![false; ops.len()];
    go(model, ops, &mut placed, &model.init())
}

/// The search's verdict on `ops` is the brute force's, and its order is a linearization.
fn agrees<V: Clone + Eq + Hash + std::fmt::Debug>(ops: &[Op<V>]) -> Searched<Option<V>, Answer<V>> {
    let register = Register::<V>::new();
    let found = search(&register, ops, &Budget::default()).unwrap();
    let expected = brute(&register, ops);
    match &found.verdict {
        Verdict::Linearizable { order } => {
            assert!(expected, "the search passed what has no order: {ops:?}");
            verify(&register, ops, order).unwrap_or_else(|error| panic!("{error}: {ops:?}"));
        }
        Verdict::NotLinearizable(counterexample) => {
            assert!(!expected, "the search refused what has an order: {ops:?}");
            assert!(counterexample.stuck < ops.len());
            assert!(ops[counterexample.stuck].ret.is_some());
        }
        Verdict::Unknown(spent) => panic!("{spent:?}: {ops:?}"),
    }
    found
}

/// The operations of the small domain: a write of 1 or 2, or a read that saw nothing, 1 or 2.
fn kind(which: u8) -> (Access<u8>, Answer<u8>) {
    match which {
        0 => (Access::Write(1), Answer::Written),
        1 => (Access::Write(2), Answer::Written),
        2 => (Access::Read, Answer::Read(None)),
        3 => (Access::Read, Answer::Read(Some(1))),
        _ => (Access::Read, Answer::Read(Some(2))),
    }
}

/// Every interval over the times `0..=3`, a call at or before its return, and a write's also with
/// no return.
fn intervals(write: bool) -> Vec<(u64, Option<u64>)> {
    let mut all = Vec::new();
    for call in 0..=3 {
        for ret in call..=3 {
            all.push((call, Some(ret)));
        }
        if write {
            all.push((call, None));
        }
    }
    all
}

/// Every history of one to three operations of the small domain over every placement of their
/// intervals, writes pending or not (each value written by more than one write too, which no unique
/// value disambiguates): an operation is one of two writes, each over 14 placements (10 intervals in
/// `0..=3` and 4 pending), or one of three reads over 10, so 58 a place and `58 + 58² + 58³`
/// histories, the search's verdict the brute force's on each.
#[test]
fn every_small_history_is_judged_as_the_tree_search_judges_it() {
    let mut histories = 0u64;
    let (mut passed, mut refused) = (0u64, 0u64);
    for len in 1..=3usize {
        let kinds = 5u32.pow(len as u32);
        for code in 0..kinds {
            let mut code = code;
            let shape: Vec<u8> = (0..len)
                .map(|_| {
                    let which = (code % 5) as u8;
                    code /= 5;
                    which
                })
                .collect();
            let choices: Vec<Vec<(u64, Option<u64>)>> =
                shape.iter().map(|which| intervals(*which < 2)).collect();
            let mut at = vec![0usize; len];
            loop {
                let ops: Vec<Op<u8>> = shape
                    .iter()
                    .zip(&at)
                    .zip(&choices)
                    .map(|((which, at), choices)| {
                        let (input, output) = kind(*which);
                        let (call, ret) = choices[*at];
                        Operation {
                            call,
                            ret,
                            input,
                            output: ret.map(|_| output),
                        }
                    })
                    .collect();
                match agrees(&ops).verdict {
                    Verdict::Linearizable { .. } => passed += 1,
                    _ => refused += 1,
                }
                histories += 1;
                // The next placement, as an odometer.
                let mut digit = 0;
                loop {
                    if digit == len {
                        break;
                    }
                    at[digit] += 1;
                    if at[digit] < choices[digit].len() {
                        break;
                    }
                    at[digit] = 0;
                    digit += 1;
                }
                if digit == len {
                    break;
                }
            }
        }
    }
    assert_eq!(histories, 58 + 58 * 58 + 58 * 58 * 58);
    assert!(
        passed > 0 && refused > 0,
        "{passed} passed, {refused} refused"
    );
}

/// A seeded history of `len` operations by `clients` clients on a register, each client's calls one
/// after the other's return: linearizable as written by construction when `honest`, its reads then
/// seeing the value at a point in their interval; otherwise each read sees a value drawn from all
/// written. A write never returns with chance `lost_in` in a thousand, and its client calls nothing
/// after it (Knossos's crashed process); it took effect, or not.
fn seeded(seed: u64, len: usize, clients: usize, honest: bool, lost_in: u64) -> Vec<Op<u64>> {
    let mut draws = Seeded::new(seed);
    // Each client's next free time, and the order points: (point, op) where each op takes effect.
    let mut free = vec![0u64; clients];
    let mut ops: Vec<Op<u64>> = Vec::new();
    let mut points: Vec<(u64, usize)> = Vec::new();
    for at in 0..len {
        let client = draws.below(clients as u64) as usize;
        let call = free[client] + draws.below(3);
        let point = call + draws.below(4);
        let ret = point + draws.below(4);
        let write = draws.below(2) == 0;
        let lost = write && draws.below(1_000) < lost_in;
        free[client] = if lost { u64::MAX / 4 } else { ret + 1 };
        let input = if write {
            Access::Write(at as u64 + 1)
        } else {
            Access::Read
        };
        ops.push(Operation {
            call,
            ret: (!lost).then_some(ret),
            input,
            output: None,
        });
        points.push((point * 2 + u64::from(lost), at));
    }
    // Free clients whose last write was lost only for this history's length.
    points.sort_unstable();
    let mut value: Option<u64> = None;
    for (_, at) in points {
        let op = &mut ops[at];
        match op.input {
            Access::Write(written) => {
                // A lost write took effect, or did not.
                if op.ret.is_some() || draws.below(2) == 0 {
                    value = Some(written);
                }
                if op.ret.is_some() {
                    op.output = Some(Answer::Written);
                }
            }
            Access::Read => {
                let seen = if honest {
                    value
                } else {
                    let drawn = draws.below(len as u64 + 1);
                    (drawn > 0).then_some(drawn)
                };
                op.output = Some(Answer::Read(seen));
            }
        }
    }
    ops
}

/// Seeded histories of four to eight operations by one to four clients, honest and not: the
/// search's verdict is the brute force's on every one.
#[test]
fn seeded_histories_are_judged_as_the_tree_search_judges_them() {
    let (mut passed, mut refused) = (0u64, 0u64);
    for seed in 0..6_000u64 {
        let len = 4 + (seed % 5) as usize;
        let clients = 1 + (seed / 5 % 4) as usize;
        let honest = seed % 2 == 0;
        let ops = seeded(seed, len, clients, honest, 125);
        match agrees(&ops).verdict {
            Verdict::Linearizable { .. } => passed += 1,
            _ => {
                assert!(!honest, "seed {seed}: an honest history refused");
                refused += 1;
            }
        }
    }
    assert!(
        passed > 3_000 && refused > 0,
        "{passed} passed, {refused} refused"
    );
}

/// Long honest histories, of three hundred operations by up to four clients, now and then a write
/// lost, are passed; the orders verified.
#[test]
fn long_honest_histories_are_passed() {
    let register = Register::<u64>::new();
    for seed in 0..64u64 {
        let ops = seeded(seed, 300, 1 + (seed % 4) as usize, true, 10);
        let found = search(&register, &ops, &Budget::default()).unwrap();
        let Verdict::Linearizable { order } = &found.verdict else {
            panic!("seed {seed}: {:?}", found.verdict);
        };
        verify(&register, &ops, order).unwrap();
    }
}

/// Lowe §4's bound on a register's configurations, `(N+1)·2^p·(p+1)` for `N` operations and `p`
/// clients, holds for the memo: on every seeded history of up to eight operations, and on the long
/// ones.
#[test]
fn a_registers_configurations_are_within_lowes_bound() {
    let register = Register::<u64>::new();
    for seed in 0..2_000u64 {
        let clients = 1 + (seed % 4) as usize;
        let (len, lost) = if seed < 1_900 {
            (4 + (seed % 5) as usize, 125)
        } else {
            (200, 10)
        };
        let ops = seeded(seed, len, clients, seed % 2 == 0, lost);
        // A client whose write never returned is a client of its own from there on.
        let lost = ops.iter().filter(|op| op.ret.is_none()).count();
        let p = (clients + lost) as u64;
        let n = ops.len() as u64;
        let bound = (n + 1) * (1u64 << p) * (p + 1);
        let found = search(&register, &ops, &Budget::default()).unwrap();
        assert!(
            found.configurations <= bound && found.confirmed.unwrap_or(0) <= bound,
            "seed {seed}: {} and {:?} configurations, bound {bound}",
            found.configurations,
            found.confirmed
        );
    }
}

/// A fingerprint that names every configuration alike, so every configuration after the first seems
/// seen: the search refuses what needs it to go back, and the refusal's confirmation on whole keys
/// finds the order.
struct Blind;

impl Print for Blind {
    fn print<S: Hash>(&self, _zobrist: u128, _state: &S) -> u128 {
        7
    }
}

#[test]
fn a_refusal_from_fingerprints_is_confirmed_on_whole_keys() {
    let register = Register::<String>::new();
    // Under fingerprints that name every configuration alike, the second configuration reached
    // seems seen already, so the search refuses; its confirmation on whole keys finds "b" then "a".
    let racing = [
        put(1, Some(4), "a"),
        put(2, Some(3), "b"),
        get(5, 6, Some("a")),
        get(7, 8, Some("a")),
    ];
    let found = search_with(&register, &racing, &Budget::default(), &Blind).unwrap();
    assert!(found.collision, "{found:?}");
    assert!(found.confirmed.is_some());
    let Verdict::Linearizable { order } = &found.verdict else {
        panic!("{found:?}");
    };
    verify(&register, &racing, order).unwrap();
    // A history with no order is refused under either memo.
    let lost = [
        put(1, Some(2), "a"),
        put(3, Some(4), "b"),
        get(5, 6, Some("a")),
    ];
    let found = search_with(&register, &lost, &Budget::default(), &Blind).unwrap();
    assert!(!found.collision && found.confirmed.is_some());
    assert!(matches!(found.verdict, Verdict::NotLinearizable(_)));
    // Every seeded history is judged alike under the blind fingerprint.
    let register = Register::<u64>::new();
    for seed in 0..1_000u64 {
        let ops = seeded(
            seed,
            4 + (seed % 5) as usize,
            1 + (seed % 3) as usize,
            seed % 2 == 0,
            125,
        );
        let blind = search_with(&register, &ops, &Budget::default(), &Blind).unwrap();
        let sip = search(&register, &ops, &Budget::default()).unwrap();
        assert_eq!(
            matches!(blind.verdict, Verdict::Linearizable { .. }),
            matches!(sip.verdict, Verdict::Linearizable { .. }),
            "seed {seed}"
        );
    }
}

/// A search that would hold more than its budget says `Unknown`, apart from a refusal.
#[test]
fn a_search_past_its_budget_is_unknown() {
    let register = Register::<u64>::new();
    let ops = seeded(3, 300, 4, true, 10);
    // Room for the first table and no more.
    let tight = Budget { memory: 1 };
    let found = search(&register, &ops, &tight).unwrap();
    assert!(
        matches!(found.verdict, Verdict::Unknown(Spent::Budget { .. })),
        "{found:?}"
    );
    let roomy = search(&register, &ops, &Budget::default()).unwrap();
    assert!(matches!(roomy.verdict, Verdict::Linearizable { .. }));
}

/// A malformed history is refused before any search.
#[test]
fn a_malformed_history_is_refused() {
    let register = Register::<String>::new();
    let backwards = [put(5, Some(2), "a")];
    assert!(search(&register, &backwards, &Budget::default()).is_err());
    let answered = [Operation {
        call: 1,
        ret: None,
        input: Access::Write("a".to_string()),
        output: Some(Answer::Written),
    }];
    assert!(search(&register, &answered, &Budget::default()).is_err());
}

/// A map of registers is checked key by key (Horn and Kroening's P-compositionality): each part's
/// verdict is the brute force's on that part, and the parts' orders name the whole history's places.
#[test]
fn a_map_is_checked_key_by_key() {
    let register = Register::<u64>::new();
    for seed in 0..500u64 {
        let ops = seeded(seed, 12, 3, seed % 3 != 0, 125);
        let key = |op: &Op<u64>| match op.input {
            Access::Write(value) => value % 2,
            Access::Read => op.call % 2,
        };
        let parts = search_partitions(&register, &ops, key, &Budget::default()).unwrap();
        for part in parts {
            let alone: Vec<Op<u64>> = part.ops.iter().map(|at| ops[*at].clone()).collect();
            let expected = brute(&register, &alone);
            match &part.searched.verdict {
                Verdict::Linearizable { order } => {
                    assert!(expected, "seed {seed}");
                    let local: Vec<usize> = order
                        .iter()
                        .map(|at| part.ops.iter().position(|op| op == at).unwrap())
                        .collect();
                    verify(&register, &alone, &local).unwrap();
                }
                Verdict::NotLinearizable(_) => assert!(!expected, "seed {seed}"),
                Verdict::Unknown(spent) => panic!("{spent:?}"),
            }
        }
    }
}
