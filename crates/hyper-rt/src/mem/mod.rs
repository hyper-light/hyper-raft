//! The memory structures the runtime is built on, from slates' `mem` (ORIGIN.md): generational
//! handles and the packed task word, the task slab, segmented storage, and the single- and
//! multi-producer rings.

pub mod error;
pub mod handle;
#[cfg(loom)]
pub mod loom_bounds;
pub mod mpsc;
pub mod ring;
pub mod segmented;
pub mod slab;

pub use error::MemError;
pub use handle::{Encoded, Handle};
pub use mpsc::MpscRing;
pub use ring::SpscRing;
pub use segmented::Segmented;
pub use slab::Slab;
