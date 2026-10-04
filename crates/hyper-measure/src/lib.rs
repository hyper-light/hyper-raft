//! What a run costs, counted where it happens: every allocation, reallocation
//! and free the process makes ([`alloc`]), and the page faults the operating
//! system charged it ([`faults`]). The benchmarks and the end-to-end tests of
//! this repository report both per operation (`CLAUDE.md` §1a,
//! `docs/benchmarks.md`).
//!
//! It is measurement only: no shipped crate depends on it. A benchmark or a
//! test installs [`alloc::Counting`] as its global allocator and switches the
//! counting on around what it measures.
//!
//! A test or a benchmark that hands a call a `Waker` takes a counting one from
//! [`wake`]. A real-socket test or an E2E member waits for a datagram with
//! [`wait::arrives`].
//!
//! The `unsafe` this crate needs is in four files that
//! `scripts/check-contracts.py` lists: `src/alloc.rs` (the allocator forwards
//! to the system's), `src/faults.rs` (the OS calls that read the faults),
//! `src/wake.rs` (a waker built over a leaked slot) and `src/wait_windows.rs`
//! (the poll a wait is on Windows).

pub mod alloc;
pub mod faults;
pub mod stats;
pub mod wait;
pub mod wake;
