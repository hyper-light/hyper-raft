//! Reads a submitter issues and reaps itself, with no thread between it and the device
//! (hyper-raft `docs/research/issuer-completions.md`).
//!
//! The device issuer ([`crate::issuer`]) carries a batch of reads through its own thread and a
//! pool of workers: four blocking-channel hand-offs, each a kernel wake under load. Its reads
//! bench measured a batch of four cached reads at 43 µs against 2.7 µs read in place, while a
//! batch of four device reads took half the time of four reads one after another
//! (docs/benchmarks.md, "hyper-block: a batch of reads through the issuer"). An [`AioReads`] keeps
//! the overlap and drops the hand-offs: on Linux it submits a batch's reads to the kernel's native
//! AIO (io_submit(2)) from the calling thread and takes their completions on the same thread
//! (io_getevents(2)), with a zero timeout for [`AioReads::try_answer`]. The owner's decision of
//! 2026-10-01 takes native AIO on Linux, not io_uring, whose attack surface was behind 60% of the
//! kernel exploits Google's kCTF paid for (research note §5).
//!
//! Native AIO is asynchronous only without the page cache: "libaio only supports unbuffered
//! accesses (i.e., with O_DIRECT)" (Didona et al., SYSTOR '22, §2), and a buffered read is done
//! inside io_submit. So an `AioReads` is made only over a file opened for direct I/O, and is
//! refused, typed ([`crate::DiskError::Unsupported`]), elsewhere and on every other OS: there the
//! caller reads cached pages in place and batches device reads through the issuer.
//!
//! Each read asks `RWF_NOWAIT` (Linux 4.14): a read that would block inside the kernel (a lock
//! the file system holds, a congested device) completes at once with `EAGAIN` rather than block
//! the submitter, and is then read in place on the submitter's thread. A kernel that takes no
//! `RWF_NOWAIT` refuses the submission with `EINVAL`; the reads are then submitted without it.
//!
//! Nothing is allocated per read: a batch's reads come back in the vector they were given in,
//! and the context's records are reserved once at its size.

use std::collections::VecDeque;
use std::path::PathBuf;

use crate::DiskError;
use crate::buf::AlignedBuf;
use crate::file::{Caching, DeviceFile};

#[cfg(target_os = "linux")]
mod linux;

/// A batch's reads: each buffer and the offset it is read from.
pub type Reads = Vec<(AlignedBuf, u64)>;

/// A batch's answer: its reads given back, every buffer filled, in the order given; or the first
/// failure among them.
pub type Answer = Result<Reads, DiskError>;

/// One batch out: its reads, how many are still out, and its first failure.
struct Batch {
    number: u64,
    reads: Reads,
    outstanding: usize,
    failed: Option<DiskError>,
}

/// A file's reads issued to the kernel's native AIO and reaped by the caller (module docs).
pub struct AioReads {
    /// Declared first, so dropped first: destroying the context waits for every read out before
    /// the batches that own their buffers are dropped.
    #[cfg(target_os = "linux")]
    ctx: linux::Context,
    file: DeviceFile,
    path: PathBuf,
    /// Batches the caller may have out at once.
    batches: usize,
    /// Batches out, in the order submitted.
    out: VecDeque<Batch>,
    /// Reads of the batches out not yet handed to the kernel: their tags, in order.
    waiting: VecDeque<u64>,
    /// Reads the kernel holds now: at most the context's size.
    in_flight: usize,
    next: u64,
}

impl std::fmt::Debug for AioReads {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AioReads")
            .field("path", &self.path)
            .field("batches", &self.batches)
            .field("out", &self.out.len())
            .field("in_flight", &self.in_flight)
            .finish_non_exhaustive()
    }
}

impl AioReads {
    /// Reads of `file`, at most `depth` in the kernel at once and `batches` batches out at once.
    /// Refused, typed, for a file not opened for direct I/O, for a depth or batch count of zero,
    /// and on every OS but Linux; `Io` where the kernel refuses the context (io_setup(2):
    /// `EAGAIN` past `/proc/sys/fs/aio-max-nr`).
    pub fn new(file: DeviceFile, depth: usize, batches: usize) -> Result<Self, DiskError> {
        let path = file.path().to_path_buf();
        let unsupported = |reason: &'static str| DiskError::Unsupported {
            path: path.clone(),
            reason,
        };
        if depth == 0 || batches == 0 {
            return Err(unsupported("native AIO reads with no depth or no batch"));
        }
        if file.caching() != Caching::Direct {
            return Err(unsupported(
                "native AIO reads only a file opened for direct I/O: a buffered read is done inside io_submit",
            ));
        }
        #[cfg(target_os = "linux")]
        {
            let ctx = linux::Context::new(depth).map_err(|source| DiskError::Io {
                op: "io_setup",
                path: path.clone(),
                source,
            })?;
            Ok(Self {
                ctx,
                file,
                path,
                batches,
                out: VecDeque::with_capacity(batches),
                waiting: VecDeque::with_capacity(depth),
                in_flight: 0,
                next: 0,
            })
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = file;
            Err(unsupported(
                "native AIO is Linux's: elsewhere the issuer batches device reads",
            ))
        }
    }

    /// Hands a batch of reads to the kernel and returns its number without waiting: each
    /// `(buffer, offset)` is filled, the whole of the buffer's length, from its offset. Every
    /// buffer and offset must meet the file's alignment. Refused with the batches allowed already
    /// out, before anything is submitted, and the reads are given back with the refusal. Reads past
    /// the depth wait in order and go to the kernel as earlier ones complete, at the next
    /// [`Self::answer`] or [`Self::try_answer`].
    pub fn submit_reads(&mut self, reads: Reads) -> Result<u64, (DiskError, Reads)> {
        if self.out.len() >= self.batches {
            let refused = DiskError::Unsupported {
                path: self.path.clone(),
                reason: "a batch past those allowed out; take an answer first",
            };
            return Err((refused, reads));
        }
        let align = self.file.alignment();
        if let Some((buf, at)) = reads
            .iter()
            .find(|(buf, at)| !align.is_aligned(buf.len()) || !align.is_aligned_u64(*at))
        {
            let refused = DiskError::Misaligned {
                offset: *at,
                len: buf.len(),
                align: align.get(),
            };
            return Err((refused, reads));
        }
        let number = self.next;
        self.next = self.next.wrapping_add(1);
        let count = reads.len();
        #[cfg(target_os = "linux")]
        for index in 0..count {
            self.waiting.push_back(linux::tag(number, index));
        }
        self.out.push_back(Batch {
            number,
            reads,
            outstanding: count,
            failed: None,
        });
        if let Err(e) = self.pump() {
            self.fail_waiting(&e);
        }
        Ok(number)
    }

    /// The next answer, waiting for it: a batch's number and its answer. Batches are answered as
    /// their reads end, in any order. Refused with no batch out.
    pub fn answer(&mut self) -> Result<(u64, Answer), DiskError> {
        loop {
            if let Some(done) = self.take_done() {
                return Ok(done);
            }
            if self.out.is_empty() {
                return Err(DiskError::Unsupported {
                    path: self.path.clone(),
                    reason: "an answer with no batch out",
                });
            }
            self.reap(true)?;
            if let Err(e) = self.pump() {
                self.fail_waiting(&e);
            }
        }
    }

    /// The next answer if one has come; none otherwise, or with no batch out. Never waits.
    pub fn try_answer(&mut self) -> Result<Option<(u64, Answer)>, DiskError> {
        if let Some(done) = self.take_done() {
            return Ok(Some(done));
        }
        if self.in_flight > 0 {
            self.reap(false)?;
            if let Err(e) = self.pump() {
                self.fail_waiting(&e);
            }
        }
        Ok(self.take_done())
    }

    /// Batches submitted and not yet answered.
    pub fn out(&self) -> usize {
        self.out.len()
    }

    /// The file read.
    pub fn file(&self) -> &DeviceFile {
        &self.file
    }

    /// A batch whose reads have all ended, taken out of the batches out.
    fn take_done(&mut self) -> Option<(u64, Answer)> {
        let at = self.out.iter().position(|batch| batch.outstanding == 0)?;
        let batch = self.out.remove(at)?;
        let answer = match batch.failed {
            Some(e) => Err(e),
            None => Ok(batch.reads),
        };
        Some((batch.number, answer))
    }

    /// Fails every read not yet handed to the kernel with `cause`: the submission was refused,
    /// and nothing of it is in flight.
    fn fail_waiting(&mut self, cause: &DiskError) {
        while let Some(tag) = self.waiting.pop_front() {
            #[cfg(target_os = "linux")]
            {
                let (number, _) = linux::untag(tag);
                if let Some(batch) = self
                    .out
                    .iter_mut()
                    .find(|batch| linux::same_number(batch.number, number))
                {
                    batch.outstanding = batch.outstanding.saturating_sub(1);
                    batch.failed.get_or_insert_with(|| DiskError::Unsupported {
                        path: self.path.clone(),
                        reason: match cause {
                            DiskError::Io { .. } => "io_submit refused the batch",
                            _ => "the batch was not submitted",
                        },
                    });
                }
            }
            #[cfg(not(target_os = "linux"))]
            let _ = (tag, cause);
        }
    }

    /// Hands waiting reads to the kernel up to its size.
    #[cfg(target_os = "linux")]
    fn pump(&mut self) -> Result<(), DiskError> {
        loop {
            let room = self.ctx.room().saturating_sub(self.in_flight);
            if room == 0 || self.waiting.is_empty() {
                return Ok(());
            }
            self.ctx.clear();
            let mut pushed = 0usize;
            while pushed < room {
                let Some(tag) = self.waiting.pop_front() else {
                    break;
                };
                let (number, index) = linux::untag(tag);
                let Some((buf, at)) = self
                    .out
                    .iter_mut()
                    .find(|batch| linux::same_number(batch.number, number))
                    .and_then(|batch| batch.reads.get_mut(index))
                else {
                    continue;
                };
                let bytes = buf.as_mut_slice();
                // The buffer's heap bytes stay where they are until the read ends: its batch
                // leaves `out` only once no read of it is out, and the context is dropped (and
                // waits for every read) before the batches are.
                if !self
                    .ctx
                    .push(tag, bytes.as_mut_ptr().addr(), bytes.len(), *at)
                {
                    self.waiting.push_front(tag);
                    break;
                }
                pushed = pushed.saturating_add(1);
            }
            let submitted =
                self.ctx
                    .submit(self.file.std_file())
                    .map_err(|source| DiskError::Io {
                        op: "io_submit",
                        path: self.path.clone(),
                        source,
                    })?;
            self.in_flight = self.in_flight.saturating_add(submitted);
            // What the kernel did not take goes back to the front, in order.
            let mut at = pushed;
            while at > submitted {
                at = at.saturating_sub(1);
                if let Some(tag) = self.ctx.pushed(at) {
                    self.waiting.push_front(tag);
                }
            }
            if submitted == 0 {
                return Ok(());
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn pump(&mut self) -> Result<(), DiskError> {
        Ok(())
    }

    /// Takes the kernel's completions, waiting for one when `wait` is set, and records each in
    /// its batch.
    #[cfg(target_os = "linux")]
    fn reap(&mut self, wait: bool) -> Result<(), DiskError> {
        let got = self
            .ctx
            .reap(wait && self.in_flight > 0)
            .map_err(|source| DiskError::Io {
                op: "io_getevents",
                path: self.path.clone(),
                source,
            })?;
        for at in 0..got {
            let Some(event) = self.ctx.event(at) else {
                break;
            };
            self.in_flight = self.in_flight.saturating_sub(1);
            let (number, index) = linux::untag(event.data);
            self.complete(number, index, event.res);
        }
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    fn reap(&mut self, _wait: bool) -> Result<(), DiskError> {
        Ok(())
    }

    /// Records that read `index` of batch `number` ended with `res`: bytes read, or a negated
    /// errno.
    #[cfg(target_os = "linux")]
    fn complete(&mut self, number: u64, index: usize, res: i64) {
        let path = &self.path;
        let file = &self.file;
        let Some(batch) = self
            .out
            .iter_mut()
            .find(|batch| linux::same_number(batch.number, number))
        else {
            return;
        };
        batch.outstanding = batch.outstanding.saturating_sub(1);
        let Some((buf, at)) = batch.reads.get_mut(index) else {
            return;
        };
        let len = buf.len();
        let outcome = match usize::try_from(res) {
            Ok(read) if read == len => Ok(()),
            Ok(read) => Err(DiskError::ShortRead {
                path: path.clone(),
                offset: *at,
                missing: len.saturating_sub(read),
            }),
            // A read the kernel would have blocked on (`RWF_NOWAIT`): read in place.
            Err(_) if res == -linux::EAGAIN => file.read_exact_at(buf.as_mut_slice(), *at),
            Err(_) => Err(DiskError::Io {
                op: "aio read",
                path: path.clone(),
                source: std::io::Error::from_raw_os_error(
                    i32::try_from(res.saturating_neg()).unwrap_or(i32::MAX),
                ),
            }),
        };
        if let Err(e) = outcome {
            batch.failed.get_or_insert(e);
        }
    }
}
