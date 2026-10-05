//! The strategies that choose what a run does next (`docs/sim.md` §4.5, step S-5), beside the
//! exhaustive searches of [`crate::explore`]:
//!
//! - [`swarm`]: each seed's own configuration, every feature on or off and every rate drawn
//!   (Groce et al.).
//! - [`pct`]: priorities over members and change points over the racing choice points, with the
//!   confidence a campaign of runs reaches per depth (Burckhardt et al., Theorem 9).
//! - [`tape`]: every decision of a run, replayed, mutated feasibly and shrunk.
//! - [`guided`]: coverage over abstract states, with energy for the runs that reach new ones
//!   (Gulcan et al.); the abstraction and its conformance check are [`crate::conform`].
//! - [`fork`]: crash enumeration by cloning the world at each crash point, one branch alive at a
//!   time.

pub mod fork;
pub mod guided;
pub mod pct;
pub mod swarm;
pub mod tape;
