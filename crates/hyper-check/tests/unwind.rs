//! A model's code unwinding on the explorer's workers: each worker runs it inside an unwind
//! boundary (CLAUDE.md §1), so the search answers `ExploreError::Worker`, typed, even when the
//! unwind's payload unwinds again as it is dropped.
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::disallowed_macros,
    clippy::arithmetic_side_effects
)]

use std::num::NonZeroUsize;

use hyper_check::explore::{ExploreError, Model, Step, explore};
use hyper_check::search::Budget;

/// A panic payload whose drop unwinds too.
struct Bomb;

impl Drop for Bomb {
    fn drop(&mut self) {
        panic!("a payload that unwinds as it drops");
    }
}

/// Counts up from 0; the step from `at` unwinds, with [`Bomb`] as its payload when `bomb`.
struct Unwinds {
    at: u8,
    bomb: bool,
}

impl Model for Unwinds {
    type State = u8;
    type Action = ();
    type Fault = ();
    type Key = u8;

    fn paths(&self) -> &'static [&'static str] {
        &[]
    }
    fn initial(&self) -> u8 {
        0
    }
    fn actions(&self, _state: &u8, out: &mut Vec<()>) {
        out.push(());
    }
    fn apply(&self, state: &u8, _action: ()) -> Option<Step<u8, ()>> {
        if *state == self.at {
            if self.bomb {
                std::panic::panic_any(Bomb);
            }
            panic!("a model that unwinds");
        }
        Some(Step::plain(state + 1))
    }
    fn canonical(&self, state: &u8) -> u8 {
        *state
    }
    fn unpack(&self, key: &u8) -> u8 {
        *key
    }
}

/// The host's parallelism: the explorer's workers, as many as the host runs at once.
fn workers() -> NonZeroUsize {
    std::thread::available_parallelism().unwrap()
}

/// A budget every worker's fingerprint table fits in whatever the host's width: each starts at 1,024
/// slots of a 16-byte fingerprint (16 KiB a worker, the explorer's `PrintSet::new`), and 64 MiB
/// holds 4,096 of them beside the model's four states. A budget is a bound, not an allocation: the
/// run is charged what it holds. A fixed 1 MiB was spent by a 32-thread host's tables before the
/// model's third step, so the run answered `Unknown` rather than the unwind.
const BUDGET: Budget = Budget { memory: 64 << 20 };

#[test]
fn a_model_that_unwinds_is_answered_typed() {
    for bomb in [false, true] {
        let found = explore(&Unwinds { at: 3, bomb }, workers(), BUDGET);
        assert!(
            matches!(found, Err(ExploreError::Worker)),
            "bomb {bomb}: {found:?}"
        );
    }
}
