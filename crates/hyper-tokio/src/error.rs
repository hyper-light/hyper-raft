//! What the adapter refuses with.

use std::fmt;
use std::io::ErrorKind;

/// Why the adapter refused: every failure is one of these, never a panic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// No tokio runtime is current, or its I/O or time driver is not enabled: a socket or a timer
    /// cannot be registered.
    Runtime,
    /// The [`crate::Io`] configuration is out of its range.
    Configuration,
    /// The socket failed in a way that does not pass: binding, or a receive error other than
    /// the ones a UDP socket reports for a datagram that went astray (a reset or an unreachable
    /// peer, which are counted and read past).
    Io(ErrorKind),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Runtime => write!(
                formatter,
                "no tokio runtime with its I/O and time drivers is current"
            ),
            Self::Configuration => write!(formatter, "the I/O configuration is out of range"),
            Self::Io(kind) => write!(formatter, "the socket failed: {kind}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error.kind())
    }
}
