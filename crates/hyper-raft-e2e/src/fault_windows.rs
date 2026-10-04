//! The Windows half of [`super`]: a vectored exception handler that writes a fatal fault's account
//! to standard error and passes the fault on.
//!
//! A vectored handler runs before the faulting frame's own handlers, so a fault that code goes on
//! to handle itself is written too; the account that explains an ending is the last one written.
//!
//! The account names the fault, the image the instruction is in and its offset there, the thread's
//! registers and stack at the fault, the faulting frame's words, and the code bytes around the
//! instruction, then a backtrace. A backtrace names the function; the registers and the instruction
//! name the register that held the address touched, and the value it held; the frame's words show
//! where that value came from and what lies beside it.
#![allow(unsafe_code)]

use std::ffi::c_void;
use std::io::{Cursor, Stderr, Write};

use windows_sys::Win32::Foundation::{
    EXCEPTION_ACCESS_VIOLATION, EXCEPTION_DATATYPE_MISALIGNMENT, EXCEPTION_ILLEGAL_INSTRUCTION,
    EXCEPTION_IN_PAGE_ERROR, EXCEPTION_PRIV_INSTRUCTION, EXCEPTION_STACK_OVERFLOW, NTSTATUS,
};
use windows_sys::Win32::System::Diagnostics::Debug::{
    AddVectoredExceptionHandler, CONTEXT, EXCEPTION_POINTERS, EXCEPTION_RECORD,
};
use windows_sys::Win32::System::Memory::{
    MEM_COMMIT, MEMORY_BASIC_INFORMATION, PAGE_EXECUTE_READ, PAGE_EXECUTE_READWRITE,
    PAGE_EXECUTE_WRITECOPY, PAGE_GUARD, PAGE_NOACCESS, PAGE_READONLY, PAGE_READWRITE,
    PAGE_WRITECOPY, VirtualQuery,
};
use windows_sys::Win32::System::Threading::GetCurrentThreadStackLimits;

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

/// The bytes a line of the account takes at most. The first is the longest, 111: its fixed words,
/// 37 bytes with the line's end; the code, `0x` and eight hex digits; three addresses of `0x` and up
/// to sixteen; and a count of up to ten decimal digits. A line of registers takes 107 at most (four
/// of a name of up to six letters, a space and eighteen characters, two spaces between), a line of
/// the frame's words 102 (`words `, an address, a colon, four words of eighteen characters after a
/// space), the image's 65, a line of code 73 (`code `, an address, a colon, sixteen bytes) and the
/// stack's 45.
const LINE: usize = 111;

/// The registers written a line.
const REGISTERS_A_LINE: usize = 4;

/// Code bytes written before the faulting instruction: eight aarch64 instructions, where the load
/// of the register a store or load faulted through is, in a build without optimisation, which loads
/// each operand from its stack slot just before the instruction that uses it.
const CODE_BEFORE: usize = 32;

/// Code bytes written from the faulting instruction: x86_64's longest instruction is fifteen bytes
/// (Intel SDM Vol. 2, §2.3.11), so the instruction is whole here; aarch64's are four bytes, so this
/// is the instruction and the three after it.
const CODE_FROM: usize = 16;

/// Code bytes written a line.
const CODE_A_LINE: usize = 16;

/// Bytes of the stack written from the faulting frame's stack pointer up. `Node::drain`'s frame
/// measured 0x270 bytes from its stack pointer to its frame pointer on windows-11-arm (run
/// 37208590794): a KiB holds it whole, with its frame record and the bottom of its caller's. Below
/// the stack pointer the system has already laid the exception's records, so nothing there is the
/// faulting thread's own.
const FRAME_BYTES: usize = 1024;

/// Stack words written a line.
const WORDS_A_LINE: usize = 4;

/// The page protections a read is allowed under (memoryapi.h's memory protection constants).
const READABLE: u32 = PAGE_READONLY
    | PAGE_READWRITE
    | PAGE_WRITECOPY
    | PAGE_EXECUTE_READ
    | PAGE_EXECUTE_READWRITE
    | PAGE_EXECUTE_WRITECOPY;

/// aarch64's general registers in the order the context holds them, x29 and x30 by their roles.
#[cfg(target_arch = "aarch64")]
const GENERAL: [&str; 31] = [
    "x0", "x1", "x2", "x3", "x4", "x5", "x6", "x7", "x8", "x9", "x10", "x11", "x12", "x13", "x14",
    "x15", "x16", "x17", "x18", "x19", "x20", "x21", "x22", "x23", "x24", "x25", "x26", "x27",
    "x28", "fp", "lr",
];

/// Writes the account of a fatal fault and passes every fault on.
unsafe extern "system" fn report(pointers: *mut EXCEPTION_POINTERS) -> i32 {
    // SAFETY: the system calls a vectored handler with the fault's EXCEPTION_POINTERS, which it
    // keeps valid for the call, the record and the thread's context among it
    // (AddVectoredExceptionHandler's contract); a null pointer is read as none.
    let (record, context) = unsafe {
        let pointers = pointers.as_ref();
        (
            pointers.and_then(|p| p.ExceptionRecord.as_ref()),
            pointers.and_then(|p| p.ContextRecord.as_ref()),
        )
    };
    let Some(record) = record else {
        return CONTINUE_SEARCH;
    };
    if !FATAL.contains(&record.ExceptionCode) {
        return CONTINUE_SEARCH;
    }
    let mut stderr = std::io::stderr();
    account(&mut stderr, record, context);
    let _ = stderr.flush();
    // Then where it was: this allocates and takes stack, so a fault in the heap, or a stack that
    // overflowed, may end the process here, after the account is out.
    let _ = writeln!(stderr, "{}", std::backtrace::Backtrace::force_capture());
    let _ = stderr.flush();
    CONTINUE_SEARCH
}

/// The fault, the image, the registers, the stack and the code, each line written from the
/// stack, so a fault inside the allocator still gets its account out.
fn account(stderr: &mut Stderr, record: &EXCEPTION_RECORD, context: Option<&CONTEXT>) {
    // An access violation's first two parameters: whether it read (0), wrote (1) or executed (8),
    // and the address it touched (winnt.h, EXCEPTION_RECORD).
    let [access, touched, ..] = record.ExceptionInformation;
    line(stderr, |out| {
        writeln!(
            out,
            "fault {:#010x} at {:p}: access {access:#x} of {touched:#x}, {} parameters",
            record.ExceptionCode.cast_unsigned(),
            record.ExceptionAddress,
            record.NumberParameters,
        )
    });
    let at = record.ExceptionAddress.cast::<u8>().cast_const();
    let code = readable(at.cast());
    if let Some(region) = code {
        line(stderr, |out| {
            writeln!(
                out,
                "image {:#x}, the instruction at +{:#x}",
                region.base,
                at.addr().wrapping_sub(region.base),
            )
        });
    }
    if let Some(context) = context {
        write_registers(stderr, context);
        write_frame(stderr, stack_pointer(context));
    }
    let (mut low, mut high) = (0usize, 0usize);
    // SAFETY: both are writable `usize`s for the call, which writes each once with the bounds of
    // the calling thread's stack (processthreadsapi.h); the handler runs on the faulting thread.
    unsafe { GetCurrentThreadStackLimits(&raw mut low, &raw mut high) };
    line(stderr, |out| writeln!(out, "stack {low:#x}..{high:#x}"));
    if let Some(region) = code {
        write_code(stderr, at, region);
    }
}

/// The thread's registers at the fault, [`REGISTERS_A_LINE`] a line.
fn write_registers(stderr: &mut Stderr, context: &CONTEXT) {
    let mut names = registers(context).peekable();
    while names.peek().is_some() {
        line(stderr, |out| {
            for (index, (name, value)) in names.by_ref().take(REGISTERS_A_LINE).enumerate() {
                let gap = if index == 0 { "" } else { "  " };
                write!(out, "{gap}{name} {value:#018x}")?;
            }
            writeln!(out)
        });
    }
}

/// The faulting frame's words from the stack pointer `sp` up, [`WORDS_A_LINE`] a line after the
/// address of the first, as many of [`FRAME_BYTES`] as lie in the stack's readable region.
fn write_frame(stderr: &mut Stderr, sp: usize) {
    let first = std::ptr::with_exposed_provenance::<u64>(sp);
    let Some(region) = readable(first.cast()) else {
        return;
    };
    let word = size_of::<u64>();
    let end = sp.saturating_add(FRAME_BYTES).min(region.end);
    let words = end.saturating_sub(sp).checked_div(word).unwrap_or(0);
    let mut at = 0;
    while at < words {
        let start = sp.saturating_add(at.saturating_mul(word));
        line(stderr, |out| {
            write!(out, "words {start:#x}:")?;
            for index in at..words.min(at.saturating_add(WORDS_A_LINE)) {
                // SAFETY: the word lies in `[sp, end)`, inside the committed, readable region that
                // holds the stack pointer, and is aligned, the stack pointer being so (a word on
                // x86_64, sixteen bytes on aarch64). The faulting thread is stopped in this handler,
                // which runs below the stack pointer, so nothing writes these words while they are
                // read, by address and as plain integers.
                let value = unsafe { first.wrapping_add(index).read_volatile() };
                write!(out, " {value:#018x}")?;
            }
            writeln!(out)
        });
        at = at.saturating_add(WORDS_A_LINE);
    }
}

/// The code bytes around the instruction at `at`, as much of them as lies in its readable
/// `region`, [`CODE_A_LINE`] a line, each line after the address of its first byte.
fn write_code(stderr: &mut Stderr, at: *const u8, region: Region) {
    let from = at.addr().saturating_sub(CODE_BEFORE).max(region.start);
    let to = at.addr().saturating_add(CODE_FROM).min(region.end);
    let first = at.with_addr(from);
    // SAFETY: `[from, to)` lies in one region of committed pages that VirtualQuery found
    // readable, and holds the code the thread was running, which nothing writes while it runs.
    let bytes = unsafe { std::slice::from_raw_parts(first, to.saturating_sub(from)) };
    for (index, chunk) in bytes.chunks(CODE_A_LINE).enumerate() {
        let start = from.saturating_add(index.saturating_mul(CODE_A_LINE));
        line(stderr, |out| {
            write!(out, "code {start:#x}:")?;
            for byte in chunk {
                write!(out, " {byte:02x}")?;
            }
            writeln!(out)
        });
    }
}

/// Writes one line of the account from a buffer on the stack; a line that does not fit is cut at
/// [`LINE`].
fn line(stderr: &mut Stderr, write: impl FnOnce(&mut Cursor<&mut [u8]>) -> std::io::Result<()>) {
    let mut line = [0u8; LINE];
    let mut cursor = Cursor::new(&mut line[..]);
    let _ = write(&mut cursor);
    let end = usize::try_from(cursor.position()).unwrap_or(LINE);
    if let Some(written) = line.get(..end) {
        let _ = stderr.write_all(written);
    }
}

/// A run of committed, readable pages, and the base of the allocation they belong to: an image's
/// base, for pages of an image.
#[derive(Clone, Copy)]
struct Region {
    start: usize,
    end: usize,
    base: usize,
}

/// The run of pages that holds `address`, when they are committed and readable.
fn readable(address: *const c_void) -> Option<Region> {
    let mut info = MEMORY_BASIC_INFORMATION::default();
    // SAFETY: `info` is a writable MEMORY_BASIC_INFORMATION and the length passed is its size;
    // VirtualQuery reads the process's page tables, not the memory at `address`, and writes
    // `info` alone before it returns (memoryapi.h).
    let written = unsafe {
        VirtualQuery(
            address,
            &raw mut info,
            size_of::<MEMORY_BASIC_INFORMATION>(),
        )
    };
    if written == 0
        || info.State != MEM_COMMIT
        || info.Protect & (PAGE_GUARD | PAGE_NOACCESS) != 0
        || info.Protect & READABLE == 0
    {
        return None;
    }
    let start = info.BaseAddress.addr();
    Some(Region {
        start,
        end: start.checked_add(info.RegionSize)?,
        base: info.AllocationBase.addr(),
    })
}

/// The thread's registers at the fault, by name: aarch64's 31 general registers, stack pointer,
/// program counter and status register.
#[cfg(target_arch = "aarch64")]
fn registers(context: &CONTEXT) -> impl Iterator<Item = (&'static str, u64)> {
    // SAFETY: both members of the union are the 31 general registers as `u64`s, x0 to x30
    // (winnt.h's ARM64 CONTEXT), and every bit pattern is a `u64`, so reading either is sound.
    let general = unsafe { context.Anonymous.X };
    GENERAL.into_iter().zip(general).chain([
        ("sp", context.Sp),
        ("pc", context.Pc),
        ("cpsr", u64::from(context.Cpsr)),
    ])
}

/// The thread's registers at the fault, by name: x86_64's sixteen general registers, instruction
/// pointer and flags.
#[cfg(target_arch = "x86_64")]
fn registers(context: &CONTEXT) -> impl Iterator<Item = (&'static str, u64)> {
    [
        ("rax", context.Rax),
        ("rbx", context.Rbx),
        ("rcx", context.Rcx),
        ("rdx", context.Rdx),
        ("rsi", context.Rsi),
        ("rdi", context.Rdi),
        ("rbp", context.Rbp),
        ("rsp", context.Rsp),
        ("r8", context.R8),
        ("r9", context.R9),
        ("r10", context.R10),
        ("r11", context.R11),
        ("r12", context.R12),
        ("r13", context.R13),
        ("r14", context.R14),
        ("r15", context.R15),
        ("rip", context.Rip),
        ("eflags", u64::from(context.EFlags)),
    ]
    .into_iter()
}

/// The faulting thread's stack pointer.
#[cfg(target_arch = "aarch64")]
fn stack_pointer(context: &CONTEXT) -> usize {
    usize::try_from(context.Sp).unwrap_or(0)
}

/// The faulting thread's stack pointer.
#[cfg(target_arch = "x86_64")]
fn stack_pointer(context: &CONTEXT) -> usize {
    usize::try_from(context.Rsp).unwrap_or(0)
}

/// No stack pointer is read from another architecture's context.
#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
fn stack_pointer(_context: &CONTEXT) -> usize {
    0
}

/// No registers are named for another architecture's context.
#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
fn registers(_context: &CONTEXT) -> impl Iterator<Item = (&'static str, u64)> {
    std::iter::empty()
}

/// Installs the handler first in the process's list.
pub(super) fn install() {
    // SAFETY: `report` is a function of the program, valid for as long as the process runs; it
    // reads the record and the context only for the call and returns CONTINUE_SEARCH, so the fault
    // ends the process as it would have. The handle returned is kept by the system, never removed.
    unsafe { AddVectoredExceptionHandler(1, Some(report)) };
}
