//! The unwind boundary around the caller's code the explorer's workers run: a model's actions and
//! its keys' hashing and comparison (CLAUDE.md §1: production code never panics, and a caller's
//! code that can is called behind an unwind boundary).

use std::panic::{AssertUnwindSafe, catch_unwind};

/// Runs `op`, the caller's code, inside an unwind boundary: `None` if it unwound. The unwind's
/// payload is the caller's too, so it is dropped inside a boundary of its own; a payload whose drop
/// unwinds as well leaves that second payload unfreed, one allocation, rather than unwind (std's
/// own thread result aborts the process there).
pub(crate) fn contained<R>(op: impl FnOnce() -> R) -> Option<R> {
    match catch_unwind(AssertUnwindSafe(op)) {
        Ok(value) => Some(value),
        Err(payload) => {
            if let Err(again) = catch_unwind(AssertUnwindSafe(move || drop(payload))) {
                let _ = Box::leak(again);
            }
            None
        }
    }
}

/// A worker's result, `None` if its code unwound. Its thread's body runs inside [`contained`], so
/// the thread itself never unwinds; a payload its join returns all the same is dropped inside a
/// boundary.
pub(crate) fn joined<R>(result: std::thread::Result<Option<R>>) -> Option<R> {
    match result {
        Ok(value) => value,
        Err(payload) => {
            let _ = contained(move || drop(payload));
            None
        }
    }
}
