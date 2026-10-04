//! A member's account of a fault its operating system ends it for, written to its standard error
//! before it ends: what the test's failure can then say, where an exit code alone (`0xc0000005`,
//! Windows' access violation) says only that something broke an invariant.
//!
//! On Windows a vectored exception handler sees the fault first (`AddVectoredExceptionHandler`),
//! writes its code, the instruction's address and the address it touched, then a backtrace, and
//! passes the fault on: the process ends as it would have. Elsewhere a fault already reports its
//! signal, and nothing is installed.

#[cfg(windows)]
#[path = "fault_windows.rs"]
mod windows;

/// Installs the report, once, at a member's start.
pub fn report_faults() {
    #[cfg(windows)]
    windows::install();
}
