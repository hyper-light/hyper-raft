//! The Windows half of [`super`]: a vectored exception handler that writes a fatal fault's account
//! to standard error and passes the fault on.
//!
//! A vectored handler runs before the faulting frame's own handlers, so a fault that code goes on
//! to handle itself is written too; the account that explains an ending is the last one written.
#![allow(unsafe_code)]

use std::io::Write;

use windows_sys::Win32::Foundation::{
    EXCEPTION_ACCESS_VIOLATION, EXCEPTION_DATATYPE_MISALIGNMENT, EXCEPTION_ILLEGAL_INSTRUCTION,
    EXCEPTION_IN_PAGE_ERROR, EXCEPTION_PRIV_INSTRUCTION, EXCEPTION_STACK_OVERFLOW, NTSTATUS,
};
use windows_sys::Win32::System::Diagnostics::Debug::{
    AddVectoredExceptionHandler, EXCEPTION_POINTERS,
};

/// The handler's answer that the search goes on (winnt.h's `EXCEPTION_CONTINUE_SEARCH`): to the
/// handlers after this one and to the system, which ends the process.
const CONTINUE_SEARCH: i32 = 0;

/// The faults that end a process unless something handles them: an access violation, a page that
/// could not be read in, a misaligned access, an instruction this processor does not run or may not
/// run here, and a stack overflow.
const FATAL: [NTSTATUS; 6] = [
    EXCEPTION_ACCESS_VIOLATION,
    EXCEPTION_IN_PAGE_ERROR,
    EXCEPTION_DATATYPE_MISALIGNMENT,
    EXCEPTION_ILLEGAL_INSTRUCTION,
    EXCEPTION_PRIV_INSTRUCTION,
    EXCEPTION_STACK_OVERFLOW,
];

/// The bytes the fault's first line takes at most: its fixed words and five numbers in hex.
const LINE: usize = 160;

/// Writes the account of a fatal fault and passes every fault on.
unsafe extern "system" fn report(pointers: *mut EXCEPTION_POINTERS) -> i32 {
    // SAFETY: the system calls a vectored handler with the fault's EXCEPTION_POINTERS, which it
    // keeps valid for the call, its record among it (AddVectoredExceptionHandler's contract); a
    // null pointer is read as none.
    let record = unsafe { pointers.as_ref().and_then(|p| p.ExceptionRecord.as_ref()) };
    let Some(record) = record else {
        return CONTINUE_SEARCH;
    };
    if !FATAL.contains(&record.ExceptionCode) {
        return CONTINUE_SEARCH;
    }
    // An access violation's first two parameters: whether it read (0), wrote (1) or executed (8),
    // and the address it touched (winnt.h, EXCEPTION_RECORD).
    let [access, touched, ..] = record.ExceptionInformation;
    // The first line is written from the stack, so a fault inside the allocator still says it.
    let mut line = [0u8; LINE];
    let mut cursor = std::io::Cursor::new(&mut line[..]);
    let written = writeln!(
        cursor,
        "fault {:#010x} at {:p}: access {access:#x} of {touched:#x}, {} parameters",
        record.ExceptionCode.cast_unsigned(),
        record.ExceptionAddress,
        record.NumberParameters,
    );
    let end = usize::try_from(cursor.position()).unwrap_or(LINE);
    let mut stderr = std::io::stderr();
    if written.is_ok()
        && let Some(first) = line.get(..end)
    {
        let _ = stderr.write_all(first);
        let _ = stderr.flush();
    }
    // Then where it was: this allocates and takes stack, so a fault in the heap, or a stack that
    // overflowed, may end the process here, after the line above is out.
    let _ = writeln!(stderr, "{}", std::backtrace::Backtrace::force_capture());
    let _ = stderr.flush();
    CONTINUE_SEARCH
}

/// Installs the handler first in the process's list.
pub(super) fn install() {
    // SAFETY: `report` is a function of the program, valid for as long as the process runs; it
    // reads the record only for the call and returns CONTINUE_SEARCH, so the fault ends the
    // process as it would have. The handle returned is kept by the system, never removed.
    unsafe { AddVectoredExceptionHandler(1, Some(report)) };
}
