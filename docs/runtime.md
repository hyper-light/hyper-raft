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
| MOM | G. Lugosi, S. Mendelson. "Mean Estimation and Regression Under Heavy-Tailed Distributions: A Survey." Foundations of Computational Mathematics 19, 2019 (arXiv:1906.04280, read there), §2.1 and Theorem 2; the median of means after Nemirovsky and Yudin 1983, Jerrum, Valiant and Vazirani 1986, Alon, Matias and Szegedy 1996 (through MOM) | peer-reviewed |
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

**A task's code runs inside an unwind boundary of the shard's own** (2026-10-11; CLAUDE.md §1, production
code never panics). A task's poll and its future's drop are the caller's code, run on the shard's thread
among every other task's: a poll that unwinds ends its task `Outcome::Unwound`, its joiner told so, its
future dropped inside a boundary too, and a drop that unwinds is counted (`Counters::unwound`); the shard
and its other tasks go on. Before, a task that panicked unwound the shard's loop, every task on the shard
with it (`tests/unwind.rs`; on main, the unwind came out of `run_until_idle`).

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
objective states it, and the smaller of the two applies (`RuntimeConfig::latency_objective_ns`).

**A task that wakes itself waits for the inputs.** A step's polls take the wakes their polls make (PR
#16: a local request and reply need not repeat the loop for each end), and a task that woke itself
in its own poll, by `futures::yield_now` or as any future with more to do, was listed again at once
and polled again until the step's count ran out, while every wake from another thread waited in the
bitmap: eight clients onto a shard beside a task computing in 50 µs slices waited 0.75–0.97 ms at the
median, the task holding 96–98% of the shard (`docs/benchmarks.md`, "hyper-rt: the loop under load,
against slates' runtime and tokio"). Such a task is now listed apart (`queue::LocalQueue::defer`) and
joins the ring only once the loop has read its inputs again (control, wakes, timers, pollers, the
driver's completions), behind what they made ready: those clients wait 0.06 ms, about one slice. tokio
defers its `yield_now` the same way: "Don't wake the task immediately, as that would push it right back
onto the run queue and it could be polled again before other tasks or the IO/timer driver get a chance
to run" (tokio 1.53.1, `src/task/yield_now.rs`; since 1.23, PR #5223); a plain self-wake it lists again
at once, and the same clients waited 11.8 ms there. When only such tasks remain, the batch reads its
inputs and goes on rather than ending the step, so a round of yields costs a reading of the inputs, not
a step: one task yielding costs 680–686 instructions a yield, against 1,032–1,269 when each yield ended
its step and 425–445 when it read nothing between, and sixteen 469–475 against 500–523 and 423–436
(`tests/yield_defers.rs`).

**The budget is a time; the count is it over a trivial item's cost.** Once the polls since the inputs
were last read have run the budget and listed tasks of their own (a wake of the shard's, a spawn),
which go ahead of every wake still unread, the batch reads the inputs again before its next poll, and
the step goes on to its count. Polls that list nothing leave the reading to the step's end: what
arrives goes behind the tasks already listed either way, and a drain is a read-modify-write on lines
the wakers write too; read after every long poll, eight clients onto one shard lost 3–6% of their
throughput to it (`tests/batch_time.rs`). Ending the step at the budget instead put each long poll in a
step of its own and undid §3.9's judging of long polls by step (`tests/attribution_by_step.rs` failed).

### 3.4 The loop

Each step: drain the control queue (spawns, cancels, shutdown), drain the wake bitmap, take the driver's
completions, expire timers, list the tasks that woke themselves behind them, poll up to `batch` ready
tasks, reading those inputs again within the step as §3.3 says. When nothing is ready the shard spins for what
blocking costs it and then parks in its driver until a kick, a completion or the next deadline: the
2-competitive spin-then-block rule [KARLIN], with the threshold the CPU one park/wake cycle costs,
measured on the shard's own parks (§3.9, D2; it was the measured mean wake, slates' rule). slates'
attribution of a long poll to the task or to the host (`attribution.rs`) is kept.

**The driver is polled only when it can hold work, and the spin polls it once a quantum.** A busy loop
takes the driver's completions without blocking once a quantum, and the spin asks it as well as its
inboxes. Each ask is a system call (`kevent`, `epoll_wait`, `GetQueuedCompletionStatusEx`), and mantle's E2
range bench (macOS, request and response between client threads and range tasks, 2026-10-07) found a
shard with no readiness wait spending 50–60% of its time in `kevent` (`sample`): the spin asked every
turn, about every few hundred nanoseconds, and a single client's round trip took 13 µs at the median for
a 1 µs operation. The shard arms its driver only for its tasks' readiness waits (`apply_interest`,
`deliver`), so with none registered the driver holds nothing for a task, and `harvest_io` makes no call
(a kick left behind ends the next park at once, as it would have). With waits registered, the spin asks
the driver at its start and then once a quantum, the measured wake's cost: a readiness waits no longer
than a wake would, while a turn costs no system call (`tests/idle_spin.rs`: no poll with no wait; at most
one poll a spin with a wait held and a quantum longer than the window, where every turn polled before:
10,434 polls in one spin).

**Every way of running the loop takes the driver's completions.** The harvest is part of the step
(`harvest_turn`), not of the threaded `run` alone, so `block_on`, `run_until_idle` and a caller's own
`step` and `park` deliver readiness as `run` does. A busy native turn asks the driver once a quantum, the
same quantum the spin uses, measured from the driver's last actual retrieval; an idle turn asks at once;
the simulation asks every turn, since its virtual clock does not advance while a task runs and a quantum
would never pass. Only a wait in the driver renews the retrieval's age: a park skipped for ready local
work, or a spin answered by an inbox, does not postpone it. Before, a task that yielded continuously
kept `block_on`, a manual `step` loop and a skipped `park` from ever asking, so a datagram already in the
socket waited until the yielding task stopped (`tests/busy_readiness.rs`, `manual_step_readiness.rs`,
`manual_park_readiness.rs`: each failed at its watchdog on `a63ec4b`), the simulation's manual steps
never delivered one at all (`virtual_step_readiness.rs`), and `run_until_idle` returned with a ready
datagram undelivered (`owned_loop_readiness.rs`). `run_until_idle` now makes its last turn a
nonblocking drain and returns with external waits still registered, which a later call delivers.

**Losing the driver is terminal.** One policy reads every result of a driver wait, whether a busy
harvest, the spin or a park: `DriverLost` is classified before any completion of the same wait is
delivered and before a handle is armed again, so a partial read never completes on a dead driver. The
shard closes admission, local spawns and foreign control alike (`ShardGone`), drops its control
receiver so requests already queued retire, and cancels every live task, including the one being
polled, at its next poll boundary. Ordinary tasks retire at that boundary. An admitted cooperative
service retains its future and task generation until its cancellation cleanup returns `Ready`; an
external device transfer can therefore delay exit until its own completion. After driver loss the
same wake bitmap carries channel completions. The actual shard thread is bound once during cold
context construction; after driver loss, `Thread::unpark` carries one retained cleanup permit.
The owning loop announces parking, rechecks the bitmap, and calls `thread::park` only when idle.
A stale or spurious permit carries no completion. Manual `step`/`park` remain nonblocking after
loss and leave pending services owned (`cleanup_permit_stress.rs`).
No cleanup task can arm a new readiness wait or timer on the lost driver. `block_on` then returns
`ShardGone` and the threaded runtime's join
`DriverLost`. A driver's other refusals to arm a handle again reach the waits still on it as that exact
error, never as readiness (`tests/busy_driver_loss.rs`, `manual_step_faults.rs`, `park_arm_loss.rs`,
`terminal_admission.rs`, `threaded_loss_reuse.rs`).

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
The two terms are told to the shard's tasks, `futures::timer_tick_ns` and `futures::scheduler_overrun_ns`
(the measured overrun standing for the wake's tail): a wait that must end before another's sets both aside
(focal answers "not yet" within its asker's period by them, 2026-10-10).

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

**Readiness waits** (mantle's reviews, 2026-10-06):
- **One registration per handle.** The loop keeps the waiters per handle and direction (`interests.rs`) and
  arms the driver once per handle with the union.
- **Each wait is its own.** A wait takes a slot of the desk's wait table (bounded by `interests_per_shard`)
  and is named by that slot and its generation, a *ticket*. Two waits of one task on one handle, such as a
  `join` of a read with a `race` of another read and a timer, are two nodes, so the race's loser withdraws
  only its own.
- **Only a fire makes a wait ready.** The loop marks a wait fired when its handle fires; a task woken for
  another reason finds it still armed. Any re-poll used to report ready, so a timer's wake made an idle
  socket win a biased race, and that wait was never withdrawn.
- **A dropped wait is withdrawn losslessly.** The intent ring holds two entries per wait slot, which one
  poll can never exceed. The handle table is open-addressed with backward-shift deletion and never
  reallocates. A handle whose last waiter leaves is disarmed (IOCP cancels its AFD poll).
- **An AFD poll delivers by the status its IRP completed with.** `STATUS_CANCELLED` delivers nothing, a poll
  that completed before its cancel delivers its events, and a failed one wakes both directions, as wepoll
  maps it to `EPOLLERR`. A replacement poll is issued before the old one is cancelled, so a refused issue leaves the
  waiters already served. A dropped driver awaits its cancelled polls' completions (`CancelIoEx` does not
  wait for a cancel to finish) until the last arrives, or at most the I/O manager's own timeout for a
  cancelled IRP, five minutes (Microsoft, *Canceling IRPs*). It counts a block still owed then, which is
  left to the kernel rather than freed.
- **A ticket names its desk.** A ticket carries its shard's registration, and another desk honours none of
  it. A wait dropped on another shard, or off any, is marked on its shard's registry entry: one atomic word
  per wait slot, holding the dropped wait's generation. A slot's generation is dropped at most once, so a
  mark is never refused for room. The control channel was not used for this, because a full one leaked the
  slot. The shard's next control drain sweeps the marks. A summary bitmap with one bit per slot lets the
  sweep visit only the marked slots, O(slots / 64 + marked), never a pass over tens of thousands of slots
  for one drop (`Counters::abandons_swept` counts the visits).

The tests are `tests/readiness_waits.rs`, `tests/duplex.rs` and the table's model test.

**Change: Windows readiness for writes and for TCP.** slates' AFD reactor serves reads and UDP sends;
hyper-rt registers `AFD_POLL_SEND` for writability and `AFD_POLL_ACCEPT`/`AFD_POLL_CONNECT_FAIL` for
listeners and connects, which TCP on Windows needs (§5.2).

### 3.8 Combinators and timed waits hold each future once

`combine::{race2, race3, join2, join3}` and `futures::within` (the race of work against a deadline that
every timed wait in the workspace goes through) are named futures (`Race2`, `Race3`, `Join2`, `Join3`,
`Within`) holding what they are given in place, once, projected by `pin-project-lite` (tokio's choice for
its `Timeout` and `MaybeDone`; no `unsafe` of this crate's own, §13). Until 2026-10-10 they were `async fn`s,
and an `async fn` holds each argument twice: the argument, beside the pinned local it moves into
(rust-lang/rust#62958, open on 1.98.1). Every level of nesting therefore multiplied what it held, and a
debug build copies each level's value on the stack besides. focal measured it with `-Zprint-type-sizes`
(nightly flags on the stable compiler through `RUSTC_BOOTSTRAP=1`, measurement only):

| focal future | as `async fn`s | as named futures |
|---|---|---|
| an enrollment round, timed (`within` and focal's `timeout` around it) | 61,528 B | 5,768 B |
| the round itself (it times its own calls) | 15,336 B | 5,720 B |
| five 3.4 KB writes and a transfer, joined in threes | 491,800 B | 22,208 B |
| the test holding that join (it overflowed a 2 MiB debug test thread; it now runs on 512 KiB) | 494,432 B | 24,848 B |

What each holds now, and what changed with it:

- **A race and `within`**: their futures and a flag set once the output is given. Polled again after
  that, they wait (`Pending`) rather than poll a finished future, which an `async` block refuses by
  unwinding; `within` also lets its deadline's timer go when it finishes, not when it is dropped.
- **A join**: a slot per future, the future while it runs and its output once it finishes, so the
  finished future is dropped then (what it holds, a timer or a reservation, let go with it) rather than
  when the last one finishes. The slot keeps its tag out of the future's bytes (`#[repr(C)]`, as tokio's
  `MaybeDone` does for rust-lang/miri#3780).
- `tests/combine.rs` holds every combinator, nested ones included, to its futures' sizes plus 256 bytes
  each, and checks the early drop and the wait after the output; each failed on the `async fn`s (the
  nested size by overflowing its test thread's stack).

The rule for a consumer is the same: a wait wrapped around work is a named future, never an `async` block
that awaits a future it was handed (focal's `focal_rt::time::Timeout`).

**The dependency.** `pin-project-lite` is a hyper-rt dependency by the owner's approval (2026-10-11), for
the pin projection of these named futures; the change waited for it outside `main`. Measured here, the same
test on both trees, futures holding 16 KiB each (bytes):

| combinator | as `async fn`s (`506c059`) | as named futures |
|---|---|---|
| `race2` | 65,584 | 32,773 |
| `race3` | 98,368 | 49,159 |
| `join2` | 65,600 | 32,784 |
| `join3` | 213,192 | 49,176 |
| `within` | 32,856 | 16,432 |
| `join3(join3(·,·,·), join2(·, within(·)), race2(·,·))`, seven futures | 1,755,856 | 114,800 |

The nested one, 15 times smaller, overflowed a 2 MiB debug test thread as `async fn`s (`SIGABRT`). Its
license (Apache-2.0 OR MIT) and its source (crates.io) pass mantle's and focal's `cargo deny` policies.

### 3.9 The loop under load: one kick a park, a spin as long as blocking costs, the cheap clock, channels with no lock

Made in mantle's copy by the owner's direction and taken here as they are (mantle commits `bfdf9c2`,
`bb0779e` and `bd7c0fd`, its `docs/design/event-loop.md` D1–D4, the evidence its
`docs/research/42-event-loop-under-load.md`, cited "note 42"). The bar: a busy machine is the normal
case, and the loop's costs must not grow with the load.

- **One kick a park (D1).** A shard's park announcement is a state word (running, parked, parked with its
  kick claimed). A sender that reads it parked claims the kick with one compare-and-swap and only the
  winner makes the system call; a wake to a running shard writes nothing to its parking line. Every
  sender used to kick: 3.2 to 4.5 system calls a park in fan-in (note 42 §3); tokio's parker claims the
  same way with its `NOTIFIED` state. `tests/one_kick_per_park.rs` (one kick for four wakes held against
  a park, four before) and the loom model `parking::two_senders_never_lose_a_word_and_a_park_takes_one_kick`
  (3,520 interleavings; it fails at 63 with the claim removed).
- **A tracking shard spins as long as blocking costs it in CPU (D2).** While a shard tracks its wake,
  its idle spin lasts the CPU one park/wake cycle costs, read on the thread CPU clocks of the parking
  shard and of the sender that claimed its kick (`park_cost.rs`): spin-then-block is 2-competitive when
  the spin lasts as long as the context switch costs [KARLIN]. The spin used to last the measured wake
  latency, about eighty times the cycle's CPU and moving with the load (1.9 µs to 577 µs between
  calibrations a minute apart, note 42 §1–§2). No spin until a first park is measured, none where the OS
  keeps no fine per-thread clock (Windows). The estimator's warm-up is the stopping rule's floor
  (`MIN_SAMPLES`, 59, Wilks) and its window the rule's `N = (z·sd / (h·mean))²` on the warm-up's own
  spread. `tests/spin_by_park_cost.rs`. The CPU clock is read only by a learning shard, around a wait it
  makes, and the second time only after a first (`bd7c0fd`: reading it after every park halved every
  shard's calibrated batch, 117 to 262 ns an item; `tests/park_cost_reads.rs`).
- **The shard's clock is the cheap one, read once a poll (D3).** On macOS `mach_continuous_time` scaled
  by the timebase as a 32-bit fixed-point ratio, 4.8 ns a read where `CLOCK_MONOTONIC` costs 16.1 ns at
  1 µs resolution (note 42 §4); elsewhere `monotonic_ns`, already the cheap clock there. A step of `p`
  polls reads it `1 + p` times (`2 + p` when its drains or driver did work), where it read
  `CLOCK_MONOTONIC` `2 + 2p` times; a poll that yields cost 47.5 ns and costs 15.9 ns (note 42 §7).
  `SCALE_BITS` (32) is format: the 64×64-bit product cannot overflow 128 bits and the rounding error stays
  under 10⁻¹¹ of a tick (`machine::clock`'s tests).
- **A poll's end from the counter.** The loop reads the driver's clock once a step and carries that reading
  to the step's polls by the generic timer's counter, CNTVCT_EL0, read without synchronization
  (`machine::clock::Carried`; aarch64 under macOS and Linux, and elsewhere the clock takes every reading);
  a task's `ShardContext::now_ns` is carried the same way, never earlier than a reading before it. The OS
  clocks read the counter ordered behind the instructions before them (an `isb`, or a register that
  orders itself), so a poll's end read that way waited for the poll's outstanding work. On mantle's fill
  (its newest tree, 2026-10-13, 100M keys, one frozen runtime record for both arms) `mach_continuous_time`
  held 2.5% of the shard thread's samples (113 and 111 of about 4,425 in two runs) and, carried, 0.1% (4
  and 4), with `poll_slot`'s own samples up 0.5%. The fill's rate moved within the noise of a host at load
  22: carried over not, round by round, 0.95 to 1.11, median 1.01, eight rounds of 10M
  (`benchmark-results/hyper-rt-clock-20261013`). The counter's scale is its frequency register,
  CNTFRQ_EL0: on this machine CNTVCT_EL0 counts at 1 GHz while the timebase macOS's clocks count in is
  24 MHz, and a first trial that scaled the counter by the timebase read every poll 41.67 times too long;
  `machine::clock`'s test now holds the counter to its stated scale on each aarch64 host it runs on. Each
  step reads the clock afresh, so a scale off by a fraction would move a reading by that fraction of the
  time since its step's own, never more.
- **Channels hand values over with no lock (D4).** The bounded channel and the one-shot carry their values
  through a lock-free bounded ring (`sync/ring.rs`, Vyukov's queue with std's lap-encoded positions), wake
  a waiting receiver, task or parked thread, through its cell's waiter word (`handoff.rs`), and hold
  senders waiting for room in places of their own (`sync/room.rs`); each take grants one place its room.
  `channel_with`'s `waiters` bounds the senders waiting at once, and `blocking_send` past it is told
  `Full`, where it used to wait inside std's channel, unbounded. std's `sync_channel` registered blocked
  ends behind a mutex every send to a blocked receiver took. With plain threads one producer moves 144
  million values a second against 30 million before, and sixteen contending producers 674 thousand against
  407 thousand at a third of the CPU (note 42 §8). loom under the workspace's bounds: the ring (548
  interleavings), a thread waiter (6,289), one and two senders waiting for room (50 and 71,847), each
  failing under its recorded mutations; `tests/channel_threads.rs` runs real threads through the blocking
  calls. `Padded`'s 128-byte alignment is shape (the largest cache line targeted, Apple silicon's).
  A receive that finds the ring empty reads only lines its channel owns: whether every sender is gone is
  kept on a line of its own in the channel's block, which only the last sender's drop writes, and no
  longer in the cell's state word, in the process-wide table where a cell is 32 bytes, four to an Apple
  silicon line, beside other primitives' words that their own ends write. In a 10M run of mantle's ranges
  bench (the fill, a million gets, a million seeks) its shard took from its workers' channel 19,296,686
  times, and 19,296,557 of those found it empty.
  An empty `try_recv` cost 1.8 ns alone and 2.1–2.7 ns while two threads churned one-shots, and costs 1.4–1.7 ns
  and 1.5 ns. In mantle's 100M fill the call fell from 2.8% to 2.5% of the shard thread's samples, its
  read of the gone state from 41 samples to 14 (`benchmark-results/hyper-rt-recv-20261013`). What is
  left is the call itself, about 2 ns a take, which only fewer takes save.

## 4. Running a future: `block_on`

Every consumer needs to run one future to completion on the calling thread: a CLI command, `main`, a
test. `LocalRuntime::block_on(future) -> Result<T, RtError>` builds one shard on the calling thread, spawns
the future as its root task, runs the loop until the root completes, then cancels and joins whatever the
root left behind (structured: nothing outlives the call) and returns the output. slates has
`run_until_idle` only, which cannot return a value and does not end while a socket is merely registered.

The root is detached: its output, not a join, is how its completion reaches the caller, so its task slot
is free again once it ends, and a `LocalRuntime` runs any number of `block_on` calls with a task capacity
of one root and its children (`tests/block_on_reuse.rs`: on `a63ec4b`, three calls on a capacity of two
were refused `TooManyTasks`, each completed root still holding its slot). While a returning call cancels what its root left behind,
admission is paused, so a destructor cannot spawn work the cancellation would miss; it reopens once the
cancellation is done, while losing the driver or a shutdown keeps it closed. A turn that ends the shard
after the root stored its output but before the call took it drops that output, with the shard entered,
before the call returns its error (`tests/completed_output_loss.rs`).

A root whose future unwinds has no output to give: the shard's boundary ends its task
`Outcome::Unwound` (§3.1). The call names its root to the shard (`ShardContext::begin_root`), the shard
tells it when that task's poll unwound, and the call cancels and joins what the root left, as on a
return, and returns `RtError::RootUnwound`. A root dropped by cancellation, as the shard leaves its loop
when its driver is lost, ended `Cancelled`, not `Unwound`: that call returns `ShardGone`, as before.
Before, the call waited for an output that would never come: a test whose assertion failed inside its
root left its binary waiting on windows-11-arm (diag run 38275004157), and a command whose root panicked
would not have ended (`tests/block_on_reuse.rs`, `a_root_that_unwinds_ends_its_call_refused_and_the_next_runs`,
which waited until it was stopped before this change).

An owner with asynchronous retirement uses `spawn_service` (local), or
`spawn_service_on_with_receipt` (threaded). The service mark is installed before its first poll;
the admission receipt must be accepted before transferring physical ownership to it. Cancellation
is a level read by `cancellation_requested` or awaited with `cancelled()`. It cancels the service's
children once, but does not drop the service future: the service enters closing once, awaits its
external completions, and returns. Its join then reports `Cancelled`. Repeated cancellation cannot
interrupt closing or recycle its generational task handle. Ordinary `spawn` retains hard cancellation
semantics (`tests/cooperative_service.rs`, `service_retention_red.rs`, `service_sim_loss.rs`). A foreign
future that panics cannot resume cleanup; the unwind tests cover retirement of the remaining task
family, rather than recovery of the panicked owner's resources.

Native maintenance threads transfer before service admission. The cold runtime owner calls
`prepare_retirement` with each group's configured handle capacity, then each lease's `adopt`
with its complete handle vector. A typed refusal before acceptance returns that entire vector;
an accepted transfer with a lost receipt remains owned by the runtime. Adoption and blocking
cleanup refuse on a shard. Each service later signals once and awaits the same lease's terminal
reply, retaining it across borrowed cancellation and refusing foreign poll contexts before
consuming an answer. That reply follows every native join, including TLS destruction; a failed
join still retires the remaining handles. No service transfers a handle or joins a thread during
terminal cleanup.

An original device file can transfer before service admission through
`prepare_retirement_with_original`. Its cold adoption keeps the entire file with the
caller on refusal and moves it to the same reaper on acceptance. The service then holds
an `OriginalLease`, while the reaper owns the file and its registered physical fence.
Original retirement joins the group's maintenance workers, waits for the independent
physical terminal event, and closes the file before answering that lease. The worker
receipt remains separate, so a worker can retire before the owning attachment detaches.
An actual physical failure precedes a later join or file-destructor failure. A foreign
fence that returns without retirement keeps the file owned until an explicit terminal
retry; cancellation or shutdown cannot force its close. The actor never owns the file
after cold adoption and awaits the close receipt through channel wakes.

Live groups are bounded by the configured task arena. A preparation batch owns one cold reaper,
one command per lease plus the owner's Stop credit, and one reusable reply per lease. Completed
group counts publish before terminal replies, so immediate cold setup can prune completed owners.
Runtime shutdown first stops and joins its shard workers while reapers remain alive, then stops
and joins those reapers. Unused external leases cannot hold shutdown waiting for channel EOF.
Arbitrary adopted threads must have their input dependencies closed by their caller; the runtime
cannot make a foreign thread or a process-aborting TLS destructor complete
(`retirement_native.rs`, `retirement_shutdown_native.rs`, `retirement_extra_native.rs`).
Original-file groups reserve three command roles per group plus the independent Stop
credit: worker adoption or retirement, original publication, and original retirement.
Their ownership, canceled waits, refusal and first-error behavior are exercised by
`original_retirement_native.rs`.

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
  at most a batch of them per call. A send whose own destination has no route is reported the same way,
  every time it is tried (macOS returns `EHOSTUNREACH` for an IPv6 destination on an IPv4 socket): the
  sender sends again at once after a report, which takes an earlier datagram's, and takes a second report
  for the same datagram as its own and refuses it, counted (`Batched::send`, `UdpSocket::send_to_writable`).
  One unroutable destination held a socket's whole queue before (2026-10-10, found by focal: a connector
  that dialed a name's `::1` from an IPv4 socket stopped sending to every peer).
- **Don't-fragment** set per OS, as slates does; a send the OS has no room for is `WouldBlock`, awaited on
  writability, never a lost path (slates' AUD-29-61).
- **Socket activation** (`adopt`, `into_owned`) kept from slates, on Windows too.
- **What a socket holds, bounded and told.** `Io::held_bound` is the most a batched socket holds while it
  lives, and what its owner charges for it: its receive buffers (`batch` of `RECEIVE_BYTES` on Linux, one
  elsewhere), its outbox (`batch` slots, each one datagram or one segmented send of at most
  `RECEIVE_BYTES`) and on Linux the message headers. A slot never grows past its bound: a datagram larger
  than it, queued or filled in place, is dropped and counted (`IoStats::oversize`), since no UDP payload or
  segmented send is; `Batched::held_bytes` says what is held now (`tests/udp_batched.rs`). Before
  2026-10-10 a slot grew to whatever was put in it and nothing told an owner what the socket held: focal's
  listeners held their sockets uncharged (found by focal's `network_listener`).
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

**A kind that arrives with no subscriber** keeps its default action on Unix: the signal thread restores
`SIG_DFL` and raises it (mantle's review, finding 7). That restore races a new subscription's installation.
The order that keeps a live subscriber's handler is `signal_protocol.rs`:
- a subscription publishes its mask before installing;
- the restore reads the masks after its `SIG_DFL`, and reinstalls (handing the signal over) when one wants
  the kind;
- the kernel serializes `sigaction`, which carries the rest.

A subscription claims its slot with a sentinel bit no kind uses, resets the slot's pending kinds,
and only then publishes its mask, so a restore that hands it a signal is never undone by the reset.

Both orders are loom-checked (the first from both starting states). A restore that never rechecks fails
the first model, and a claim that publishes before resetting fails the second.

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

The pool issuer uses hyper-rt's bounded reusable channel (§8) for its numbered answers and
terminal retirement receipt. Each attachment's declared batch capacity bounds both its submission
lane and answers; the credit includes a batch queued, physically in flight or answered but not yet
consumed. `answer_async` and `retire_async` borrow that receiver: dropping the wait leaves its
number, buffers and credit with the attachment for a later poll. The channel registers the admitted
task's `Copy` waker and publishes before waking, so an issuer completion wakes exactly that owner.
Cold setup allocates these lanes once; no per-transfer channel or boxed waiter is created.
Hyper-block therefore depends on the shared hyper-rt primitive, rather than retaining another
completion implementation. Both crates must be staged and gated together before any consumer
updates its exact snapshot. Owning asynchronous cleanup is a cooperative service (§4); a canceled
borrowed receive does not establish that dropping an attachment itself is nonblocking.

`Attached::prepare_retirement_watch` registers one independent physical receipt during
cold setup. The broker acknowledges its ownership before the original file can transfer
to a native retirement group. This watch survives loss of the submitter and its ordinary
answer receiver. Normal retirement answers after every file duplicate drops; abnormal
broker or worker exit retains the watch through all native joins, including TLS. A
consumed write or flush failure remains its first physical cause, while a repaired
numbered read failure does not poison retirement. Context refusal leaves even an already
ready watch receipt untouched. The watch's blocking wait is confined to its cold native
owner; actors use the original-file lease's asynchronous receipt (§4).

Each actually started issuer worker publishes into its own cold lane: one report for its
sole counted assignment and one native-exit notice. A capacity-one notification coalesces
inspection and unparks the broker after publication. Inspection is bounded by the actual
worker cohort and precedes park. An unexpected exit closes admission and drives real
native joins rather than manufacturing an I/O completion or waiting for an absent report.

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

**The value itself crosses by move**, through a bounded ring of hyper-rt's own (`sync/ring.rs`, §3.9's
D4), in a block the ends share for as long as the cell's handles live. A send pushes, then wakes the
cell's waiter; a receive pops, and when the ring is empty the waiter registers in the cell and checks
again before it waits (no lost wake: DERIVED from the order send-then-wake against register-then-check).
A waiter is a task's word or a plain thread blocked in `blocking_recv`, parked until the publisher that
takes its registration unparks it (`handoff.rs`); senders waiting for room wait in places of their own
(`sync/room.rs`). The values used to cross through `std::sync::mpsc::sync_channel`, whose blocked ends
meet in a mutex that every send to a blocked receiver took; on macOS that lock is a pthread mutex boxed
at its first lock, and a blocked receiver grows a list, allocations a measured window counted
(`tests/service_resources.rs`).

**A receiver's drop closes the ring** before it drops what was sent: it sets a mark in the ring's tail,
std's disconnect bit (`sync/mpmc/array.rs`), and a sender claims a place only by a compare-and-swap from
a tail without it. So a send either claimed its place before the close, in the tail's one order of
changes, or is refused `Closed` with its value; a sender whose push landed fences and looks at the mark,
and drops what is left if it is set, so nothing waits in a closed ring for the last handle. The receiver
used to set a state bit that senders read before they pushed: its drop could free a full ring's room
between a sender's read and its push, which then landed after the close (`tests/sync.rs`, a task waiting
for room: 2 of 3 runs with 200 µs between the read and the push; 0 of 6 now). loom
(`sync::ring::loom_tests`): a push against the close of a full ring, 32 interleavings under the
workspace's bounds and 72 unbounded, fails at its fifth with the mark taken out of the close; a push
against a close, 48 and 2,208.

**A shard's control queue** (§3.4) is the same ring with no cell, held in the shard's registry entry
(`control.rs`): any thread pushes, the shard alone takes, and the shard learns of a message from the
entry's control flag and kick, never from a waiter. A channel's sender held in the entry would wake
through the registry as it dropped, inside the free of the entry by its last reader (`retire.rs`), a call
cycle the recursion gate refuses; std's channel, which it was, allocated as a shard losing its driver
closed it. Closing drops what is queued and refuses what follows, as a receiver's drop does: the shard
closes the ring and drains it; a sender whose push landed fences and looks at the mark, and drains for
itself if it is set. With both fences sequentially consistent at least one of the two sees the other's
write, so nothing is left in a closed queue (loom, `control::loom_tests`: 46 interleavings under the
workspace's bounds, 4,779 with preemptions unbounded; with the sender's look removed, or either fence, it
fails at its first).

| Primitive | Built as | Bound |
|---|---|---|
| `channel::<T>(n)` (many senders, one receiver) | a ring of `n` places + a cell; senders waiting for room in places of their own | `n` messages; a full channel is `Full` to `try_send`, and `send().await` waits for room, its waiters queued one to one (NOTE26 §5.3); `channel_with`'s `waiters` bounds the senders waiting at once, and `blocking_send` past it is `Full` |
| `oneshot::<T>()` | a ring of one place + a cell; the receiver marks the cell as it goes, which the sender reads (`is_closed`: an owner sweeps the replies of callers that stopped waiting) | one value |
| `watch` of a `Copy` value that fits a word (`u64`) | the value and its version packed in the cell's atomic words; the `n` receiver slots claimed when it is made, chained through their cells, each taken by one compare-and-swap and given back at drop, the sender waking the taken ones | one value; readers see the latest and its version; `n` receivers, refused exactly at `n` (mantle's final review, finding 6) |
| `watch` of a larger value | the publisher keeps it; readers are notified through the cell and ask the owner for it (a request through a `channel`) | the owner's |
| `Semaphore(n)` | an atomic count + a bounded FIFO of waiting words; a release wakes exactly the waiters it admits, in arrival order | `n` permits; waiters bounded by the configured queue, past it `Capacity` |
| `Notify` | the cell alone: `notify_one` wakes the registered waiter; a notification with no waiter is kept as one pending permit | one |

A waiter registers its task word with a swap, not a store: registration and the publisher's take are
both read-modify-writes of one word, so a waiter that registers and then checks never sleeps through a
publisher that changes the state and then wakes (`handoff.rs`, loom-checked; a plain store fails the model,
the store-buffer pattern).

No broadcast primitive is offered: a broadcast to N waiters is the O(N) hazard of NOTE26 §5.1. An event that
concerns every waiter (a shutdown) completes each waiter's cell once (NOTE26 §5.3 rule 2).

**Measured** (mantle's note 42 §8, `docs/benchmarks.md` "hyper-rt: the loop under load"): with plain
threads the ring moves 144 million values a second from one producer at capacity 1,024, where std's
`sync_channel` moved 102 million and tokio's `mpsc` 8 million. Under the shard's own loop against tokio's
`mpsc` on hyper-rt (`hyper-rt-compare`'s `hyper-mpsc` arm, `docs/benchmarks.md` "hyper-rt: the loop
under load, against slates' runtime and tokio"): a request and reply between two shards 1.14 M round
trips a second against 0.91 M, on one shard 10.1 M against 8.8 M, eight client threads onto one shard
490 k requests a second against 455 k.

## 9. Blocking work

Work that blocks a thread (a synchronous library call, `getaddrinfo`, a code sandbox, a file read on a
platform without asynchronous file I/O) runs on **one bounded pool per process**: `workers` threads,
started once and reused, each with its own job slot woken one to one (NOTE26 §2.5, §5.3). Its size is
the consumer's, derived from its own measurement (for mantle, the device's queue and measured knee, NOTE26
§2.5; for focal, the code sandbox's concurrency budget). The consumer divides it into named shares so one
kind of work cannot take all of it:
- a share runs at most `limit` of its jobs at once;
- the jobs past those wait for one of its places, in the order given, up to `waiting` of them;
- past that a job is refused `Capacity`, decided before any thread is asked to start.

While its line has room, a job is never refused because its share is busy: essential work queues for
its turn, as a busy machine's must (2026-10-10). The first pool refused a share's job the moment the
share held `limit` jobs, queued or running. Under focal's concurrent node starts, a third whole-file job
was refused and the node's bootstrap failed. `waiting: 0` keeps that behaviour where a consumer wants it;
`Share::new(name, limit)` makes such a share, the constructor a share written before `waiting` becomes.

A job's result returns through a `oneshot`. A blocking job cannot be cancelled (the call is the OS's); its
share's bound is what limits the damage, stated.

**No locks, as built** (2026-10-06, the shares' lines 2026-10-10): the workspace's wall forbids `Mutex`
and `Condvar`, so a dispatcher thread owns the shares' lines, their running counts and the idle workers'
reports. Each worker owns a one-job slot, and reports itself idle, with the share whose job it finished,
over the dispatcher's channel, so each job goes to exactly one idle worker. The shares take idle workers
in turn, and each share's jobs run in the order given. A share's admitted jobs (running or waiting) are
an atomic count checked at submission, so the dispatcher's channel holds at most the jobs admitted, one
idle report per worker and one stop. It is bounded by admission, not preallocated: the pool's memory is
that of the jobs it holds. A job that panics ends the job, not its worker (an unwind boundary), and gives its
share back; a result no one takes is dropped inside the boundary too, its drop being the job's code. A job
gives its share back before its result is delivered or its sender dropped, so a task
that sees the job end can submit again under the same share and is never refused for a slot the pool is
still about to free (2026-10-08: the reverse order failed `tests/blocking.rs` on a slow macOS x86_64
runner). One per process: a second `start` is refused; `stop` refuses new jobs, runs the jobs admitted,
and joins. Name resolution (§5.4) is `dns::resolve`, on the share named `dns`. `tests/blocking.rs` passes
on macOS and Linux 6.12; `tests/blocking_waits.rs` on macOS (2026-10-10).

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
| step quantum | the mean wake: the boot probe's median of its rounds' means, then each tracking shard's own median of its last five blocks' means, the probe's estimate their seed (§10.4) |
| spin before parking | the CPU one park/wake cycle costs, measured on each tracking shard's own parks (§3.9, D2) [KARLIN]; slates' rule, the mean wake, is the fixed `spin_ns` of a configuration that does not track |
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

**A configuration's counts must fit the fields that carry them.** Before any driver, queue or arena is
taken, a runtime, a local runtime, a shard's seed and a simulation check that the last task slot fits a
task word's slot field (`Encoded::MAX_SLOT`) and that the timer, readiness-wait and driver-ring counts
fit their `u32` indices; past either, the build is refused `BadConfig`. Zero-sized arenas keep their
refusals as before. A ring of `usize::MAX` entries used to reach the allocator and unwind on capacity
overflow (`tests/extreme_ring_refusal.rs`), and an empty simulation built nothing and so checked nothing
(`tests/sim_shape_refusal.rs`). The check is the representation's bound only: a count that fits and is
still more than the machine can allocate is the allocator's refusal, as before.

### 10.4 One probe samples a host that moves

The boot probe times wakes for a few tens of milliseconds, and a wake is mostly the host's run queue and
its cores' idle states, both of which move. On 2026-10-13 one of mantle's benchmark calibrations read a
mean of 227,774 ns where the same host's other calibrations that day read 1,444–5,904 ns
(`benchmark-results/mantle-contig-20261013`, `n10000000-r111-mcs`; in §3.9, boot calibrations a minute
apart sized slates' old spin from 1.9 µs to 577 µs). Everything §10.3 derives from the mean took that one
sample. The step quantum's prior, the tick and the loop's batch (1,397 polls, against 17–25 in the other
runs) all came from it, and through the p99 so did the control depth (32,768 against 512). The window
the shard's estimate weighted (2¹⁴ wakes) came from it too, so the shard's quantum was still 228 µs when
the run ended.

**What those values cost, measured.** That run's fill was 3.5 times slower than its neighbours', but the
values did not cause it. The same binary was given them as a frozen record, one at a time and all four
together, beside the run's normal values, in two rounds interleaved on the same host. With all four it
filled 10 M keys at 2.29 M and 2.26 M puts/s, against 2.27 M and 2.44 M with the normal values, and
each value alone stayed within the spread of its own two rounds (the widest pair was the batch's, 1.69 M
and 2.47 M; `benchmark-results/hyper-rt-calibration-20261013`, `arms-1`). The 228 µs quantum moved only the fill's latency tail. Mantle's client yields once a quantum,
so its gaps came rarer and longer: p99.9 went from about 27 µs to 1.5–8.5 µs, and p99.99 from about
52 µs to 150–345 µs. The slow run was the host's state, which also produced the probe: its merges took 5.27 s,
against 1.7–2.6 s in the other runs. A burst of CPU-bound processes at boot does not reproduce such a
probe here. Fifty-four of them held through the calibration gave 1.4–2.1 µs (`boot-1`), because
saturated cores do not idle. What a burst's values do cost by construction falls on the consumers that
read the quantum as a latency. A busy shard asks its driver for readiness once a quantum (§3.4), a timer
fires on the tick after its deadline, and a woken task waits behind up to a batch of polls.

**The estimate now outlasts a burst.** The probe reports the mean as the median of its five rounds'
means [MOM], where it used the mean of every wake pooled. A burst confined to fewer than half the rounds
cannot move that median outside the other rounds' means (DERIVED). Under any finite variance its error
is sub-Gaussian: with `k` blocks of `m` wakes, it exceeds `σ·√(4/m)` with probability at most `e^(−k/8)`
[MOM Theorem 2]. Its interval is the bootstrap resampled within each round. Three hundred 40 ms probes on
this host, half a second apart, show the event it removes. In one probe, a round's waiter woke once in
its share, after 7.1 ms. That single wake raised the pooled mean to 9,474 ns, while the median of the
round means read 6,307 ns. Over all 300, the pooled mean's p50, p99 and largest value were 2,983,
5,852 and 9,474 ns, and the median's were 2,919, 5,881 and 6,341 (`spread-1.txt`). The spread that sizes
the online window (`WakeLatency::sd_ns`) is the median of the rounds' coefficients of variation, for
the same reason: one round's held wake widened the pooled spread, and the windows of one host's probes
ranged from 2⁸ to 2²¹ wakes. A probe whose rounds kept fewer wakes than the stopping rule's floor
reports them `quick`, and is refused only when it kept none. The refusal had stopped the runtime from
starting on a loaded host (windows-2025, CI run 38376252976).

**A shard's own wakes replace the boot's.** A tracking shard keeps the median of its last five blocks'
means (`WakeTrack`), with the probe's estimate seeding every block. The first blocks hold twelve wakes,
the stopping rule's floor over five, so the boot's value is outvoted after 36 measured wakes. At the
rate mantle's fill shard parked (123 times in 13.8 s), that is about four seconds of parks. The weighted
mean it replaces had moved under one percent of the way by the run's end. Every five blocks the next ones
are twice as long, until the five hold the window the probe's precision asks for. After that a lasting
change is followed once it fills three blocks, and one wake held a quarter of a second moves one
block's mean and not the estimate. `machine::wake`'s tests hold each of these exactly, and
`tests/wake_estimate.rs` holds a shard seeded with a one-second prior that leaves it for its measured
wakes.

**What is left.** The tick, the batch and the control depth are fixed when a shard is built. A burst
over all five rounds is the host's state at boot, not a burst in it, and it still sets them; only the
quantum recovers. A shard that never parks keeps the boot's estimate, since it has no wake to learn
from.

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
3. Measured 2026-10-13 (`docs/benchmarks.md`, "hyper-rt: the loop under load, against slates' runtime
   and tokio"): the wake bitmap against slates' rings on the cross-shard and foreign-wake rows (§3.2),
   both runtimes carrying values on tokio's `mpsc`: a request and reply between two shards 909 k round
   trips a second on hyper-rt against 159 k on slates' runtime, eight client threads onto one shard 455 k
   requests a second against 305 k, the one-way p99 29.2 µs against 36.9.
4. Measured: std's `sync_channel` against tokio's `mpsc` and a ring of hyper-rt's own (§8), with plain
   threads (mantle's note 42 §8) and under the shard's loop (`hyper-rt-compare`'s `hyper-mpsc` arm).
5. `SBITMAP` and `AFUNIX` read at a named revision.
6. A plain `Release` store of the wake bitmap's pending flag failed loom 0.7.2's parking model: the sender's
   own later load read the flag's earlier value, which coherence forbids. The flag is published with a swap
   (the model passes); the execution is to be reduced to a loom report or to an error of this model's.
7. One-shot against edge-triggered registration (§3.7), and packed wake words against one per cache line
   (§3.2), measured on §12's rows.
8. The spin window from the startup wake probe rarely hit in slates' bench configuration (§3.2), though it
   is not the kick result's cause: measure the startup mean against an online p50/p90 on its own merits
   (spin hits, CPU, p99 under load). Measured beside slates' runtime and tokio (2026-10-13): a client
   pausing 200 µs costs hyper-rt 18.7 µs of CPU a request against their 12.0–13.7, the deterministic
   spin-then-block rule at its worst case, a spin as long as blocking costs paid in full before every park
   [KARLIN]. A rule that learns the idle gaps it sees, or a randomized threshold (Karlin et al.'s
   e/(e−1)), is what to measure against it.
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
