//! The tokio adapter (mantle note 32 §3.2, step T-1's second half): drives hyper-transport's
//! [`Endpoint`](hyper_transport::Endpoint) and hyper-datagram's
//! [`Plane`](hyper_datagram::Plane) on tokio, for the consumers that run tokio (focal, and
//! mantle's network side). The sans-io crates never name a runtime; this crate is the only one
//! that does, and slates never depends on it (note 32 §6 item 6).
//!
//! **Shape.** A [`Driver`] owns one endpoint, its UDP socket and one timer. The owner holds the
//! driver in its task and awaits [`Driver::event`], which sends what the endpoint transmits,
//! feeds it the datagrams that arrive and the timeouts that fall due (each with the time it
//! happened), and returns the endpoint's next event. Between events the owner calls the endpoint
//! through [`Driver::endpoint`]. The owner's task is the one tokio wakes, through the socket's
//! and the timer's wakers: there is no task, thread, channel or lock inside, and no thread per
//! connection or exchange. `event` is cancel-safe, so an owner selects over it and its own work.
//! A [`PlaneSocket`] does the same for a plane on a socket of its own.
//!
//! **Bounds.** The outbox holds at most [`Io::batch`] datagrams, and a datagram past it with the
//! socket full is dropped and counted; receiving takes at most a batch per turn; one poll takes
//! at most [`TURNS`] turns before it yields. The endpoint's and the plane's own tables keep their
//! own bounds.
//!
//! **Sockets.** Linux sends with `sendmmsg(2)` and receives with `recvmmsg(2)`, each message a
//! segmented send (`UDP_SEGMENT`) or a coalesced receive (`UDP_GRO`) where the kernel offers
//! them; tokio's reactor is epoll there, kqueue on macOS and IOCP on Windows, where a datagram
//! is one system call. No io_uring, no AF_XDP.
//!
//! **`Arc`.** None in this crate. tokio holds its own scheduler and driver handles by `Arc`
//! inside the runtime (the socket's and the timer's registrations): that is tokio's code, not a
//! site here, and it is why the adapter is a crate of its own.

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

mod clock;
mod driver;
mod error;
mod plane;
mod socket;
mod sys;

pub use clock::{Arrival, Clock};
pub use driver::{Driver, TURNS};
pub use error::Error;
pub use plane::PlaneSocket;
pub use socket::{Io, IoStats, MAX_BATCH, RECEIVE_BYTES};
