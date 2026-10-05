//! PCT, the probabilistic concurrency testing scheduler (Burckhardt, Kothari, Musuvathi and
//! Nagarakatte, ASPLOS 2010; `docs/research/sim.md` §2), over the members of a group
//! (`docs/sim.md` §4.5): PCT's threads become members, and its steps the choice points at which
//! events for two or more members race.
//!
//! As the paper's §2 states the algorithm: the `n` members get the priorities `d, …, d + n − 1` in
//! a random order; `d − 1` change points `k₁, …, k_{d−1}` are drawn uniform in `[1, k]`; at each
//! choice point the enabled member of highest priority runs, and at the `kᵢ`-th choice point the
//! member that would run is lowered to priority `i` (below every first priority) and the choice is
//! made again. Theorem 9: a bug of depth `d` is found with probability at least `1/(n·k^(d−1))`
//! per run, for a program of at most `n` threads and `k` steps; [`confidence`] gives what `R`
//! runs reach, `1 − (1 − 1/(n·k^(d−1)))^R`.
//!
//! A choice point at which every enabled event is one member's is no race: it counts as no step
//! (§4.5: "`k` counts only the choice points where events of different processes race"), and its
//! event is taken in the order the harness offers.

use hyper_sim::Seeded;

/// Why a scheduler could not be drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PctError {
    /// No members, or a depth of zero.
    Empty,
}

/// The scheduler of one run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pct {
    /// Each member's priority, by its place: higher runs first.
    priorities: Vec<u64>,
    /// The change points still ahead, ascending, each with the priority it lowers to.
    changes: Vec<(u64, u64)>,
    /// Racing choice points so far.
    step: u64,
}

impl Pct {
    /// A scheduler for `members` members at depth `depth`, over at most `steps` racing choice
    /// points, drawn from `draws`.
    pub fn new(
        members: usize,
        depth: u32,
        steps: u64,
        draws: &mut Seeded,
    ) -> Result<Self, PctError> {
        if members == 0 || depth == 0 {
            return Err(PctError::Empty);
        }
        let first = u64::from(depth);
        let mut priorities: Vec<u64> = (0..members)
            .map(|place| first.saturating_add(u64::try_from(place).unwrap_or(u64::MAX)))
            .collect();
        // A uniform order by Fisher and Yates's shuffle (Knuth's Algorithm P).
        for at in (1..members).rev() {
            let bound = u64::try_from(at).unwrap_or(u64::MAX).saturating_add(1);
            let other = usize::try_from(draws.below(bound)).unwrap_or(0);
            priorities.swap(at, other);
        }
        let mut changes: Vec<(u64, u64)> = (1..first)
            .map(|lowered| (draws.below(steps.max(1)).saturating_add(1), lowered))
            .collect();
        changes.sort_unstable();
        Ok(Self {
            priorities,
            changes,
            step: 0,
        })
    }

    /// A scheduler drawn from `seed`'s own stream (`hyper_sim::rng::stream_seed` named "pct"), so
    /// its draws never move a schedule's drawn from the same seed.
    pub fn of(seed: u64, members: usize, depth: u32, steps: u64) -> Result<Self, PctError> {
        let mut draws = Seeded::new(hyper_sim::rng::stream_seed(seed, "pct", &[]));
        Self::new(members, depth, steps, &mut draws)
    }

    /// Racing choice points so far.
    pub fn steps(&self) -> u64 {
        self.step
    }

    fn priority(&self, member: usize) -> u64 {
        self.priorities.get(member).copied().unwrap_or(0)
    }

    /// The candidate to run among `enabled`, each the member it is for: the first candidate of the
    /// member of highest priority. A race (two or more members among them) is a step, and at a
    /// change point the member that would run is lowered first. `None` for no candidates.
    pub fn pick(&mut self, enabled: &[usize]) -> Option<usize> {
        let first = *enabled.first()?;
        let races = enabled.iter().any(|member| *member != first);
        if races {
            self.step = self.step.saturating_add(1);
            while let Some(&(at, lowered)) = self.changes.first() {
                if at > self.step {
                    break;
                }
                self.changes.remove(0);
                if at == self.step
                    && let Some(top) = self.top(enabled)
                    && let Some(member) = enabled.get(top)
                    && let Some(priority) = self.priorities.get_mut(*member)
                {
                    *priority = lowered;
                }
            }
        }
        self.top(enabled)
    }

    fn top(&self, enabled: &[usize]) -> Option<usize> {
        let mut best: Option<(u64, usize)> = None;
        for (at, member) in enabled.iter().enumerate() {
            let priority = self.priority(*member);
            if best.is_none_or(|(top, _)| priority > top) {
                best = Some((priority, at));
            }
        }
        best.map(|(_, at)| at)
    }
}

/// The probability that `runs` runs of PCT find a bug of depth `depth` at least once, over `members`
/// members and `steps` racing choice points a run: `1 − (1 − 1/(n·k^(d−1)))^R` (Theorem 9 applied to
/// independent runs), computed as `−expm1(R·ln1p(−p))` so it stays exact where `p` is tiny.
pub fn confidence(runs: u64, members: u64, steps: u64, depth: u32) -> f64 {
    let exponent = i32::try_from(depth.saturating_sub(1)).unwrap_or(i32::MAX);
    #[allow(
        clippy::cast_precision_loss,
        reason = "a probability: f64's 53 bits are its precision"
    )]
    let (runs, members, steps) = (runs as f64, members.max(1) as f64, steps.max(1) as f64);
    let per_run = 1.0 / (members * steps.powi(exponent));
    -(runs * (-per_run).ln_1p()).exp_m1()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_member_of_highest_priority_runs_until_a_change_point() {
        let mut draws = Seeded::new(5);
        let mut pct = Pct::new(3, 1, 10, &mut draws).unwrap();
        let first = pct.pick(&[0, 1, 2]).unwrap();
        for _ in 0..20 {
            assert_eq!(pct.pick(&[0, 1, 2]), Some(first));
        }
        // Depth two: one change point, after which another member runs.
        let mut draws = Seeded::new(5);
        let mut pct = Pct::new(3, 2, 10, &mut draws).unwrap();
        let picks: Vec<usize> = (0..12).map(|_| pct.pick(&[0, 1, 2]).unwrap()).collect();
        assert!(
            picks.windows(2).filter(|pair| pair[0] != pair[1]).count() == 1,
            "{picks:?}"
        );
    }

    #[test]
    fn one_members_events_are_no_race() {
        let mut draws = Seeded::new(1);
        let mut pct = Pct::new(3, 2, 10, &mut draws).unwrap();
        assert_eq!(pct.pick(&[2, 2, 2]), Some(0));
        assert_eq!(pct.steps(), 0);
    }

    #[test]
    fn confidence_is_the_theorem_s_bound() {
        // n = 3, d = 1: 1 − (2/3)^R; fourteen runs pass 0.99.
        assert!(confidence(14, 3, 1_000, 1) > 0.99);
        assert!(confidence(11, 3, 1_000, 1) < 0.99);
        let exact = 1.0 - (1.0 - 1.0 / (3.0 * 100.0)) * (1.0 - 1.0 / 300.0);
        assert!((confidence(2, 3, 100, 2) - exact).abs() < 1e-15);
    }
}
