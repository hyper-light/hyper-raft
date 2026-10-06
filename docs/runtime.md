# The shared runtime: `hyper-rt`

> Status (2026-10-05): **design, for review before building.** The owner's decisions: focal leaves tokio
> and quinn for hyper-quic and hyper-transport over a thread-per-core runtime of the projects' own,
> taken from slates' `rt` and shared here as `hyper-rt` ("could we make this hyper-rt?", and then "we fix
> each and every one of those missing items and gaps"); mantle's node takes the same direction for its
> HTTP/1.1 S3 listener (no tokio, no hyper). slates' `rt` is the starting point, but this is a design: each
> part below is kept, changed or added from what the three consumers need and what the sources say,
> and each departure from slates' code is stated with its reason.

## 0. How to read this

**Citation tags.** `[KEY §section]` for papers and standards; source files as `path:line` at the revision
named in the Sources table; mantle's research notes as "note NN §x"; slates' design as "slates §x".
Statements no source makes are marked **DERIVED** (arithmetic on stated facts) or **INFERENCE**. A source
not yet read against its primary text is marked **UNVERIFIED**; those are listed in §15 to be checked
before the part that rests on them is built.

### Sources

| Key | Source | Label |
|---|---|---|
| SLATES-RT | slates `crates/rt`, `crates/mem`, `crates/machine` at `6b9ce5c` | primary (source) |
| NOTE08 | mantle `docs/research/08-slates-storage-runtime.md` §1, §3, §5 (a review of SLATES-RT at an earlier revision, with line citations) | review |
| NOTE25 | mantle `docs/research/25-node-runtime.md` §1, §13, §14, §16 | review |
| NOTE26 | mantle `docs/research/26-concurrency-model.md` §1–§8 | review |
| NOTE32 | mantle `docs/research/32-shared-transport-and-raft.md` §3, §6 | review |
| WHEELS | G. Varghese, A. Lauck. "Hashed and Hierarchical Timing Wheels." IEEE/ACM ToN 5(6), 1997 (cited through NOTE25 §14) | peer-reviewed |
| KARLIN | A. R. Karlin, K. Li, M. S. Manasse, S. Owicki. "Empirical Studies of Competitive Spinning for a Shared-Memory Multiprocessor." SOSP '91 | peer-reviewed |
| SEASTAR | Seastar, "Shared-nothing Design" and `doc/tutorial.md` (cited through NOTE26 §4.1) | project documentation |
| LOZI | J.-P. Lozi et al. "The Linux Scheduler: a Decade of Wasted Cores." EuroSys '16 (through NOTE26 §4.5) | peer-reviewed |
| CAPRICCIO | R. von Behren et al. "Capriccio: Scalable Threads for Internet Services." SOSP '03 (through NOTE26 §4.3) | peer-reviewed |
| LITTLE | J. D. C. Little. "A Proof for the Queuing Formula: L = λW." Operations Research 9(3), 1961 | peer-reviewed |
| WILKS | S. S. Wilks. "Determination of Sample Sizes for Setting Tolerance Limits." Ann. Math. Statist. 12(1), 1941 | peer-reviewed |
| KJ | T. Kalibera, R. Jones. "Rigorous Benchmarking in Reasonable Time." ISMM '13 | peer-reviewed |
| LEMON | J. Lemon. "Kqueue: A generic and scalable event notification facility." USENIX ATC '01, FREENIX track | peer-reviewed |
| URING | J. Axboe, "Efficient IO with io_uring", 2019 (through NOTE26 §2.2) | NON-PEER-REVIEWED |
| SBITMAP | Linux `lib/sbitmap.c`, `include/linux/sbitmap.h` (blk-mq's scalable bitmap of tags) | primary (source), UNVERIFIED revision |
| AFD | the `IOCTL_AFD_POLL` technique of wepoll and mio (`mio/src/sys/windows/afd.rs`); SLATES-RT `afd.rs` | NON-PEER-REVIEWED |
| AFUNIX | Microsoft, "AF_UNIX comes to Windows" (Windows 10 1803), and `SIO_AF_UNIX_GETPEERPID` in `afunix.h` | vendor, UNVERIFIED |
| SELFPIPE | D. J. Bernstein, "The self-pipe trick"; POSIX `sigaction`, async-signal-safe functions (XSH 2.4.3) | NON-PEER-REVIEWED; primary |
| RFC5482 | TCP User Timeout Option | primary |
| RFC896 | Congestion Control in IP/TCP Internetworks (Nagle) | primary |
| NOTSENT | Linux `tcp(7)` `TCP_NOTSENT_LOWAT`; Apple `TCP_NOTSENT_LOWAT` in `netinet/tcp.h` | primary, UNVERIFIED for macOS semantics |
| GAI | POSIX `getaddrinfo`; Microsoft `GetAddrInfoExW` | primary |
| FDB | J. Zhou et al. "FoundationDB: A Distributed Unbundled Transactional Key Value Store." SIGMOD '21, §4 (simulation) | peer-reviewed |
| WAKER | Rust `core::task::{Waker, RawWakerVTable}` documentation, 1.98 | primary |
| VYUKOV | D. Vyukov, "Intrusive MPSC node-based queue" and "Bounded MPMC queue", 1024cores.net | NON-PEER-REVIEWED |

## 1. What the three consumers need

| Need | focal | slates | mantle | slates-rt today |
|---|---|---|---|---|
| Shards, tasks, `Copy` wakers, timers | yes (replaces tokio's scheduler, 1,138 uses across 164 files) | yes | yes (node.md shards) | yes |
| UDP, IPv4 and IPv6, batched (GSO/GRO, `sendmmsg`/`recvmmsg`), kernel receive stamps | yes (QUIC peers, clients) | yes (fleet plane) | yes (cell transport) | IPv4 only, one datagram a call (NOTE08 §3.5) |
| TCP on Linux, macOS and Windows, IPv4 and IPv6, backpressure, `TCP_NOTSENT_LOWAT`, `TCP_USER_TIMEOUT`, a connection budget | yes (comparison drivers, the deployment probes) | yes (NFS loopback) | yes (the S3 HTTP/1.1 listener) | Unix and IPv4 only |
| Local stream sockets with the peer's identity | yes (`focal.sock`, the admin socket; today tokio's Unix socket and Windows named pipe) | its own shared-memory rings (`slates-ipc`) | — | none |
| Signals (interrupt, terminate; Windows console events) | yes (graceful shutdown) | yes (anchor and daemon) | yes | none |
| Asynchronous stdio | yes (`focal serve mcp` over stdio) | the MCP edge | — | none |
| Name resolution | yes (contexts name endpoints by host) | — | — | none |
| Synchronization between tasks and threads: channels, one-shots, a watched value, semaphores, notifications | yes (tokio's `mpsc`, `oneshot`, `watch`, `Semaphore`, `Notify`) | spawn and cancel messages only | yes (tickets from device issuers) | none general (NOTE08 §3.1) |
| A completion seam for device issuers (hyper-block's), so one shard waits on network and disk together | yes (hyper-log's writer) | — | yes (Linux native AIO, IOCP, the macOS pool; owner's decision) | none |
| Blocking work off the shards, bounded | yes (`spawn_blocking`: the code-mode sandbox, file reads) | — | yes | none |
| Running a future to completion on the calling thread (`main`, a CLI command, a test) | yes (305 `#[tokio::test]`, every CLI command) | yes | yes | `LocalRuntime::run_until_idle` only |
| Deterministic simulation that hyper-sim's world drives | yes | yes | yes | its own fabric (`sim.rs`) |
| The hyper-transport and hyper-datagram driver | yes | yes (X-2) | yes | — (hyper-tokio serves tokio) |
| Configuration derived from the machine, cheap enough for a short command | yes (a CLI command must not spend a second calibrating) | the daemon calibrates at boot | yes | a full profile at boot (codec, hash, memcpy probes included) |

## 2. Rules this crate meets

hyper-raft's CLAUDE.md, the union of the three projects' rules, plus what this crate's owner role adds:

- **No shared ownership, no locks, no `RefCell`.** `Arc`, `Rc`, `Mutex`, `RwLock` and `Condvar` are denied
  by `clippy.toml`. `RefCell` is not used (the owner's rule, "avoid Arc and RefCell, even in tests", given to
mantle): `scripts/check-contracts.py` refuses it in this crate, and the rule moves to `clippy.toml` once
the crates that still hold one (hyper-log's device and ticket tables, hyper-block's simulation,
hyper-durable's held scratch, and test support) have moved off it. A shard's state is owned
  by its thread; everything that crosses threads is a `Copy` word, an atomic, or a value moved through a
  bounded queue.
- **No panics in shipped code**, no unchecked arithmetic, no indexing: the workspace wall.
- **No thread per unit.** Threads are one per shard, plus the bounded pools of §9, each with its bound
  stated.
- **Bounded everything.** Every table, queue, ring and wait has a stated bound and a typed refusal at it.
- **No arbitrary constants.** Each is a standard's value with its citation, derived by a stated formula from
  a measurement, or configuration with a justified default. Sample counts come from WILKS (59 for a
  95 %/95 % one-sided tolerance bound) or a derived catch rate.
- **`unsafe` only in OS-interface files** listed by `scripts/check-contracts.py` (§13), each block with a
  `// SAFETY:` line.
- **The shared-crates law.** Allocations, reallocations and page faults are measured on every hot path;
  benchmarked against tokio (focal's and mantle's present runtime) and slates-rt; real end-to-end tests;
  six-target CI.

## 3. The shape

**Thread-per-core, shared nothing** [SEASTAR; NOTE26 §4.1]. A runtime is a set of shards; a shard is one
OS thread with its own task arena, run queue, timing wheel and driver. A task lives on one shard for its
whole life and is never stolen. Keeping runnable threads at the core count keeps the process's
performance off the OS's load balancing [LOZI; NOTE26 §4.5]. Work moves between shards as messages
(§8), never as shared mutable state.

**Kept from slates-rt:** the task arena with generational slots, the `Copy` waker word, the process-static
shard registry with odd/even generations and counted foreign readers (retirement now never waits for
them: the last reader frees the entry, `src/retire.rs`, loom-modelled; mantle's review, finding 10b),
the park-and-kick protocol and its loom model, the hierarchical timing wheel, structured cancellation (a parent's end cancels and joins
its children), admission receipts, long-poll attribution, the simulation driver. Each is described below
with what changes.

### 3.1 Tasks

A task is a boxed future in a slot of the shard's arena (`Slab<TaskSlot>`), bounded at `tasks_per_shard`;
a spawn past it is refused `TooManyTasks` with the receipt saying so. A task spawned on its own shard may
be `!Send`; a task sent to another shard is `Send` and travels through that shard's control queue.
`tasks_per_shard` is Little's law on the consumer's measured arrival rate and p99 service time
[LITTLE], as slates derives it (`runtime.rs` `admission_limit`); a consumer that has not measured them
states its configured value and why.

**Change: one allocation per spawn, measured.** slates boxes every future (NOTE08 §3.1). Kept for now (a
future's size is the caller's), with the allocation counted in the benchmarks of §12; a per-shard pool of
fixed-size task frames is the measured follow-up, not a default.

### 3.2 Wakers

A `Waker`'s data pointer *is* the task's word, `shard:16 | slot:24 | generation:24`; its vtable's
`clone` and `drop` do nothing, so cloning a waker costs nothing [WAKER; SLATES-RT `waker.rs`]. The
vtable is `unsafe` by signature and lives in `waker.rs`, an OS-interface file in the sense of §13.

**Change: one wake path, a bitmap per shard instead of rings.** slates routes a wake three ways: the
local queue, a single-producer ring per shard pair (shards² rings), and one multi-producer ring per shard
for foreign threads; a full ring makes the sender spin until the consumer drains it, a loop with no
counted bound (`registry.rs` `send_foreign`). And the pair-ring path kicks unconditionally, contradicting
its own "a message to a spinning shard costs no syscall" (NOTE08 §3.1).

hyper-rt keeps the local push and replaces both rings with a **wake bitmap** per shard: one bit per task
slot, in 64-bit words with a summary word above every 64 (a two-level scalable bitmap, the structure
blk-mq uses for its tags [SBITMAP]). A wake from another thread is one `fetch_or` on the slot's bit, one
on its summary bit **only when the word went from empty to non-empty** (as sbitmap does: most wakes then
touch one cache line, not the summary word all wakers of 4,096 slots share), and a kick only if the shard
is parked (the loom-checked protocol, §3.5). The shard drains by swapping each marked summary word to zero
and then each marked word: **summary before word**, so a wake landing between them is either in the word
the drain takes or marks the summary again for the next drain. A drain **starts where the last one
stopped** and wraps, so under a step's batch bound high slots do not always queue behind low ones (a p99
tail on a loaded shard otherwise). The three changes follow slates-dc's review (2026-10-06); the two
wakers-of-one-word-against-a-drain interleavings are a loom model in `wakes.rs` (47 interleavings), which
fails when the drain clears the summary after the word.

- **Bounded by construction.** The bitmap is `tasks_per_shard` bits plus `tasks_per_shard / 64` summary
  bits, allocated once at the shard's build; it cannot fill, so no sender ever waits (DERIVED).
- **Duplicates collapse**, as slates' run queue already collapses them with a pending flag per slot.
- **The generation is not carried.** A wake whose task ended and whose slot was reused wakes the new
  occupant once, spuriously; every future tolerates a spurious wake [WAKER, "spurious wake-ups"]. The
  cost is one poll, bounded by the number of wakes that raced a slot's reuse. The generation stays in
  the word, so a wake for a slot of a *different shard incarnation* (a registry slot reused by a later
  runtime) is still refused at the registry by the slot's generation, as slates does. A drained wake whose
  slot holds no task is counted (`Counters::wakes_to_free_slots`) and not queued: the visible half of that
  race, and the non-vacuity counter for its bound (`tests/foreign_wake.rs`).
- **INFERENCE to measure:** with the summary marked only on empty words, a wake contends with wakers of
  the same 64-slot word. Packed words put 512 slots on one 64-byte line, so unrelated wakers may still
  bounce it; the §12 foreign-wake row measures packed words against one word per cache line (memory
  `tasks_per_shard / 64 × 128` bytes) and the cross-shard row the bitmap against slates' rings, with a p99
  for wakes to high slots under load, before slates' rings are removed.
- **Measured against, in slates (2026-10-06):** kicking only a parked shard on slates' pair rings was
  correct (loom 3/3, Miri clean, 200/200 wakes to a busy shard with every kick saved) and *slower* for a
  spinning shard: the cross-shard round trip went 1,958–2,000 → 2,291–2,416 ns on Linux 6.12 and
  4,750–5,250 → 12,292–13,292 ns (p99 26–40 µs) on macOS under load, while a parking shard got faster on
  Linux and stayed even on macOS. No wake was lost; the sender reached its next event later without its own
  kick, a scheduling effect not yet identified (slates `docs/wip/BENCHMARKS.md`, "Measured and rejected").
  hyper-rt routes every cross-thread wake through kick-if-parked, so §12 carries the **spinning** round
  trip on both OSes under load, beside the parking one, and the choice stands only if it holds there.
  **slates' nanosecond timeline (2026-10-06):** the result is a phase effect, not a fixed cost — probing
  each event reversed the A/B (always-kick 2,458 ns, kick-if-parked 1,917 ns). ~2,250 of 8,000 kicks hit a
  target mid-step and left a pending eventfd/kevent, so its next park returned at once: an accidental extra
  inbox poll. And in every run, both builds and both OSes, **the idle spin never hit** (one miss per round
  trip): its window was the startup probe's mean wake (1.7 µs on macOS), far below what a wake costs under
  load. hyper-rt derives `spin_ns` the same way (§10.3), so §12's spinning row runs **with the online wake
  estimate on** (`WakeTracking`), under load, and counts spin hits and misses; a spin window that never hits
  under load is a defect of the derivation, to fix at its cause, not a tuning.
  **Then, in slates' daemon configuration** (spin window ×100, online estimate on, macOS at load 14–40,
  three alternating rounds): always-kick 4,875–5,917 ns, kick-if-parked 12,167–12,791 ns, with only 1–2
  real driver waits per 2,000 round trips — so the spin window is **not** what costs the extra ~7 µs, and
  no kernel wake sits in it. What is left are 10–13 µs gaps in which a shard thread that makes no system
  call logs nothing: the OS scheduling two spinning threads, the kick's syscall apparently keeping the peer
  promptly scheduled (inferred, not measured). So the kick question is decided on §12's spinning row under
  load as stated above, and the spin window's derivation (startup mean against an online p50/p90) is
  measured separately, on its own merits: spin hits, CPU, p99 under load.

### 3.3 The run queue

Slot indices in a pre-sized ring with one pending flag per slot (slates' `queue.rs`), at most `batch`
polls per step. `batch` is a latency budget over the measured cost of one loop item (slates'
`calibrate_batch`). **The budget, derived:** a task woken while a batch runs waits at most that batch
before the loop next reads its wakes, and a waker on another shard would otherwise have paid a park's
wake to reach it, so the batch may delay it by at most the expected wake: budget = the mean wake, the
same quantum as §3.4's step (DERIVED from §3.4's spin-then-park rule). A consumer with a stricter latency
objective states it, and the smaller of the two applies.

### 3.4 The loop

Each step: drain the control queue (spawns, cancels, shutdown), drain the wake bitmap, take the driver's
completions, expire timers, poll up to `batch` ready tasks. When nothing is ready the shard spins for the
expected cost of parking and then parks in its driver until a kick, a completion or the next deadline:
the 2-competitive spin-then-block rule [KARLIN], with the threshold the measured mean wake (§10). slates'
attribution of a long poll to the task or to the host (`attribution.rs`) is kept.

**The idle window follows requests.** A shard that served a client's request within the measured idle
window spins rather than parks, so the client's next request costs no kernel wake (slates §4.7,
`ShardContext::note_activity`). Every path that serves requests must mark it, per request: slates' FUSE
serve path did not, and an uncached lookup's median went from 23 µs to about 60 µs on Linux 6.12 under
load once the window lapsed between requests (slates-dc, 2026-10-05). hyper-rt makes it structural
rather than a call each server must remember: the transport driver (§14), the TCP and local-socket
reads (§5), the device reads (slates' `/dev/fuse` and vhost-user queues, awaited through
`readiness::readable`) and the channel receive (§8) mark activity on the shard when they hand a request
to a task.

**Change: exclusive access is structural; nothing nests.** slates keeps the shard's mutable state in a
`RefCell<ShardInner>` and refuses a nested borrow with `try_borrow_mut`, counting it (`shard.rs`): a task's
poll that spawns, arms a timer or registers interest borrows the state the loop is running. A
take-and-put `Cell<Option<Box<_>>>` would be the same runtime-checked borrow under another name (mantle's
review). The owner's rule is no `Arc` and no `RefCell`, even in tests (§2).

hyper-rt splits the shard in two, so that a poll can never reach the loop's state:

- **The loop's state** (`Loop`: the task futures, the timing wheel, the driver, the control receiver, the
  counters) is owned by value by the loop's frame and lent `&mut` down the step. No task ever holds a
  reference to it.
- **The desk** (`Desk`): what a polled task may ask of its shard, as **intents** in queues that are
  pre-sized at the shard's build and owned per shard, plus what the loop publishes for tasks to read. A
  task reaches the desk through the thread's current-shard pointer, valid for the span of the step
  (`registry.rs`, an OS-interface file in the sense of §13, as slates' is). Between polls the loop drains
  every intent queue into its state.

| A task asks | Intent | Answered synchronously by |
|---|---|---|
| spawn (child or sibling) | the boxed future and its parent, in the spawn queue | a task slot popped from the desk's **free-slot stack**, so the `TaskId` exists at once; an empty stack is `TooManyTasks` |
| arm a timer | `(deadline, task word, timer slot)` in the timer queue | a timer slot popped from the desk's free-timer stack (the `TimerId`); empty: the sleep waits for a timer to free (§3.6) |
| disarm a timer | the timer slot | — |
| register readable or writable interest | `(handle, interest, task word)` | — |
| wake a task of this shard | the slot onto the run queue | — |
| cancel, detach | the task id | — |
| join | — | the slot's published outcome and a waiter word, both in the desk |
| the clock | — | the step's time, published by the loop |

Each queue is a ring of `Cell` slots (a `Copy` intent in a `Cell<T>`; the spawn queue's boxed futures in
`Cell<Option<_>>`, written once by the task and taken once by the loop: a move, not a borrow) with
`Cell` head and tail. Every queue's bound follows from what feeds it (DERIVED): spawns cannot outnumber
free slots, timer arms cannot outnumber free timers, disarms cannot outnumber armed timers; interest
registrations are bounded by the shard's configured handle count, past which a registration is refused
`Capacity`. Intents take effect before the next poll and before the shard parks, so a spawned child runs
in the next step, and an armed deadline or a registered interest is in the wheel or the driver before
any wait that could miss it.

**The two narrow paths that are not intents, and why.** The free-slot and free-timer stacks are popped by
tasks and pushed by the loop: an identifier must exist when `spawn` or `sleep` returns, so its allocation
cannot wait for the loop. Each stack is one field, `Cell` words and a `Cell` length, with no borrow and no
refusal path but "empty". Kept values (slates' per-shard singletons, `Kept`) are filled before the shard's
first step and immutable afterwards, so tasks reach them by shared reference only. Nothing else in a
task's reach is mutable.

With nothing nesting, slates' nested-borrow counter has nothing to count, and goes.

### 3.5 Parking and the kick

Kept: the shard announces it is parked, fences, re-checks its inboxes and waits in its driver; a sender
publishes, fences and kicks only a parked shard (`parking.rs`, loom-checked: 27 bounded and 116
exhaustive interleavings; the model found a lost wake that the fence fixed; NOTE08 §3.1). With §3.2 every
cross-thread wake takes this path, so a wake to a spinning shard costs no system call (the property
slates' pair rings broke).

The kick per OS: an eventfd on Linux (epoll), `EVFILT_USER` on macOS and FreeBSD [LEMON],
`PostQueuedCompletionStatus` on Windows, a flag in simulation.

### 3.6 Timers

A hierarchical timing wheel [WHEELS §VI.B], entries in a slab reserved once, doubly linked slot lists:
arming, cancelling and firing never allocate (slates' `timer.rs`, `tests/timer_allocations.rs`). The
wheel spans `levels × log2(slots)` bits of ticks, sized so the longest configured wait fits (§10).

**The tick, derived.** slates sets it to `max(mean wake, 100 × clock-read cost)`; the 100 is a pick
(mantle's review). What a tick costs is lateness: a timer fires up to one tick after its deadline, and a
parked shard adds its wake, so a timer's lateness is bounded by `tick + wake p99` (DERIVED). A tick finer
than the mean wake buys nothing, since a parked shard cannot run sooner than it wakes; so the tick is the
mean wake. The clock-read term goes: the loop reads the clock once a step whatever the tick, so the tick
does not change what the clock costs (DERIVED). A consumer states its timers' lateness tolerance; a
configuration whose `tick + wake p99` exceeds it is refused at build, naming both, rather than run late.

**Change: the next deadline in O(levels), not O(armed timers).** slates rescans every armed entry after a
fire or the earliest cancel, every step (NOTE08 §3.2). hyper-rt keeps an occupancy bitmap per level (64
slots, one `u64`), so the next occupied slot is a `trailing_zeros` per level (DERIVED: six levels, six
instructions of search).

**Change: a full wheel delays, never refuses early.** slates already makes a sleep that finds the wheel
full wait for a timer to free (AUD-29-39); kept, with the count exposed.

### 3.7 Drivers

One seam (`trait Driver`): wait for completions until a deadline, register one-shot readable or writable
interest, report the clock. Readiness mode on every OS:

| OS | Driver | Notes |
|---|---|---|
| Linux | epoll | `EPOLLONESHOT`; the kick an eventfd |
| macOS, FreeBSD | kqueue | one-shot `EVFILT_READ`/`EVFILT_WRITE`, `EVFILT_USER` kick [LEMON] |
| Windows | IOCP with AFD polling | `IOCTL_AFD_POLL` on `\Device\Afd` for socket readiness [AFD]; the kick a posted packet |
| all | simulation | §11 |

**Change: io_uring is not carried.** slates' io_uring driver runs in readiness mode only (`PollAdd`, no
read, write or fsync; NOTE08 §3.4), so it does what epoll does at the cost of a second path, and its
zero-timeout harvest needed a raw `enter` to stop sleeping 0.3–1.0 ms (slates bug 2026-09-26). One path
per mechanism (NOTE32 §3.1): epoll on Linux. Mantle's disk path is Linux native AIO by the owner's
decision, not io_uring (§7). If a measurement shows io_uring's completion mode winning for sockets, it
returns as the one Linux path, not beside epoll. The `Driver` seam therefore stays open to completion
mode: slates' FUSE requests cost two kernel handoffs each (19,222 across one `pip install`, slates
BENCHMARKS 2026-10-06), and FUSE over io_uring (Linux 6.14, **UNVERIFIED**) is the lever that would bring
it back under that rule.

**To measure: one-shot against edge-triggered.** `EPOLLONESHOT` costs an `epoll_ctl(MOD)` per readiness
event per descriptor, a syscall per wake on a hot descriptor (a fleet UDP socket, a long-lived TCP
connection, `/dev/fuse`). Persistent `EPOLLET` with drain-to-`EAGAIN` (kqueue `EV_CLEAR`) is measured
against it on those rows of §12 before the choice is fixed (slates-dc's review).

**Change: Windows readiness for writes and for TCP.** slates' AFD reactor serves reads and UDP sends;
hyper-rt registers `AFD_POLL_SEND` for writability and `AFD_POLL_ACCEPT`/`AFD_POLL_CONNECT_FAIL` for
listeners and connects, which TCP on Windows needs (§5.2).

## 4. Running a future: `block_on`

Every consumer needs to run one future to completion on the calling thread: a CLI command, `main`, a
test. `LocalRuntime::block_on(future) -> Result<T, RtError>` builds one shard on the calling thread, spawns
the future as its root task, runs the loop until the root completes, then cancels and joins whatever the
root left behind (structured: nothing outlives the call) and returns the output. slates has
`run_until_idle` only, which cannot return a value and does not end while a socket is merely registered.

The test harness is the same call: `hyper_rt::test::run(|| async { ... })` inside a plain `#[test]`, with
a test configuration (§10.3). No procedural macro crate.

## 5. Sockets

All sockets are non-blocking, close-on-exec, and awaited through the shard's driver. One module per kind;
the platform calls behind one seam per kind (`netsys`), Unix through `rustix`, Windows through
`windows-sys` in the listed OS-interface files.

### 5.1 UDP

- **IPv4 and IPv6.** `SocketAddr`, not `SocketAddrV4` (slates is IPv4 only, NOTE08 §3.5); an IPv6 socket
  is `IPV6_V6ONLY` so a dual-stack bind is two sockets, stated.
- **Batched where the kernel offers it**, from hyper-tokio's `sys` layer, moved here so both drivers share
  it: Linux `sendmmsg`/`recvmmsg` with `UDP_SEGMENT` and `UDP_GRO`; macOS one call a datagram
  (`recvmsg` when stamped); Windows one call a datagram, `UDP_SEND_MSG_SIZE` and
  `UDP_RECV_MAX_COALESCED_SIZE` owed (docs/transport.md §4b).
- **Kernel receive stamps**: `SO_TIMESTAMPNS` on Linux, `SO_TIMESTAMP_MONOTONIC` on macOS, the read's time
  on Windows and in simulation, as hyper-tokio does, handed over on the **shard's clock**, the one its
  timers run on. The kernel stamps on a clock of its own (`CLOCK_REALTIME`; `mach_absolute_time`, which
  stops while the host sleeps where the shard's `CLOCK_MONOTONIC` does not), so a stamp is carried over by
  its age: the stamps' clock read after the receive and the shard's after it, so a preemption between the
  two makes a stamp late, never early; held within the read's time and no earlier than the arrival before
  it (`udp/batched.rs`).
- **Astray reports.** A UDP socket reports an earlier datagram's ICMP refusal or unreachable (and, on
  Windows, a send to a closed port as `WSAECONNRESET`) on a later call that moves no data. The seam returns
  it as its own outcome, counted and read past, never an error of the socket; a batched receive reads past
  at most a batch of them per call.
- **Don't-fragment** set per OS, as slates does; a send the OS has no room for is `WouldBlock`, awaited on
  writability, never a lost path (slates' AUD-29-61).
- **Socket activation** (`adopt`, `into_owned`) kept from slates, on Windows too.
- **Done** (2026-10-06): `UdpSocket` and `Batched` on IPv4 and IPv6; the batched calls and stamps moved from
  hyper-tokio's `sys` layer into `udp/linux.rs` and `udp/macos.rs`. Measured by `tests/udp_batched.rs` on
  macOS and on Linux 6.12 (Docker): a 32-datagram batch crosses loopback in fewer calls than datagrams
  both ways on Linux, every arrival between its send and its read; Windows is clippy-checked, its run owed
  to the Windows CI lanes.

### 5.2 TCP

On all three OSes, IPv4 and IPv6: `TcpListener::bind(addr, backlog)`, `accept`, `TcpStream::connect`,
`read`, `write`, `write_all`, `shutdown`, vectored reads and writes.

- **Reads into the caller's buffer.** `read(&mut [u8])` and `read_vectored(&mut [IoSliceMut])` read
  straight into slices the caller owns, with no allocation in the runtime, so a request body streams into
  hyper-block's aligned buffers (mantle's S3 listener). Nothing on the read path copies.
- **Accepting on every shard** (`tcp::serve`). On Linux each shard binds its own listener with
  `SO_REUSEPORT` and the kernel spreads connections among them by a hash of their addresses. macOS has
  `SO_REUSEPORT` with BSD's meaning, which does not balance (measured: 32 of 32 connections on the last of
  four listeners bound; XNU has no `SO_REUSEPORT_LB`), and Windows has none, so on both one acceptor shard
  accepts and hands each accepted socket to the least-loaded shard (the fewest open connections, counted
  per shard on process-wide cells) by spawn request, the socket moving as its owned handle and the handler's
  future made on the target, so it need not be `Send`. Handlers run detached. An accept loop that runs out
  of descriptors waits for one of its own connections to close; with none open it stops and records the
  error (`Serving::ended`), a descriptor limit below the process's need being configuration, not a wait. The connection budget is process-wide:
  one atomic count every shard's accept checks and takes from before serving.
- **TLS composes on top.** hyper-tls is sans-I/O; a TLS session over a `TcpStream` is the session's bytes
  read into and written from the stream's buffers. An end-to-end test runs a TLS 1.3 HTTP/1.1 exchange
  over hyper-rt's `TcpStream` (mantle's listener runs plain HTTP on loopback and TLS elsewhere).

- **Backpressure.** A write that finds the send buffer full awaits writability; the task yields, the shard
  does not block.
- **`TCP_NODELAY`** on every stream (slates' measurement: a reply in two writes waited 40 ms for the
  peer's delayed acknowledgement with Nagle on [RFC896]).
- **`TCP_NOTSENT_LOWAT`** where the OS offers it (Linux, macOS) [NOTSENT], so writability means "the
  unsent queue is low", which keeps a slow peer's backlog in the application's hands, where a
  connection budget can see it. The value is the consumer's (one write's worth is the stated default:
  DERIVED from the rule that the next write should find room).
- **`TCP_USER_TIMEOUT`** where offered (Linux) [RFC5482]: the consumer states how long unacknowledged
  data may wait before the connection is declared dead.
- **A connection budget.** The listener takes the maximum it will hold open, process-wide (above); an
  accept past it is closed at once and counted (refusal by close, which an HTTP client sees as a reset;
  mantle's listener decides whether to answer 503 first by reserving one slot for that).
- **Windows** through AFD: `AFD_POLL_ACCEPT` for listeners, `AFD_POLL_SEND` for writes,
  `AFD_POLL_CONNECT_FAIL` for connects.
- **The retransmission deadline on all three**: Linux `TCP_USER_TIMEOUT`, macOS `TCP_RXT_CONNDROPTIME`
  (whole seconds, rounded up), Windows `TCP_MAXRTMS`. `TCP_NOTSENT_LOWAT` on Linux and macOS; Windows has
  no writability threshold (`SIO_IDEAL_SEND_BACKLOG_QUERY` is a query), so it is reported not offered.
- **No `SIGPIPE`**: `MSG_NOSIGNAL` on Linux, `SO_NOSIGPIPE` on macOS; Windows raises none.
- **Done** (2026-10-06): `TcpListener`/`TcpStream` on all three OSes through `tcpsys.rs`, IPv4 and IPv6,
  vectored reads and writes, the options above read back, `ConnectionBudget` on a process-wide cell
  (wait-free take; a slot travels with its stream through `into_parts`), `bind_shared` (`SO_REUSEPORT`).
  `tests/tcp.rs` and `tests/tcp_streams.rs` pass on macOS and Linux 6.12; Windows is clippy-checked, its
  run owed to the Windows lanes. `tcp::serve` done the same day (`tests/tcp_serve.rs`, macOS
  and Linux). TLS composition: `hyper-tls/tests/over_hyper_rt.rs` runs an HTTP/1.1 exchange over TLS 1.3
  with a mebibyte response on hyper-rt's `TcpStream` (macOS and Linux); the test lives in hyper-tls so
  hyper-rt's own tests build no C for a cross target. The pump keeps ciphertext the session has not taken
  pending until the plaintext before it is read: feeding a whole read at once overflowed hyper-tls's
  plaintext buffer, the backpressure a consumer's pump must keep too.

### 5.3 Local stream sockets and the peer's identity

`LocalListener` and `LocalStream`: `AF_UNIX` stream sockets on all three OSes. Windows has had `AF_UNIX`
stream sockets since Windows 10 1803 [AFUNIX], so one mechanism serves all three, and AFD polls them as
it polls TCP (**UNVERIFIED** that `IOCTL_AFD_POLL` reports readiness on `AF_UNIX` sockets; §15).

The peer's identity is read from the kernel, never from the bytes: `SO_PEERCRED` on Linux,
`LOCAL_PEERCRED` (or `getpeereid`) on macOS, and on Windows `SIO_AF_UNIX_GETPEERPID` then the process
token's user SID [AFUNIX]. Access is first kept by the directory: the socket lives in a directory only its
owner may enter (focal's `PrivateDir`, the owner-only DACL on Windows), so a peer of another user cannot
connect at all, and the identity check is the second wall.

**Done** (2026-10-06): `local::{LocalListener, LocalStream, current_user}` over `localsys.rs`, the stream
calls shared with TCP. `tests/local.rs` passes on macOS and Linux 6.12: each end sees the other as this
process and this user, and a bind over an existing path is refused. A Linux connect to a full accept queue
fails `EAGAIN` (unix(7)), which writability would never end, so it is a typed refusal the caller retries.
Windows is clippy-checked; its run (and AFD's readiness on `AF_UNIX`) is owed to the Windows lanes.

focal today uses a named pipe on Windows; moving it to `AF_UNIX` is focal's decision, recorded in its own
documents. If AFD does not poll `AF_UNIX`, hyper-rt adds named pipes through overlapped I/O on the
completion port, as the one Windows mechanism, and records why.

### 5.4 Name resolution

`resolve(host, port) -> Vec<SocketAddr>` through the platform resolver (`getaddrinfo`, and
`GetAddrInfoExW` on Windows) [GAI], so `/etc/hosts`, `nsswitch`, mDNS and enterprise resolvers behave as
every other program on the host sees them. `getaddrinfo` blocks, so it runs on the bounded blocking pool
of §9, with its own share (resolution is rare: a context's endpoint, a join). A resolver of hyper-rt's
own over UDP would see none of the host's configuration; rejected.

## 6. Signals and stdio

### 6.1 Signals

`signal(Kind) -> SignalStream` for `Interrupt`, `Terminate`, `Hangup` (Unix) and the console's Ctrl-C,
Ctrl-Break, close, logoff and shutdown (Windows).

- **Unix:** one process-wide handler per signal installed with `sigaction`; the handler does only what is
  async-signal-safe: it sets the signal's bit in an atomic word and writes one byte to a non-blocking
  pipe, ignoring a full pipe (the bit already records it) [SELFPIPE]. A subscribed task awaits the pipe's
  readability through its shard's driver, swaps the bits to zero, and yields one event per signal kind
  that arrived. A signal that arrives several times between reads is one event (as POSIX signals
  already coalesce). The handler's installation is `unsafe` (a raw function pointer the kernel calls),
  in `sys/signal.rs`, an OS-interface file.
- **Windows:** `SetConsoleCtrlHandler`; the handler (the OS runs it on a thread of its own) sets the
  event's bit and posts a packet to each subscribed shard's completion port.
- **Bound:** one handler per kind, subscribers bounded by the shard count (DERIVED: at most one
  subscription per kind per shard is needed, since a shard's tasks share it).

**Done** (2026-10-06), with one change: on Unix the subscribers are woken by **one signal thread** that
reads the self-pipe, not by each shard awaiting the pipe, so one pipe serves any number of subscribers
without one draining it for the rest; Windows' handler, on the OS's thread, wakes them directly. A kind
with a subscriber is handled on Windows (the default end does not run). `tests/signals.rs` (Unix).

### 6.2 Stdio

Reading standard input and writing standard output without blocking a shard. Readiness works on Unix for a
pipe, a socket or a terminal but not for a regular file (epoll refuses one with `EPERM`), and on Windows a
console or an anonymous pipe cannot be polled at all. One mechanism therefore serves every OS and every
kind of handle: **one reader thread and one writer thread for the process**, each moving bytes between
the handle and a bounded queue of fixed-size chunks (the chunk is one page, the queue's depth the
consumer's), handing them to tasks through §8's channel. Bounded: two threads, `depth × page` bytes each
way (DERIVED). **Done** (2026-10-06): the buffers are allocated once and recycled between task and thread
(full ones one way, empty ones back), and stdout is locked per chunk, not for the thread's life, so other
writers interleave. `tests/stdio.rs` streams a mebibyte through four 4 KiB buffers each way in a child
process.

## 7. The completion seam for device issuers

A device issuer (hyper-block's writer and readers: Linux native AIO by mantle's owner's decision, IOCP
on Windows, the bounded pool on macOS; NOTE26 §2.6) completes an I/O on its own thread and must wake the
task waiting for it, so that one shard waits on network and disk together without a thread per request.

That seam is `std::task::Waker`, and nothing more is needed: the waiting task hands the issuer its waker
(a `Copy` word, §3.2), the issuer writes the result into the slot it owns for that request (hyper-block's
`Pending`), and calls `wake`; the wake sets the task's bit and kicks the shard if parked. The cost is one
`fetch_or` and at most one kick per completion, and an issuer completing a batch kicks once (the second
wake finds the shard already kicked or running) (DERIVED from §3.5). This is the one-to-one wake of NOTE26
§5.2: no waiter is woken that is not admitted. NOTE32 §3.1 already chose `Waker` as the crossing for every
shared crate, so hyper-block needs no dependency on hyper-rt.

## 8. Synchronization between tasks and threads

tokio's `sync` module is used across focal (`watch` 30, `oneshot` 16, `mpsc` 11, `Semaphore` 8,
`Notify` 8, `Mutex` 2). hyper-rt offers one primitive per need, each usable between any two of: a task on
the same shard, a task on another shard, a plain thread. One mechanism per primitive, not a local and a
remote variant.

**The shared word without `Arc`: a wake cell.** Every primitive has state that both ends must see: at
least the waiting task's word and whether the other end is gone. hyper-rt keeps that state in a **cell**
of a process-wide table, segmented and allocated a segment at a time up to a configured maximum (the
consumer's bound on live primitives; past it, `Capacity`), each cell with a generation (odd free, even
live, as the shard registry's slots), the waiter's word (`AtomicU64`), and a count of live handles. A
handle is `(cell, generation)`, `Copy`-sized; the last handle to drop frees the cell and advances its
generation. A handle count in an arena cell is a generational arena, not shared ownership of a value: no
`Arc`, no `Mutex` (DERIVED: the cell holds no `T`).

**The value itself crosses by move**, through `std::sync::mpsc::sync_channel` (bounded, already used by
slates for control messages; std's own internals are std's, as tokio's `Arc`s are tokio's in
hyper-tokio). A send is `try_send`, then a wake of the cell's waiter; a receive is `try_recv`, and when
empty, the task registers its word in the cell and re-checks before returning `Pending` (no lost wake:
DERIVED from the order send-then-wake against register-then-check). A plain thread uses the channel's
blocking `recv` and never touches the cell.

| Primitive | Built as | Bound |
|---|---|---|
| `channel::<T>(n)` (many senders, one receiver) | `sync_channel(n)` + a cell | `n` messages; a full channel is `Full` to `try_send`, and `send().await` waits for room, its waiters queued one to one (NOTE26 §5.3) |
| `oneshot::<T>()` | `sync_channel(1)` + a cell | one value |
| `watch` of a `Copy` value that fits a word (`u64`) | the value and its version packed in the cell's atomic words | one value; readers see the latest and its version |
| `watch` of a larger value | the publisher keeps it; readers are notified through the cell and ask the owner for it (a request through a `channel`) | the owner's |
| `Semaphore(n)` | an atomic count + a bounded FIFO of waiting words; a release wakes exactly the waiters it admits, in arrival order | `n` permits; waiters bounded by the configured queue, past it `Capacity` |
| `Notify` | the cell alone: `notify_one` wakes the registered waiter; a notification with no waiter is kept as one pending permit | one |

No broadcast primitive is offered: a broadcast to N waiters is the O(N) hazard of NOTE26 §5.1. An event that
concerns every waiter (a shutdown) completes each waiter's cell once (NOTE26 §5.3 rule 2).

**INFERENCE to measure:** std's channel costs one or two atomic operations a message on its fast path;
§12 measures it against tokio's `mpsc` and against a ring of hyper-rt's own before the choice is final.

## 9. Blocking work

Work that blocks a thread (a synchronous library call, `getaddrinfo`, a code sandbox, a file read on a
platform without asynchronous file I/O) runs on **one bounded pool per process**: `workers` threads,
started once and reused, each with its own job slot woken one to one (NOTE26 §2.5, §5.3); jobs wait in a
bounded queue; past it, `Capacity`, decided before any thread is asked to start. Its size is the
consumer's, derived from its own measurement (for mantle, the device's queue and measured knee, NOTE26
§2.5; for focal, the code sandbox's concurrency budget). Each consumer may divide it into named shares
so one kind of work cannot take all of it. A job's result returns through a `oneshot`. A blocking job
cannot be cancelled (the call is the OS's); its share's bound is what limits the damage, stated.

**No locks, as built** (2026-10-06): the workspace's wall forbids `Mutex` and `Condvar`, so a dispatcher
thread owns the job queue's receiving end and the idle workers' reports; each worker owns a one-job slot
and reports itself idle over a bounded channel, so each job goes to exactly one idle worker. Shares and
counters are atomics. A job that panics ends the job, not its worker (an unwind boundary), and gives its
share back. One per process: a second `start` is refused; `stop` drains and joins. Name resolution
(§5.4) is `dns::resolve`, on the share named `dns`. `tests/blocking.rs` passes on macOS and Linux 6.12.

## 10. Configuration from the machine

### 10.1 What the runtime needs

Of slates' machine profile, the runtime reads: the cores the process may run on and their classes, the CPU
budget a cgroup grants, the base page, the mean and p99 wake (park to running), the null system call's
cost, and the clock read's cost. The codec, hash, memcpy, fault and lock probes serve slates' storage
and are not carried.

### 10.2 Calibration, measured once and kept

`Calibration::measure(budget)` runs the wake, system-call and clock probes under slates' stopping rule
[KJ] with the sample count from WILKS (59 for a 95 %/95 % bound on the p99) or the probe's derived catch
rate; it takes on the order of the probes' wall budget. A long-running node measures at start. A short
command must not: `Calibration` encodes to a fixed, versioned, checksummed record the consumer stores
(focal in its data directory), keyed by the machine's identity (CPU model, OS build, core count,
page size, power source), and a stored record is reused while its key matches and its age is under the
consumer's bound; otherwise it is measured again. A power-source change is part of the key, so it
re-measures (all of it: the record holds only the two probes that cost the budget, the wake and the null
system call; the facts are queried afresh each time). **Done** (2026-10-06): `machine::record`
(`Calibration::{encode, decode, load_or_measure}`), CRC-32C over a fixed little-endian layout, the key
bounded at 1 KiB, wall time an input; every one-byte corruption and every truncation is refused.

### 10.3 What is derived

| Constant | Formula (slates' derivations, kept) |
|---|---|
| spin before parking, step quantum | the mean wake [KARLIN] |
| timer tick | the mean wake, refused if `tick + wake p99` exceeds the consumer's stated lateness tolerance (§3.6) |
| control-queue depth | `wake p99 / system call` rounded up to a power of two (Little's law at the overflow target: a producer sending one message per system call for as long as the consumer's wake takes at its p99) |
| loop batch | min(mean wake, the consumer's latency objective) / measured per-item cost (§3.3) |
| shards | the fastest class of cores the process may run on that its CPU budget runs at once, less the consumer's **reserved cores**, at least one; pinned only where the process owns its cores |

**Reserved cores** replace slates' fixed "less one for control and the OS" (a pick). A consumer states
the cores its non-shard threads need (its device issuers, the blocking pool's busy workers, stdio), as
configuration: from the CPU those threads are measured to take, rounded up to whole cores, where it has
measured them, and otherwise the count of its own threads that run continuously. The default is zero
reserved cores with a note in the runtime's start report, since a process with no such threads loses a
shard to nothing.

A test or a short command passes a stated configuration instead (one shard, the defaults named with their
reason), so `block_on` costs no calibration.

## 11. Simulation

slates' `SimRuntime` steps every shard on one thread and jumps a virtual clock to the next event
(NOTE08 §3.6), with a seeded UDP fabric (delay, jitter, reordering, a drop-tail bottleneck,
Gilbert–Elliott loss, path and interface MTU, NAT expiry). hyper-rt keeps it and adds:

- **hyper-sim drives it.** The simulation driver takes its clock and its datagram fabric from a
  hyper-sim world through a trait, so a world of focal nodes, slates daemons or mantle nodes runs under
  the one seeded scheduler and checker [FDB §4], instead of a second fabric here.
- **TCP and local sockets in simulation**, as ordered byte streams with the same fault model, since mantle's
  listener and focal's local clients need them.
- **Completions from simulated devices** through the same `Waker` seam as §7.

## 12. Measurement and tests

Benchmarks (`docs/benchmarks.md`, hardware, date and command each), each row against what each consumer
runs today, on the same machine, with allocations, reallocations and page faults counted per operation
(hyper-measure): tokio 1.53 (multi-thread and current-thread) for focal; slates-rt at `6b9ce5c` for
slates; and for mantle, which has no async runtime, its current drivers: `bench chunk`'s clients as
records and the device issuer's pool. Rows:

- spawn and run a trivial task; a local wake; a cross-shard round trip, both parked and both spinning; a
  foreign-thread wake; a timer of 100 µs (lateness at p50, p99, max);
- a channel round trip within a shard, across shards and from a thread; `oneshot`; semaphore hand-off;
- UDP echo, one datagram and batched; TCP echo and a 1 MiB transfer; local-socket echo;
- the hyper-transport exchange of `docs/transport.md` §4a under hyper-rt against hyper-tokio.

Sample counts by WILKS; latencies from intended start where the load is open [NOTE26 §3.2].

Tests: slates' own (`tests/*.rs`, ported with their origin listed in `ORIGIN.md`) as the oracle that the
port changed nothing it did not mean to; loom for the park/kick protocol and the wake bitmap; the
differential test (the same task program on the OS driver and the simulator yields the same trace); and
end-to-end tests of real processes on real sockets: a focal node and client, a slates daemon, mantle's
listener under an HTTP load.

## 13. `unsafe`, and where

The files to be listed in `scripts/check-contracts.py`, each with the interface it binds:

| File | Interface |
|---|---|
| `src/waker.rs` | `RawWakerVTable` over the `Copy` word |
| `src/registry.rs` | the static slot table's entry pointer under counted readers (slates' protocol; NOTE08 §3.8 flags one `&mut Entry` formed while foreign readers hold `&Entry`, to be closed in the port) |
| `src/retire.rs` | the counted-pin protocol's raw entry pointer: published from a `Box`, read under a pin, freed by the last reader of its retirement (loom: one and two readers against a retirement, and a retirement inside a read) |
| `src/sys/kqueue.rs` | `kevent` (rustix's is safe; the `EVFILT_USER` trigger is not covered) |
| `src/sys/epoll.rs` | none expected (rustix) |
| `src/sys/windows/{iocp,afd,net}.rs` | completion ports, `NtDeviceIoControlFile` for AFD, Winsock |
| `src/sys/{linux,macos,windows}/udp.rs` | `sendmmsg`/`recvmmsg`, `UDP_SEGMENT`/`UDP_GRO`, receive stamps (from hyper-tokio's `sys`, which is listed today) |
| `src/sys/signal.rs` | `sigaction`; `SetConsoleCtrlHandler` |
| `src/sys/peer.rs` | `SIO_AF_UNIX_GETPEERPID`, `OpenProcessToken` (Windows) |
| `src/machine/facts.rs`, `src/machine/pin.rs` | `sysctlbyname`, thread affinity |
| `src/thread_clock.rs` | per-thread CPU clocks |

slates' `LocalRuntime` holds its context as a `NonNull` and dereferences it (`runtime.rs`, Miri-checked);
hyper-rt holds the context by value in `LocalRuntime` and lends it, so that `unsafe` goes.

## 14. The transport driver

`hyper_rt::transport::Driver<C, B, D>` drives a hyper-transport `Endpoint` and a hyper-datagram `Plane`
on a shard, with the API of hyper-tokio's `Driver` (`bind`, `new`, `endpoint`, `event`, `poll_event`,
`flush`, `local_addr`, `stats`) so a consumer moves from one to the other by type. The UDP layer is §5.1's,
shared with hyper-tokio once hyper-tokio takes it from here (one `sys` layer, two drivers).

**Done** (2026-10-06) as the crate `hyper-rt-transport` rather than a module: hyper-transport builds aws-lc
(C), and hyper-rt stays pure Rust so it checks for every target from any host. The endpoint's `Instant`s
are derived from the shard's clock (one anchor), so on the simulation runtime the endpoint runs on
simulated time. `tests/exchanges.rs` runs hyper-tokio's exchange scenario — 19 exchanges in every class,
9 MiB each way, every byte checked, the driver's future dropped mid-wait by a 1 ms race — with both
endpoints on one shard:

| release, 3 runs | handshake | 19 exchanges | datagrams a call (send / receive) |
|---|---|---|---|
| macOS, hyper-rt-transport, one shard | 5.1–6.2 ms | 103–106 ms | 1 / 1 |
| macOS, hyper-tokio e2e, two processes | 26.2–26.6 ms | 111–128 ms | 1 / 1 |
| Linux 6.12 (Docker), hyper-rt-transport | 3.2–30.9 ms | 47–65 ms | 8.6 / 43 |

Not yet a like-for-like comparison: hyper-tokio's scenario runs its ends in two processes, this one on one
thread. §12's row runs both drivers the same way.

## 15. Open items, to settle before the part that rests on them

1. Whether `IOCTL_AFD_POLL` reports readiness on Windows `AF_UNIX` sockets (§5.3). Measured on the Windows
   runners first; the named-pipe fallback is designed if not.
2. macOS `TCP_NOTSENT_LOWAT` semantics against Linux's (§5.2), read against `xnu` source.
3. The wake bitmap against slates' rings on the cross-shard and foreign-wake rows (§3.2), before the rings
   go.
4. std's `sync_channel` against tokio's `mpsc` and a ring of hyper-rt's own (§8).
5. `SBITMAP` and `AFUNIX` read at a named revision.
6. A plain `Release` store of the wake bitmap's pending flag failed loom 0.7.2's parking model: the sender's
   own later load read the flag's earlier value, which coherence forbids. The flag is published with a swap
   (the model passes); the execution is to be reduced to a loom report or to an error of this model's.
7. One-shot against edge-triggered registration (§3.7), and packed wake words against one per cache line
   (§3.2), measured on §12's rows.
8. The spin window from the startup wake probe rarely hit in slates' bench configuration (§3.2), though it
   is not the kick result's cause: measure the startup mean against an online p50/p90 on its own merits
   (spin hits, CPU, p99 under load).
   And why a shard that makes no system call while spinning sees 10–13 µs scheduling gaps that a kick's
   syscall seems to close (slates, inferred): the mechanism, measured, before kick-if-parked is kept.
9. Closed 2026-10-06 (mantle's review): `Runtime`'s `stop` retried a full control channel with
   `yield_now` and no counted bound (slates' shape). The stop is now a flag on the shard's registry entry
   (`registry::request_stop`), published and kicked like a message but taking no channel slot; the shard's
   next drain applies it after that batch. `tests/admission.rs` holds the shard until it sees the stop,
   with the channel full: the old send never found room and hung.
10. The calibration record's key and its age bound for focal's short commands (§10.2): what a stale record
   costs (a mis-sized spin) against what a fresh one costs (the probes' budget per command).

## 16. Order of work

1. This document, reviewed by mantle for its needs.
2. The crate from slates' `rt`, `mem` (handle, slab, segmented, rings) and `machine` (facts, placement, wake,
   stats, the stopping rule) at `6b9ce5c` or later, conformed to this repository's wall, its tests
   passing: the port, with `ORIGIN.md`. slates' locked arenas (`ChunkArena`, per-block locking since
   `6b9ce5c`) are storage, not runtime, and are not carried; the runtime locks and pre-faults nothing.
3. The departures of §3 (bitmap wakes, the loop and the desk, O(levels) deadline, no io_uring), each against
   its benchmark row.
4. `block_on` and the test harness (§4); calibration records (§10).
5. Sockets (§5), signals and stdio (§6), synchronization (§8), the blocking pool (§9).
6. The transport driver (§14) and the shared UDP `sys` layer.
7. Simulation through hyper-sim (§11).
8. Benchmarks and end-to-end runs (§12); six-target CI.
9. focal moves onto it (focal's own plan); slates and mantle each on their own schedule.
