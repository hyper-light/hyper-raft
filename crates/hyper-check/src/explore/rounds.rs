//! Exhaustive search of an implementation at a tiny scope (`docs/sim.md` §4.5, "Exhaustive,
//! implementation level"): every sequence of a few rounds of the real system, each round one of a
//! stated set of scenarios (Twins §4.2: per round a leader and the partitions its messages cross),
//! reduced by the state each round reaches (FlyMC §4.1's state symmetry and independence, here as
//! the harness's key: two prefixes reaching one key have the same futures, so one is explored).
//!
//! The search is depth first, so the systems alive at once are one per round of the path, the
//! bound `docs/sim.md` §7 states for forks: `1 +` the depth. The keys reached are held in a
//! fingerprint set within a [`Budget`]; past it the search says [`Played::Unknown`].

use std::fmt;

use super::fingerprint;
use crate::search::{Budget, Spent};
use crate::table::PrintSet;

/// A system the round search drives.
pub trait System: Clone {
    /// Why a round is a violation.
    type Fault: Clone + fmt::Debug;
    /// The scenarios a round may play from here: `0..choices()`.
    fn choices(&self) -> usize;
    /// Plays scenario `choice`; a fault ends the path.
    fn play(&mut self, choice: usize) -> Result<(), Self::Fault>;
    /// The state the futures depend on, fingerprinted: two systems with one key have the same
    /// futures, which is the harness's claim and the reduction's soundness.
    fn key(&self) -> u128;
}

/// What a round search did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rounds {
    /// Rounds played: each a scenario run on the real system.
    pub played: u64,
    /// Distinct keys reached with rounds left to play.
    pub states: u64,
    /// Prefixes not followed, their key already explored with at least as many rounds left.
    pub pruned: u64,
}

/// How a round search ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Played<F> {
    /// Every path of the depth was played or pruned, with no fault.
    Exhausted(Rounds),
    /// A path ended in a fault: its scenarios, and the fault.
    Fault {
        /// What was done up to it.
        rounds: Rounds,
        /// The scenarios of the path, in order.
        path: Vec<usize>,
        /// The fault.
        fault: F,
    },
    /// The budget was reached first.
    Unknown {
        /// What was done.
        rounds: Rounds,
        /// What refused.
        spent: Spent,
    },
}

struct Search {
    depth: u32,
    seen: PrintSet,
    rounds: Rounds,
    path: Vec<usize>,
}

/// A key with the rounds left after it.
fn keyed(key: u128, left: u32) -> u128 {
    fingerprint(&(key, left))
}

enum Stop<F> {
    Fault(F),
    Spent(Spent),
}

/// What the search does next with the path.
enum Next {
    /// Takes up the system at the path's end: passed over if its rounds are spent or its key was
    /// explored with as many rounds left, its first scenario played otherwise.
    Enter,
    /// Plays this scenario from the system at the path's end.
    Play(usize),
    /// Leaves the system at the path's end for its parent's next scenario.
    Back,
}

/// The search, depth first, as one loop over the path (mantle CLAUDE.md §10: no call of its own,
/// nothing waits on another's result). The systems kept are those after each prefix of the path,
/// at most `1 +` the depth, which each step extends, passes over or cuts back; the paths are taken
/// in their scenarios' lexicographic order, as a recursive descent would take them.
fn explore<S: System>(search: &mut Search, start: &S) -> Result<(), Stop<S::Fault>> {
    let mut systems = vec![start.clone()];
    let mut next = Next::Enter;
    loop {
        let Some(system) = systems.last() else {
            return Ok(());
        };
        next = match next {
            Next::Enter => {
                let taken = u32::try_from(search.path.len()).unwrap_or(u32::MAX);
                let left = search.depth.saturating_sub(taken);
                if left == 0 {
                    Next::Back
                } else {
                    let key = system.key();
                    // Explored before with at least as many rounds left: every future of this
                    // prefix was.
                    if (left..=search.depth).any(|more| search.seen.contains(keyed(key, more))) {
                        search.rounds.pruned = search.rounds.pruned.saturating_add(1);
                        Next::Back
                    } else {
                        search.seen.insert(keyed(key, left)).map_err(Stop::Spent)?;
                        search.rounds.states = search.seen.len();
                        Next::Play(0)
                    }
                }
            }
            Next::Play(choice) if choice < system.choices() => {
                let mut played = system.clone();
                search.rounds.played = search.rounds.played.saturating_add(1);
                search.path.push(choice);
                played.play(choice).map_err(Stop::Fault)?;
                systems.push(played);
                Next::Enter
            }
            Next::Play(_) | Next::Back => {
                systems.pop();
                match search.path.pop() {
                    Some(choice) => Next::Play(choice.saturating_add(1)),
                    // The start is left: every path was taken.
                    None => return Ok(()),
                }
            }
        };
    }
}

/// Every path of `depth` rounds from `start`, depth first, within `budget`.
pub fn rounds<S: System>(start: &S, depth: u32, budget: Budget) -> Played<S::Fault> {
    let seen = match PrintSet::new(budget.memory, 1024) {
        Ok(seen) => seen,
        Err(spent) => {
            return Played::Unknown {
                rounds: Rounds::default(),
                spent,
            };
        }
    };
    let mut search = Search {
        depth,
        seen,
        rounds: Rounds::default(),
        path: Vec::new(),
    };
    match explore(&mut search, start) {
        Ok(()) => Played::Exhausted(search.rounds),
        Err(Stop::Fault(fault)) => Played::Fault {
            rounds: search.rounds,
            path: search.path,
            fault,
        },
        Err(Stop::Spent(spent)) => Played::Unknown {
            rounds: search.rounds,
            spent,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A counter that faults on reaching 5; scenarios add 1 or 2.
    #[derive(Clone)]
    struct Counter(u32);

    impl System for Counter {
        type Fault = u32;
        fn choices(&self) -> usize {
            2
        }
        fn play(&mut self, choice: usize) -> Result<(), u32> {
            self.0 += u32::try_from(choice).map_err(|_| 0u32)? + 1;
            if self.0 == 5 { Err(self.0) } else { Ok(()) }
        }
        fn key(&self) -> u128 {
            u128::from(self.0)
        }
    }

    #[test]
    fn a_fault_within_the_depth_is_found_and_one_beyond_is_not() {
        assert!(matches!(
            rounds(&Counter(0), 2, Budget { memory: 1 << 16 }),
            Played::Exhausted(_)
        ));
        let Played::Fault { path, fault, .. } = rounds(&Counter(0), 3, Budget { memory: 1 << 16 })
        else {
            panic!("the counter reaches 5 in three rounds");
        };
        assert_eq!(fault, 5);
        assert_eq!(path.iter().map(|c| c + 1).sum::<usize>(), 5);
    }

    #[test]
    fn a_state_reached_twice_is_explored_once() {
        // 0 → 1 → 3 and 0 → 2 → 3: the second reach of 3 with as many rounds left is pruned.
        #[derive(Clone)]
        struct Free(u32);
        impl System for Free {
            type Fault = ();
            fn choices(&self) -> usize {
                2
            }
            fn play(&mut self, choice: usize) -> Result<(), ()> {
                self.0 = (self.0 + u32::try_from(choice).map_err(|_| ())? + 1).min(4);
                Ok(())
            }
            fn key(&self) -> u128 {
                u128::from(self.0)
            }
        }
        let Played::Exhausted(done) = rounds(&Free(0), 4, Budget { memory: 1 << 16 }) else {
            panic!("no fault");
        };
        assert!(done.pruned > 0, "{done:?}");
    }
}
