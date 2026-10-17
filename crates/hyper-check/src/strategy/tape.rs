//! Shrinking a decision tape (`docs/sim.md` §4.5, "Replay and shrink"). The tape itself, its
//! player and its mutations are hyper-sim's (`hyper_sim::tape`), so a world plays one as a harness's
//! own player does (`docs/sim.md` §18); they are named here as before.

pub use hyper_sim::tape::{Bounds, Player, Tape, TapeError};

/// What shrinking a failing tape gave.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shrunk {
    /// The shortest failing tape found.
    pub tape: Tape,
    /// Runs it took.
    pub runs: u64,
    /// Whether no single step of it can be removed with the run still failing: false when the
    /// budget of runs ended first.
    pub minimal: bool,
}

/// `tape` shrunk while `fails` holds of it (`docs/sim.md` §4.5, "Replay and shrink"): chunks of
/// steps removed, halving their size down to one step (Zeller and Hildebrandt's ddmin, its
/// complement steps), then single steps removed until a whole pass removes none, which is a
/// 1-minimal tape. A removal is kept when the run still fails the same way, which `fails` judges.
/// At most `budget` runs.
pub fn shrink(tape: Tape, budget: u64, mut fails: impl FnMut(&Tape) -> bool) -> Shrunk {
    let mut best = tape;
    let mut runs = 0u64;
    let mut chunk = best.len().div_ceil(2).max(1);
    loop {
        let mut removed = false;
        let mut start = 0usize;
        while start < best.len() {
            if runs >= budget {
                return Shrunk {
                    tape: best,
                    runs,
                    minimal: false,
                };
            }
            let end = start.saturating_add(chunk);
            let candidate = best.filtered(|step| step < start || step >= end);
            runs = runs.saturating_add(1);
            if !candidate.is_empty() && fails(&candidate) {
                best = candidate;
                removed = true;
            } else {
                start = end;
            }
        }
        if chunk == 1 && !removed {
            return Shrunk {
                tape: best,
                runs,
                minimal: true,
            };
        }
        if !removed {
            chunk = chunk.div_ceil(2).max(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOUNDS: Bounds = Bounds {
        steps: 1_000,
        words: 10_000,
    };

    /// A run that draws a step's words by what it has seen: as a harness does.
    fn run(player: &mut Player, steps: usize) -> Vec<u64> {
        let mut seen = Vec::new();
        for _ in 0..steps {
            let first = player.below(5).unwrap();
            for _ in 0..first {
                seen.push(player.below(100).unwrap());
            }
            seen.push(first);
            player.end_step(u8::try_from(first % 2).unwrap()).unwrap();
        }
        seen
    }

    #[test]
    fn shrinking_reaches_a_one_minimal_tape() {
        // Fails while the tape holds a step of kind 1 followed later by two more of kind 1.
        let fails = |tape: &Tape| tape.kinds().filter(|k| *k == 1).count() >= 3;
        let mut fresh = Player::record(1, BOUNDS);
        run(&mut fresh, 60);
        let tape = fresh.finish();
        assert!(fails(&tape));
        let shrunk = shrink(tape, 10_000, fails);
        assert!(shrunk.minimal);
        assert_eq!(shrunk.tape.len(), 3);
        assert!(shrunk.tape.kinds().all(|k| k == 1));
    }
}
