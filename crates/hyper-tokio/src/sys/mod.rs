//! The operating system's batched UDP calls, where the portable path through tokio has none.

#[cfg(target_os = "linux")]
pub(crate) mod linux;
