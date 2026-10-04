#![cfg_attr(
    test,
    allow(
        clippy::panic,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::disallowed_macros,
        clippy::cognitive_complexity
    )
)]
//! MLRaft over hyper-raft (`docs/multilog.md`; sources in `docs/research/multilog.md`): one
//! group's log divided into `n` Raft logs over the same voters, each electing its own leader, and
//! merged into one application order every member reaches alike.
//!
//! - **Routing** ([`route`]): a keyed command, which reads and writes one key's state and reads
//!   the global state, goes to the log its key hashes to; a global command, which may read and
//!   write anything, to log 0.
//! - **Barriers**: a global command takes one place in every other log's order, stated there by a
//!   barrier naming it; every member proposes the barriers it owes, and a log's leader keeps one a
//!   global ([`MultiLog::barriers`]).
//! - **The merge** ([`merge`]): a pure function of what each log committed. Keyed commands in
//!   their log's order, each in the epoch its log's barriers opened; a global command once every
//!   other log stands at a barrier naming it. Every member's application is one command history:
//!   every pair of commands that interfere in one order, on every member, whatever order the logs'
//!   commits arrive in (`docs/multilog.md` §4).
//! - **Images** ([`point`]): taken only at a canonical cut, which every member's state is
//!   comparable with, so that any member installs any other's (`docs/multilog.md` §5).
//!
//! With one log it is the single log, by the same code: no barrier, and the merge is log 0 in its
//! order. The layer is sans-io as the core is, holds no copy of any entry, and has no `unsafe`.

pub mod entry;
pub mod error;
pub mod merge;
mod multilog;
pub mod point;
pub mod route;

pub use error::{Error, Result};
pub use merge::{Advance, Applied, Command, Cut, Flow, Logs, Merge, Refusal};
pub use multilog::{CUT, Installed, Limits, MultiLog};
pub use point::{At, Point, PointError};
pub use route::{Route, log_of};
