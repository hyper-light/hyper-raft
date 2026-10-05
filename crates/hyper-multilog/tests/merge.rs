//! The merge's determinism (`docs/multilog.md` §4, §11 step 2): every history of up to three logs
//! over a small alphabet, and for each every interleaving of the logs' commits arriving one entry
//! at a time, the merge called after every arrival and again only at the end; then longer
//! histories, random interleavings, budgets and stops, and restarts from canonical cuts, by
//! property tests. Every run is held to an oracle written from §4.2's definition of the order
//! (`≺`), not from the merge: the set it applies, the order it applies in, the epoch each keyed
//! command sees, and the entries it refuses.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::disallowed_macros,
    clippy::cognitive_complexity,
    clippy::cast_possible_truncation,
    clippy::needless_range_loop,
    missing_docs,
    unreachable_pub
)]

use std::collections::{BTreeMap, BTreeSet};

use hyper_multilog::{Advance, Applied, Command, Cut, Flow, Logs, Merge, Refusal, entry, log_of};
use hyper_raft::StorageError;
use hyper_raft::proto::{Entry, EntryType};
use proptest::prelude::*;

/// What a test history holds at one position of a log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sym {
    /// A keyed command of the key the test routes to this log.
    Keyed,
    /// A keyed command of a key that routes to another log.
    Misrouted,
    /// A global command.
    Global,
    /// A barrier naming log 0's index.
    Barrier(u64),
    /// The core's own empty entry.
    Own,
    /// Bytes the layer never writes.
    Malformed,
}

/// The first key that routes to `log` of `logs`.
fn key_in(log: usize, logs: usize) -> u64 {
    (0..).find(|key| log_of(*key, logs) == log).unwrap()
}

/// A history's entries: log `k`'s position `p` at index `p`, each command's bytes naming its log
/// and index, so that a command is told apart wherever it is applied.
fn entries(history: &[Vec<Sym>]) -> Vec<Vec<Entry>> {
    let logs = history.len();
    history
        .iter()
        .enumerate()
        .map(|(log, syms)| {
            syms.iter()
                .enumerate()
                .map(|(at, sym)| {
                    let index = at as u64 + 1;
                    let command = vec![log as u8, index as u8];
                    let data = match sym {
                        Sym::Keyed => entry::keyed_command(command, key_in(log, logs)).unwrap(),
                        Sym::Misrouted => {
                            entry::keyed_command(command, key_in((log + 1) % logs, logs)).unwrap()
                        }
                        Sym::Global => entry::global(command).unwrap(),
                        Sym::Barrier(named) => entry::barrier_naming(*named).unwrap(),
                        Sym::Own => Vec::new(),
                        Sym::Malformed => vec![0xff],
                    };
                    Entry {
                        entry_type: EntryType::EntryNormal,
                        term: 1,
                        index,
                        data,
                        context: Vec::new(),
                    }
                })
                .collect()
        })
        .collect()
}

/// The logs as committed so far: each log's entries through `through[k]`.
struct Held<'a> {
    logs: &'a [Vec<Entry>],
    through: Vec<u64>,
}

impl Logs for Held<'_> {
    fn count(&self) -> usize {
        self.logs.len()
    }
    fn through(&self, log: usize) -> u64 {
        self.through[log]
    }
    fn walk(
        &self,
        log: usize,
        from: u64,
        through: u64,
        visit: &mut dyn FnMut(&Entry) -> bool,
    ) -> Result<(), StorageError> {
        for index in from..=through {
            let entry = self.logs[log]
                .get(index as usize - 1)
                .ok_or(StorageError::Unavailable)?;
            if visit(entry) {
                break;
            }
        }
        Ok(())
    }
}

/// What a run handed the owner, in order: `(log, index)` with what was applied there.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Out {
    Command {
        log: usize,
        index: u64,
        key: Option<u64>,
        epoch: u64,
    },
    Refused {
        log: usize,
        index: u64,
        why: Refusal,
    },
}

impl Out {
    fn place(&self) -> (usize, u64) {
        match self {
            Self::Command { log, index, .. } | Self::Refused { log, index, .. } => (*log, *index),
        }
    }
    fn of(applied: Applied<'_>) -> Self {
        match applied {
            Applied::Command(Command {
                log,
                index,
                key,
                data,
                epoch,
            }) => {
                assert_eq!(data, &[log as u8, index as u8], "a command's own bytes");
                Self::Command {
                    log,
                    index,
                    key,
                    epoch,
                }
            }
            Applied::Refused { log, index, why } => Self::Refused { log, index, why },
            Applied::Resized { .. } => panic!("these histories hold no resize"),
        }
    }
}

/// The oracle: what §4.2's definition says of a history whose logs are committed through
/// `through`: the entries applied (`D`, as `(log, position)`), each one's epoch, and the
/// refusals.
struct Oracle {
    /// For each log, how far the applied set reaches.
    reach: Vec<u64>,
    /// The epoch each keyed command applies in, and each global the global before it.
    epochs: BTreeMap<(usize, u64), u64>,
    /// The entries refused.
    refused: BTreeMap<(usize, u64), Refusal>,
    /// For each applied entry of a log `k ≥ 1`, the greatest index the barriers before it name
    /// (`M`); for each global, its `q*` in every other log.
    m: BTreeMap<(usize, u64), u64>,
    q: BTreeMap<(u64, usize), u64>,
}

impl Oracle {
    fn new(history: &[Vec<Sym>], through: &[u64]) -> Self {
        let logs = history.len();
        // Valid barriers: those in a log other than log 0. Valid globals: in log 0.
        let globals: Vec<u64> = history[0]
            .iter()
            .enumerate()
            .filter(|(_, sym)| **sym == Sym::Global)
            .map(|(at, _)| at as u64 + 1)
            .collect();
        let barrier = |log: usize, at: usize| match history[log][at] {
            Sym::Barrier(named) if log != 0 => Some(named),
            _ => None,
        };
        // q*_k(g) within the committed prefix.
        let mut q = BTreeMap::new();
        for g in &globals {
            for log in 1..logs {
                let found = (0..through[log] as usize)
                    .find(|at| barrier(log, *at).is_some_and(|n| n >= *g));
                if let Some(at) = found {
                    q.insert((*g, log), at as u64 + 1);
                }
            }
        }
        // M(k, p) for every position.
        let mut m = BTreeMap::new();
        for (log, syms) in history.iter().enumerate().skip(1) {
            let mut most = 0;
            for at in 0..syms.len() {
                m.insert((log, at as u64 + 1), most);
                if let Some(named) = barrier(log, at) {
                    most = most.max(named);
                }
            }
        }
        // The fixpoint: extend each log's applied prefix while its next entry's conditions hold.
        let mut reach = vec![0u64; logs];
        loop {
            let mut moved = false;
            for log in 0..logs {
                let p = reach[log] + 1;
                if p > through[log] {
                    continue;
                }
                let ok = if log == 0 {
                    match history[0][p as usize - 1] {
                        // Every other log has consumed exactly what precedes its first
                        // barrier naming the global or later.
                        Sym::Global => {
                            (1..logs).all(|k| q.get(&(p, k)).is_some_and(|qk| reach[k] + 1 == *qk))
                        }
                        _ => true,
                    }
                } else {
                    // Every barrier before p, and the barrier at p if it is one, names an index log
                    // 0 has applied through.
                    let named_here = barrier(log, p as usize - 1).unwrap_or(0);
                    let need = m[&(log, p)].max(named_here);
                    need == 0 || reach[0] >= need
                };
                if ok {
                    reach[log] = p;
                    moved = true;
                }
            }
            if !moved {
                break;
            }
        }
        // Epochs and refusals of what is applied.
        let mut epochs = BTreeMap::new();
        let mut refused = BTreeMap::new();
        for log in 0..logs {
            for p in 1..=reach[log] {
                let sym = history[log][p as usize - 1];
                let before_global = |index: u64| {
                    globals
                        .iter()
                        .copied()
                        .filter(|g| *g < index)
                        .max()
                        .unwrap_or(0)
                };
                let at_most_global = |index: u64| {
                    globals
                        .iter()
                        .copied()
                        .filter(|g| *g <= index)
                        .max()
                        .unwrap_or(0)
                };
                match (log, sym) {
                    (0, Sym::Global) => {
                        epochs.insert((0, p), before_global(p));
                    }
                    (0, Sym::Keyed) => {
                        epochs.insert((0, p), before_global(p));
                    }
                    (_, Sym::Keyed) => {
                        epochs.insert((log, p), at_most_global(m[&(log, p)]));
                    }
                    (_, Sym::Misrouted) => {
                        refused.insert(
                            (log, p),
                            Refusal::Misrouted {
                                key: key_in((log + 1) % logs, logs),
                            },
                        );
                    }
                    (0, Sym::Barrier(_)) => {
                        refused.insert((0, p), Refusal::BarrierInLogZero);
                    }
                    (_, Sym::Global) => {
                        refused.insert((log, p), Refusal::GlobalOutsideLogZero);
                    }
                    (_, Sym::Malformed) => {
                        refused.insert((log, p), Refusal::Malformed);
                    }
                    (_, Sym::Own | Sym::Barrier(_)) => {}
                }
            }
        }
        Self {
            reach,
            epochs,
            refused,
            m,
            q,
        }
    }

    /// Holds a run's output to the oracle: the same applied set, epochs and refusals, and an order
    /// that extends `≺` (its generating edges, which a sequence extends iff it extends their
    /// closure).
    fn check(&self, history: &[Vec<Sym>], out: &[Out], merge: &Merge, what: &str) {
        let logs = history.len();
        for log in 0..logs {
            assert_eq!(
                merge.next(log),
                Some(self.reach[log] + 1),
                "{what}: log {log}'s reach"
            );
        }
        let mut seen = BTreeMap::new();
        for (at, item) in out.iter().enumerate() {
            assert!(
                seen.insert(item.place(), at).is_none(),
                "{what}: {item:?} twice"
            );
            match item {
                Out::Command {
                    log,
                    index,
                    key,
                    epoch,
                } => {
                    assert_eq!(
                        self.epochs.get(&(*log, *index)),
                        Some(epoch),
                        "{what}: epoch of {item:?}"
                    );
                    let expected = if *log == 0 && history[0][*index as usize - 1] == Sym::Global {
                        None
                    } else {
                        Some(key_in(*log, logs))
                    };
                    assert_eq!(*key, expected, "{what}: key of {item:?}");
                }
                Out::Refused { log, index, why } => {
                    assert_eq!(
                        self.refused.get(&(*log, *index)),
                        Some(why),
                        "{what}: {item:?}"
                    );
                }
            }
        }
        let expected: BTreeSet<(usize, u64)> = self
            .epochs
            .keys()
            .chain(self.refused.keys())
            .copied()
            .collect();
        let got: BTreeSet<(usize, u64)> = seen.keys().copied().collect();
        assert_eq!(got, expected, "{what}: the set applied");
        // (i): each log in order.
        for log in 0..logs {
            let order: Vec<u64> = out
                .iter()
                .filter(|o| o.place().0 == log)
                .map(|o| o.place().1)
                .collect();
            assert!(
                order.windows(2).all(|w| w[0] < w[1]),
                "{what}: log {log} out of order"
            );
        }
        // (ii): what log k applies after its barriers comes after log 0 through what they name.
        for (place, at) in &seen {
            let (log, index) = *place;
            if log == 0 {
                continue;
            }
            let need = self.m[&(log, index)];
            for (other, other_at) in &seen {
                if other.0 == 0 && other.1 <= need {
                    assert!(
                        other_at < at,
                        "{what}: log 0's {} after log {log}'s {index}",
                        other.1
                    );
                }
            }
        }
        // (iii): a global comes after every entry of each other log before its first barrier
        // naming it or later.
        for (place, at) in &seen {
            if place.0 != 0 || history[0][place.1 as usize - 1] != Sym::Global {
                continue;
            }
            for log in 1..logs {
                let qk = self.q[&(place.1, log)];
                for (other, other_at) in &seen {
                    if other.0 == log && other.1 < qk {
                        assert!(
                            other_at < at,
                            "{what}: log {log}'s {} after the global at {}",
                            other.1,
                            place.1
                        );
                    }
                }
            }
        }
    }
}

/// Runs the merge over `history` as its commits arrive in `order` (a log's number per arrival),
/// calling it after every arrival (or only at the end), with `budget` a call; returns what it
/// handed out and the merge.
fn run(
    history: &[Vec<Sym>],
    logs: &[Vec<Entry>],
    order: &[usize],
    every: bool,
    budget: u64,
) -> (Vec<Out>, Merge) {
    let mut merge = Merge::new(history.len()).unwrap();
    let mut held = Held {
        logs,
        through: vec![0; history.len()],
    };
    let mut out = Vec::new();
    let drain = |merge: &mut Merge, held: &Held<'_>, out: &mut Vec<Out>| loop {
        let Advance { more, .. } = merge
            .advance(held, budget, &mut |applied| {
                out.push(Out::of(applied));
                Flow::Continue
            })
            .unwrap();
        if !more {
            break;
        }
    };
    for log in order {
        held.through[*log] += 1;
        if every {
            drain(&mut merge, &held, &mut out);
        }
    }
    drain(&mut merge, &held, &mut out);
    (out, merge)
}

/// Every sequence over `alphabet` of length at most `most`.
fn sequences(alphabet: &[Sym], most: usize) -> Vec<Vec<Sym>> {
    let mut all = vec![Vec::new()];
    let mut frontier = vec![Vec::new()];
    for _ in 0..most {
        let mut next = Vec::new();
        for seq in &frontier {
            for sym in alphabet {
                let mut longer: Vec<Sym> = seq.clone();
                longer.push(*sym);
                next.push(longer);
            }
        }
        all.extend(next.iter().cloned());
        frontier = next;
    }
    all
}

/// Every interleaving of arrivals: each log `k` arriving `lengths[k]` times, in every order.
fn interleavings(lengths: &[usize]) -> Vec<Vec<usize>> {
    fn go(left: &mut Vec<usize>, prefix: &mut Vec<usize>, all: &mut Vec<Vec<usize>>) {
        if left.iter().all(|l| *l == 0) {
            all.push(prefix.clone());
            return;
        }
        for log in 0..left.len() {
            if left[log] > 0 {
                left[log] -= 1;
                prefix.push(log);
                go(left, prefix, all);
                prefix.pop();
                left[log] += 1;
            }
        }
    }
    let mut all = Vec::new();
    go(&mut lengths.to_vec(), &mut Vec::new(), &mut all);
    all
}

/// The command history of a run: each key's commands in order with their epochs, the globals in
/// order, and the refusals; what Theorem 1 says every run of one history shares.
fn history_of(out: &[Out]) -> BTreeMap<Option<u64>, Vec<(usize, u64, u64)>> {
    let mut keys: BTreeMap<Option<u64>, Vec<(usize, u64, u64)>> = BTreeMap::new();
    for item in out {
        if let Out::Command {
            log,
            index,
            key,
            epoch,
        } = item
        {
            keys.entry(*key).or_default().push((*log, *index, *epoch));
        }
    }
    keys
}

/// Holds every interleaving of every history `histories` lists to the oracle, both ways of
/// calling the merge; how many runs it made.
fn every_interleaving(histories: &[Vec<Vec<Sym>>]) -> u64 {
    let mut runs = 0u64;
    for history in histories {
        let logs = entries(history);
        let lengths: Vec<usize> = history.iter().map(Vec::len).collect();
        let through: Vec<u64> = lengths.iter().map(|l| *l as u64).collect();
        let oracle = Oracle::new(history, &through);
        let mut reference = None;
        for order in interleavings(&lengths) {
            for every in [true, false] {
                let (out, merge) = run(history, &logs, &order, every, u64::MAX);
                let what = format!("{history:?} arriving {order:?}, every {every}");
                oracle.check(history, &out, &merge, &what);
                let shared = history_of(&out);
                assert_eq!(
                    reference.get_or_insert_with(|| shared.clone()),
                    &shared,
                    "{what}"
                );
                runs += 1;
            }
        }
    }
    runs
}

/// Two logs, each of up to three entries: log 0 of keyed and global commands, log 1 of keyed
/// commands and barriers naming every index of log 0 and one past it.
#[test]
fn two_logs_merge_alike_in_every_interleaving() {
    let zero = sequences(&[Sym::Keyed, Sym::Global], 3);
    let one = sequences(
        &[
            Sym::Keyed,
            Sym::Barrier(1),
            Sym::Barrier(2),
            Sym::Barrier(3),
            Sym::Barrier(4),
        ],
        3,
    );
    let mut histories = Vec::new();
    for a in &zero {
        for b in &one {
            histories.push(vec![a.clone(), b.clone()]);
        }
    }
    let runs = every_interleaving(&histories);
    eprintln!("{} histories of two logs, {runs} runs", histories.len());
    assert_eq!(histories.len(), 15 * 156);
}

/// Three logs, each of up to two entries.
#[test]
fn three_logs_merge_alike_in_every_interleaving() {
    let zero = sequences(&[Sym::Keyed, Sym::Global], 2);
    let other = sequences(
        &[
            Sym::Keyed,
            Sym::Barrier(1),
            Sym::Barrier(2),
            Sym::Barrier(3),
        ],
        2,
    );
    let mut histories = Vec::new();
    for a in &zero {
        for b in &other {
            for c in &other {
                histories.push(vec![a.clone(), b.clone(), c.clone()]);
            }
        }
    }
    let runs = every_interleaving(&histories);
    eprintln!("{} histories of three logs, {runs} runs", histories.len());
    assert_eq!(histories.len(), 7 * 21 * 21);
}

/// Entries out of place and the core's own, in every place of two short logs: each refused, or
/// passed as nothing, alike in every interleaving.
#[test]
fn what_is_out_of_place_is_refused_alike_in_every_interleaving() {
    let zero = sequences(
        &[
            Sym::Keyed,
            Sym::Global,
            Sym::Barrier(1),
            Sym::Misrouted,
            Sym::Own,
            Sym::Malformed,
        ],
        2,
    );
    let one = sequences(
        &[
            Sym::Keyed,
            Sym::Barrier(1),
            Sym::Barrier(2),
            Sym::Global,
            Sym::Misrouted,
            Sym::Own,
            Sym::Malformed,
        ],
        2,
    );
    let mut histories = Vec::new();
    for a in &zero {
        for b in &one {
            histories.push(vec![a.clone(), b.clone()]);
        }
    }
    let runs = every_interleaving(&histories);
    eprintln!(
        "{} histories with entries out of place, {runs} runs",
        histories.len()
    );
}

/// One log: no barrier, and the merge is the log in its order, each command in the epoch the last
/// global before it opened.
#[test]
fn one_log_applies_in_its_own_order() {
    let history = vec![vec![
        Sym::Keyed,
        Sym::Global,
        Sym::Own,
        Sym::Keyed,
        Sym::Global,
        Sym::Keyed,
    ]];
    let logs = entries(&history);
    let (out, merge) = run(&history, &logs, &[0, 0, 0, 0, 0, 0], true, u64::MAX);
    let key = Some(key_in(0, 1));
    assert_eq!(
        out,
        vec![
            Out::Command {
                log: 0,
                index: 1,
                key,
                epoch: 0
            },
            Out::Command {
                log: 0,
                index: 2,
                key: None,
                epoch: 0
            },
            Out::Command {
                log: 0,
                index: 4,
                key,
                epoch: 2
            },
            Out::Command {
                log: 0,
                index: 5,
                key: None,
                epoch: 2
            },
            Out::Command {
                log: 0,
                index: 6,
                key,
                epoch: 5
            },
        ]
    );
    assert!(merge.canonical(), "every position of one log is canonical");
}

/// slates' case: a global waits until every other log has a barrier naming it, so the keyed
/// commands a log ordered before its barrier are applied before it, and those after, after it.
#[test]
fn a_global_command_waits_for_every_logs_barrier() {
    let history = vec![
        vec![Sym::Global],
        vec![Sym::Keyed, Sym::Barrier(1), Sym::Keyed],
        vec![Sym::Keyed, Sym::Barrier(1)],
    ];
    let logs = entries(&history);
    let mut merge = Merge::new(3).unwrap();
    let mut held = Held {
        logs: &logs,
        through: vec![1, 1, 1],
    };
    let mut out = Vec::new();
    let mut take = |merge: &mut Merge, held: &Held<'_>| {
        merge
            .advance(held, u64::MAX, &mut |applied| {
                out.push(Out::of(applied));
                Flow::Continue
            })
            .unwrap();
    };
    take(&mut merge, &held);
    held.through = vec![1, 3, 1];
    take(&mut merge, &held);
    held.through = vec![1, 3, 2];
    take(&mut merge, &held);
    let (k1, k2) = (Some(key_in(1, 3)), Some(key_in(2, 3)));
    assert_eq!(
        out,
        vec![
            Out::Command {
                log: 1,
                index: 1,
                key: k1,
                epoch: 0
            },
            Out::Command {
                log: 2,
                index: 1,
                key: k2,
                epoch: 0
            },
            Out::Command {
                log: 0,
                index: 1,
                key: None,
                epoch: 0
            },
            Out::Command {
                log: 1,
                index: 3,
                key: k1,
                epoch: 1
            },
        ],
        "the two keyed commands before any barrier, then the global once log 2's barrier came, then what log 1 ordered after it"
    );
}

/// A barrier naming an index whose entry is no global passes once log 0 is consumed through it:
/// slates' rule (a barrier passes once its global is applied) would hold its log forever.
#[test]
fn a_barrier_naming_no_global_holds_its_log_only_until_log_0_reaches_it() {
    let history = vec![
        vec![Sym::Keyed, Sym::Keyed, Sym::Global],
        vec![Sym::Barrier(2), Sym::Keyed, Sym::Barrier(3), Sym::Keyed],
    ];
    let logs = entries(&history);
    let (out, merge) = run(&history, &logs, &[1, 1, 1, 1, 0, 0, 0], true, u64::MAX);
    assert_eq!(merge.next(1), Some(5), "log 1 consumed whole");
    let k1 = Some(key_in(1, 2));
    assert!(
        out.contains(&Out::Command {
            log: 1,
            index: 2,
            key: k1,
            epoch: 0
        }),
        "{out:?}"
    );
    assert!(
        out.contains(&Out::Command {
            log: 1,
            index: 4,
            key: k1,
            epoch: 3
        }),
        "{out:?}"
    );
}

/// A canonical cut is comparable with every position any interleaving reaches, and holding it is
/// read off log 0 alone; a cut at a keyed command of log 0, with every other log at its first
/// barrier naming it or later, is not canonical: some interleaving reaches a position neither
/// ahead of it nor behind (`docs/multilog.md` §5.1).
#[test]
fn canonical_cuts_are_comparable_with_every_reachable_position() {
    let history = vec![
        vec![Sym::Keyed, Sym::Global, Sym::Keyed, Sym::Keyed, Sym::Global],
        vec![Sym::Barrier(2), Sym::Keyed, Sym::Barrier(5)],
        vec![Sym::Barrier(2), Sym::Barrier(5)],
    ];
    let logs = entries(&history);
    let lengths = [5, 3, 2];
    // Every position every interleaving passes through, called after every arrival with a budget
    // of one entry a call.
    let mut positions = BTreeSet::new();
    let mut canonical = BTreeSet::new();
    for order in interleavings(&lengths) {
        let mut merge = Merge::new(3).unwrap();
        let mut held = Held {
            logs: &logs,
            through: vec![0; 3],
        };
        for log in &order {
            held.through[*log] += 1;
            loop {
                positions.insert(merge.cut().next().to_vec());
                if merge.canonical() {
                    canonical.insert((merge.cut().next().to_vec(), merge.epoch()));
                }
                let advance = merge.advance(&held, 0, &mut |_| Flow::Continue).unwrap();
                if advance.consumed == 0 {
                    break;
                }
            }
        }
        positions.insert(merge.cut().next().to_vec());
    }
    let expected: BTreeSet<(Vec<u64>, u64)> =
        [(vec![1, 1, 1], 0), (vec![3, 1, 1], 2), (vec![6, 3, 2], 5)]
            .into_iter()
            .collect();
    assert_eq!(canonical, expected, "the origin and each global's cut");
    let le = |a: &[u64], b: &[u64]| a.iter().zip(b).all(|(x, y)| x <= y);
    for (cut, epoch) in &canonical {
        let cut_value = Cut::new(cut.clone(), *epoch).unwrap();
        for position in &positions {
            assert!(
                le(position, cut) || le(cut, position),
                "{position:?} against the canonical {cut:?}"
            );
            let held_by = Cut::new(position.clone(), 0).unwrap().holds(&cut_value);
            assert_eq!(
                held_by,
                le(cut, position),
                "holding {cut:?} read off log 0 at {position:?}"
            );
        }
    }
    // Log 0 through its keyed command at 3, log 1 at its barrier naming 5, log 2 at its own.
    let keyed = vec![4u64, 3, 2];
    let incomparable: Vec<&Vec<u64>> = positions
        .iter()
        .filter(|p| !le(p, &keyed) && !le(&keyed, p))
        .collect();
    assert!(
        !incomparable.is_empty(),
        "a cut at a keyed command of log 0 is comparable with everything"
    );
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

    /// Longer histories of two to four logs, with entries out of place, random interleavings,
    /// budgets and stops after globals, held to the oracle; a restart from a canonical cut the
    /// run stopped at goes on as the run did.
    #[test]
    fn longer_histories_merge_alike(
        logs in 1usize..=4,
        raw in proptest::collection::vec(proptest::collection::vec((0u8..8, 0u64..14), 0..12), 4),
        arrivals in proptest::collection::vec(0usize..4, 0..64),
        budget in 0u64..6,
        stop_every in 1u64..4,
    ) {
        let history: Vec<Vec<Sym>> = raw.iter().take(logs).enumerate().map(|(log, syms)| {
            syms.iter().map(|(kind, named)| match (log == 0, kind) {
                (true, 0..=3) => Sym::Keyed,
                (true, 4 | 5) => Sym::Global,
                (true, _) => Sym::Own,
                (false, 0..=2) => Sym::Keyed,
                (false, 3..=5) => Sym::Barrier(*named),
                (false, 6) if logs > 1 => Sym::Misrouted,
                (false, _) => Sym::Own,
            }).collect()
        }).collect();
        let entries = entries(&history);
        let lengths: Vec<usize> = history.iter().map(Vec::len).collect();
        // The order: the arrivals drawn, then whatever is left, in log order.
        let mut left = lengths.clone();
        let mut order = Vec::new();
        for log in arrivals {
            let log = log % logs;
            if left[log] > 0 {
                left[log] -= 1;
                order.push(log);
            }
        }
        for (log, count) in left.iter().enumerate() {
            order.extend(std::iter::repeat_n(log, *count));
        }
        let through: Vec<u64> = lengths.iter().map(|l| *l as u64).collect();
        let oracle = Oracle::new(&history, &through);
        let (whole, merge) = run(&history, &entries, &order, true, budget);
        oracle.check(&history, &whole, &merge, &format!("{history:?} by {order:?}"));
        // The same with a stop after every `stop_every`-th global, a restart at each such cut.
        let mut merge = Merge::new(logs).unwrap();
        let mut held = Held { logs: &entries, through: vec![0; logs] };
        let mut out: Vec<Out> = Vec::new();
        let mut globals = 0u64;
        for log in order.iter().copied().chain(std::iter::once(usize::MAX)) {
            if log != usize::MAX {
                held.through[log] += 1;
            }
            loop {
                let advance = merge
                    .advance(&held, budget, &mut |applied| {
                        let item = Out::of(applied);
                        let global = matches!(item, Out::Command { key: None, .. });
                        out.push(item);
                        if global {
                            globals += 1;
                            if globals.is_multiple_of(stop_every) {
                                return Flow::Stop;
                            }
                        }
                        Flow::Continue
                    })
                    .unwrap();
                if merge.canonical() && merge.epoch() > 0 {
                    // An image here, and a member restarting from it.
                    merge = Merge::at(&merge.cut());
                }
                if !advance.more {
                    break;
                }
            }
        }
        oracle.check(&history, &out, &merge, &format!("{history:?} by {order:?}, stopping"));
        prop_assert_eq!(history_of(&out), history_of(&whole));
    }
}

/// An entry of `data` at `index`.
fn at(index: u64, data: Vec<u8>) -> Entry {
    Entry {
        entry_type: EntryType::EntryNormal,
        term: 1,
        index,
        data,
        context: Vec::new(),
    }
}

/// What a run over `logs`, each held whole, handed the owner.
fn run_whole(merge: &mut Merge, logs: &[Vec<Entry>]) -> (Vec<String>, Advance) {
    let held = Held {
        logs,
        through: logs.iter().map(|log| log.len() as u64).collect(),
    };
    let mut out = Vec::new();
    let advance = merge
        .advance(&held, u64::MAX, &mut |applied| {
            out.push(match applied {
                Applied::Command(command) => format!(
                    "{}@{} key {:?} epoch {}",
                    command.log, command.index, command.key, command.epoch
                ),
                Applied::Refused { log, index, why } => format!("{log}@{index} refused {why:?}"),
                Applied::Resized { index, logs, was } => {
                    format!("0@{index} resized {was} to {logs}")
                }
            });
            Flow::Continue
        })
        .unwrap();
    (out, advance)
}

/// `docs/multilog.md` §3.5, growing: the resize is taken as a global is, once every log stands at
/// a barrier naming it, and the merge stops after it; the log it adds is read from its first
/// entry, and every keyed command consumed after it is routed by the new count, so one routed by
/// the old count and committed after the resize is refused, alike on every member.
#[test]
fn a_resize_that_adds_a_log_routes_what_follows_it_by_the_new_count() {
    let moved = (0..)
        .find(|key| log_of(*key, 2) == 1 && log_of(*key, 3) != 1)
        .unwrap();
    let stays = (0..)
        .find(|key| log_of(*key, 2) == 1 && log_of(*key, 3) == 1)
        .unwrap();
    let added = key_in(2, 3);
    let logs = vec![
        vec![
            at(1, entry::resize(3).unwrap()),
            at(2, entry::keyed_command(vec![2], added).unwrap()),
        ],
        vec![
            at(1, entry::keyed_command(vec![1], stays).unwrap()),
            at(2, entry::barrier_naming(1).unwrap()),
            at(3, entry::keyed_command(vec![3], moved).unwrap()),
            at(4, entry::keyed_command(vec![4], stays).unwrap()),
        ],
        vec![at(1, entry::keyed_command(vec![5], added).unwrap())],
    ];
    let mut merge = Merge::new(2).unwrap();
    let (before, advance) = run_whole(&mut merge, &logs[..2]);
    assert_eq!(
        before,
        [
            format!("1@1 key {:?} epoch 0", Some(stays)),
            "0@1 resized 2 to 3".to_owned(),
        ]
    );
    assert!(advance.more, "the merge stops at the resize");
    assert_eq!(merge.logs(), 3);
    assert_eq!(merge.next(2), Some(1));
    let (after, _) = run_whole(&mut merge, &logs);
    assert_eq!(
        after,
        [
            format!("1@3 refused {:?}", Refusal::Misrouted { key: moved }),
            format!("1@4 key {:?} epoch 1", Some(stays)),
            format!("2@1 key {:?} epoch 1", Some(added)),
            format!("0@2 refused {:?}", Refusal::Misrouted { key: added }),
        ]
    );
}

/// `docs/multilog.md` §3.5, shrinking: the logs past the new count end at their barrier for the
/// resize, and nothing after it there is read; every key routes among the logs that are left.
#[test]
fn a_resize_that_ends_logs_reads_nothing_past_their_barrier() {
    let elsewhere = key_in(2, 3);
    let logs = vec![
        vec![
            at(1, entry::resize(1).unwrap()),
            at(2, entry::keyed_command(vec![1], elsewhere).unwrap()),
        ],
        vec![
            at(1, entry::barrier_naming(1).unwrap()),
            at(2, entry::keyed_command(vec![2], key_in(1, 3)).unwrap()),
        ],
        vec![
            at(1, entry::barrier_naming(1).unwrap()),
            at(2, entry::resize(5).unwrap()),
        ],
    ];
    let mut merge = Merge::new(3).unwrap();
    let (before, _) = run_whole(&mut merge, &logs);
    assert_eq!(before, ["0@1 resized 3 to 1".to_owned()]);
    assert_eq!(merge.logs(), 1);
    let (after, advance) = run_whole(&mut merge, &logs[..1]);
    assert_eq!(after, [format!("0@2 key {:?} epoch 1", Some(elsewhere))]);
    assert!(!advance.more);
}

/// A resize anywhere but log 0 is refused alike on every member, and changes nothing.
#[test]
fn a_resize_outside_log_0_is_refused() {
    let logs = vec![vec![], vec![at(1, entry::resize(4).unwrap())]];
    let mut merge = Merge::new(2).unwrap();
    let (out, _) = run_whole(&mut merge, &logs);
    assert_eq!(
        out,
        [format!("1@1 refused {:?}", Refusal::ResizeOutsideLogZero)]
    );
    assert_eq!(merge.logs(), 2);
}
