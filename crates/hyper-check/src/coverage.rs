//! Non-vacuity (`docs/sim.md` §4.4): a run that never reached a path proves nothing of it.
//!
//! A harness declares the paths it claims as named [`Counters`] — FoundationDB's `TEST(...)`
//! macros, Antithesis's `sometimes` and `reachable`, slates' floors, hyper-raft's `Coverage` — and
//! holds a [`Floor`] for each: more than one a seed for a common path, at least one a campaign for a
//! rare one (slates' explorer: `count > seeds` for each common path, `count > 0` for each rare
//! one). Each floor states the count it was set from, measured, and that count must itself clear the
//! floor, so a floor is never set above what was measured. A floor that falls is a failed test, not
//! a warning.

use std::fmt;

/// How often a path must be reached.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Per {
    /// More than once a seed: more reaches than seeds run.
    Seed,
    /// At least once in the campaign.
    Campaign,
}

/// A measured count a floor was set from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Measured {
    /// The count.
    pub count: u64,
    /// Over this many seeds.
    pub seeds: u64,
}

/// A path's floor, with the measurement it was set from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Floor {
    /// The path.
    pub path: &'static str,
    /// How often.
    pub per: Per,
    /// The count it was set from.
    pub measured: Measured,
}

impl Floor {
    /// Whether `count` reaches over `seeds` clear this floor.
    pub fn holds(&self, count: u64, seeds: u64) -> bool {
        match self.per {
            Per::Seed => count > seeds,
            Per::Campaign => count > 0,
        }
    }
}

/// Named counters of the paths a run reached.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Counters {
    paths: &'static [&'static str],
    counts: Vec<u64>,
}

impl Counters {
    /// Counters of `paths`, each at zero.
    pub fn new(paths: &'static [&'static str]) -> Self {
        Self {
            paths,
            counts: vec![0; paths.len()],
        }
    }

    /// The path named `path` was reached `times` more; false for a name not declared.
    pub fn add(&mut self, path: &str, times: u64) -> bool {
        let Some(at) = self.paths.iter().position(|name| *name == path) else {
            return false;
        };
        match self.counts.get_mut(at) {
            Some(count) => {
                *count = count.saturating_add(times);
                true
            }
            None => false,
        }
    }

    /// The path named `path` was reached once more; false for a name not declared.
    pub fn hit(&mut self, path: &str) -> bool {
        self.add(path, 1)
    }

    /// How often `path` was reached.
    pub fn count(&self, path: &str) -> u64 {
        self.paths
            .iter()
            .position(|name| *name == path)
            .and_then(|at| self.counts.get(at))
            .copied()
            .unwrap_or(0)
    }

    /// `other`'s counts added to these, path by path; false where their paths differ.
    pub fn merge(&mut self, other: &Self) -> bool {
        if self.paths != other.paths {
            return false;
        }
        for (count, more) in self.counts.iter_mut().zip(&other.counts) {
            *count = count.saturating_add(*more);
        }
        true
    }

    /// Each path with its count.
    pub fn iter(&self) -> impl Iterator<Item = (&'static str, u64)> + '_ {
        self.paths.iter().copied().zip(self.counts.iter().copied())
    }
}

/// Why floors did not hold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fallen {
    /// A path was reached less than its floor asks.
    Below {
        /// The path.
        path: &'static str,
        /// How often it was reached.
        count: u64,
        /// Over how many seeds.
        seeds: u64,
    },
    /// A floor's own measurement does not clear it: it was set above what was measured.
    Unmeasured {
        /// The path.
        path: &'static str,
    },
    /// A floor names a path the counters do not declare.
    Undeclared {
        /// The path.
        path: &'static str,
    },
}

impl fmt::Display for Fallen {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Below { path, count, seeds } => {
                write!(
                    f,
                    "\"{path}\" was reached {count} times over {seeds} seeds, below its floor"
                )
            }
            Self::Unmeasured { path } => {
                write!(
                    f,
                    "the floor of \"{path}\" is above the count it states it was set from"
                )
            }
            Self::Undeclared { path } => write!(f, "\"{path}\" is not a declared path"),
        }
    }
}

impl std::error::Error for Fallen {}

/// Every floor of `floors` holds for `counters` over `seeds` seeds.
pub fn hold(floors: &[Floor], counters: &Counters, seeds: u64) -> Result<(), Fallen> {
    for floor in floors {
        if !counters.paths.contains(&floor.path) {
            return Err(Fallen::Undeclared { path: floor.path });
        }
        if !floor.holds(floor.measured.count, floor.measured.seeds) {
            return Err(Fallen::Unmeasured { path: floor.path });
        }
        let count = counters.count(floor.path);
        if !floor.holds(count, seeds) {
            return Err(Fallen::Below {
                path: floor.path,
                count,
                seeds,
            });
        }
    }
    Ok(())
}
