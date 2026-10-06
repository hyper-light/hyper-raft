//! Signals (docs/runtime.md §6.1): a task subscribes to the kinds it wants and awaits them.
//!
//! - **Unix**: one process-wide `sigaction` handler per kind, installed at the first subscription to it,
//!   does only what is async-signal-safe [SELFPIPE]: it sets the kind's bit in an atomic word and writes one
//!   byte to a non-blocking pipe, a full pipe ignored (the bit already records the signal). One signal thread
//!   reads the pipe, swaps the bits to zero, and wakes each subscriber of a kind that arrived.
//! - **Windows**: `SetConsoleCtrlHandler`; the OS runs the handler on a thread of its own, which marks and
//!   wakes the subscribers directly. A kind with a subscriber is handled (the default action, ending the
//!   process, does not run); one without is left to the default.
//!
//! **Bounded**: subscribers live in a process-wide table of [`crate::registry::MAX_SHARDS`] slots (DERIVED:
//! one subscription per shard covers its tasks, for every kind); past it, `Capacity`. A signal arriving
//! several times between awaits is one event, as POSIX signals already coalesce. A subscription is a
//! slot index, not shared ownership: dropping it frees the slot.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::task::{Context, Poll};

use crate::error::RtError;
use crate::mem::Encoded;
use crate::registry::{self, MAX_SHARDS};
use crate::waker::polling_task;

/// A kind of signal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signal {
    /// `SIGINT`; Windows' Ctrl-C.
    Interrupt,
    /// `SIGTERM` (Unix).
    Terminate,
    /// `SIGHUP` (Unix).
    Hangup,
    /// Windows' Ctrl-Break.
    Break,
    /// Windows' console close.
    Close,
    /// Windows' user logoff (delivered to services).
    Logoff,
    /// Windows' system shutdown (delivered to services).
    Shutdown,
}

impl Signal {
    /// The kind's bit in a set.
    const fn bit(self) -> u32 {
        match self {
            Self::Interrupt => 1,
            Self::Terminate => 1 << 1,
            Self::Hangup => 1 << 2,
            Self::Break => 1 << 3,
            Self::Close => 1 << 4,
            Self::Logoff => 1 << 5,
            Self::Shutdown => 1 << 6,
        }
    }

    /// Every kind.
    const ALL: [Signal; 7] = [
        Self::Interrupt,
        Self::Terminate,
        Self::Hangup,
        Self::Break,
        Self::Close,
        Self::Logoff,
        Self::Shutdown,
    ];
}

/// A set of kinds that arrived.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Signals(u32);

impl Signals {
    /// Whether `kind` is in the set.
    pub fn contains(self, kind: Signal) -> bool {
        self.0 & kind.bit() != 0
    }

    /// The kinds in the set.
    pub fn iter(self) -> impl Iterator<Item = Signal> {
        Signal::ALL
            .into_iter()
            .filter(move |kind| self.contains(*kind))
    }
}

/// Format: "no task waits" in a slot's waiter word (no task word is all ones; see `sync::cell`).
const NO_WAITER: u64 = u64::MAX;

/// One subscription's place in the table.
struct Slot {
    /// The kinds subscribed; 0 for a free slot.
    mask: AtomicU32,
    /// The kinds that arrived since the subscriber last looked.
    pending: AtomicU32,
    /// The waiting task's word.
    waiter: AtomicU64,
}

static SLOTS: [Slot; MAX_SHARDS] = [const {
    Slot {
        mask: AtomicU32::new(0),
        pending: AtomicU32::new(0),
        waiter: AtomicU64::new(NO_WAITER),
    }
}; MAX_SHARDS];

/// Marks `arrived` on every subscriber of those kinds and wakes it; the kinds some subscriber wanted.
fn deliver(arrived: u32) -> u32 {
    let mut wanted = 0;
    for slot in &SLOTS {
        let hit = slot.mask.load(Ordering::Acquire) & arrived;
        if hit == 0 {
            continue;
        }
        wanted |= hit;
        slot.pending.fetch_or(hit, Ordering::AcqRel);
        let waiter = slot.waiter.swap(NO_WAITER, Ordering::AcqRel);
        if waiter != NO_WAITER {
            registry::wake(Encoded::from_word(waiter));
        }
    }
    wanted
}

/// A subscription to some kinds of signal.
#[derive(Debug)]
pub struct SignalStream {
    index: usize,
}

/// Subscribes to `kinds`, installing their handlers on first use. Refused `Capacity` when the table is
/// full, `BadConfig` for a kind this OS does not deliver (`Terminate`/`Hangup` on Windows; the console
/// kinds on Unix).
pub fn subscribe(kinds: &[Signal]) -> Result<SignalStream, RtError> {
    let mask = kinds.iter().fold(0u32, |mask, kind| mask | kind.bit());
    if mask == 0 || mask & !os::DELIVERED != 0 {
        return Err(RtError::BadConfig {
            what: "a signal kind this OS does not deliver",
        });
    }
    os::install(mask)?;
    for (index, slot) in SLOTS.iter().enumerate() {
        if slot
            .mask
            .compare_exchange(0, mask, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            slot.pending.store(0, Ordering::Release);
            slot.waiter.store(NO_WAITER, Ordering::Release);
            return Ok(SignalStream { index });
        }
    }
    Err(RtError::Capacity {
        what: "signal subscriptions",
        bound: MAX_SHARDS,
    })
}

impl SignalStream {
    /// Waits for the next signals of the subscribed kinds: the set that arrived since the last wait.
    pub fn recv(&self) -> Recv<'_> {
        Recv { stream: self }
    }

    fn slot(&self) -> Option<&'static Slot> {
        SLOTS.get(self.index)
    }
}

impl Drop for SignalStream {
    fn drop(&mut self) {
        if let Some(slot) = self.slot() {
            slot.waiter.store(NO_WAITER, Ordering::Release);
            slot.mask.store(0, Ordering::Release);
        }
    }
}

/// The wait of [`SignalStream::recv`].
#[derive(Debug)]
pub struct Recv<'a> {
    stream: &'a SignalStream,
}

impl Future for Recv<'_> {
    type Output = Result<Signals, RtError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let Some(slot) = self.stream.slot() else {
            return Poll::Ready(Err(RtError::BadConfig {
                what: "a signal subscription outside the table",
            }));
        };
        let arrived = slot.pending.swap(0, Ordering::AcqRel);
        if arrived != 0 {
            return Poll::Ready(Ok(Signals(arrived)));
        }
        let Some(word) = polling_task(cx.waker()) else {
            return Poll::Ready(Err(RtError::NotOnShardThread));
        };
        // Register, then look again: a delivery between the first look and the registration marked
        // `pending` before it read the waiter, so this second look sees it.
        slot.waiter.store(word.word(), Ordering::Release);
        let arrived = slot.pending.swap(0, Ordering::AcqRel);
        if arrived != 0 {
            return Poll::Ready(Ok(Signals(arrived)));
        }
        Poll::Pending
    }
}

// ============================================================================== Unix

#[cfg(unix)]
mod os {
    #![allow(unsafe_code)]

    use std::os::fd::{AsRawFd, OwnedFd};
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};

    use super::{Signal, deliver};
    use crate::driver::refused;
    use crate::error::RtError;

    /// The kinds Unix delivers.
    pub(super) const DELIVERED: u32 =
        Signal::Interrupt.bit() | Signal::Terminate.bit() | Signal::Hangup.bit();

    /// The kinds that arrived since the signal thread last looked (the handler's half).
    static RAW: AtomicU32 = AtomicU32::new(0);
    /// The pipe's write end, for the handler; -1 before the first installation.
    static WRITE: AtomicI32 = AtomicI32::new(-1);
    /// The kinds whose handler is installed.
    static INSTALLED: AtomicU32 = AtomicU32::new(0);
    /// The pipe, made once with the signal thread.
    static PIPE: OnceLock<Result<OwnedFd, RtError>> = OnceLock::new();

    fn number(kind: Signal) -> Option<libc::c_int> {
        match kind {
            Signal::Interrupt => Some(libc::SIGINT),
            Signal::Terminate => Some(libc::SIGTERM),
            Signal::Hangup => Some(libc::SIGHUP),
            _ => None,
        }
    }

    /// The handler: async-signal-safe only — atomics and `write(2)` (POSIX.1-2017 §2.4.3).
    extern "C" fn on_signal(signal: libc::c_int) {
        // `write(2)` to a full pipe sets errno, which the interrupted thread may be about to read for its own
        // failed call: saved and restored (mantle's review, finding 6).
        let errno = errno_location();
        // SAFETY: the calling thread's errno, a live thread-local int.
        let saved = unsafe { *errno };
        let bit = super::Signal::ALL
            .iter()
            .find(|kind| number(**kind) == Some(signal))
            .map_or(0, |kind| kind.bit());
        RAW.fetch_or(bit, Ordering::AcqRel);
        let fd = WRITE.load(Ordering::Acquire);
        if fd >= 0 {
            let byte = 1u8;
            // SAFETY: `fd` is the pipe's write end, open for the process's life; one byte from a live local.
            // A full pipe fails `EAGAIN`, ignored: the bit already records the signal.
            let _ = unsafe { libc::write(fd, (&raw const byte).cast(), 1) };
        }
        // SAFETY: as above.
        unsafe { *errno = saved };
    }

    /// The calling thread's errno.
    #[cfg(target_os = "linux")]
    fn errno_location() -> *mut libc::c_int {
        // SAFETY: a pure accessor of the thread's errno, async-signal-safe.
        unsafe { libc::__errno_location() }
    }

    /// The calling thread's errno.
    #[cfg(not(target_os = "linux"))]
    fn errno_location() -> *mut libc::c_int {
        // SAFETY: a pure accessor of the thread's errno, async-signal-safe.
        unsafe { libc::__error() }
    }

    /// Makes the pipe and starts the signal thread, once.
    fn pipe() -> Result<(), RtError> {
        let made = PIPE.get_or_init(|| {
            let (read, write) = rustix::pipe::pipe().map_err(|e| refused("pipe", e))?;
            for fd in [&read, &write] {
                rustix::io::fcntl_setfd(fd, rustix::io::FdFlags::CLOEXEC)
                    .map_err(|e| refused("fcntl(CLOEXEC)", e))?;
            }
            rustix::io::ioctl_fionbio(&write, true).map_err(|e| refused("ioctl(FIONBIO)", e))?;
            #[allow(
                clippy::disallowed_methods,
                reason = "the process's signal thread (docs/runtime.md §6.1), started once for its life"
            )]
            let spawned = std::thread::Builder::new()
                .name("hyper-rt-signals".to_owned())
                .spawn(move || listen(&read));
            spawned.map_err(|_| RtError::BadConfig {
                what: "the OS refused the signal thread",
            })?;
            WRITE.store(write.as_raw_fd(), Ordering::Release);
            Ok(write)
        });
        made.as_ref().map(|_| ()).map_err(Clone::clone)
    }

    /// The signal thread: waits on the pipe, then hands what arrived to the subscribers.
    fn listen(read: &OwnedFd) {
        let mut buf = [0u8; 64];
        loop {
            match rustix::io::read(read, &mut buf) {
                Ok(0) => return,
                Ok(_) | Err(rustix::io::Errno::INTR) => {
                    let arrived = RAW.swap(0, Ordering::AcqRel);
                    if arrived != 0 {
                        let wanted = deliver(arrived);
                        default_action(arrived & !wanted);
                    }
                }
                Err(_) => return,
            }
        }
    }

    /// The kinds in `unwanted` arrived with no subscriber left: each gets its default action back and is raised
    /// again, so a SIGINT or SIGTERM with nobody listening ends the process as it would have (mantle's review,
    /// finding 7). A later subscription installs the handler again.
    fn default_action(unwanted: u32) {
        for kind in Signal::ALL {
            let bit = kind.bit();
            if unwanted & bit == 0 {
                continue;
            }
            let Some(signal) = number(kind) else {
                continue;
            };
            INSTALLED.fetch_and(!bit, Ordering::AcqRel);
            // SAFETY: an all-zero `sigaction` is a valid value; SIG_DFL restores the default action.
            let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
            action.sa_sigaction = libc::SIG_DFL;
            // SAFETY: a valid action for a valid signal number; no previous action is read. `raise` sends the
            // signal to this thread, whose default action now runs.
            unsafe {
                libc::sigaction(signal, &raw const action, std::ptr::null_mut());
                libc::raise(signal);
            }
        }
    }

    /// Installs the handlers of the kinds in `mask` not yet installed.
    pub(super) fn install(mask: u32) -> Result<(), RtError> {
        pipe()?;
        for kind in Signal::ALL {
            let bit = kind.bit();
            if mask & bit == 0 || INSTALLED.fetch_or(bit, Ordering::AcqRel) & bit != 0 {
                continue;
            }
            let Some(signal) = number(kind) else {
                continue;
            };
            // SAFETY: an all-zero `sigaction` is a valid value of the C struct; the handler is set below.
            let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
            action.sa_sigaction = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
            action.sa_flags = libc::SA_RESTART;
            // SAFETY: `action` names a handler that is async-signal-safe (above), with an empty mask and
            // `SA_RESTART`, so interrupted system calls of other threads resume; no previous action is read.
            if unsafe { libc::sigaction(signal, &raw const action, std::ptr::null_mut()) } != 0 {
                INSTALLED.fetch_and(!bit, Ordering::AcqRel);
                return Err(RtError::os("sigaction"));
            }
        }
        Ok(())
    }
}

// ============================================================================== Windows

#[cfg(windows)]
mod os {
    #![allow(unsafe_code)]

    use std::sync::atomic::{AtomicBool, Ordering};

    use windows_sys::Win32::System::Console::{
        CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT,
        SetConsoleCtrlHandler,
    };

    use super::{Signal, deliver};
    use crate::error::RtError;

    /// The kinds Windows delivers.
    pub(super) const DELIVERED: u32 = Signal::Interrupt.bit()
        | Signal::Break.bit()
        | Signal::Close.bit()
        | Signal::Logoff.bit()
        | Signal::Shutdown.bit();

    static INSTALLED: AtomicBool = AtomicBool::new(false);

    /// The console control handler, on the OS's thread: handles a kind some subscriber wants.
    unsafe extern "system" fn on_control(event: u32) -> windows_sys::core::BOOL {
        let kind = match event {
            CTRL_C_EVENT => Signal::Interrupt,
            CTRL_BREAK_EVENT => Signal::Break,
            CTRL_CLOSE_EVENT => Signal::Close,
            CTRL_LOGOFF_EVENT => Signal::Logoff,
            CTRL_SHUTDOWN_EVENT => Signal::Shutdown,
            _ => return 0,
        };
        i32::from(deliver(kind.bit()) != 0)
    }

    pub(super) fn install(_mask: u32) -> Result<(), RtError> {
        if INSTALLED.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        // SAFETY: registers a handler the OS calls on a thread of its own; the function lives for the
        // process.
        if unsafe { SetConsoleCtrlHandler(Some(on_control), 1) } == 0 {
            INSTALLED.store(false, Ordering::Release);
            return Err(RtError::os("SetConsoleCtrlHandler"));
        }
        Ok(())
    }
}
