//! The member's log file: a real file on the machine's disk, read and written with direct I/O
//! where the file system takes it and flushed with the platform's full flush (hyper-block's
//! `DeviceFile`), whose next flush the test can make fail, as a device's can, or whose flushes it can
//! stall for good, as a device that stops.
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

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
