//! The member's run (`hyper_liveness::Settings::run`): a count it keeps beside its log and raises
//! at every start, before its liveness stream's first heartbeat, so each run's number is greater
//! than every earlier run's and a heartbeat of a superseded run, which a peer may read after the
//! new run's first, is refused as stale and never taken as another restart (`docs/timing.md` §2.8,
//! "A restart"). The count is a record kept whole (`hyper_block::record`: written to a temporary
//! name, flushed with the platform's full flush, renamed over the record, its directory flushed
//! after), so a start that crashed before its run was durable sent nothing under it, and the next
//! start may take the same number. hyper-durable-e2e's members keep theirs the same way, in the
//! same record.

use std::path::{Path, PathBuf};

use hyper_block::{DiskError, record};

/// The record's bytes: the count, little-endian.
const RECORD_BYTES: usize = 8;

/// Why the member has no run.
#[derive(Debug)]
pub enum RunError {
    /// The record could not be read or written, or does not hold what was written.
    Disk(DiskError),
    /// The record holds the largest count: no later run can be numbered.
    Exhausted,
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Disk(error) => write!(f, "the run record: {error}"),
            Self::Exhausted => f.write_str("the run record holds the largest run"),
        }
    }
}

impl std::error::Error for RunError {}

impl From<DiskError> for RunError {
    fn from(error: DiskError) -> Self {
        Self::Disk(error)
    }
}

/// Where the member keeps its run: beside its log, the log's name with `.run` added.
pub fn path(wal: &Path) -> PathBuf {
    let mut name = wal.as_os_str().to_owned();
    name.push(".run");
    PathBuf::from(name)
}

/// This start's run: the record's count raised by one, one where there is none, durable when it
/// is returned.
pub fn raise(path: &Path) -> Result<u64, RunError> {
    let previous = match record::read(path, RECORD_BYTES)? {
        None => 0,
        Some(bytes) => {
            let count = <[u8; RECORD_BYTES]>::try_from(bytes.as_slice()).map_err(|_| {
                DiskError::Corrupt {
                    path: path.to_path_buf(),
                    what: "a run record that is not one count",
                }
            })?;
            u64::from_le_bytes(count)
        }
    };
    let run = previous.checked_add(1).ok_or(RunError::Exhausted)?;
    record::write(path, &run.to_le_bytes())?;
    Ok(run)
}
