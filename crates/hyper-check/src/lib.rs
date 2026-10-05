//! The judges of every simulation here (`docs/sim.md` §4, step S-4).
//!
//! - **Oracles** ([`oracle`], §4.1): the properties every step of a run must keep, over the whole
//!   history, each written against its specification and fed the observations a harness makes:
//!   Election Safety, Log Matching, Leader Completeness, State Machine Safety, fast agreement,
//!   durability over a member's [`oracle::DurableView`], read safety, exactly once and same history.
//! - **Liveness** ([`liveness`], §4.2): the quiet period after which a group that moves nothing is
//!   stuck, from the members' own settings, and monitors with hot and cold states for the
//!   properties that are not convergence.
//! - **Linearizability** (§4.3): the [`witness`] checker, which holds a history to the system's own
//!   order in one pass, and the [`search`] checker, which finds an order or proves there is none,
//!   with no word from the system; [`agree`] runs both on one history, and [`history::verify`]
//!   holds every order either exhibits.
//! - **Non-vacuity** ([`coverage`], §4.4): named counters of the paths a harness claims, and floors
//!   each stated with the count it was set from.
//!
//! It is held to the production lints, has no `unsafe` and no dependency outside `std` and
//! `hyper-sim` (`docs/sim.md` §2), and makes no OS call.

#![cfg_attr(
    test,
    allow(
        clippy::panic,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::disallowed_macros
    )
)]

pub mod agree;
pub mod conform;
pub mod coverage;
pub mod explore;
pub mod history;
pub mod liveness;
pub mod model;
pub mod oracle;
pub mod search;
pub mod strategy;
mod table;
pub mod witness;

pub use agree::{Agreed, Agreement, Disagreement, agree};
pub use history::{Operation, verify};
pub use model::{Access, Answer, Model, Register};
pub use search::{Budget, Counterexample, Searched, Verdict, search, search_partitions};
