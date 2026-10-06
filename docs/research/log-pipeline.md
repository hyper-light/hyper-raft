# Research: several group-commit frames in flight in hyper-log

Source notes for hyper-log's pipelined commit (mantle docs/research/33 §2 row 3; mantle
docs/design/engine-structure.md §8, step E0). Today hyper-log runs one group-commit frame at a
time (mantle docs/design/raft-log.md §3): it gathers every update that arrived during the last
flush, writes one frame, flushes the file, and answers once a later durable record confirms the
flush. The question is how to keep `K` frames in flight with the same guarantee: an append is
answered only when it and every earlier append are on stable media, and recovery loses nothing
answered. Kernel sources are read at git.kernel.org master on 2026-10-06; each claim says where
it is from. **[unverified]** marks what no primary source confirmed.

## 1. The rule with K frames in flight

hyper-log's recovery rests on one fact (raft-log.md §2, §6; `writer.rs`): frame `n` is written
only after frame `n−1` is flushed. So a valid later frame proves every earlier one, and frame
`n`'s persist record confirms frame `n−1`. With `K` frames in flight that fact becomes a window,
and every rule built on it has to generalise:

- **Issue.** Frame `n` is submitted only once frame `n−K` is durable. The window is bounded in
  frames and in bytes (CLAUDE.md §2).
- **Answer.** The durable prefix `D` is the highest `n` such that frames `1..n` each completed
  durably. An update is answered once its frame is at or below `D`. This is the counter of every
  system below: SiloR's `pepoch`, the passive group commit's minimum `dgsn`, ScyllaDB's
  `_flush_pos` (§4).
- **Recover.** Keep the run of consecutive valid frames from the last confirmed point, and stop
  at the first hole `h`. A valid frame past `h` was never answered, because answers need the
  prefix to pass `h`, so dropping it loses nothing answered.
- **Damage against a torn tail.** A valid frame at or past `h + K` was issued only after `h` was
  durable. A hole with such a frame beyond it is damage at rest, not a torn tail, and takes the
  existing damage path. At `K = 1` this is exactly today's rule.
- **Erase.** Before anything new is written past a hole, the window past it (at most `K` frames,
  the in-flight byte bound) is zeroed. Otherwise a durable but never-answered frame `h+1` would
  read as a continuation once the hole is refilled. This extends raft-log.md §6's erase step.
- **One write to a place at a time.** Neither the block layer nor NVMe orders concurrent commands.
  Linux `Documentation/block/writeback_cache_control.rst`: a preflush covers only *previously
  completed* writes. Overwrites of the same blocks (the record slots, the confirmations) are
  therefore serialised per slot. A segment's opening and a sweep frame drain the window first.

## 2. Linux: native AIO, O_DIRECT

- **Per-write durability flags.**
  - `pwritev2(2)`: `RWF_DSYNC` (since 4.7) is "a per-write equivalent of the O_DSYNC open(2) flag
    ... applies only to the data range written".
  - `io_submit(2)`: `aio_rw_flags` carries `RWF_DSYNC` since 4.13 and `RWF_ATOMIC` since 6.11
    ("will never be torn from power fail").
  - `fs/aio.c`'s `IOCB_CMD_FDSYNC` runs `vfs_fsync` from a workqueue; its first kernel version
    is **[unverified]**.
- **How the block layer honours them** (`block/blk-flush.c`'s header; `writeback_cache_control.rst`):
  - On a device without a volatile cache, "PREFLUSH and FUA don't make any difference": the
    flags are stripped and the write is plain, durable at completion.
  - With a write-back cache that supports FUA, the write carries FUA, whose completion "is only
    signaled after the data has been committed to non-volatile storage", for that request alone.
  - Without FUA support, FUA becomes a flush after the write. Concurrent such writes share
    merged flushes: one in progress, deferred while sequenced writes run.
- **NVMe** (`drivers/nvme/host/core.c`): a controller with VWC present gets both the write-cache
  and FUA features, and `REQ_FUA` is the Write command's FUA bit. A controller with power-loss
  protection that reports no VWC gets plain writes and no-op flushes, which is correct.
- **Through a file system** (`fs/iomap/direct-io.c`, used by XFS and ext4), an `O_DSYNC` direct
  write is sent with FUA only when all of these hold:
  - it needs no completion work: no unwritten, new or shared extent;
  - the inode is not datasync-dirty;
  - it does not extend the file: ext4's `IOMAP_F_DIRTY` is set by
    `ext4_inode_datasync_dirty || offset + length > i_size` (`fs/ext4/inode.c`); XFS's by its
    datasync sequence or a write past EOF (`fs/xfs/xfs_iomap.c`).

  Otherwise it falls back to `generic_write_sync`, a full `fdatasync` per write. A raw block device
  (`block/fops.c`) always sends FUA. XFS gained the FUA path in 4.18 (Microsoft's Bob Dorr, "SQL
  Server on Linux: FUA internals": about 50% less I/O traffic).
- **What hyper-log must therefore do on Linux:**
  - Preallocate the log file and write it full of zeros, `fdatasync` it, and `fsync` its
    directory, so every frame is a pure overwrite inside `i_size`. ScyllaDB recycles preallocated
    segments with `commitlog_use_o_dsync` on by default for this reason (`db/commitlog/commitlog.cc`).
  - Submit up to `K` writes, `IOCB_CMD_PWRITE` with `RWF_DSYNC`, each a whole padded frame, on
    hyper-block's issuer. Each completion means that frame is durable.
  - Fallback where FUA is absent or not trusted: plain writes, then one `IOCB_CMD_FDSYNC` once
    frames up to `n` have *completed*, which covers exactly them. The writes of the next batch
    overlap the flush. Whether `fdatasync` waits for direct AIO still in flight is
    **[unverified]**, so a flush is issued only after the writes it covers complete.

## 3. macOS and Windows

**macOS, APFS** (`fcntl(2)`, `fsync(2)`, `IOStorage.h`, XNU `bsd/kern/kern_descrip.c`):
- `fsync` alone does not flush the drive's cache ("This is not a theoretical edge case").
- `F_FULLFSYNC` drains the device's queue as a barrier: what was synced before is durable when it
  returns.
- `F_BARRIERFSYNC` orders but promises nothing durable, and XNU promotes it to `F_FULLFSYNC` where
  a file system refuses it.
- There is no per-write durable flag. This Mac's internal SSD advertises `IOStorageFeatures` of
  `{Unmap, Priority, Barrier}`, with no FUA. What `O_DSYNC` does on APFS is **[unverified]**, so
  hyper-log does not rely on it.

The pipeline is therefore two stages:
1. The pool writes a batch's frames concurrently.
2. Once all have returned, one `F_FULLFSYNC` covers them. The next batch's writes overlap it,
   since writes issued during an `F_FULLFSYNC` are not promised covered.

At most one flush runs at a time, because each drains the whole device. Hector Martin's 2022
measurement of an M1's internal SSD gave 46 writes a second with `F_FULLFSYNC`, against about
40,000 without (reported by mjtsai.com). So on macOS the overlap gains little; larger natural
batches gain more.

**Windows** (`CreateFileW`, `FlushFileBuffers`, `STORAGE_WRITE_CACHE_PROPERTY`, KB156932):
- With `FILE_FLAG_WRITE_THROUGH | FILE_FLAG_NO_BUFFERING` the system "requests a write-through of
  the hard disk's local hardware cache to persistent media". This is the documented replacement
  for `FlushFileBuffers` after each write.
- `K` overlapped writes on the device's completion port each complete durable.
- A write that extends the file is synchronous (KB156932), so the log is preallocated by writing
  zeros.
- Whether StorNVMe maps the request to NVMe FUA is **[unverified]**.
- `IOCTL_STORAGE_QUERY_PROPERTY` (`StorageDeviceWriteCacheProperty`) reports the cache type, its
  state, write-through and flush support, and a battery-backed cache.

## 4. The durable prefix in the literature

- **Zheng et al., *Fast Databases with Fast Durability and Recovery Through Multicore
  Parallelism* (SiloR), OSDI '14, §3.** Each logger syncs and publishes its epoch. One logger takes
  the minimum less one, syncs it to the `pepoch` file, and only then are results released.
  Recovery ignores anything past `pepoch`. This is the closest analogue of hyper-log's persist
  record.
- **Wang and Johnson, *Scalable Logging through Emerging Non-Volatile Memory*, PVLDB 7 (2014),
  §4.1.** Passive group commit: each thread holds the highest sequence it can guarantee
  persistent, and records at or below the minimum are durable. 1.6–3.2× over Aether (§6).
- **ScyllaDB's commit log** (`db/commitlog/commitlog.cc`, `utils/flush_queue.hh`).
  - `flush_queue` runs a key's post step only after every lower key's has completed.
  - Data past a flush point is written while the flush is pending.
  - `_flush_pos` is the durable-prefix counter.
- **Chen et al., *SpanDB*, FAST '21, §4.2 and Figures 3 and 10.**
  - Loggers issue concurrent batch writes through SPDK, up to 8 groups outstanding. The only
    synchronisation is the CAS that allocates pages.
  - Its correctness argument rests on RocksDB's read-committed isolation, not a durable-prefix
    rule, so it is no model for Raft's prefix.
  - Its devices (Optane P4800X, P4610) have power-loss protection and it issues no flush.
  - The 7.6–8.8× is the whole system against RocksDB on ext4. Its ablation credits parallel
    logging with asynchronous processing at 4.5× over RocksDB with an SPDK log.
  - WAL throughput saturates at 3 loggers × 3 requests on Optane and 2 × 4 on the P4610, so the
    useful `K` is small.
- **Johnson et al., *Aether*, PVLDB 3 (2010).** Flush pipelining decouples threads from the
  flush, with one flush in flight. The abstract reports 20–69%; its section text was not read
  **[unverified]**.

## 5. Measurements to expect, and to make

- **Mark Callaghan, Small Datum (2026-01-07)**, fio, `O_DIRECT` 16 KB writes, `fdatasync` after
  each, one job:

  | Drive | Power-loss protection | `fdatasync` | Writes/s |
  |---|---|---|---|
  | Crucial T500 | no | 447 µs | 2,100 |
  | Samsung 990 Pro | no | 2,783 µs | 331 |
  | Intel/Solidigm D7-P5520 | yes | 9.8 µs | 27,600 |
  | Samsung PM9A3 | yes | 0.7 µs | 46,700 |

  "For an SSD without power loss protection, writes are fast but fsync is slow."
- **Won et al., *Barrier-Enabled IO Stack for Flash Storage*, FAST '18.** Write-then-flush keeps
  the device queue at depth one: 1% of the orderless IOPS on a 32-channel array, 25% on a
  supercap SSD, against 80–90% kept by barrier writes. These are SATA and UFS devices, not NVMe.
- **What hyper-log measures before choosing `K`** (CLAUDE.md §4–5), on each device class:
  - durable appends per second and their p50–p99.9 at `K` = 1, 2, 4, 8, 16;
  - FUA writes against write-then-flush;
  - consumer drives with a volatile cache against power-loss-protected ones.
  
  `K` is the knee of the measured curve, not a picked number. Whether a consumer drive's FUA
  writes run in parallel or as internal flushes is unknown until measured.

- **Measured: macOS, APFS, Apple SSD AP8192Z (M5 Max, macOS 26.4.1), 2026-10-06.** This is
  `cargo bench -p hyper-block --bench pipeline`: `K` writers and one flusher running
  `F_FULLFSYNC` back to back, under the issue rule of §1, on a preallocated, zero-written file
  with `F_NOCACHE`. Latency runs from submission to durable.

  | `K` | Frame | Frames | Frames/s | Flushes | p50 µs | p99 µs |
  |---|---|---|---|---|---|---|
  | 1 | 4 KiB | 400 | 242 | 400 | 4,247 | 6,293 |
  | 2 | 4 KiB | 400 | 249 | 395 | 8,425 | 12,411 |
  | 4 | 4 KiB | 400 | 499 | 196 | 8,443 | 10,558 |
  | 16 | 4 KiB | 400 | 1,967 | 50 | 8,377 | 12,509 |
  | 64 | 4 KiB | 1,600 | 7,940 | 49 | 8,282 | 10,478 |
  | 128 | 4 KiB | 1,600 | 15,910 | 23 | 8,218 | 9,478 |
  | 1 | 64 KiB | 800 | 258 | 800 | 4,235 | 6,283 |
  | 64 | 64 KiB | 800 | 7,051 | 25 | 8,472 | 11,824 |

  An `F_FULLFSYNC` costs about 4 ms on this device whatever it covers, from one 4 KiB frame to
  sixty-four 64 KiB ones. A `K = 1` cycle (4.1 ms) is the flush alone, so a direct 4 KiB write is
  negligible beside it. Frames a second therefore grow with frames per flush, with no knee up to
  128. That is batching, which hyper-log's group commit already does: every update waiting
  during a flush goes into the next frame. What `K > 1` adds on macOS is the write overlapping
  the flush, under 1% of the cycle here, and it costs latency: p50 rises from one flush to
  nearly two, because a frame written during a flush waits for the next one. On this device class,
  hyper-log keeps `K = 1` and lets the frame grow. This confirms §3's expectation by measurement.
  The gain §2 describes needs a device where writes complete durably in parallel (FUA, or
  power-loss protection). That curve is measured on Linux and Windows hardware, not on this Mac.

## 6. Detection and the risks

- **What a device says** (Linux, NVMe, macOS, Windows), cross-checked by measurement as CLAUDE.md
  §5 requires:
  - Linux: `/sys/block/<dev>/queue/write_cache` and `queue/fua` (`Documentation/ABI/stable/sysfs-block`).
  - NVMe directly: Identify Controller's VWC bit and Identify Namespace's NSFEAT bit 5.
  - macOS: IORegistry's `IOStorageFeatures`.
  - Windows: `STORAGE_WRITE_CACHE_PROPERTY`.
- **A device that lies** (`fcntl(2)` names FireWire drives) shows in measurement:
  - a volatile cache whose flush costs nothing;
  - or no volatile cache and consumer write latency without power-loss protection.
  
  Only a power cut proves durability (Zheng et al., *Understanding the Robustness of SSDs under
  Power Fault*, FAST '13 **[unverified]**).
- **FUA lost to the file system.** An unwritten extent, a write past `i_size` or a
  datasync-dirty inode turns each write into a per-write `fdatasync` without error. It is still
  correct, only slow, and shows as a latency step and in blktrace.
- **Torn frames.** Any subset of the in-flight frames may persist, and a frame larger than the
  device's atomic write unit (NVMe AWUPF) may tear. The CRC and §1's hole, damage and erase rules
  cover both.
- **A failed write or flush** fences the log, as today. A failed flush is never retried.
