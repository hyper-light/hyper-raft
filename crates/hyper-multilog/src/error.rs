//! What the layer refuses, and why. As the core's errors (`hyper_raft::Error`), of three kinds: a
//! **refusal** changed nothing and the caller may go on; a **violation** is a peer's message that
//! contradicts this member, dropped with nothing changed; a **fatal** error means the member's own
//! state no longer adds up, and it stops and is reopened from its durable state.

/// Everything the layer refuses or reports; [`Error::is_fatal`] tells the fatal apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// What a log's member answered, of the kind it states.
    #[error("log {log}: {error}")]
    Core {
        /// The log.
        log: usize,
        /// The member's error.
        error: hyper_raft::Error,
    },
    /// A refusal: no log of this number.
    #[error("no log {0}")]
    NoLog(usize),
    /// A refusal: what the owner opened the layer with cannot run.
    #[error("settings: {0}")]
    Settings(&'static str),
    /// A refusal: a bound is reached (`docs/multilog.md` §6).
    #[error("capacity: {0}")]
    Capacity(&'static str),
    /// A violation: a peer's proposal states what no member may propose there (`docs/multilog.md`
    /// §3.2); it is dropped and nothing changed.
    #[error("a peer's proposal is out of place: {0}")]
    Violation(&'static str),
    /// Fatal: the layer's state no longer adds up with its logs'.
    #[error("the layer's state is inconsistent: {0}")]
    Invariant(&'static str),
}

impl Error {
    /// Whether the member must stop and be reopened.
    pub fn is_fatal(&self) -> bool {
        match self {
            Self::Core { error, .. } => error.is_fatal(),
            Self::Invariant(_) => true,
            Self::NoLog(_) | Self::Settings(_) | Self::Capacity(_) | Self::Violation(_) => false,
        }
    }

    /// The error a log's member answered.
    pub fn of(log: usize, error: hyper_raft::Error) -> Self {
        Self::Core { log, error }
    }
}

/// What the layer's operations return.
pub type Result<T> = std::result::Result<T, Error>;
