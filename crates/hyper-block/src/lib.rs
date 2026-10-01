//! Block I/O for the shared log (mantle note 32 §3.9): how to move bytes to a device with the
//! alignment and durability the device and OS actually guarantee, from mantle-disk at mantle
//! `147f035` (`ORIGIN.md`).
//!
//! Identifying and measuring a device stays in mantle-disk: a caller that knows the device's
//! alignment, queue and measured depth hands them in.
#![allow(missing_docs)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::disallowed_macros
    )
)]

use std::path::PathBuf;

pub mod block;
pub mod buf;
pub mod commit;
pub mod file;
#[cfg(all(target_vendor = "apple", any(test, feature = "sim")))]
pub mod image;
pub mod issuer;
mod node;
pub mod scratch;
#[cfg(any(test, feature = "sim"))]
pub mod sim;
pub mod threads;

#[derive(Debug, thiserror::Error)]
pub enum DiskError {
    #[error("{op} {}: {source}", path.display())]
    Io {
        op: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("direct transfer at offset {offset} of {len} bytes is not aligned to {align}")]
    Misaligned {
        offset: u64,
        len: usize,
        align: usize,
    },
    #[error("{} ended at offset {offset}: {missing} bytes short", path.display())]
    ShortRead {
        path: PathBuf,
        offset: u64,
        missing: usize,
    },
    #[error(transparent)]
    Buf(#[from] buf::BufError),
    /// A pool for `path` would take more threads than the process budget has left; refused
    /// before any thread starts (docs/design/node.md §1.2).
    #[error(
        "{}: {asked} threads asked of a process budget with {left} of {ceiling} left",
        path.display()
    )]
    Threads {
        path: PathBuf,
        asked: usize,
        left: usize,
        ceiling: usize,
    },
    /// Storage mantle cannot write as it must, refused before any write.
    #[error("{}: {reason}", path.display())]
    Unsupported { path: PathBuf, reason: &'static str },
}
