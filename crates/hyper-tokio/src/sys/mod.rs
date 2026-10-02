//! The operating system's batched UDP calls, receive timestamps and monotonic clock, where the
//! portable path through tokio and std has none.

#[cfg(target_os = "linux")]
pub(crate) mod linux;
#[cfg(target_os = "macos")]
pub(crate) mod macos;
#[cfg(windows)]
pub(crate) mod windows;
