//! The member's run (`hyper_liveness::Settings::run`): a count it keeps beside its log and raises
//! at every start, before its liveness stream's first heartbeat, so each run's number is greater
//! than every earlier run's and a heartbeat of a superseded run, which the plane may deliver after
//! the new run's first, is refused as stale and never taken as another restart. The count is a
//! record kept whole (`hyper_block::record`: written to a temporary name, flushed with the
//! platform's full flush, renamed over the record, its directory flushed after), so a start that
//! crashed before its run was durable sent nothing under it, and the next start may take the same
//! number. A process id, which the operating system reuses, numbered a restarted member's run as
//! an earlier one's: its heartbeats, numbered from zero again, were stale for good.

use std::path::{Path, PathBuf};

use hyper_block::DiskError;
use hyper_block::record;

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
            Self::Disk(e) => write!(f, "the run record: {e}"),
            Self::Exhausted => f.write_str("the run record holds the largest run"),
        }
    }
}

impl std::error::Error for RunError {}

impl From<DiskError> for RunError {
    fn from(e: DiskError) -> Self {
        Self::Disk(e)
    }
}

/// Where the member keeps its run: beside its log, the log's name with `.run` added.
pub fn path(log: &Path) -> PathBuf {
    let mut name = log.as_os_str().to_owned();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_start_raises_the_run_kept_beside_the_log() {
        let dir = tempfile::tempdir().unwrap();
        let at = path(&dir.path().join("member.log"));
        assert_eq!(at, dir.path().join("member.log.run"));
        assert_eq!(raise(&at).unwrap(), 1);
        assert_eq!(raise(&at).unwrap(), 2);
        assert_eq!(raise(&at).unwrap(), 3);
    }
}
