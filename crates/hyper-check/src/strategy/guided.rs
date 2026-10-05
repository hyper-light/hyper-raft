//! Coverage-guided search over schedules (`docs/sim.md` §4.5; Gulcan, Ozkan, Majumdar and
//! Nagendra, "Model-Guided Fuzzing of Distributed Systems", §3.2, Algorithm 1): each run's abstract
//! states are its coverage; a run that reaches points no run reached before joins the corpus with
//! as much energy as it found new points, and the next runs are its tape mutated
//! ([`super::tape::Tape::mutate`]), one run a unit of energy.
//!
//! Both of what it holds have bounds: the points in a fingerprint set within a stated
//! [`Budget`] (`docs/sim.md` §7, "Coverage set"), which refuses past it so a campaign says when
//! its coverage stopped being counted; the corpus at a stated count of tapes, the entry with the
//! least energy left giving way to a new one.

use hyper_sim::Seeded;

use crate::search::{Budget, Spent};
use crate::strategy::tape::Tape;
use crate::table::PrintSet;

/// A tape in the corpus and the runs it is still owed.
#[derive(Clone, Debug)]
struct Entry {
    tape: Tape,
    energy: u64,
}

/// The coverage and the corpus of one campaign.
pub struct Guided {
    points: PrintSet,
    corpus: Vec<Entry>,
    bound: usize,
    draws: Seeded,
}

impl Guided {
    /// A campaign whose points fit `budget` and whose corpus holds at most `corpus` tapes, its
    /// choices drawn from `seed`.
    pub fn new(budget: Budget, corpus: usize, seed: u64) -> Result<Self, Spent> {
        Ok(Self {
            points: PrintSet::new(budget.memory, 1024)?,
            corpus: Vec::new(),
            bound: corpus.max(1),
            draws: Seeded::new(seed),
        })
    }

    /// `point` was reached by the run under way: true when no run had reached it.
    pub fn reached(&mut self, point: u128) -> Result<bool, Spent> {
        self.points.insert(point)
    }

    /// Points reached by every run so far.
    pub fn points(&self) -> u64 {
        self.points.len()
    }

    /// Tapes in the corpus.
    pub fn corpus(&self) -> usize {
        self.corpus.len()
    }

    /// A finished run's tape, which reached `new` points no run had: it joins the corpus with that
    /// energy, the entry with the least energy left giving way when the corpus is full.
    pub fn finished(&mut self, tape: Tape, new: u64) {
        if new == 0 {
            return;
        }
        if self.corpus.len() >= self.bound {
            let weakest = self
                .corpus
                .iter()
                .enumerate()
                .min_by_key(|(_, entry)| entry.energy)
                .map(|(at, _)| at);
            match weakest {
                Some(at) if self.corpus.get(at).is_some_and(|entry| entry.energy < new) => {
                    self.corpus.swap_remove(at);
                }
                _ => return,
            }
        }
        if self.corpus.try_reserve(1).is_ok() {
            self.corpus.push(Entry { tape, energy: new });
        }
    }

    /// The next tape to run: a mutation of the corpus's entry with the most energy left, which
    /// spends a unit of it; `None` when no entry has energy left (the campaign then runs a fresh
    /// seed).
    pub fn mutated(&mut self) -> Option<Tape> {
        let at = self
            .corpus
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.energy > 0)
            .max_by_key(|(_, entry)| entry.energy)
            .map(|(at, _)| at)?;
        let entry = self.corpus.get_mut(at)?;
        entry.energy = entry.energy.saturating_sub(1);
        Some(entry.tape.mutate(&mut self.draws))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategy::tape::{Bounds, Player};

    #[test]
    fn a_run_that_reaches_new_points_earns_as_many_mutations() {
        let mut guided = Guided::new(Budget { memory: 1 << 20 }, 4, 1).unwrap();
        assert!(guided.mutated().is_none());
        assert!(guided.reached(7).unwrap());
        assert!(!guided.reached(7).unwrap());
        let mut player = Player::record(
            3,
            Bounds {
                steps: 10,
                words: 100,
            },
        );
        for _ in 0..5 {
            player.word().unwrap();
            player.end_step(0).unwrap();
        }
        guided.finished(player.finish(), 3);
        assert_eq!(
            (0..5).filter_map(|_| guided.mutated()).count(),
            3,
            "three units of energy, three runs"
        );
    }

    #[test]
    fn a_full_corpus_gives_way_only_to_more_energy() {
        let mut guided = Guided::new(Budget { memory: 1 << 20 }, 1, 1).unwrap();
        guided.finished(Tape::default(), 5);
        guided.finished(Tape::default(), 2);
        assert_eq!(guided.corpus(), 1);
        assert_eq!((0..9).filter_map(|_| guided.mutated()).count(), 5);
    }
}
