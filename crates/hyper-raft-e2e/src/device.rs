//! The device a test's members share, measured by the test itself (`docs/timing.md` §2.9): one
//! flush of the test's own file on it, timed, when a member's write has been out past what the
//! members' reported writes excuse.
//!
//! A member's write slower than any write before it is no evidence that the member is stuck when
//! the device is slow for everyone: on a machine whose other processes keep the device busy, every
//! flush on it waits, the test's as well. The test's stalls are made inside a member's process
//! (`hyper-durable-e2e`'s `FaultFile`), so the device answers the test's flush promptly while a
//! member's write is held; a device that is merely busy answers it as slowly as it answers the
//! members. The flush is the members' own (`File::sync_data`, the platform's full flush; `wal`).
//!
//! The flush is waited for at most [`FLUSH_BOUND`], on a thread of its own, one at a time.

use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::quiet::{Quiet, Stuck};

/// The longest a flush is waited for: past it the kernel itself has failed the request, Linux's
/// SCSI disk driver timing a flush out at `SD_TIMEOUT` (30 s) times `SD_FLUSH_TIMEOUT_MULTIPLIER`
/// (2), its NVMe driver any I/O at `nvme_io_timeout` (30 s); the larger, 60 s
/// (drivers/scsi/sd.h, drivers/nvme/host/core.c). A device that answers no flush in it has failed.
pub const FLUSH_BOUND: Duration = Duration::from_secs(60);

/// The test's file on the members' device.
#[derive(Debug)]
pub struct Probe {
    path: PathBuf,
}

impl Probe {
    /// A probe whose file is `path`, beside the members' logs.
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// One byte written to the file and flushed with the platform's full flush: how long the flush
    /// took, or, past [`FLUSH_BOUND`], `Err` with how long it was waited for.
    #[allow(
        clippy::disallowed_methods,
        reason = "the test's own device measurement: one thread at a time, on the host's clock (CLAUDE.md §1a, end to end)"
    )]
    pub fn measure(&self) -> Result<Duration, Duration> {
        let path = self.path.clone();
        let (tx, rx) = mpsc::channel();
        let began = Instant::now();
        std::thread::spawn(move || {
            let flushed = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .and_then(|mut file| {
                    file.write_all(&[0])?;
                    file.sync_data()
                });
            let _ = tx.send((flushed.is_ok(), began.elapsed()));
        });
        match rx.recv_timeout(FLUSH_BOUND) {
            Ok((true, took)) => Ok(took),
            Ok((false, took)) => Err(took),
            Err(_) => Err(began.elapsed()),
        }
    }
}

impl Drop for Probe {
    #[allow(
        clippy::disallowed_methods,
        reason = "the test's own file on the members' device, removed with the probe (CLAUDE.md §1a, end to end)"
    )]
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Whether `stuck` stands once the device is measured: a member silent, or with a write out, past
/// what the members' writes excuse is judged only after a flush of the test's own began since the
/// silence or the write did. Until then the device is measured (`measured` notes when), its time
/// joins the excuse ([`Quiet::device`]) and the watch goes on (`None`); a device that answered no
/// flush within [`FLUSH_BOUND`] is the failure ([`Stuck::Device`]).
pub fn reconsider(
    stuck: Stuck,
    probe: &Probe,
    measured: &mut Option<Instant>,
    quiet: &mut Quiet,
    now: Instant,
) -> Option<Stuck> {
    let began = match &stuck {
        Stuck::Silent { silence, .. } => now.checked_sub(*silence),
        Stuck::Held { writing, .. } => now.checked_sub(*writing),
        Stuck::Quiet(_) | Stuck::Device { .. } => return Some(stuck),
    };
    // Measured since it began: the member is slower than its device, and stuck.
    if measured.zip(began).is_some_and(|(at, began)| at >= began) {
        return Some(stuck);
    }
    *measured = Some(now);
    match probe.measure() {
        Ok(took) => {
            quiet.device(took);
            None
        }
        Err(waited) => Some(Stuck::Device { waited }),
    }
}
