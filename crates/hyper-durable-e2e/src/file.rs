//! The member's log file: a real file on the machine's disk, read and written with direct I/O
//! where the file system takes it and flushed with the platform's full flush (hyper-block's
//! `DeviceFile`), whose next flush the test can make fail, as a device's can, whose flushes it can
//! hold for a time, as a device a machine's processes share holds them all, or whose flushes it can
//! stall for good, as a device that stops.
use std::path::Path;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use hyper_block::DiskError;
use hyper_block::block::BlockFile;
use hyper_block::buf::Alignment;
use hyper_block::file::{CachingRequest, DeviceFile};

/// The direct-I/O alignment the file is opened with: the 4 KiB every device this repository runs
/// on reads and writes in (hyper-log-e2e's `BLOCK`, mantle `docs/design/raft-log.md` §2).
pub const BLOCK: usize = 4096;

/// Set by the test (`control::Order::FailFlush`): the file's next flush fails. One flag a
/// process, read and cleared by the log's device thread, set by the member's thread: a swap with
/// acquire-release ordering hands the order over once, and nothing else rides on it.
static FAIL_NEXT_FLUSH: AtomicBool = AtomicBool::new(false);

/// Makes the next flush of this process's log file fail.
pub fn fail_next_flush() {
    FAIL_NEXT_FLUSH.store(true, Ordering::Release);
}

/// Set by the test (`control::Order::StallFlush`): no flush of the file completes again. Read by
/// the log's device thread, set once by the member's thread; nothing else rides on it.
static STALL: AtomicBool = AtomicBool::new(false);

/// Stalls every flush of this process's log file from now on.
pub fn stall_flushes() {
    STALL.store(true, Ordering::Release);
}

/// When the process's clock for the hold began: the first hold the test ordered.
static ORIGIN: OnceLock<Instant> = OnceLock::new();

/// Set by the test (`hyper_raft_e2e::stream::put_stall`): until when, in nanoseconds since
/// [`ORIGIN`], the file answers no flush. Read by whichever thread does the log's device job, set
/// by the member's thread; a store with release ordering hands the time over, and nothing else
/// rides on it.
static HELD_UNTIL: AtomicU64 = AtomicU64::new(0);

/// Holds every flush of this process's log file for `hold` from now: a flush begun meanwhile ends
/// when the hold does, as on a device a machine's processes share, which holds them all at once
/// (Docker Desktop's virtual machine held every member's flush 1.8 s together,
/// `docs/timing.md` §2.9).
#[allow(
    clippy::disallowed_methods,
    reason = "the harness's device keeps the host's clock for a hold the test asked for"
)]
pub fn hold_flushes(hold: Duration) {
    let origin = *ORIGIN.get_or_init(Instant::now);
    let until = Instant::now()
        .saturating_duration_since(origin)
        .saturating_add(hold);
    HELD_UNTIL.store(
        u64::try_from(until.as_nanos()).unwrap_or(u64::MAX),
        Ordering::Release,
    );
}

/// Holds the calling thread, the one doing a flush, until the hold the test ordered ends, if one
/// was ordered and has not.
#[allow(
    clippy::disallowed_methods,
    reason = "the harness's device holds its thread through a hold the test asked for, as a device that stalls holds it"
)]
fn held() {
    let Some(origin) = ORIGIN.get() else {
        return;
    };
    let until = Duration::from_nanos(HELD_UNTIL.load(Ordering::Acquire));
    let left = until.saturating_sub(Instant::now().saturating_duration_since(*origin));
    if !left.is_zero() {
        std::thread::sleep(left);
    }
}

/// The log's file, whose next flush fails when the test says so.
#[derive(Debug)]
pub struct FaultFile(DeviceFile);

impl FaultFile {
    /// The file at `path`, created when `create`.
    pub fn open(path: &Path, create: bool) -> Result<Self, DiskError> {
        let align = Alignment::new(BLOCK).map_err(DiskError::Buf)?;
        DeviceFile::open(path, create, CachingRequest::PreferDirect, align).map(Self)
    }
}

impl BlockFile for FaultFile {
    fn alignment(&self) -> Alignment {
        BlockFile::alignment(&self.0)
    }
    fn len(&self) -> Result<u64, DiskError> {
        BlockFile::len(&self.0)
    }
    fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> Result<(), DiskError> {
        BlockFile::read_exact_at(&self.0, buf, offset)
    }
    fn write_all_at(&self, buf: &[u8], offset: u64) -> Result<(), DiskError> {
        BlockFile::write_all_at(&self.0, buf, offset)
    }
    fn sync_data(&self) -> Result<(), DiskError> {
        // A disk that stopped: the device thread holds its flush for as long as the process
        // lives, as a device that never answers holds its thread.
        while STALL.load(Ordering::Acquire) {
            std::thread::park();
        }
        held();
        if FAIL_NEXT_FLUSH.swap(false, Ordering::AcqRel) {
            return Err(DiskError::Io {
                op: "flush",
                path: self.0.path().to_path_buf(),
                source: std::io::Error::other("a flush the test made fail"),
            });
        }
        BlockFile::sync_data(&self.0)
    }
}
