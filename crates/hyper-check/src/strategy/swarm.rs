//! Swarm configurations (Groce, Zhang, Eide, Chen and Regehr, "Swarm Testing", ISSTA 2012;
//! `docs/sim.md` §3.8): each seed first draws its own configuration, every feature of the test's
//! list on or off with even odds and every rate or tunable uniform over its legal range, and runs
//! under it. A test states the features and ranges; the run states what it drew, so a failure
//! names its configuration.
//!
//! The draws come from a stream of their own, named for the seed (`hyper_sim::rng::stream_seed`),
//! so the configuration's draws never move the schedule's.

use hyper_sim::Seeded;
use hyper_sim::rng::stream_seed;

/// The configuration draws of one seed.
#[derive(Clone, Debug)]
pub struct Swarm {
    draws: Seeded,
}

impl Swarm {
    /// The configuration stream of `seed`.
    pub fn of(seed: u64) -> Self {
        Self {
            draws: Seeded::new(stream_seed(seed, "swarm", &[])),
        }
    }

    /// Whether a feature is on: even odds, Groce et al.'s §2 ("each feature is omitted with
    /// probability 1/2").
    pub fn feature(&mut self) -> bool {
        self.draws.below(2) == 1
    }

    /// Uniform in `[low, high]`; `low` when the range is empty.
    pub fn within(&mut self, low: u64, high: u64) -> u64 {
        let span = high.saturating_sub(low).saturating_add(1);
        if high < low {
            return low;
        }
        low.saturating_add(self.draws.below(span))
    }

    /// One of `choices`, uniform; `None` for none.
    pub fn one<T: Copy>(&mut self, choices: &[T]) -> Option<T> {
        let bound = u64::try_from(choices.len()).ok()?;
        let at = usize::try_from(self.draws.below(bound)).ok()?;
        choices.get(at).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_seed_draws_one_configuration() {
        let draw = |seed| {
            let mut swarm = Swarm::of(seed);
            (swarm.feature(), swarm.within(3, 9), swarm.one(&[1, 2, 3]))
        };
        assert_eq!(draw(4), draw(4));
        let drawn: Vec<_> = (0..64).map(draw).collect();
        assert!(drawn.iter().any(|d| d.0) && drawn.iter().any(|d| !d.0));
        assert!(drawn.iter().all(|d| (3..=9).contains(&d.1)));
    }
}
