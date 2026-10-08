# A device issuer whose submitter reaps its own completions

The question: how a submitter on a latency path, an engine's seek above all, keeps several device
transfers in flight and takes their completions itself, with no thread woken between it and the
device. The answer has to fit the owner's standing decision of 2026-10-01: Linux disk I/O goes
through native AIO (`io_submit`/`io_getevents`), not io_uring; Windows uses IOCP; macOS keeps the
pool. This note collects the evidence that decision rests on, the measurement that makes the work
due, and the design it leads to. The design is written in `docs/durable.md`'s hyper-block section
once built; until then this note is the record.

## 1. Why now: the pool's hand-offs

`crates/hyper-block/benches/reads.rs` (docs/benchmarks.md, "hyper-block: a batch of reads through the
issuer", 2026-10-07, M5 Max, macOS, load 8.2–8.5) reads four 4 KiB pages per operation:

| | p50 µs | p99 µs |
|---|---|---|
| direct I/O, sequential `pread` | 298.17 | 381.58 |
| direct I/O, one batch through the pool issuer | 148.38 | 216.33 |
| page cache, sequential `pread` | 2.71 | 4.50 |
| page cache, one batch through the pool issuer | 43.21 | 99.50 |

The batch halves device reads but costs sixteen times a cached read. A batch crosses four
blocking-channel hand-offs, submitter → issuer thread → workers → issuer thread → submitter, and each
is a kernel wake under load. A seek that overlaps its device reads must not pay that. The fix is not a
faster hand-off but no hand-off: the submitter submits to the kernel and reaps from the kernel itself.
This is Haas and Leis's conclusion Q6: "I/O should be performed directly by worker threads"
(mantle research/03, HL23-F8, §3.4 and §6). Their measurements also show a single dedicated I/O
thread capping at 630k–820k IOPS (HL23-F8). The issuer thread is exactly that bottleneck.

## 2. Linux: native AIO

**The interface** (man-pages, io_setup(2), io_submit(2), io_getevents(2)):
- `io_setup(nr_events, &ctx)` "creates an asynchronous I/O context suitable for concurrently processing
  n operations". It fails with EAGAIN when "the specified n exceeds the limit of available events, as
  defined in /proc/sys/fs/aio-max-nr". The kernel's sysctl documentation adds: "If aio-nr reaches
  aio-nr-max then io_setup will fail with EAGAIN" (docs.kernel.org, admin-guide/sysctl/fs). A context
  is therefore a bounded, refusable resource, sized by the submitter's depth.
- `io_submit(ctx, n, iocbs)` "queues n I/O request blocks"; it "returns the number of iocbs submitted
  (which may be less than n…)". A partial submission is normal and must be handled: what was not
  submitted is still the caller's.
- Each `iocb` carries `aio_data`, "copied into the data field of the io_event structure upon I/O
  completion": the submitter's tag for the transfer, a batch number and an index here. The opcodes
  include `IOCB_CMD_PREAD`, `IOCB_CMD_PWRITE`, `IOCB_CMD_FSYNC`, `IOCB_CMD_FDSYNC`, `IOCB_CMD_PREADV`
  and `IOCB_CMD_PWRITEV`.
- `io_getevents(ctx, min_nr, nr, events, timeout)` "attempts to read at least min_nr events and up to
  nr events". It "returns the number of events read. This may be 0 … if the timeout expired". A zero
  timeout with `min_nr` 0 is the submitter's non-blocking reap: `try_answer` polls the context and
  never sleeps. `answer` passes `min_nr` 1 and no timeout.
- `IOCB_FLAG_RESFD`: AIO "must signal the file descriptor mentioned in aio_resfd upon completion". An
  eventfd in a hyper-rt shard's epoll set then wakes a parked shard on a disk completion, together
  with its sockets and timers, so a parked submitter needs no thread of its own either (mantle
  `docs/design/node.md` §1.2 discussion, 2026-10-01).

**Only with direct I/O.** "libaio only supports unbuffered accesses (i.e., with O_DIRECT)" (Didona et
al., SYSTOR '22, §2; mantle research/03 DPI+22). fio's documentation says the same: "Linux may only
support queued behavior with non-buffered I/O" (mantle research/26). A buffered AIO read is performed
synchronously inside `io_submit`. The AIO path is therefore taken only for a file opened with
`O_DIRECT`. A file the file system refuses `O_DIRECT` for (tmpfs) stays on the pool, and so do cached
reads. §1 already shows a cached page is best read in place, not batched.

**Where `io_submit` still blocks.** Goldwyn Rodrigues' "No wait AIO" series (LWN 724631 and its
earlier versions, 2017) lists them: "block allocation for files, data writebacks for direct I/O,
sleeping because of waiting to acquire i_rwsem, and congested block devices". `RWF_NOWAIT` (Linux
4.14, io_submit(2): "Don't wait if the I/O will block") makes each of those complete at once with
`-EAGAIN` in the event's `res`, rather than block the submitter. The design follows from this:
- reads are submitted with `RWF_NOWAIT`. An `-EAGAIN` completion is resubmitted through the pool,
  counted, and the count is reported;
- writes go to preallocated extents only (mantle's log and chunk files are preallocated), so block
  allocation is not on the path;
- a write past the allocated length takes the pool, as an extending write must on Windows too (§3).

**The flush.** `IOCB_CMD_FSYNC` and `IOCB_CMD_FDSYNC` were implemented by Christoph Hellwig, commit
`a3c0d439` ("aio: implement IOCB_CMD_FSYNC and IOCB_CMD_FDSYNC", 2018-05-02, rebased on 4.17-rc3 for
the following release), "with a simple workqueue offload" (LWN 753381). The kernel defined the
opcodes long before and rejected them, so the issuer probes once at start: an `IOCB_CMD_FDSYNC` on a
scratch file either completes or fails with `EINVAL`. With no AIO flush, the flush stays on the pool.
On every path a batch's flush follows its own writes' completions, as today.

## 3. Windows: IOCP from the submitter

**Asynchronous only without the cache.** Microsoft's KB 156932 ("Asynchronous disk I/O appears
synchronous") says `FILE_FLAG_NO_BUFFERING` "is the best way to guarantee that I/O requests are
asynchronous". It lists what makes overlapped I/O complete synchronously anyway:
- NTFS compression and encryption: "all operations are made synchronous";
- "any write operation to a file that extends its length will be synchronous" (the remedy,
  `SetFileValidData`, needs `SeManageVolumePrivilege` and is not for us);
- a cached read whose data is in memory, which "will be completed and the ReadFile or WriteFile
  function will return TRUE".

`FILE_FLAG_NO_BUFFERING` requires "file access sizes, including the optional file offset … an
integer multiple of the volume sector size". Buffer addresses "should be physical sector-aligned"
("File Buffering", Microsoft Learn), which hyper-block's `AlignedBuf` already meets.

**Reaping.** `GetQueuedCompletionStatusEx` "retrieves multiple completion port entries
simultaneously". With `dwMilliseconds` zero, "if … there is no I/O operation to dequeue, the function
will time out immediately". That is `try_answer`'s non-blocking reap, as `io_getevents` with a zero
timeout is on Linux. Each entry returns "the number of bytes transferred …, the completion key … and
the overlapped structure address", which carries the transfer's tag. The port belongs to the
submitter: one completion port per `Attached`, so no other thread dequeues its completions ("as I/O
operations complete, they are queued to this port in first-in-first-out order").

**A completion that came back at once.** With `FILE_SKIP_COMPLETION_PORT_ON_SUCCESS`
(`SetFileCompletionNotificationModes`), when "a request returns success immediately without returning
ERROR_PENDING", "the I/O Manager does not queue a completion entry to the port". The submitter then
takes the result from the call itself and spends no reap on it. A mode once set "cannot be removed",
so the issuer sets it on its own duplicate of the handle.

## 4. macOS: the pool stays

macOS offers no interface that keeps more than 16 file I/Os in flight without a blocked thread per
I/O, and none that carries `F_FULLFSYNC` (mantle research/26 §2.4–§2.5, the module header of
`issuer.rs`). The pool remains its path. §1's numbers are the pool's, and the guidance that follows
from them stands on macOS: batch only device reads, read cached pages in place.

## 5. Not io_uring

The owner's decision of 2026-10-01 rests on io_uring's attack surface:
- Google's kCTF VRP: "60% of the submissions exploited the io_uring component of the Linux kernel …
  io_uring vulnerabilities were used in all the submissions which bypassed our mitigations". Google
  "disabled io_uring" on ChromeOS, made it "unreachable to apps" on Android, and "it is disabled on
  production Google servers"; they "consider it safe only for use by trusted components" (Google
  Security Blog, "Learnings from kCTF VRP's 42 Linux kernel exploits submissions", 2023-06-14).
- Docker's default seccomp profile blocks `io_uring_setup`, `io_uring_enter` and `io_uring_register`
  (moby/moby#46762; moby/moby#47532). A containerised node could not use io_uring by default.
- The kernel's own `kernel.io_uring_disabled`: "Prevents all processes from creating new io_uring
  instances. Enabling this shrinks the kernel's attack surface"; at 2, "io_uring_setup() always fails
  with -EPERM" (docs.kernel.org, admin-guide/sysctl/kernel).

On performance, the literature does not separate them at the depths a seek needs. Up to queue depth
16, io_uring and libaio are close: "79 KIOPS and 72 KIOPS; median latency 185 vs 190 µs" (DPI+22-F3).
In Haas and Leis's full engine, io_uring without polling is "surprisingly slightly slower (≈2% on
average) than libaio" (HL23-F10). io_uring stays an operator opt-in, to be measured, under the
restrictions the decision names.

## 6. The design this leads to

- **The submitter owns its completion queue.** On Linux, each `Attached` opened for direct I/O holds
  an AIO context of its depth (`io_setup(depth)`, refused with `EAGAIN` past `aio-max-nr`). On
  Windows, it holds a completion port associated with its own handle duplicate. `submit` and
  `submit_reads` submit directly. `answer` and `try_answer` reap directly. No issuer thread and no
  worker is on the path, and the issuer thread keeps only the pool path.
- **Bounds.** A submitter's transfers out never exceed its context's size, and a batch that would
  exceed it is refused before anything is submitted, as now. A partial `io_submit` keeps the
  remainder in the submitter's own bounded queue, submitted again at the next call: the batch's
  depth, never more.
- **Errors.** An event's `res` is the transfer's byte count or a negated errno. A short count is a
  short read, typed as one (`read_exact_at`'s rule). `-EAGAIN` under `RWF_NOWAIT` goes to the pool
  (§2). A failed write fails its batch, and its flush is never issued, as now.
- **Teardown.** It follows the decision's contract for every backend. A submitted buffer belongs to
  the device until its completion. Dropping an `Attached` stops admitting, cancels what it can
  (`io_cancel`, `CancelIoEx`), and reaps one completion per submission, bounded by the depth and
  counted, not timed, before the buffers are freed and the context destroyed.
- **`unsafe`.** The AIO and IOCP calls go over rustix's raw syscalls and windows-sys, in OS-interface
  files that `scripts/check-contracts.py` lists, each block with its `SAFETY` comment (the
  build-not-pull rule: no libaio).

## 7. What is measured before and after

`benches/reads.rs` gains the backend as a row: 4-page batches, direct and cached, against sequential
`pread`, with p50, p99, the Wilks bound on the p99.9, and allocator calls and page faults per
operation. Linux runs in mantle's `scripts/linux-test.sh` container and on a Linux host. Docker's
default seccomp profile allows `io_setup`/`io_submit`/`io_getevents` (§5 names only the io_uring
calls). Windows runs on CI's windows runners. The bar for the direct rows is the device's own
parallelism: four device reads in about one read's time. The bar for the cached rows is the
sequential `pread`, since a cached page is read in place.

## 8. As built: reads (2026-10-08)

The first piece is reads, the seek fan-out's need: `hyper_block::aio::AioReads` over a file opened
for direct I/O, one AIO context of the caller's depth, `submit_reads`, `answer` and `try_answer`
in the issuer's shape, the batch's vector given back with its answer. Each read asks
`RWF_NOWAIT`, and one that would block is read in place on the submitter's thread; a kernel that
refuses the flag (`EINVAL`) is sent the reads again without it. Partial submissions wait in order
for room; a refusal past the batches allowed out hands the reads back. Dropping it destroys the
context, which waits for every read out before the buffers go. The `unsafe` is in
`src/aio/linux.rs`, listed in `scripts/check-contracts.py`. Elsewhere, and over a buffered file,
it is refused, typed. Measured (docs/benchmarks.md, "hyper-block: a batch of reads through native
AIO"): four direct reads in 84.5 µs at the median against 257.5 µs one after another, 0 allocations
an operation. Still to build: writes and the flush through the same context (§2's `IOCB_CMD_FDSYNC`
probe), and Windows' IOCP (§3).

## Sources

- man-pages: io_setup(2), io_submit(2), io_getevents(2) (man7.org).
- docs.kernel.org: admin-guide/sysctl/fs (aio-nr, aio-max-nr); admin-guide/sysctl/kernel
  (io_uring_disabled).
- Goldwyn Rodrigues, "No wait AIO" patch series, LWN 724631 (2017).
- Christoph Hellwig, "aio: implement IOCB_CMD_FSYNC and IOCB_CMD_FDSYNC", commit a3c0d439
  (2018-05-02); series announcement LWN 753381.
- Microsoft Learn: GetQueuedCompletionStatusEx; SetFileCompletionNotificationModes; "File
  Buffering"; KB 156932 "Asynchronous disk I/O appears synchronous".
- Google Security Blog, "Learnings from kCTF VRP's 42 Linux kernel exploits submissions", 2023-06-14.
- moby/moby#46762 and #47532 (io_uring in Docker's default seccomp profile).
- Haas and Leis, VLDB 2023 [HL23], and Didona et al., SYSTOR '22 [DPI+22], as mantle research/03
  quotes them.
