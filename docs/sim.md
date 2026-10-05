# Simulation and checking: `hyper-sim` and `hyper-check`

> Status (2026-10-04): **S-1 built** (`crates/hyper-sim`: the generator and named streams, time and
> node clocks, the world under both disciplines, the trace, the digest and the run-twice check; §12
> records it as built and where it departs from this design), and its lints of §3.9 are in the
> workspace (§12.6). **S-2 built** (§13): the network, with focal's path tests carried onto it,
> hyper-transport's test network and hyper-liveness's simulation on it, and hyper-swim's first
> simulation; its costs against the harnesses it replaced are still to measure. **S-4 built** (§14):
> `crates/hyper-check`'s oracles, liveness, floors and the two checkers of linearizability, judging
> hyper-raft's schedules and mantle's range simulation, the planted mutants caught. **S-5 built**
> (§15): the exhaustive model search with slates' models at slates' class counts (463,715 and the
> rest) and every rejected variant refused; the swarm, PCT, coverage over the TLA+ abstraction with
> its conformance check, the round search of the real core, crash enumeration by fork and
> shrinking, with each strategy's runs to catch each planted defect. S-3 and S-6 to S-8 are
> designed, not built. Sources and
> what each establishes are in `docs/research/sim.md`. The plan's starting point was mantle note 32
> §3.10 and `docs/raft.md` §5; this design keeps their list of pieces and departs from it where §1
> below shows a piece falls short.

The two crates are the test infrastructure every other crate here and every consumer runs its
deterministic simulations and checks on. **`hyper-sim`** is the world under the test's control: time,
randomness, the network, the disks and the processes, with faults, all deterministic from a seed.
**`hyper-check`** judges and drives it: the oracles held after every step, the linearizability and
history checkers, the liveness checks, and the strategies that choose what the world does next.

Neither is extracted from one project. Each part is taken from whichever of the three projects (or
this repository) has the strongest version, measured against the literature, and the gaps that
none of them closes are closed here (§1.5).

## 1. What exists, and what each falls short of

The harnesses below simulate a group, a network or a device. They were read at hyper-raft `634d45c`,
mantle `origin/dev` (`1c179e8`), focal `9549f4f` and slates `ec5e0df`. Costs are the debug
run of each at its default scale on the owner's machine under load (§10, "Recorded costs").

### 1.1 hyper-raft

| Harness | Design | Invariants | Coverage, cost | Gaps |
|---|---|---|---|---|
| `crates/hyper-raft/tests/support` (`mod.rs`, `cluster.rs`, `lagged.rs`; from focal-raft) | Untimed step schedule: every step is one `Op` (deliver at an index with keep or lose, tick, propose, fast, change, transfer, read, restart, compact, block, heal, priority, window, campaign, unreachable, ping, a persistence step), drawn from a `Mix` in parts of a hundred. A `Replica` trait over two cores (`Old` = raft-rs, `New`) and `Lagged` (readies ahead of persistence with explicit `Take`, `Durable`, `Notify` steps). A typed `Disk` per member, held by the cluster while the member is down. Network: a `Vec<Message>` of 2,048, oldest lost. | After every step: one entry per index (`chosen`), one leader per term, every answered read at or above the commit when it was asked; with `Lagged`, the durability oracle I1–I5, I7 and R-6 against each member's disk; `settles()` after the run. | `Coverage` counts (behind, several, refused, lost, held back, answers, fenced, stated, unpersisted), asserted non-zero. 96 seeds × 4,000 steps by default; the fast track's defects needed 40,000 seeds (`docs/raft.md` §3). | Schedules keep their meaning only by hand: `choose` skips draws "so a schedule of members that do not draws as it always did". Crash at every persistence step replays the whole schedule per crash point (quadratic). No time: elections are counted in ticks; whether a group settles is judged by its progress, a quiet period of twice the longest timeout a member draws (was a literal budget, `settles(400)`). |
| `tests/differential.rs` | Both cores in lockstep on one schedule, every output compared field by field. | Equality with raft-rs, with two named divergences. | 96 × 4,000 by default; focal ran 15,000 schedules, 80.8 million steps. | raft-rs is no oracle beyond depth one (`docs/durable.md` §2.1). |
| `tests/pipeline.rs` | `Lagged` members under `LAG` = 35 % persistence steps and a slow leader's disk (`SLOW_LEADER` = 25, measured). | As above. | 24 × 2,000; crash at each of every persistence step of 3 seeds × 400 steps. | As the support. |
| `docs/models/FastTrack.tla` | TLC, 12 configurations (seven pass, five must be refused), each with its exact state count (`StateBudget`). | `OneLeader`, `LogMatching`, `LeaderHolds`, `Agreement`, `Committed`. | 3.2–3.3 million states for the largest that pass; CI only. | Missed both fast-track defects until a step and a change of configuration were added. |
| `crates/hyper-durable/tests/support/cluster.rs`, `sim.rs` | Untimed steps over real `Replica`s on `SimStore` (a typed write queue made durable when the schedule says, refusable, failable, with an answer taken later) and `Kv`; five shapes. Its own SplitMix64. `now` from the host clock once, advanced by steps. | I1–I5, I7, I8 against each member's durable state at every release; I4 equality of applied entries. | 128 seeds × 5 shapes × 5,000 steps; crash after each of every event of 4 seeds × 800 steps, two shapes. | Same quadratic crash enumeration; no network time; host `Instant` as epoch. |
| `crates/hyper-durable/tests/support/device.rs` | Members on hyper-log over `SimFile`, power cut at the `n`-th device operation, every `n` in turn: mantle's directed runs and focal's F17 cases. | A founder elects itself after a cut anywhere; a restart-acted entry never regresses. | Enumerates every device operation of a run (`PATIENT_ROUNDS` = 400, derived from the election tick). | hyper-log's device thread makes the run deterministic only by driving one write at a time. |
| `crates/hyper-block/src/sim.rs` (from mantle-disk) | `SimFile`: a visible image and a durable image, sector granularity, alignment checks; a crash keeps each unflushed sector with probability ½, none, or all; faults: read EIO, a bit flipped in transit or on the medium, write EIO, flush EIO leaving sectors either way with the cache lying (Rebello et al.), ENOSPC, power cut after `n` operations. `RefCell` inside, one owner. | — | Its own seven tests. | No lost or misdirected writes, no zeros-or-junk block (Ganesan et al. Table 1; `docs/durable.md` §12 asks for both); no latency; synchronous only; one handle, so mantle's chunk tests put it behind a thread and an `Arc` (§1.2). |
| `crates/hyper-transport/tests/common` `Net` | Two endpoints on the caller's clock, datagrams arrive at once, time jumps to the next timer; buffers reused. | — | — | No loss, delay, reordering, duplication or third endpoint. |
| `crates/hyper-liveness/tests/sim.rs` | Timed discrete-event world: a heap of `(time, key)`; delays, flushes and timer lateness drawn by the inverse transform from a host's measured quantiles, and its measured freezes replayed (`tests/support/worlds.rs`, written by `scripts/liveness-worlds.py` from hyper-timing-trace's output); a flush queue per node; owners whose timers fire late by a lateness drawn once, when the deadline is set (as `World::wake`); hyper-sim's SplitMix64, a named stream a source (since 2026-10-02: picked uniform delays, stalls, loss and freezes; xorshift with `% span` from one stream; a lateness drawn again at every turn and anchored at the present, which pushed due wakes past the world's bound). | The crate's stated detection bounds, Theorem 7's allowance, and the owner's contract after every step; each test waits on the facts it asserts. | Seeds per test, `HYPER_LIVENESS_SEEDS` for a soak. | Its own event queue and network until S-2. |
| `crates/hyper-swim` | Real processes only (`tests/cluster.rs`, 2,000 runs recorded). | Detection within stated bounds, Theorem 7. | — | **No deterministic simulation at all.** |
| `crates/hyper-log/tests/equivalence.rs` | Recorded-seed equivalence against mantle-log: per seed, the FNV-1a of the transcript and of the canonical image. | Byte-identical results. | — | Segment nonces come from the OS and are canonicalized away. |
| `hyper-raft-e2e`, `hyper-durable-e2e`, `hyper-log-e2e`, the liveness and swim process tests | Real processes on UDP and fsynced files, `SIGKILL` at named points and at random; waits on facts while the group moves, the quiet period from the members' stated law (hyper-raft-e2e and hyper-durable-e2e, L-4). | Every acknowledged write read back through the leader; equal digests of applied history. | — | Reads back acknowledged writes; no linearizability check of the client history. |

### 1.2 mantle

`crates/range/tests/sim.rs` (1,880 lines) and `support/linear.rs`:
- **Design.** One range group of three (odd seeds) or five (even seeds) real `Replica`s, each on hyper-log
  over a `SimFile` with a model engine; a wire of `(arrival step, message)`; steps of 3,000, then
  healed until settled within 20,000. Faults per step in per mille: crashes up to a minority, damage at
  rest (a bit flipped in the last frame, in a fast-track proposal's frame, or in an earlier frame), restarts,
  isolation, heal, a clock stepped by up to ±2 s, a failed write or flush, compaction; two members lost
  for good and replaced by joint changes; three gateways with sessions putting and getting two keys.
- **Invariants.** Every index applied with the same answers; every put answered held exactly once;
  every member the same rows; per-key linearizability (`linear::check`, WGL with Lowe's memo, budget
  10⁷ steps); configurations equal to the live members; marks repaired. `a_seed_runs_the_same_every_time`
  runs seeds 3 and 4 twice and compares every field and the history.
- **Coverage.** Floors on stalls, flushing, held messages and ticks, two down, duplicates, clock steps, each
  kind of damage repaired.
- **Gaps.** `Rng::below` is `next() % n` (biased, and a fourth generator). One member damaged at a time is
  a hand-written envelope (`repairing`), the same idea as TigerBeetle's fault atlas. `HashMap` and `HashSet`
  in the world (iteration order not seeded; today iterated only in the final check). The checker caches whole keys
  (`HashSet<(Vec<u64>, State)>`), the cost slates measured at twice the memory of fingerprints.
- `crates/chunk/tests/common/device.rs`: several handles on one simulated device are a thread owning the
  `SimFile` and an `Arc` per handle, because `SimFile` has one owner.

### 1.3 focal

- **`crates/focal-sim`** (1,777 lines, held to the production lints): `Seeded` (SplitMix64 with an
  unbiased `below` by redraw, tested for uniformity); `network.rs` (explicitly scheduled messages,
  equal-time delivery FIFO by send ordinal, partitions dropping at delivery, bounded in messages and bytes);
  `path.rs` (the P7 path model, 32 tests: one-way delay ± jitter, in order or reordering, Gilbert–Elliott
  loss per directed flow in parts per million, a bottleneck link with a drop-tail queue shared by flows
  (RFC 5166's dumbbell), a path MTU black hole (RFC 8899), a NAT whose mapping expires (RFC 4787,
  RFC 9000 §9.3), `FabricLimits` refusing a scenario that names more than its tables hold); `disk.rs` (file
  data and directory entries durable independently: a file sync does not publish its new name, a
  directory sync does not persist the bytes; failure before the `n`-th operation; bounded bytes);
  `history.rs` (the witness checker, `docs/research/sim.md` §5).
- **`focal-consensus` `sim_election_tests.rs`, `sim_fast_tests.rs`**: real `DurableNode`s over the path
  model at LAN, regional and geographic profiles in virtual time, with the derived tick. Their logs are
  real files in a `tempfile` directory.
- **`focal-node/tests/history_black_box.rs`**: real clients' traces merged with the owner's receipts and
  checked by the witness checker.
- **Gaps.** The disk under the elections is a real one (not simulated, not deterministic in its timing,
  and a contact slates forbids its tests); the witness checker is coupled to `focal_model`.

### 1.4 slates

- **`crates/cluster/tests/explore.rs`**, the safety explorer (24 seeds × 4,000 steps per size in the
  workspace run, 400 × 4,000 in CI's release run; the "200 × 3,000" of the plan is `multilog.rs`'s
  explorer of the MLRaft layer, which runs on the same pattern):
  real `RaftNode`s, a bag of 256 messages (oldest dropped and counted), any delivery order, drops,
  duplicates, crashes restoring from the last `saved()`, partitions, transfers, compaction and corrupted
  snapshots, priorities, membership changes through staging, the fast track with windows and pipelining.
  **Calm and adversarial stretches of 250 steps** alternate, because without them five voters committed
  108 entries over 400 seeds. Checked after **every** step over the whole history: Election Safety, Log
  Matching, Leader Completeness, State Machine Safety, and fast agreement counted from a **ghost of every
  vote cast**. Non-vacuity floors for 26 paths (each reached more than once a seed) and six rarer ones
  (each at least once). `replay_to_the_first_violation` prints a bounded trace (2,000 events).
- **`support/exhaustive.rs`**: breadth-first search of a `Model` (initial state, candidate actions, apply,
  a packed canonical representative under renaming), serial with shortest counterexamples, or parallel
  level by level on scoped threads with nothing shared mutably; 128-bit fingerprints; a memory ceiling
  derived from measured bytes per state (4 GiB for a 7 GB runner). `prefix_model.rs` and `slot_model.rs`
  run on it with `Variant`s — rejected rules the search must refuse.
- **`support/timed.rs`**: a timed discrete-event Raft world on the council's drive, per-pair latency from
  Microsoft's published Azure P50 matrix, jitter, loss, partitions and crash windows; measures commit
  latency and unavailability.
- **`crates/rt/src/sim.rs`**: the runtime's own simulation (shards, kicks, a shared virtual clock) with the
  same path model as focal's (`SimPath`, `SimLink`, `SimLoss`, `SimNat`), run under Miri in CI.
- **`tests/raft.rs`**: the safety oracle as an executable conformance suite of directed cases.
- **`crates/test-seeds`**: property-test seeds compiled into the binary; a new failing seed printed for a
  human to add; no test opens a file.
- **Gaps.** No disk model ("none (RAM)", note 32 §2.6); no linearizability checker in code; two of its
  bugs were simulations reading the host clock or freeing their clock under a reference
  (`docs/research/sim.md` §7).

### 1.5 What the inventory shows

1. **Five generators, three biased.** SplitMix64 four times with three different `below`s, and a
   xorshift. A run means something only with its generator, so moving any harness changes every seed.
2. **Schedules are fragile.** Adding a fault kind shifts every later draw; hyper-raft keeps old seeds
   meaningful by skipping draws by hand. Nothing lets one component's draws change without moving
   the others'.
3. **Two families that do not meet.** Untimed step schedules (hyper-raft, hyper-durable, mantle, the
   slates explorer) check safety over orders; timed worlds (hyper-liveness, focal, slates' timed) check
   what time decides. No harness does both, and the untimed ones state liveness budgets as literals.
4. **Crash enumeration is quadratic.** Every "crash at every event" test reruns the schedule's prefix
   for each crash point.
5. **Storage faults are short of the literature.** Lost and misdirected writes, zeros or junk, latency,
   asynchronous completion, several handles, and file-system namespace semantics are each missing from
   the device the logs run on; focal has the namespace model, TigerBeetle the rest.
6. **Exploration is random walk only**, at fixed seed counts with no stated guarantee; the defects
   found so far needed 400× the default seeds (`docs/raft.md` §3). Nothing uses PCT, swarm
   configurations, coverage guidance or the TLA+ model's action map.
7. **Histories are under-checked at the edges.** The E2E crates read back acknowledged writes but do
   not check linearizability; mantle's WGL holds whole keys; focal's witness checker is domain-bound;
   slates has neither.

## 2. The two crates

| | `hyper-sim` | `hyper-check` |
|---|---|---|
| Is | The world: randomness, time, events, network, storage, processes, forks, the decision trace | The judges and the drivers: oracles, history checkers, liveness, strategies, exhaustive search |
| Depends on | `std` only | `hyper-sim`, `std` |
| Used as | a dev-dependency of every crate's tests and of the consumers' simulations | the same |

Rules, beyond the workspace's:
- **Held to the production lints** (focal's rule for `focal-sim`): no panics, typed errors, checked
  arithmetic, every constant with its derivation. A harness bug must not look like a system bug.
- **No `unsafe`**, so both are Miri-clean by construction; slates runs its simulation tests under Miri.
- **No dependency** outside `std` and this workspace (build, not pull). Note 06 B6 found no outside
  crate that models power loss, and every algorithm below is short enough to own.
- **No OS call inside a run**: no clock read, no entropy, no file, no socket, no thread — with two
  stated exceptions: one host `Instant` taken when a world is made, for crates whose `now` is a
  `std::time::Instant` (§3.2), and the exhaustive search's scoped worker threads (§4.5), which share
  nothing mutably and join before each level ends (slates' design).
- **Single owner, no `Arc`, no lock.** The world owns every node, link, device and process in arenas
  addressed by generational handles; the clock is a field of the world lent by borrow (slates' bug of
  2026-09-30 was a clock held by reference). Forks are `Clone`.

## 3. `hyper-sim`

### 3.1 Randomness

- **One generator**: SplitMix64, the one all four projects already use. `below(n)` is exact: a draw past
  the last whole multiple of `n` is drawn again, as focal's `Seeded::below` does (tested there for
  uniformity), so no harness inherits a bias.
- **Named streams.** A world's seed derives one stream per named source — the schedule, each link's
  fate, each device's crash and fault draws, each node's own randomness (election timeouts), the
  workload — as the SplitMix64 finalizer of the seed and the stream's name. Adding draws to one stream
  leaves every other stream's draws unchanged, which is what hyper-raft's hand-skipped draws approximate
  (§1.5 item 2). A test holds the property: a world with a new fault kind enabled on one link
  replays every other stream's sequence unchanged.
- **The decision trace.** Every choice the world makes — which event runs next, each fate drawn — is
  appended to the run's trace as a small integer. A run is reproduced by its seed and revision (FDB
  §6.2; TigerBeetle's "seed and Git commit") *or* by its trace, which needs no seed. Strategies that
  mutate or shrink (§4.5) work on traces. The trace is bounded by the run's step budget (§7).

### 3.2 Time

- **Virtual time** is `u64` nanoseconds from the world's start. An idle world jumps to its next event
  (FDB §4).
- **Each node's clocks** are views of virtual time: a monotonic clock with an offset and a rate in parts
  per million, never going backward, and a wall clock that may also step forward or back (mantle's ±2 s
  steps; Antithesis's clock skips). The rate's bound is the deployment's stated drift bound, the
  quantity a lease rests on (Gray and Cheriton §5, mantle note 06 A7 [via]).
- **Timers fire late** by a drawn lateness, as hyper-liveness's owners wake late and as hyper-timing
  measures granularity; lateness is a per-node distribution the swarm draws (§3.8).
- **`Instant`.** Crates that take `now` as `std::time::Instant` (hyper-durable, hyper-transport through
  quinn-proto) get `anchor + virtual time`, the anchor read once when the world is made. Only differences
  of `Instant`s may enter a decision for the run to be deterministic, which the run-twice check (§3.9)
  tests. Crates with their own time type (hyper-liveness's nanoseconds) take virtual time directly.

### 3.3 The world, its events and its choice points

The world is one value: arenas of nodes, links, devices and processes; one event queue; the clock;
the streams; the trace; the observation sink hyper-check reads. Events are message arrivals, timer
expiries, device completions, process faults and workload operations.

**Choice points.** Every event that is enabled is a candidate, and a **strategy** (§4.5) chooses one.
What is enabled depends on the discipline the test states:
- **Ordered** (timed): only the events at the earliest time are enabled, ties broken by the strategy
  (FDB's "randomizes event times"; focal's network breaks ties by send ordinal). This is the timed
  family: elections, detectors, liveness, latency.
- **Free** (untimed): every pending event is enabled, whatever its time — any delivery order, any
  interleaving of persistence steps with deliveries. This is the untimed family: safety over orders,
  with the clock advanced by explicit tick events as hyper-raft and slates do today.

Both disciplines run the same world, the same processes and the same oracles, so a harness moves
between them by one setting and the two families of §1.5 item 3 become one. A run may switch from
free to ordered for its liveness phase (§3.8): safety under any order, then convergence in time.

This is P#'s architecture (every source of nondeterminism declared and controlled by the tester,
Deligiannis et al. §1–§2) applied to sans-io crates, where the declaration is already made: a crate
here never reads a clock, a socket or a disk, so every input it gets is an event the world chose.

**No buggify points in production code.** FDB needs them because Flow code makes its own choices;
a sans-io crate makes none, so the unusual-but-legal behaviours FDB buggifies — an operation that fails,
a delay, an odd tuning value — are the world's to give at the boundary: a refused write, a late
completion, a `Limits` drawn from its legal range (§3.8).

### 3.4 Network

One network model with two payloads: **messages** (a crate's typed messages, for the core and the
shell, as hyper-raft and the explorer deliver them) and **datagrams** (bytes, for hyper-transport,
hyper-quic, hyper-datagram, hyper-swim and hyper-liveness). A payload states its size, so byte bounds
and serialization times apply to both.

Per directed link, the path of focal's `path.rs` (itself slates' `SimPath`): one-way delay with jitter,
in order or reordering; Gilbert–Elliott loss in parts per million per flow, its chain stepped per
message or in time (`Loss::bursty_in_time`, whose state after `δ` is the two-state chain's transition
function in fixed point, so a copy sent with a lost original shares its burst;
`docs/research/burst-loss.md`); a bottleneck link with a
drop-tail queue shared by the flows through it; an MTU black hole; a NAT mapping that expires. Added
from the inventory and the literature:
- **Duplication** as a re-delivery of a message still held, not a copy (hyper-raft's `keep`), so it adds
  nothing to the bound.
- **Partitions** of every shape VOPR draws: a random split of uniform size, uniform membership, one
  node isolated, symmetric or asymmetric, with start and heal rates and minimum stabilities; and the
  explicit cuts the directed tests make (`Block`, `Heal`). Partial partitions are asymmetric ones.
- **Adversarial datagrams** for the sealed plane: replay of a past datagram, a forged or truncated one
  from a third address (hyper-datagram's E2E does this on sockets; here it runs in every seed).
- **Capacity.** The links together hold at most `C` messages (§7); past it the oldest is lost and counted,
  as slates and hyper-raft do — a loss the protocol must tolerate (Raft thesis §3.3) — and a test that
  counts more overflow losses than drawn losses has a bound too small for its schedule.

Every probability is in parts per million, so a profile is exact and replayable (focal's rule).

### 3.5 Storage

Two devices, because the crates persist at two levels.

**The write queue** (typed). A device whose writes are values the harness defines (a member's hard
state and entries; a `Write` of hyper-durable's `LogStore`). Submitted writes become durable in
submission order when the world completes them, may be refused or failed, and their answers are taken
later still; a crash loses every write not durable. This is what `Lagged`'s `Take`/`Durable`/`Notify`
and hyper-durable's `SimStore` are, made one device with latency and completion events, so readies in
flight are a property of the device's depth, not of the harness.

**The block device** (bytes), the model of hyper-block's `SimFile` moved here, with what the literature
adds:

| Behaviour | Source | Today |
|---|---|---|
| Writes land in a volatile cache; durable only after a successful flush | Pillai et al. §2; Rebello et al. | yes |
| At a crash each unflushed sector survives independently (torn writes, later writes without earlier) | Pillai et al. §2.2 | yes |
| A failed flush leaves sectors either way and the cache returning new data | Rebello et al. §3.3 | yes |
| Read EIO, bit flipped in transit or on the medium, write EIO, ENOSPC | Ganesan et al. Table 1 | yes |
| **Lost write**: acknowledged, never written | Ganesan et al. Table 1; `docs/durable.md` §12 | new |
| **Misdirected write**: written to another offset of the same zone, leaving the target stale and the other corrupt | Ganesan et al. Table 1; TigerBeetle `storage.zig` | new |
| **A block read as zeros or junk** | Ganesan et al. §3.1 | new (a flip is one bit) |
| **Latency** per read, write and flush: a floor and a distribution the swarm draws | TigerBeetle `storage.zig`; hyper-liveness's flush times | new |
| **Asynchronous completion**: submit now, complete as an event, depth bounded | hyper-block's issuer; the readies of R-4 | new (synchronous) |
| **Several handles** on one device without `Arc`: the device is in the world's arena, a handle is a generational index | mantle `chunk/tests/common/device.rs` | new |

Above the block device, an optional **namespace** model for crates that create, rename and delete files:
focal's `disk.rs` semantics (a file's data and its directory entry are durable independently; a file sync
does not publish its name, a directory sync does not persist its bytes), with ALICE's default persistence
model for appends (an append's size may land before its data, the gap reading as junk; §3.2.2) — the
contract mantle's `CLAUDE.md` §6 states as "the parent directory flushed after a create or rename".

**Crash states.** A crash is a draw from one of three generators, chosen per test:
- **Random**: each unflushed sector survives with probability ½, or none, or all (hyper-block's `Crash`).
- **After every persistence point** (CrashMonkey's B3, §4): one crash state per flush or `fsync` in the
  run, each with nothing unflushed surviving — linear in the run, and where every bug in Mohan et al.'s
  study lay.
- **Within a persistence point** (ALICE §3.3): each prefix of the operations since the last flush, and
  each with one operation omitted — the atomicity and ordering a protocol may wrongly assume.

**The fault envelope.** Faults a correct system must survive are distributed so that recovery stays
possible: one fault per member's structure at a time, and never the same index lost on a quorum — the
rule mantle's simulation keeps by hand (`repairing`, one member at a time, AGL+18 §3.2) and TigerBeetle
keeps as its `ClusterFaultAtlas` ("at least one replica will have a valid copy"). The envelope is a
stated type with the assumption it encodes. **Beyond it**, a separate set of runs places correlated
faults and asserts the outcome the design states instead of recovery: a typed refusal or a fenced
member, never silent loss of an acknowledged write.

### 3.6 Processes

A process is a sans-io state machine behind a test adapter (`Process`): it is started on what its
devices hold, given events (a message, a timer, a completion), and gives outputs (messages, timers,
device operations, observations) through a context that lends it its node's clocks and its own
random stream. Its durable state lives in the world's devices, never in the process, so:
- **crash** drops the process and applies the device's crash rule; **restart** starts a new process on
  what survived (hyper-raft's `Member::Down(Store)`, hyper-durable's `Node::down`);
- **pause** stops delivering a node's events for a drawn time, as a stalled scheduler or disk does
  (hyper-liveness's stalled disk; Antithesis's node pause);
- **kill at a point** crashes a node at a named durability point, the points hyper-durable-e2e names
  (`control::Point`) and `docs/durable.md` §12 enumerates;
- **amnesia** starts a node on a device image older than what it acknowledged — Twins' amnesia
  (Bano et al. §3) for a crash-fault protocol, which Raft forbids (thesis §3.8) and repair (R-5, R-7)
  must detect. Run beyond the envelope (§3.5), it is how a test proves the detection.

### 3.7 Forks

A world is a `Clone` value of owned data, so it can be **forked**: run to a point, keep the fork, and
run branches from it (Antithesis's "multiverse" of timelines branching at events).
- **Crash at every event.** Run the schedule once; at each event that a crash after it would cut into,
  fork, crash the node in the fork and run the fork to its end and its liveness phase. This replaces
  the replay of the prefix for every crash point (§1.5 item 4). The prefix is no longer rerun: the steps
  saved are the sum of the crash points' positions, about half of the quadratic total, and every
  branch's outcome equals the replay's by construction, which S-5's gate checks on the existing seeds.
- **Branching where it matters.** Coverage-guided strategies (§4.5) fork at a state that reached new
  coverage and explore its tails, instead of replaying the prefix for each mutation.
- A process that is not `Clone` cannot be forked; such a harness falls back to replay by trace (§3.1).
  Whether the core's `RawNode` and the shell's `Replica` are `Clone` at an acceptable cost is open
  (§11 item 2).

### 3.8 Swarm configurations and phases

- **Swarm.** Each seed first draws its configuration: the number of members and voters within the
  test's range, which fault kinds are on and their rates, the device latencies, the path profiles, and
  the core's and shell's tunables (`Limits`, windows, depth) from their legal ranges. Groce et al.
  found that omitting features per test finds more (42 % more distinct compiler crashes in a week);
  FDB randomizes "cluster size and configuration, ... fault injection parameters, random tuning
  parameters" per run, and VOPR draws its cluster per seed. A test states the ranges; the run states
  what it drew, so a failure names its configuration. Directed tests fix their configuration.
- **Calm and adversarial stretches** alternate within a run, slates' measured remedy for runs that
  commit nothing; the stretch length is the test's, stated with its measurement.
- **The liveness phase.** After the faults, every run heals to a core — all members, or (VOPR) a random
  core with the rest crashed or cut for good — switches to the ordered discipline and must converge
  within a bound derived from the timing law (§4.2). FDB calls this recoverability; P# approximates an
  infinite execution by "a large user-supplied bound"; here the bound is derived.

### 3.9 Determinism, enforced

A simulation is worth its replay, and three projects' bugs (§1.4, `docs/research/sim.md` §7) were
determinism broken silently. So:
- **Run twice.** Every simulation test's first seed runs twice and compares a digest of every decision
  and every observation (mantle's `a_seed_runs_the_same_every_time`, generalized and made automatic).
- **Lints.** `clippy.toml` gains `std::time::Instant::now`, `std::time::SystemTime::now`,
  `std::thread::spawn` and `std::env::var` as disallowed methods, allowed only where a crate states the
  reason in place (hyper-log's device, hyper-tokio, the E2E crates, the anchor of §3.2). Today nothing
  stops a sans-io crate reading the host clock; slates' bug of 2026-09-25 was exactly that.
- **No unordered iteration.** World state uses ordered maps or fixed-hash tables; a `HashMap` with a
  random state is refused in `hyper-sim` and `hyper-check` (mantle's world has two, only looked up today).
- **Entropy is an input.** Anything a crate draws from the OS (hyper-log's segment nonces, which its
  equivalence test canonicalizes away) is given to it by its owner, and by the world under simulation.

## 4. `hyper-check`

### 4.1 Observations and oracles

A process reports what a test may judge as **observations**: a commit at an index, a leader of a term,
an applied entry and the state machine's digest, a read answered, a message released with what the
sender's device held at release, a member's durable view. Oracles consume observations after every
step, over the whole history and not only at its end (slates' rule: "ever, not only among the current
roles"). An oracle is written against the specification, not the implementation (focal's 06 §1: "keep
the oracle ... simple and independent").

The oracles, each the union of what the projects check today:

| Oracle | Statement | From |
|---|---|---|
| Election Safety | at most one leader per term, ever | all three; Raft thesis Fig. 3.2 |
| Log Matching | two logs with an entry of one term at an index agree through it; terms never decrease | slates' explorer, the TLA+ model |
| Leader Completeness | a leader holds every entry committed before its term, at its index | slates, TLA+ `LeaderHolds` |
| State Machine Safety | one entry committed per index, compared by what it states (not its term, for the fast track) | hyper-raft `chosen`, slates, mantle `by_index` |
| Fast agreement | no two values chosen at one index, counted from a ghost of every vote cast | slates' explorer |
| Durability (I1–I8, R-6) | no output leaves before the sender's durable state supports it; a leader commits only what a majority of each half holds durably; the commit fence | hyper-raft `lagged.rs`, hyper-durable `cluster.rs`, `docs/durable.md` §3 |
| Read safety | a read is answered at or above the commit when it was asked | hyper-raft `asked` |
| Exactly once | every acknowledged write applied once on every member | mantle, focal `DuplicateCommit` |
| Same history | every member that applied an index applied the same entry and reached the same digest | mantle rows, the E2E digests, TigerBeetle's byte-identical replicas |

The durability oracle is generic over a `DurableView` the harness provides (a member's term, vote,
log terms and stated commit as its device holds them), so hyper-raft's `Lagged`, hyper-durable's shell
and mantle's replica are held to the same statement.

### 4.2 Liveness

Every run ends in its liveness phase (§3.8) and must converge: a leader elected and an entry proposed
after healing applied by every member of the configuration. The bound is neither a literal nor a
confidence level someone picks: Raft does not bound the elections a group needs (split votes repeat
with nonzero probability, Ongaro §9.3), so any fixed count of elections is a false-failure rate chosen
by hand, which the owner ruled out (hyper-raft-e2e and hyper-durable-e2e removed theirs). The phase
waits on progress instead, as those harnesses do:
- In the **ordered** discipline, the phase goes on while any member's term, commit, applied index or
  last index moves, and fails once a quiet period passes in which nothing moved: the detector's stated
  detection time, one election round (the longest randomized delay of the election law's span and its
  vote rounds, `docs/timing.md` §2.3) and a replication round, all from the members' own settings. A
  live group elects or starts a new term within that period, so a period without movement is a stuck
  group, not a split vote.
- In the **free** discipline, ticks stand for time and the same rule counts rounds: the quiet period is
  `2 · election_tick` ticks and the rounds to replicate.
Because a group that keeps moving without converging waits on, the world's own budget (§3.8, the run's
step bound) ends such a run, and reports it as unconverged with the trace, never as a pass.

A liveness property that is not convergence is a monitor with hot and cold states (P#, §2.5): a run
that ends its bound in a hot state fails.

### 4.3 Linearizability

Two checkers, because the histories come in two kinds.

**The witness checker** (focal's design, generalized from `focal_model` to a trait): when the system
under test publishes its own commit order — the core's committed indexes, a state machine's sequence —
the checker verifies in one pass that the publication order is a legal sequential history and that each
operation's publication lies within its call and return: contiguous publication, one commit per request,
no success before publication, no read below its prefix or with a state that is not its prefix's. It is
linear in the history. It proves the history linearizable by exhibiting the linearization, and needs the
system's word for the order.

**The search checker** (WGL) for histories with no trusted witness — the E2E clients', and every
simulated history as a second, independent judgement:
- **Algorithm.** Horn and Kroening's Algorithm 1 over Wing and Gong's linked list, with Lowe's memo of
  `⟨linearized set, state⟩` (§3.1) and just-in-time linearization (§4), per partition (Herlihy and Wing's
  Theorem 1; Horn and Kroening Def. 6 — a key of a map, an object).
- **Indeterminate operations** get a return at +∞ and accept any result (Herlihy and Wing's extension;
  Porcupine; Knossos's crashed process, whose client is retired). The model's `step` must accept an
  unknown output in every state, which a property test of each model holds; mantle's register does.
- **Complexity.** NP-complete in general (Gibbons and Korach, as Lowe and Horn report it); with
  just-in-time linearization a register's configurations are at most `(N+1)·2^p·(p+1)` for `N`
  operations and `p` concurrent clients (Lowe §4): linear in the history, exponential in the concurrency.
  Tests therefore bound the clients per key (mantle's three gateways) and prefer many short histories
  (Wing and Gong §4.3).
- **Memory.** The memo holds 128-bit fingerprints of configurations, not the configurations (slates
  measured whole keys at about twice the memory, `docs/research/sim.md` §7); the bitset's part is updated
  in constant time by XOR (Horn and Kroening §5.1). A fingerprint collision can only make the search skip
  a configuration and report a history non-linearizable that is not; so a refusal is confirmed by
  re-running that partition with whole keys before it is reported. A pass is never in doubt.
- **Budget.** A partition is checked within a step budget derived from the memory ceiling (§7); a
  partition that reaches it is `Unknown`, reported apart from a refusal.
- **Counterexample.** The longest linearized prefix, the operation that could not follow it, the model
  state there and the outputs that would have been legal (Lowe §3).
- **Competition.** The tree search and the graph search run together on two scoped threads and the
  first to finish wins (Lowe §6), once measured to help on the projects' histories (§11 item 5).

Where both checkers apply (every simulated history with a commit order), they must agree; a
disagreement is a bug in one of them or in the witness.

**Transactions** (mantle's multi-key metadata) need Elle's dependency-graph checker over list-append
histories (Kingsbury and Alvaro §2.1, §3; linear in the history, §6.1). It is not designed here: no
shared crate commits multi-key transactions. It joins when a consumer's transactional layer is shared.

### 4.4 Non-vacuity

A run that never reached a path proves nothing of it. Each harness declares the paths it claims —
FDB's `TEST(...)` macros, Antithesis's `sometimes` and `reachable`, slates' floors, hyper-raft's
`Coverage`, hyper-durable's `Reached` — as named counters, and the test asserts a floor for each:
more than one a seed for common paths, at least one a campaign for rare ones, each floor stated with the
measured count it was set from (slates' practice). A floor that falls is a failed test, not a warning.

### 4.5 Strategies

A strategy chooses at each choice point (§3.3). All of them record the trace, so any run found by any
strategy replays exactly.

| Strategy | What it guarantees, and its evidence | Use |
|---|---|---|
| **Random walk** under a swarm configuration | Nothing per run; diversity across configurations (Groce et al.; FDB §4) | The default safety campaign |
| **PCT** | A bug of depth `d` found with probability ≥ `1/(n·k^(d−1))` per run over `n` processes and `k` steps (Burckhardt et al. Theorem 9); for partially ordered events of width `w` and size `n`, ≥ `1/(w²·n^(d−1))` (Ozkan et al., abstract). Of eleven MigratingTable bugs, P#'s random scheduler triggered seven in 100,000 runs and its priority-based scheduler was needed for the other four (Table 2). | Directed campaigns with a stated confidence (below) |
| **Coverage-guided** | No guarantee; measured: model-state coverage found bugs in etcd-raft and RedisRaft that random and scheduler-coverage guidance did not (Gulcan et al.); happens-before summaries 54 % more distinct states than Jepsen (Meng et al.) | Long campaigns in CI's release lane |
| **Exhaustive, model level** | Every reachable class of a Rust model within its scope, with symmetry reduction, fingerprints and a derived memory ceiling (slates' `exhaustive.rs`) | The fast track's models beside TLC |
| **Exhaustive, implementation level** | Every schedule of the real core at a tiny scope, reduced by state symmetry and event independence (FlyMC §4.1); scenarios of partitions × leaders × rounds (Twins §4.2) | Elections and changes at three members, a few rounds |
| **Replay and shrink** | A failing trace shortened while it still fails | Every failure |

**PCT's runs, and what they guarantee.** A campaign of `R` runs finds a bug of depth `d` with
probability at least `1 − δ`, `δ = (1 − 1/(n · k^(d−1)))^R ≈ exp(−R / (n · k^(d−1)))` (Burckhardt et al.,
ASPLOS'10, Theorem 9). `δ` is not picked: `R` is what the campaign's measured budget runs (its time
bound in CI, divided by the measured cost of a run), and the confidence it reaches is reported beside
the result, per depth, so a campaign says what it covered rather than claiming a confidence it chose.
The bugs that matter are shallow: 92 % of TaxDC's are triggered by one untimely event and over 90 %
involve one to three messages and nodes (Findings #1, #3); 98 % of Yuan et al.'s failures manifest on
three nodes or fewer. At `n = 3` members and `d = 1` the confidence rises as `1 − exp(−R/3)` — 14 runs
already pass 0.99 — while at `d = 2` it falls with `k`, which is why `k` counts only the choice points
where events of different processes race, not every step. The priorities are over processes (members)
and the change points over those racing choice points — PCT's threads become members, as PCTCP's chains
are the processes' event chains.

**Coverage, defined.** The abstract state is the TLA+ model's: `docs/models/README.md` already maps
every model action to the core's functions, which is the `map` Gulcan et al.'s Algorithm 1 needs. An
abstraction function computes the model's variables (logs, terms, commits, held proposals, the
configuration) from the members' observations after each step, and its fingerprint is the coverage
point; a run that reaches a new one gets energy, and its trace is mutated by the feasible mutations of
Gulcan et al. §3.2 (swap which link is delivered from, swap which member crashes, swap how many
messages are delivered) — decisions in the trace name links, not messages, so a mutated trace stays
feasible. The abstraction is also checked on every step: the observed transition must be a model step,
which turns every simulated run into a conformance check of the implementation against the model
(SandTable's and Mocket's idea, run on the implementation's own schedules).

**Exhaustive search** is slates' `exhaustive.rs` moved here with its `Model` trait, its serial
shortest-counterexample search and its parallel level-synchronous search; the fingerprint collision
bound (`n²/2¹²⁹`) and the memory ceiling come with it. `FastTrack.tla`'s fast track is encoded as a
`Model` and searched at slates' scopes, including three indexes and four terms (note 32 §3.8), so the
fast track's decision has TLC and the Rust search as two checks (owner's decision 7).

**Shrinking** removes decisions from a failing trace and keeps a removal when the run still fails the
same oracle, until no single removal does; focal's 06 §1 asks for "the smallest reproduced
counterexample" in every failing history, and slates' serial search already gives a shortest one for
models.

### 4.6 Testing the tests

Each oracle and each strategy has planted defects it must catch: the TLA+ configurations that must be
refused (`FastTrackAnyRound`), slates' `Variant`s, and for the implementation,
mutants behind a test-only setting — committing by counting an older term's replicas, a ReadIndex
without the leader's first entry, a vote not made durable before it is sent, the fast track without each
of its two rules (seeds 9843 and 54104). Twins validates its generator the same way (Bano et al. §6.1).
A strategy's measured worth is how many runs it needs to catch each planted defect (§11 item 1).

### 4.7 Differential and equivalence

- **Lockstep comparison** of two implementations on one schedule, field by field after every step:
  hyper-raft's differential against raft-rs, made generic over the two `Process`es and their adapter.
- **N = 1 equivalence** (slates' rule R8): one member and a simulated group give the same observable
  outcomes for the same workload.
- **Recorded equivalence** when code moves (note 32 §5.2): a run's digest of decisions and
  observations recorded per seed, as hyper-log's equivalence test records transcripts and images.

## 5. How the crates use them

| Crate | Discipline | Devices | Network | Checks | Strategies |
|---|---|---|---|---|---|
| `hyper-raft` | free; ordered for elections | write queue (typed `Disk`) | messages | Raft safety, durability, reads, raft-rs lockstep | random, PCT, coverage (TLA+ map), exhaustive at tiny scope |
| `hyper-durable` | free and ordered | write queue (`LogStore`) and block device under hyper-log | messages | durability I1–I8, exactly once, same history, F17 cases | random, crash at every event by fork, B3 crash states |
| `hyper-log` | ordered | block device (and namespace) | — | recovered = acknowledged; equivalence images | B3 and ALICE crash states, random power cuts |
| `hyper-block` | ordered | block device (its `sim.rs` becomes an adapter) | — | its own tests | — |
| `hyper-transport`, `hyper-quic` | ordered | — | datagrams with paths | delivery and budget invariants of the transport | random; profiles from focal's grids |
| `hyper-datagram` | ordered | — | datagrams, replay and forgery | no forged or replayed datagram accepted | random |
| `hyper-swim` | ordered | — | datagrams | detection within the stated bound; Theorem 7's allowance on live members | random over swarm profiles |
| `hyper-liveness` | ordered | flush latency on a block device | datagrams | the same, and the flush proof | random |
| `hyper-timing` | — | — | delay models shared with its trace replay | Theorem 7 on synthetic traces | — |

hyper-swim gets its first deterministic simulation (§1.5's inventory found none). hyper-log needs a
way to run deterministically under the world: today it owns a device thread (`CLAUDE.md` §1's one
exception). Under simulation the world is its device: its issuer submits to a block device whose
completions are world events, and its owner is driven as a step function (§11 item 6).

## 6. Real processes and TLA+

- **Real processes stay**, beside the simulation and never instead (`CLAUDE.md` §1a): they test what
  simulation cannot — the operating system's real contract, real timing and performance, the platform
  flush (FDB §4: "bugs have resulted from the true operating system contract being weaker than it was
  believed to be"). They gain hyper-check's history checking: E2E clients record call and return on one
  host's monotonic clock in one process, calls stamped before any return that observes them (Lowe
  §7.1), and the history goes to the search checker. Reading back acknowledged writes, which the E2E
  crates do today, checks durability; it does not check that reads were linearizable.
- **TLA+** stays in this repository's CI only, with its stated state budgets (owner's decision 7). It
  relates to the simulation three ways: its action map defines the coverage of §4.5; every simulated step
  is checked to be a model step; and the Rust model of the same algorithm is searched beside TLC. A
  defect TLC misses for want of a step (`docs/raft.md` §3) is then also within reach of the random and
  guided runs of the real core.

## 7. Bounds

Every resource the tools use has a bound, derived, with a typed refusal or a counted loss at it.

| Resource | Bound | Derivation |
|---|---|---|
| Messages in flight `C` | the smallest capacity at which overflow losses are fewer than drawn losses over the test's seeds | measured per harness: below it the bound, not the protocol, decides what is lost; hyper-raft's 2,048, hyper-durable's 4,096 and slates' 256 are restated by this rule at S-6 |
| Event queue | `C` + timers (nodes × timer kinds) + completions (devices × depth) | each term is a bound of its own |
| Trace | steps × decisions per step, 4 bytes each | the step budget; 4,000 steps of a few decisions each are tens of kilobytes |
| Rendered diagnosis | the last `T` steps' events | slates' 2,000; stated per test |
| Forks alive | 1 + depth of nesting; crash enumeration needs one | world size × forks, world size measured (§11 item 2) |
| Block device | the test's workload bytes × 2 (visible and durable images) | hyper-block's `MAX_SIM_LEN` (1 GiB) stays the ceiling |
| WGL memo | `M / b` configurations, `b` the bytes a fingerprinted configuration costs at peak | `M` the memory ceiling; `b` by slates' formula, `(16 + 1) × 8/7 × 3` |
| Exhaustive search | slates' derived ceiling: 4 GiB on a 7 GB runner, bytes per state from measurement | `exhaustive.rs` |
| Coverage set | the same fingerprint bytes per point, under the same ceiling | as WGL |
| Witness checker | a declared event budget | focal's `max_events` |
| Seeds and steps | a campaign's runs `R` from §4.5 for PCT; for random walk, the CPU budget of its lane divided by the measured cost of a run, with every floor of §4.4 met | stated per test with its measurement |
| Liveness phase | §4.2's `k` elections from `Pr(split)` | `docs/timing.md` §2.3 |

The memory ceiling `M` is the CI runner's: slates derived 4 GiB from GitHub's smallest runner (7 GB).

## 8. What each harness must change to adopt them

**hyper-raft (S-6).**
- `tests/support`: `Seeded` and the network give way to the world; `Disk`/`Store` become the write
  queue's values; `Lagged`'s steps become device completions; `Mix` becomes a swarm configuration.
- The oracle in `Cluster::report` and `check_durable` moves to hyper-check's oracles.
- The crash-at-every-step tests fork.
- Seeds change meaning once, at the switch to named streams.
- The gate is the outcome, not the seed: every invariant and every coverage floor at the same seed
  counts, the differential unchanged, and the defects of seeds 9843 and 54104 still caught (as planted
  mutants, §4.6).

**hyper-durable (S-6).** `SimStore` becomes the write-queue device; `support/device.rs` runs hyper-log
on the block device without the one-write-at-a-time restriction; the host `Instant` epoch becomes the
world's anchor.

**mantle** (its own gated commits, under its rules):
- `crates/range/tests/sim.rs`: `World` becomes a hyper-sim world. The wire and its arrival steps
  become links with delays, `blocked` becomes partitions, `skew` the node clocks, `inject` a swarm
  configuration, and `damage` corruption at rest within the fault envelope. Lost and misdirected
  writes join.
- `support/linear.rs` becomes hyper-check's search checker: the same algorithm, with just-in-time
  linearization and fingerprints.
- `a_seed_runs_the_same_every_time` becomes the automatic run-twice check.
- `chunk/tests/common/device.rs` loses its thread and `Arc`: handles on one block device.
- Recorded seeds change meaning (§3.1). The gate is the suite's outcome over the same seed counts
  (`docs/durable.md` §12's rule: the outcome, not the sequence).

**focal** (needs the owner's permission rule for changes in `~/Projects/focal`):
- `focal-sim`'s `network.rs`, `path.rs` and `disk.rs` are superseded by hyper-sim's network, path
  model and namespace model; its 32 path tests move with the model, as its gate.
- `history.rs` becomes an instance of hyper-check's witness checker over `focal_model`'s types. Note 32
  kept it in focal because of that coupling; the trait removes the coupling, and focal decides whether
  to take it.
- `sim_election_tests.rs` and `sim_fast_tests.rs` run on simulated devices instead of `tempfile`
  logs, which makes their timing deterministic.

**slates** (X-1 and its own commits):
- `explore.rs` retargets at hyper-raft with hyper-check's oracles, its ghost acceptor state, its calm
  stretches and its floors.
- `support/exhaustive.rs`, `prefix_model.rs` and `slot_model.rs` move into hyper-check with their
  state counts as gates.
- `support/timed.rs` runs on the ordered discipline, with the Azure matrix as a path profile.
- `rt/src/sim.rs` stays: it simulates slates' runtime. Its path model (`SimPath`, `SimLink`,
  `SimLoss`, `SimNat`) is the same as hyper-sim's and can take it.
- slates' rules are met by construction: no file contact (A-50), Miri-clean (no `unsafe`), no TLA+
  in its CI (the Rust search is the check it runs).
- The linearizability and Elle checks its `CLAUDE.md` names as nightly come from hyper-check.

## 9. Steps and gates

Each step is one gated commit (`bash scripts/gates.sh`), revertible alone.

| Step | What | Gate |
|---|---|---|
| **S-1** | `hyper-sim` core: the generator and named streams, virtual time and node clocks, the world, events, the two disciplines, choice points, the trace and digest, the run-twice check; the lints of §3.9 | Unit and property tests; the stream-independence test; run-twice on every existing simulation once ported; Miri on the crate in CI |
| **S-2** | Network: messages and datagrams, focal's path model, partitions of VOPR's shapes, duplication, adversarial datagrams, capacity | focal's 32 path tests ported and passing; hyper-transport's `Net` replaced with no change in its tests' outcomes; hyper-liveness's simulation ported, every assertion holding; hyper-swim's first simulation |
| **S-3** | Storage: the write queue; the block device with lost and misdirected writes, zeros or junk, latency, completions, handles; the namespace model; the three crash-state generators; the fault envelope | hyper-block's `sim.rs` tests through the adapter; hyper-log's equivalence hashes identical per seed (the bytes on the device unchanged); mantle's chunk device without `Arc`; every new fault detected by hyper-log's checksums in a directed test |
| **S-4** | hyper-check oracles, non-vacuity, liveness bounds; the witness and search checkers | mantle's `linear.rs` cases and focal's `history.rs` cases pass on the generic checkers; the two checkers agree on every history of hyper-raft's and mantle's seeds; planted mutants caught |
| **S-5** | Strategies: random under swarm, PCT, coverage over the TLA+ abstraction with the conformance check, exhaustive (slates' search and models), implementation-level exhaustive at tiny scope, fork-based crash enumeration, shrinking | slates' models reach their recorded class counts (463,715 for the measured scope) and refuse every `Variant`; fork enumeration gives each crash point the outcome the replay gives; the planted fast-track mutants caught, and the runs each strategy needed recorded |
| **S-6** | hyper-raft's and hyper-durable's harnesses onto both crates | §8's gate for each; costs against the harness replaced (§10) |
| **S-7** | The E2E crates record histories for the search checker | Every E2E scenario's history linearizable; a planted stale read in a test member refused |
| **S-8** | Consumers: mantle's range simulation and chunk tests, slates' explorer and models at X-1, focal's simulations with its owner's permission | Each consumer's suite under its own rules, gated by outcome over the same seed counts (§8) |

## 10. Measured against what it replaces

The law of `CLAUDE.md` §1a applies to test infrastructure too: a harness moves onto `hyper-sim` only
where a step costs no more time and allocates no more than the harness it replaces, measured on the
same schedules: hyper-raft's support, hyper-durable's cluster, hyper-liveness's world, mantle's range
world, focal-sim's fabric, slates' explorer and exhaustive search. Results go in `docs/benchmarks.md`
with the hardware, the date, the load and the command.

**Recorded costs** of the harnesses as they are: the wall time of each test binary in the debug gate
run of 2026-10-02 (Apple M5 Max, 18 cores, load average 21–22), at its default scale, with the
binary's tests on the harness's threads. They are the baseline S-6 compares against; per-step time and
allocations in release are measured at S-6 with the same schedules. The table and its command are in
`docs/benchmarks.md`, "Simulation harnesses as they are".

| Binary | Scale | Wall |
|---|---|---|
| `hyper-raft` `tests/differential.rs` | 7 tests, most of 96 seeds × 4,000 steps | 25.5 s |
| `hyper-raft` `tests/fast.rs` | 96 × 4,000, and directed cases | 5.4 s |
| `hyper-raft` `tests/group.rs` | 96 × 4,000 and directed cases | 4.4 s |
| `hyper-raft` `tests/pipeline.rs` | 24 × 2,000 at four settings; crash at every persistence step of 3 × 400, two settings | 1.2 s |
| `hyper-durable` `tests/sim.rs` | 128 × 5,000 at five shapes; crash after every event of 4 × 800, two shapes | 2.0 s |
| `hyper-liveness` `tests/sim.rs` | seven timed scenarios | 0.9 s |
| `hyper-log` `tests/equivalence.rs` | recorded seeds against mantle-log's hashes | 1.6 s |
| `hyper-log-e2e` `tests/kill.rs` | 24 SIGKILLs of a real writer | 9.0 s |

## 11. Open, to be measured

1. **Which strategies earn their place** (measured at S-5, §15.8). For each planted defect of §4.6
   and slates' two rejected slot rules: the runs random walk, swarm, PCT and coverage guidance each
   need to find it. A strategy that never beats random walk on any is dropped.
2. **Forks** (answered at S-5, §15.6: the core derives `Clone`, at no cost to production). The size
   of a world with the core's and the shell's members, the cost of a clone against the steps it
   saves, and whether `RawNode` and `Replica` can be `Clone` without cost to production.
3. **The abstraction's cost** (measured at S-5, §15.10) per step for coverage and conformance,
   against the step itself.
4. **Swarm ranges and stretch lengths.** Distinct abstract states per CPU-second under each setting,
   the measure FDB's "carefully tuned" rates leave unstated.
5. **Competition parallel** in the search checker, on mantle's and the E2E histories.
6. **hyper-log under the world.** Whether its owner can be driven as a step function with its device
   as world completions without a second code path in production, or whether its simulation keeps a
   thread and a deterministic hand-off.
7. **PCT for message passing** (per-member priorities built at S-5, §15.3; chains still unread).
   Reading Ozkan et al.'s construction (only the abstract was reachable) and choosing between
   per-member priorities and chain partitioning by measurement.
8. **The time type.** Whether the sans-io crates should take time as nanoseconds rather than
   `std::time::Instant`, which removes the anchor of §3.2; it touches every crate's API and is the
   timing work's to decide.

## 12. S-1 as built (2026-10-02)

`crates/hyper-sim`, `std` only, held to the production lints, no `unsafe`; 23 tests (property tests
among them) and the step benchmark (`benches/step.rs`). What §9 assigns S-1, and what was built:

### 12.1 Randomness (§3.1)

- `Seeded` is focal's `focal-sim` `Seeded` line for line: SplitMix64 (Steele, Lea and Flood, OOPSLA
  2014) and `below` by redraw with `REDRAWS` = 64. A test holds it equal to a verbatim copy of
  focal's draw for draw, over bounds 0 to `u64::MAX`, and at focal's fixture value.
- A stream's first state is the SplitMix64 finalizer folded over the seed, the label's length and
  bytes, the count of parts and each part. The length and the count make the encoding prefix-free.
  A name given twice is refused (`DuplicateStream`), since two sources on one sequence would draw
  each other's values.
- The stream-independence test: a world with a new fault stream named first and drawn between
  every other draw, and one link drawing more often, gives every other stream the same sequence.
- Exactness: at bound 3·2⁶², where a bare remainder gives `[0, 2⁶²)` half the draws, the share
  measured is a third.

### 12.2 The trace and the digest (§3.1, §3.9)

- Every draw with a bound of 2 or more is a decision: one `u32` word when the bound is at most 2³²,
  two above. A bound of 0 or 1 is no decision and takes no word (a bound of 1 still advances its
  stream, as focal's `below` does). A replay reads the words back. A word not below the bound asked
  is `Diverged`, and a trace that runs out is `TraceEnded`.
- The trace's room is the run's stated bound (`Limits::trace_words`), reserved when the world is
  made. Decisions never allocate, and a bound the host cannot hold is refused before the run.
- **Departure.** The digest folds each event the world runs (its time, node and kind) and each
  observation as they happen. It folds the decisions once, from the trace, when the run finishes,
  not as they are made. Folded inline they were a third of a decision's cost: the 3-node timed
  workload went from 40.8 to 25.2 ns a step with the change. The digest still covers every decision,
  and `twice` compares the traces word for word as well.
- `twice(seed, run)` runs a seed twice and its first trace once, and refuses a run whose digest or
  trace differs: `Twice::Seed` names the first word where the traces part, and `Twice::Replay`
  catches a draw made outside the world. Tests hold both refusals, the first with state leaking
  between runs and the second with a draw from the seed directly.

### 12.3 Time and node clocks (§3.2)

- Virtual time is `u64` nanoseconds. A node's monotonic clock is `offset + ⌊v · (10⁶ + rate) / 10⁶⌋`,
  and `virtual_at` gives the first virtual time it reads a deadline, exactly. A property test checks
  each against the formula in 128-bit arithmetic. Both are computed in `u64` by splitting at 10⁶ and
  at the pace: the 128-bit division they first used cost the 3-node timed workload about 3 ns a
  step (25.6 to 22.8 ns).
- The wall clock is the monotonic clock moved to the node's epoch, and steps forward or back. A rate
  at or below −10⁶ ppm is refused.
- `instant(node)` is the anchor plus the node's monotonic reading. The anchor is the run's one host
  clock read, taken when the world is made, and the one allowed `Instant::now` in the crate.

### 12.4 The world, its events and its choice points (§3.3)

- Events are a slab of payloads with their keys `(time, scheduling ordinal, node, slot)`. Under the
  ordered discipline the keys sit in a binary heap; under the free discipline they sit in a dense
  vector, any key taken by `swap_remove`. A switch moves the keys, once.
- **A timer per node**, as a sans-io crate states its timer (`wake()`, `poll_timeout()`). The timers
  sit in an indexed min-heap by `(time, node)`: re-arming moves one entry, a node holds one entry
  for the whole run, and the bound is the node count. Lateness is drawn from the node's own stream
  when the timer is armed. Setting the deadline a timer already has draws nothing, so a harness may
  re-arm after every poll without moving the trace.
- **Ordered**: the events and timers at the earliest time are the candidates. A lone one runs
  without a choice, which is most steps. A tie goes to the strategy, events in scheduling order and
  then timers in node order. **Free**: every pending event and every armed timer is a candidate, and
  the clock moves to `max(now, at)`, never backward.
- **A tie costs its size at each of its steps.** The ordered discipline gathers a tie anew at every
  step: it takes the `k` events at the earliest time from the heap, the strategy picks one, and the
  other `k − 1` go back. A tie of `k` costs `O(k² log n)` to drain. Measured (2026-10-04),
  `tests/net.rs`' order-keeping path ties its arrivals at a few instants. That took 0.26 s natively
  and more than 35 minutes under Miri, past the job's budget; the test now runs a twentieth of its
  messages there.

  Owed with S-3: its write queue's completions tie at each flush's instant. So the tie is kept
  between steps, and an event scheduled at the tie's time joins it, which drains a tie in
  `O(k log n)` and asks the strategy the same questions.
- A `Strategy` is asked only when two or more candidates are enabled, and it draws through `Draw`,
  from the world's `schedule` stream, into the trace. `Random` and `Fifo` are built. `Fifo` is
  earliest due, events first, then send order, with no draw: focal's network order, for directed
  tests. A strategy's out-of-range pick is refused, and nothing is taken.
- `earliest()` and `advance(to)` serve a harness that runs until a time (hyper-liveness's
  `run(until)`). Under the ordered discipline a jump past something due is refused (`Skips`).
- Bounds (§7): events, nodes, streams, steps and trace words are each stated in `Limits` and
  refused at their bound (`Full`, or `Step::Spent` for steps).
- Not in S-1: processes and their adapter (§3.6), the observation sink beyond the digest (§4.1),
  forks (§3.7, though `World` is `Clone` when its payload is). These come with S-2 to S-5.

### 12.5 Determinism, enforced (§3.9)

- **Run twice.** `twice` is the check. `scripts/check-contracts.py` refuses any test file that makes
  a `hyper_sim` world and never calls `twice`, so every simulation ported from S-2 on runs its first
  seed twice and its trace once. **Departure from §9's gate:** no existing simulation is ported at
  S-1 (ports are S-2 and S-6), so the run-twice gate holds over the crate's own simulations: a token
  ring of five nodes with drifting clocks, late timers, losses and per-link delays, 5,000 steps under
  each discipline.
- **No unordered iteration.** `check-contracts.py` also refuses `HashMap`, `HashSet` and
  `RandomState` anywhere in `crates/hyper-sim` and `crates/hyper-check`.
- **Miri.** CI's `miri` job runs `cargo miri test -p hyper-sim` on nightly-2026-09-05, with
  proptest's failure file off so the tests touch no file system. It passed on the owner's machine in
  9 min 20 s, with no undefined behaviour reported.

### 12.6 The lints (§3.9)

`clippy.toml` gains `std::time::Instant::now`, `std::time::SystemTime::now`, `std::thread::spawn`
and `std::env::var` as §3.9 states, and `std::thread::Builder::spawn` and `std::env::var_os`, the
same acts by other names. **Departure:** the two extra methods. They landed after the timing step
L-2, and on that tree they fire at 193 sites.

**27 sites moved to simulated time**, where a host read was not the boundary:
- **Instant epochs:** 24 sites took `Instant::now()` only as an epoch for an API whose `now` is an
  `Instant`. They now take it from `hyper_sim::Anchor`, the one anchor of simulated time (§3.2), so
  the host is read in one stated place. The world's own `instant()` uses the same anchor.
  - hyper-timing's progress waits: 7.
  - hyper-transport's progress, credit and round tests (9), its exchange tests (5) and its
    simulated `Net` (1).
  - hyper-quic's in-process handshake: 1.
  - hyper-tokio's refusals: 1.
- **The shell's owner clock:** 3 sites: hyper-durable's shell, hyperlog and threads tests drove the shell
  with nanoseconds since a host reading. They now drive it with a simulated owner clock, each
  reading a nanosecond after the last, deterministic. No test there waits on elapsed time, and those
  that judge time state it outright.

**166 sites are the boundary**, and are allowed at their function (105 functions), with the reason
stated:
- hyper-log's device, owner and threads (`CLAUDE.md` §1's exception), and hyper-block's issuer
  workers;
- hyper-tokio's driver (the runtime adapter);
- quinn-proto's default `TimeSource` and qlog start time, and rustls's `SSLKEYLOGFILE` (recorded in
  each `VENDORED.md`);
- the E2E crates and the real-process tests (hyper-raft-e2e, hyper-durable-e2e, and the e2e,
  process and cluster tests of hyper-transport, hyper-tokio, hyper-liveness, hyper-swim and
  hyper-datagram);
- the benchmarks timing themselves;
- the soaks of hyper-raft and hyper-durable, which read their seed counts from the environment,
  and hyper-log's equivalence test, which reads an opt-in output directory from it.

### 12.7 Cost against the harnesses it replaces (§10)

`docs/benchmarks.md`, "hyper-sim's world against the harnesses (S-1)". In brief, at load 22.6, in
ns a step:

| Workload | Replaced harness | World |
|---|---|---|
| hyper-raft's support, 16 / 256 / 2,048 messages in flight | 29 / 236 / 2,677 | 24 / 23 / 26 |
| hyper-liveness's sim, 3 / 8 / 64 nodes | 14.8 / 34.4 / 135 | 22.0 / 33.2 / 45.5 |
| hyper-liveness itself, 3 nodes, a heartbeat | 553 | 538 |

The world allocates nothing a step in any workload. At three timed nodes it costs 7 ns a step more
than hyper-liveness's harness: about 3 ns recording the trace and 2.4 ns the exact draws (measured
by taking each out), neither of which that harness has. This is to be closed or justified when
hyper-liveness's simulation moves at S-2.

## 13. S-2 as built (2026-10-04)

`crates/hyper-sim/src/net.rs`, its tests in `crates/hyper-sim/tests/net.rs`. What §3.4 assigns S-2,
and what was built:

- **One model, two payloads.** `Net<P>` carries any payload with a stated size: typed messages and
  datagrams alike, so byte bounds and serialization time apply to both. Per directed pair a `Path`
  (focal's: one-way delay with uniform jitter, in order or reordering, Gilbert–Elliott loss per flow,
  a bottleneck `Link` with drop-tail, step or CoDel marking, an MTU, a `Nat` whose mapping expires),
  with focal's named profiles (`LAN`, `REGIONAL`, `GEOGRAPHIC`).
- **Every draw from its flow's stream.** A flow names its streams (`net.delay`, `net.loss`,
  `net.duplicate`, each with `[from, to]`) at its first draw, so adding a flow, or a flow's draws,
  leaves every other flow's draws unchanged (`a_flows_draws_do_not_depend_on_other_flows`). A
  lossless path, and a path without jitter, draws nothing.
- **Arrivals are the world's events.** A send that departs puts its message in the network's arena
  and gives the world `arrival(ticket)` at the arrival time; the harness hands the ticket back to
  `Net::deliver`, so the world's strategy orders arrivals with every other event, under either
  discipline.
- **Capacity.** The arena holds at most `NetLimits::messages` messages and `NetLimits::bytes` bytes,
  the `C` of §7. Past it the oldest message in flight (the least send number) is lost and counted
  in `dropped_capacity`, and the arrival that would have carried it finds its ticket empty. A
  message larger than the network holds in all is refused and loses nothing else. Evicting scans
  the arena, which is `C` slots: eviction happens only at the bound.
- **Duplication** is a delivery that keeps its message: it stays in its slot and arrives again after
  a fresh propagation delay (hyper-raft's `keep`), so a duplicate holds no more room.
- **Partitions** cut at the send and at the arrival. The test's own cuts (`partition`, `heal`) and
  a drawn partition are held apart, so healing one leaves the other. Drawn partitions take
  TigerBeetle's packet simulator's shapes (`Split::UniformSize`, `UniformPartition`,
  `IsolateSingle`; symmetric or asymmetric), and a `churn` the harness calls starts or heals one with
  its stated chance once the present state has lasted its stability.
- **The plane's adversary** (`Adversary`) keeps the latest datagrams the network carried, within a
  stated count and size, and sends a replay from the datagram's sender or a truncated or one-byte
  forged copy from a third address, so every seed of a run on the sealed plane tries them.

**Exact tests, not statistical ones.** focal's tests of loss and jitter asserted rates inside
bands (5% of 100,000 within 4,600 to 5,400). Carried here, each instead replays the draws of the
stream the network names, in a world of its own on the same seed (a stream's draws depend on the
seed and the name alone), and requires the network's fate for each message to be the model's by its
definition: the Gilbert–Elliott process for loss (per message, and in time from the time since the
flow's previous message), `one_way − jitter + U[0, 2·jitter]` held behind the
flow's previous arrival for delay. The rest of focal's tests (the bottleneck, CoDel, ECN marking,
the NAT, partitions, bounds, accounting) were exact already and are carried as they were. With the
new pieces' own, 26 tests.

**The gate's other items.**
- hyper-transport's test network (`tests/common`, `Net<A, B>`) runs on the world and the network:
  node 0 and node 1 on the zero path, the world's time the caller's from its instant. Its interface
  is unchanged, and so are the outcomes of the 26 tests that use it (`tests/exchange.rs`).
- hyper-swim's first simulation (`crates/hyper-swim/tests/sim.rs`): the cluster test's five
  members on the world and the network's LAN path, each clock with its own offset, a rate within
  RFC 5905's 15 ppm and timers late by a drawn amount; once every pair is judged a member is
  killed, and every survivor holds it dead within the bound its detector stated, while none holds a
  live one dead. Sixteen seeds, each through the run-twice check, its members' clocks drawn through
  the world so the trace replays them.
- **Every one of them runs a seed through the run-twice check** (§3.9, `scripts/check-contracts.py`):
  the network's replay scenario, hyper-swim's sixteen seeds, hyper-liveness's live peers to their
  doubling, and hyper-transport's connection, whose endpoints draw their keys and nonces outside the
  world: what the network orders does not depend on them.

- **Measured delays.** A `Path` may propagate as a host measured (`Path::measured`): its quantiles
  at probabilities in parts per million, drawn from by the inverse transform, linear between the
  grid's points, the draw one integer below a million and the interpolation exact in integers
  (hyper-timing-trace's grid, whose finest point, 0.99999, is a whole part per million). A table
  that is no distribution is refused, typed (`SimError::NotADistribution`).

- hyper-liveness's simulation (`crates/hyper-liveness/tests/sim.rs`) runs on the world and the
  network. Its heartbeats travel a measured path, its host's one-way delay table on every pair;
  its disks' flushes and its owners' timer lateness are drawn by the owner from streams of the world
  through the same inverse transform (`Measured::at`), the timers set to the instant they fire
  with no lateness of the world's own, since a host's measured sweep, a table for each asked wait,
  is not a floor and a spread; and its freezes, its writes and every arrival are events the world
  orders, events before timers and each in the order made (`Fifo`), as its own queue ordered them.
  Every one of its 15 tests passes with its assertions unchanged, at the default seeds and at 80
  seeds a test (3,840 runs).
- **The trace's reservation**, which the world makes when a run begins (§12.2), is each harness's
  bound stated from the runs it makes: four times the most any run took, measured and named where
  the bound is stated. hyper-liveness's runs took at most 38,725 steps, 39,383 decisions and 13
  events pending over 80 seeds a test; hyper-swim's at most 1,240 steps and 1,574 decisions over its sixteen. A soak
  that passes one has stopped converging or found a longer tail, and the bound is measured again.

**Open in S-2.**
- hyper-liveness's harness against the world, in cost (`benches/step.rs` holds both, §12.7, where
  the world cost 7 ns a step more at three nodes).
- hyper-transport's allocation bench counts the endpoints' allocations, assuming the network makes
  none once warm. On the moved network every row with a body, and the lane frame, is identical to
  the run on the old one at the same commit's parent (2026-10-04, one build job, load from the
  other sessions' builds; counts are exact whatever the load). The two rows without a body are
  0.01 allocation an exchange above it: 8.36 against 8.35, and the bare stream 8.09 against 8.08,
  with one byte more. That is twenty allocations in 2,000 exchanges that are the network's own,
  still to trace: the world's queue and the network's arena push only into reused vectors, so it
  is not their steps.


## 14. S-4 as built (2026-10-04)

`crates/hyper-check`: its oracles (`src/oracle/`), liveness (`src/liveness.rs`), the floors
(`src/coverage.rs`), the two checkers of linearizability (`src/search/`, `src/witness.rs`) and their
agreement (`src/agree.rs`), with `std` and `hyper-sim` its only dependencies and no `unsafe`. Its
tests are in `crates/hyper-check/tests`; hyper-raft's schedules are judged by all of it in
`crates/hyper-raft/tests/check.rs`. What §4.1–§4.4 and §4.6 assign S-4, and what was built:

### 14.1 The oracles (§4.1)

Each is a type fed observations after every step and returning a typed `Violation` that names its
oracle; each holds a stated bound of what it keeps and refuses past it (`Violation::Full`). Each has
a test of what it allows and of a planted case it refuses (`tests/oracles.rs`).

| Oracle | Built as | Source of its statement |
|---|---|---|
| Election Safety | one leader per term, ever, over every term seen | Raft thesis Fig. 3.2 |
| Log Matching | each (index, term) the first value seen there, across every member and over time; a term never falls along a log. `Terms::Ignored` where the fast track gives an entry its new leader's term (an entry restamped at an election is the same entry under a later term) | thesis Fig. 3.2; slates' explorer |
| Leader Completeness | a leader, the first time it is seen leading its term, holds every entry committed in an earlier term at its index, read through a `LogView` | thesis §3.6; TLA+ `LeaderHolds` |
| State Machine Safety | one entry per index, compared by what it states (`Terms::Ignored` in fast groups) | thesis Fig. 3.2; hyper-raft `chosen` |
| Fast agreement | a ghost of every vote cast, counted from the moment it is cast. Its round is a term and its quorums the protocol's: a fast quorum of every set of voters the term's leader counted by (the one it was elected under and the one other a change named), none for a term elected under a joint configuration or naming a third set (`docs/raft.md` §3, the second rule). A vote is an acceptor's only where the index is open: above every index committed, and, while the term's leader leads, for the entry that leader holds there if it holds one | Lamport, Fast Paxos §3.3; slates' `votes_cast` |
| Durability | over a `DurableView` (a member's term, vote, log terms and stated commit as its device holds them): I1 promises, I2 acknowledgements, I3 self-count, I4 apply, I5 restart, R-6 the commit fence, I7 order, I8 starts; every message released held to its sender's device | `docs/durable.md` §3 |
| Read safety | a read is answered at or above the highest commit any member had heard of when it was asked | thesis §6.4 |
| Exactly once | each acknowledged write applied once at one index on every member, and on every member of the settled configuration | mantle, focal `DuplicateCommit` |
| Same history | each index applied reaches one digest on every member | mantle's rows, the E2E digests |

**What the judging changed in the oracles.** Four statements were sharpened on evidence from the
schedules, each a false alarm traced to its cause, not a looser check:
- Fast agreement first counted the voters of the configuration in force when a leader was first
  seen, and every vote. At seed 72 votes from members that were no voters of the term were counted;
  at seed 17,961 of the fast schedules a member behind its group sent again, in later terms, what
  it held beside its log at an index committed in term 3, and in configurations of one voter its
  vote alone made a fast quorum. Neither is an acceptor's vote in the protocol: a leader counts no
  vote at an index it holds otherwise, and every later leader holds what was committed.
- Log Matching compared terms in fast groups, where an election restamps what it recovers (seed 500
  of the fast schedules).
- Leader Completeness held a leader to every entry committed, including what a later term committed
  before that leader's own late election was seen (seed 34,639 of the fast schedules). It now holds
  a leader to what was committed in an earlier term, the term an index was committed in being the
  term of the member the group's commit first reached it at (the thesis's §3.6 statement).
- The serving monitor held a read confirmed by a member that a change then removed, which is sent
  nothing more (seed 2,976): its owner gives such a read up, as it does at a new term or a restart.

### 14.2 Liveness and floors (§4.2, §4.4)

- **Quiet period** (`Quiet`): on ticks, `2·(2·election_tick − 1 + patience) + 1`, twice the longest
  timeout a member draws with its patience and a round; by suspicion, in the ordered discipline,
  the detector's detection time, the span the members draw from, the vote rounds and a replication
  round (`Quiet::ordered`), and in rounds of a tick (`Quiet::in_rounds`). hyper-raft's
  `Cluster::settles` now takes its period from these, with the members' own settings; the numbers
  are those it had (`the_quiet_period_comes_from_the_members_settings`).
- **Progress** watches each member's term, commit, applied and last index, and says stuck once a
  quiet period passes in which none moved.
- **Monitor** (P# §2.5): obligations raised hot and met cold; a run that ends hot fails, naming the
  first open obligation. The judge's serving monitor holds every read a member confirmed until it
  is served or given up.
- **Floors** (`Floor`, `hold`): a named counter per path, `Per::Seed` (more than one a seed) or
  `Per::Campaign` (at least one), each stated with the count and seeds it was measured at; a floor
  above its own measurement, or on a path no counter declares, fails as surely as one that fell.
  hyper-raft's judged schedules hold 19 paths each, from the counts of their default seeds
  (`group_floors`, `fast_floors`, `pipelined_floors`).

### 14.3 The search checker (§4.3)

Built as §4.3 states, with these readings of the papers (`docs/research/sim.md` §5):
- **The search** is Horn and Kroening's Algorithm 1 over Wing and Gong's list of call and return
  events (`search/list.rs`: `lift` unlinks a call then its return, `unlift` relinks them in the
  other order). Candidates are tried in Lowe's just-in-time order: an operation that must go now (a
  return reached with its call not linearized) first, then those whose call precedes the first
  return left, operations that return before those that never do. WGL searches orders, not
  linearization points, so Lemma 6 shrinks nothing it searches; it orders the trying, and gives a
  configuration's compact form.
- **The memo** holds a configuration as `⟨linearized set, state⟩`. Its set part is a Zobrist key, the
  XOR of a 64-bit key drawn per operation from SplitMix64 (constant-time update, Horn and Kroening
  §5.1), and the state's hash beside it; the two make a 128-bit fingerprint through SipHash-1-3 in
  two lanes. The table is open addressing at a load of ¾ (Knuth's `½(1 + 1/(1−α)²)`, 8.5 probes for
  a new key at its fullest), doubled within the budget, the old and new tables counted together
  while it grows.
- **A refusal from fingerprints is confirmed on whole keys**: the partition is searched again with
  a memo of (first return left, the operations linearized ahead of it, the state), exactly; only a
  confirmed refusal is reported. `a_refusal_from_fingerprints_is_confirmed_on_whole_keys` plants a
  fingerprint that collides on purpose and sees the confirmation pass the history.
- **Budget**: the memory ceiling of §7, 4 GiB (slates' `MEMORY_CEILING_BYTES`); a partition that
  reaches it is `Unknown` with what it spent, never a pass or a refusal.
- **Counterexample** (Lowe §3): the deepest linearized prefix reached, the operation that could not
  follow it, the state there and the outputs that would have been legal.
- **Indeterminate operations** return at +∞ and accept any output; a write that failed for good is
  no operation (`Outcome::Failed`, Knossos's `:fail`).
- **Partitions** by key (Herlihy and Wing Theorem 1), each searched alone, the orders put back on
  the whole history's places.
- **Lowe's bound holds for the memo's configurations**: a WGL configuration's linearized set is every
  operation returned before the first return left and some of those pending there, which is Lowe's
  configuration; `a_registers_configurations_are_within_lowes_bound` holds the searches of 2,000
  seeded register histories, short and long, to `(N+1)·2^p·(p+1)`, `p` counting lost writes.

**Evidence.** The search is checked against an exhaustive tree search on every history of up to
three operations over every placement of their intervals (two writes each over 14 placements, ten
in `0..=3` and four pending, and three reads over ten: `58 + 58² + 58³` = 198,534 histories), and on
6,000 seeded histories of four to eight operations by one to four clients, honest and not;
every order it exhibits passes `verify`, an independent statement of Herlihy and Wing's §2.2
definition (each operation placed once, real-time order by a running maximum of calls, the model
replayed). mantle's two `linear.rs` cases (`sequential_and_overlapping_histories_that_fit`,
`histories_that_do_not_fit_are_refused`) pass under their own names in `tests/search.rs`.

### 14.4 The witness checker and the agreement (§4.3)

`witness::check` is focal's checker (`focal-sim/src/history.rs`) over a `Model` rather than its
ledger: invocations, publications in a contiguous sequence per object, completions. Its errors are
focal's (`Capacity`, `Identity`, `Prefix`, `DuplicateCommit`, `PrematureSuccess`,
`ResponseMismatch`, `StaleRead`, `StateMismatch`) and five this design adds: a publication nobody
asked under complete tracing (`Unasked`), one with no attempt of its key under way
(`Unattributed`), a refused attempt that took effect (`RefusedButPublished`), a read that changed
the state (`MutatingRead`) and a failed request that was published (`FailedButPublished`). focal's
eleven `history.rs` cases pass on it with focal's ledger as the model (`tests/witness.rs`).

`agree` runs both on one stream: the witness's publication order is turned into operations
(`agree::operations`, which drops failed requests and reads never answered) and given to the
search; each order either exhibits is verified with `verify`. A disagreement is typed
(`SearchRefused`, `WitnessRefused`, `Unknown`, `Unverified`, `Search`). `tests/agree.rs` runs a
simulated service with planted defects: a value nobody wrote and a stale read are refused by both;
defects in the system's own order (a request published twice, a success answered before its
publication) are the witness's to find, and the search, which does not trust that order, passes the operations' history.

**Agreement counts.** Every history of every schedule judged here, both checkers passing and each
order verified:

| Histories | Seeds | Operations |
|---|---|---|
| mantle's range simulation (§14.7) | 48 | 2,880 |
| hyper-raft's group schedules | 96 | 24,532 events |
| hyper-raft's fast schedules | 96 | 6,615 events |
| hyper-raft's pipelined schedules | 48 | 2,332 events |
| hyper-raft's group schedules, lossy and repeating (§14.5) | 3 × 96 | 32,898 events |
| campaigns: group 5,000 and fast 20,000 seeds | 25,000 | 2,619,966 events |

### 14.5 hyper-raft's schedules, judged

`crates/hyper-raft/tests/check.rs` runs `tests/group.rs`'s, `tests/fast.rs`'s and
`tests/pipeline.rs`'s schedules with every oracle after every step (an `Observer` on `Cluster`) and
records a client history of two registers, three clients each (§4.3's bound, mantle's three
gateways): a write answered committed when applied, failed when another entry took its index or its
term ended past the commit, unknown when its member restarted or changed term, with a retry
answered when its index is decided; a read confirmed by a read index and served once its member
applied through it. The harness's own checks are off (`Settings::judged`) so that the oracles alone
judge. A mutant's catch is corroborated by the same seed without it keeping every oracle.

**Hostile networks.** The group schedules again with the network losing and repeating 25, 50 and 75
messages in a hundred, every oracle holding and both checkers agreeing on every history
(`the_group_schedules_on_hostile_networks_keep_every_oracle_and_their_histories_are_linearizable`):
17,723, 9,790 and 5,385 events over 96 seeds each. Reordering is the schedules' own (any message in
flight is delivered next); partitions are their `windows`.

**What the judging found in hyper-raft.** Two defects of the core, each fixed at its cause with a
directed test that fails without the fix:
- *A fast-track leader served reads below a commit made before they were asked* (read safety, seed
  15,761 of the fast schedules). A leader serves reads once it commits in its term (thesis §6.4);
  in a fast group what a new leader recovers bears its term and sits below its blank entry, and may
  lie below an index a fast quorum committed earlier. Reads now wait for the blank entry the leader
  began its term with (`Raft::commit_to_current_term`; `docs/raft.md` §3.3;
  `a_fast_leaders_reads_wait_for_the_entry_it_began_its_term_with`).
- *A leader never caught up a member that lost what it kept ahead of a hole* (liveness, seed 1,318 of
  the group schedules, beyond the 96 the gate runs). R17's leader takes a refused append as kept and
  sends again only the holes before it; a member that restarted had dropped what it kept, so the
  hole before it was in no message the window would send, and every new append was refused for as
  long as the group ran. A window holding what a member kept now waits on the hole before it as on a
  resend: a beat with no answer for it has the member probed (`docs/raft.md` §3.3;
  `a_member_that_lost_what_it_kept_ahead_is_probed`). A first fix, probing at the refusal when the
  message holding the member's first missing index was taken as kept, failed R17's own measure on
  reordering paths (`on_paths_that_reorder_what_arrives_ahead_is_not_sent_again`: a late refusal
  probed what was arriving), and was replaced.

### 14.6 The planted mutants (§4.6)

Behind hyper-raft's `mutants` feature, which no consumer enables: `RawNode::plant(Some(Mutant))`.
Each is caught by an oracle, and the same schedule without it keeps every oracle.

| Mutant (§4.6) | Where it is planted | Caught by | Where | Clean without it |
|---|---|---|---|---|
| A commit counted from an older term's replicas | `maybe_commit` takes the entry's term for the leader's | Leader Completeness: the later leader lacks index 2 | the thesis's Figure 3.7, played by three members (`a_commit_counted_from_an_older_term_is_caught`); 2,096 random group seeds never reached it | the same play |
| A read served before the term's first commit | `read_index` skips the wait | Read safety | seed 0 of the group schedules | the default seeds |
| A vote sent before it is durable | `RawNode`'s ready lets a vote leave before persisting | Durability I1 | seed 0 of the group schedules on members whose writes lag, depth 2 | the default seeds |
| The fast track without its first rule (seed 9843's) | the `beside` rule skipped in `fast_commit` | Leader Completeness | seed 47,818, the first of a campaign from 0 (`the_fast_track_without_its_first_rule_is_caught`) | the same seed |
| The fast track without its second rule (seed 54104's) | the fast quorum counted of the configuration in force alone | Leader Completeness | seed 121,040, the first of a campaign from 0 | the same seed |

Each fast-track mutant's original seed no longer reaches its defect on today's core. The campaigns
(2026-10-04, release, two at once at load 64–104) ran 30 and 91 minutes. Earlier catches at lower
seeds (82, 500, 4,846, 15,761, 17,961, 34,639) were each a false alarm of an oracle or the read
defect of §14.5, found by running the same seed without the mutant; each was traced to its cause
(§14.1, §14.5) before the campaign was run again.

### 14.7 mantle's range simulation

`tests/data/mantle-range-histories.txt` holds the clients' histories of mantle's range simulation
(`crates/range/tests/sim.rs` at mantle `21ae613`), every default seed (1 to 48), with mantle's commit
order as the witness: made by the recorder in `tests/data/mantle-range-recorder.patch`, which adds a
record of each gateway's call, publication and reply and changes nothing else (mantle's own
`a_group_under_faults_is_linearizable_and_applies_every_put_once` and
`a_seed_runs_the_same_every_time` pass on the recorded copy). Each key is a register. Both checkers
pass all 48 histories (2,880 operations); with a value nobody put planted in each history's first
read, both refuse all 48 (`tests/mantle.rs`).

### 14.8 Costs, and Miri

Per history checked, both checkers on one history, p50 / p99 / max over the seeds (by nearest rank:
at 96 or fewer seeds the p99 is the most), run one test at a time so the process's counts are the
history's, release, Apple M5 Max, 18 cores, the load beside each (`docs/benchmarks.md`, "hyper-check's
checkers"):

| Histories | Seeds | Load | CPU µs (user + system) | Instructions | Allocations | Peak bytes |
|---|---|---|---|---|---|---|
| hyper-raft group | 5,000 | 67.5 | 60 / 127 / 202 | 481k / 1,095k / 1,490k | 224 / 389 / 495 | 29,056 / 61,160 / 105,552 |
| hyper-raft fast | 20,000 | 15.8 | 22 / 49 / 112 | 122k / 327k / 711k | 94 / 180 / 312 | 8,392 / 18,496 / 46,816 |
| hyper-raft pipelined | 48 | 83.4 | 20 / 42 / 42 | 108k / 211k / 211k | 86 / 133 / 133 | 4,985 / 9,336 / 9,336 |
| group, 25 in 100 lost and repeated | 96 | 83.4 | 46 / 105 / 105 | 323k / 1,095k / 1,095k | 176 / 382 / 382 | 16,632 / 60,664 / 60,664 |
| mantle's range simulation | 48 | 47.9 | 46 / 126 / 126 | 667k / 899k / 899k | 671 / 700 / 700 | 19,440 / 22,035 / 22,035 |

(The CPU column sums each measure's own tails, so it bounds the sum's tail from above; each is
stated apart in `docs/benchmarks.md`.)

The search's peak memory, by the counting allocator, is held against the ceiling of §7 on every
history (`agreed` asserts it below 4 GiB): the most any history held was 105,552 bytes.

**Miri.** hyper-check holds no `unsafe` (the workspace denies it, and `scripts/check-contracts.py`
lists no file of it). Under Miri (`nightly-2026-09-05`, the CI job's) its oracle, liveness and witness
tests take 3 s, 1 s and 4 s on this machine at load 13–14, and join CI's `miri` job beside
hyper-sim's 9 min 20 s, within the job's timeout. The search, agreement and mantle tests do not fit
it: the agreement's first test ran 16 min and the mantle histories 20 min without finishing (load
13–80). `hyper_measure::usage` reports nothing under Miri, which interprets no foreign call.

### 14.9 The papers read

Fetched 2026-10-04 from the URLs mantle note 06 A6 names; kept outside the repository, their
SHA-256 here so a later reading can tell it read the same file:

| Paper | SHA-256 |
|---|---|
| Herlihy, Wing, TOPLAS 1990 | `79e2ff81a15c55de870492231e81e36c50366389c118a7317b5877f603cfa2a0` |
| Horn, Kroening, FORTE 2015 (arXiv:1504.00204v1) | `47766e24087f1b69de61c67f53d0245cd4ca0f95c3f5aadd6887f7202a52fad2` |
| Lowe, CCPE 2017 (author's preprint) | `f7367bd6a8458daebfc68edf0f4f4da25ba651c8ea2a0b1273a694ec9c549bb9` |
| Wing, Gong, JPDC 1993 (a scan) | `01209cf96366e9eaff39c56b40005555b2178d070b2ba3e4386dea5fafbd9b9d` |

### 14.10 Open in S-4

- **Competition parallel** (§4.3, §11 item 5): not built. On every history judged here the search
  took at most 205 configurations (mantle's 58); there is nothing yet for a second search to win.
- **Elle** for transactions stays out of scope (§4.3).
- **The strategies' worth** against the mutants (§11 item 1) is S-5's: S-4 records the seeds a
  random campaign from seed 0 needed.

## 15. S-5 as built (2026-10-04)

The strategies of §4.5, built in `crates/hyper-check` beside S-4's judges and run on hyper-raft's
core:
- `src/explore/`: the exhaustive search of a model (`mod.rs`: the `Model` trait, the serial
  breadth-first search with shortest histories, the parallel level-synchronous search; `pack.rs`,
  `symmetry.rs`), and the round search of an implementation (`rounds.rs`);
- `src/strategy/`: the swarm's draws (`swarm.rs`), PCT and its confidence (`pct.rs`), the decision
  tape with its feasible mutations and shrinking (`tape.rs`), coverage guidance (`guided.rs`), and
  crash enumeration by fork (`fork.rs`);
- `src/conform.rs`: the TLA+ abstraction, its conformance check and its coverage points;
- `src/table.rs`: the budgeted open-addressing table, moved out of the search checker's memo so the
  visited sets and the coverage set use it too;
- `tests/exhaustive.rs` with `tests/models/`: slates' slot and prefix models, built again;
- in hyper-raft, `tests/strategies.rs` (the swarm, PCT, coverage with conformance, the round
  search, shrinking and the campaigns), `tests/pipeline.rs` (crash enumeration by fork, held to the
  replay), and `tests/support/judge.rs` (S-4's judge, moved out of `tests/check.rs` so both
  files judge with it, and its runs driven by any `Driver`).

### 15.1 Exhaustive search, model level (§4.5)

slates' design, built again under the production lints: a `Model` names its states, actions,
faults and a packed representative of a state's class; `shortest` is breadth first on one thread
with each class's key and parent in one table (the ids are breadth-first order, so the queue is a
cursor over it) and replays the chain of representatives into a concrete shortest history;
`explore` is breadth first level by level on a stated number of scoped workers over 128-bit
fingerprints, one shard a worker, nothing shared mutably, the workers joined before each phase
ends. The models' packer (`Packer`) and the representative's tie orders (`least_over_ties`) are
in the crate, each refusing what does not fit with a typed error.

**Memory, counted exactly.** slates held its counted bytes to the ceiling over a measured factor
of two between resident and counted memory (`RESIDENT_PER_ACCOUNTED`), for the growth it did not
count. Here every table and vector is counted at its capacity, a growth with its old and new buffer
together, before it is made; a search that would pass its `Budget` stops with `Outcome::Unknown`
and what it reached, never a pass. The counted peak bounds the resident one: 2.85 GB counted
against 2.19 GB resident at the largest scope (slot model, five members, four terms), and the
serial search's peak by the counting allocator stays within a 1 MiB budget that refuses
463,715 classes (`a_search_past_its_budget_says_unknown_and_holds_no_more`). The budget is §7's
4 GiB.

**Workers.** Four, GitHub's standard Linux runner's vCPUs, where CI's `explore` job runs the full
scopes. The classes counted do not depend on them: 56,971 at (4, 1, 2, 3) with 1, 2, 3 and 7
workers and serially (`the_workers_change_no_count`).

**The slot model** (`tests/models/slot.rs`) reaches every count slates recorded, exactly:

| Rule | Scope (members, indexes, values, terms) | Classes | slates |
|---|---|---|---|
| ballots | 4, 1, 2, 4 | **463,715** | 463,715 |
| ballots | 5, 1, 2, 3 | 1,586,398 | 1,586,398 |
| ballots | 4, 1, 3, 4 | 642,654 | 642,654 |
| ballots | 5, 1, 2, 4 | 23,552,907 | 23,552,907 |
| ballots | 3, 2, 2, 2 | 639,871 (every path, a commit above an uncommitted index among them) | not recorded |
| published, as written | 4, 1, 2, 4 | disagreement in 15 steps, at 1,049,232 classes | 1,049,232 |
| published, once a term | 4, 1, 2, 4 | disagreement in 18 steps, at 1,199,113 classes | 1,199,113 |
| published, keeping leader-approved entries | 4, 1, 2, 4 | 999,583, no disagreement; stalls after one crash | 999,583 |

The serial counts to a fault match too, so the search meets each fault where slates' did: its order
of actions is slates'. slates' two scripted histories (five members losing a fast-committed entry;
the stall) are tests here as there.

**The prefix model** (`tests/models/prefix.rs`). Built as slates' was, it reaches slates' counts
only when a cut log keeps its stale tail past its length as slates' array does: slates' signature
reads every place of the array, so one orbit may take two keys (`docs/research/sim.md` §7). The
model states which (`Tail`), and is held to both:

| Variant | Scope | Classes, tail cleared (one key an orbit) | Classes, tail stale | slates |
|---|---|---|---|---|
| design | 3, 3, 1, 2 | 1,951,672 | 1,951,672 | 1,951,672 |
| design | 3, 3, 1, 3 | 21,771,580 | 21,776,022 | 21,776,022 |
| design | 4, 2, 2, 3 | 12,551,719 | 12,559,351 | 12,559,351 |
| design | 3, 2, 2, 4 | 3,400,472 | 3,401,082 | 3,401,082 |
| reporting logs too | 3, 3, 1, 3 | 21,785,636, 49,654 recoveries from a log | 21,789,824, the same 49,654 | 21,789,824, 49,654 |

**Every `Variant` refused**, each by its shortest history at slates' length, and at slates' serial
count with the stale tail:

| Variant | Scope | Fault | Steps | Classes reached (cleared / stale) | slates |
|---|---|---|---|---|---|
| commits counted from windows | 3, 3, 1, 3 | a new leader lacks index 2 | 12 | 382,705 / 382,952 | 12 steps, 382,952 |
| slots dropped once covered | 3, 3, 1, 3 | a new leader lacks index 2 | 16 | 2,014,571 / 2,015,576 | 16, 2,015,576 |
| slots pruned at a commit counting fast ones | 3, 3, 1, 3 | logs diverge at index 2 | 12 | 860,670 / 860,982 | 12, 860,982 |
| slots dropped at a sync | 3, 3, 1, 4 | a new leader lacks index 2 | 18 | 15,374,180 / 15,379,817 | 18, 15,379,817 |
| voters report their logs too | 3, 3, 1, 3 | no property breaks; refused for recovering a deposed leader's entry from a log (49,654 times), which the design never does | — | — | the same |

The default suite runs the 463,715 scope, the workers' check, the scripted histories and the
budget's refusal (14 s in debug); everything else runs in release in CI's new `explore` job and
took 14.7 min on the owner's machine at load 69, 2.48 GB resident at most.

### 15.2 Random walk under swarm configurations (§3.8)

`strategy::Swarm` draws each seed's configuration from a stream of its own (named for the seed, so
it never moves the schedule's draws): each feature on with even odds (Groce et al. §2), each rate
uniform over its range. hyper-raft's `Configuration::swarm` states the ranges, each the schedule
tests' own: the rules (pre-vote, check-quorum, refusing what arrives ahead of a hole, a round a read,
bare answers, precedence by length, and for pipelined members applying before durability), the
windows and message sizes the tests use, every kind of step on or off (changes, a leader leaving,
restarts, compaction, partitions, priorities, windows, bursts of reads), losses and repeats up to
three in four (`tests/check.rs`' hostile networks), the fast track's share up to all, persistence
steps up to three in four, and three or five members. The group's kind (fast, pipelined) is the
harness's: the fast track's defects need the fast track.

### 15.3 PCT over members (§4.5, §11 item 7)

`strategy::Pct` is the paper's algorithm with members for threads (priorities `d … d + n − 1` by
Fisher and Yates's shuffle, `d − 1` change points uniform in `[1, k]`, the member that would run
lowered at each). Its steps are the deliveries at which messages for two or more members race; a
delivery with one member's messages only is no step. hyper-raft's `Prioritized` driver lets
`Cluster::choose` draw everything as before and replaces only which message a delivery takes: the
oldest message to the enabled member of highest priority. **Departure from §4.5's open choice
(§11 item 7):** per-member priorities, not chain partitioning (Ozkan et al.'s construction, whose
paper was not reachable): a member's messages are its chain, taken oldest first.

`k`, measured: the most racing deliveries any of the 96 default seeds' runs took — 1,939 (group),
377 (pipelined), 2,051 (fast), 2,035 (the first rule's harness) (`racing_deliveries_are_counted`).
`confidence` gives `1 − (1 − 1/(n·k^(d−1)))^R`; at these `k` a depth-two bug is found with
probability at least 1.0·10⁻⁴ a run at five members, so the campaigns' run counts reach, for
depths one, two and three, the confidences tabled in §15.8.

### 15.4 Coverage over the TLA+ abstraction, and the conformance check (§4.5, §11 item 3)

`conform::Abstract` reads a group as `FastTrack.tla`'s variables, from what each member holds
durably (its device's term, vote, log and commit, what it holds beside its log) with whether it
runs and leads. **The conformance check** holds every step to the relations every action of the
model's `Next` keeps (`src/conform.rs` names each with its actions): a term never falls; a vote cast
stays within its term; a commit never falls and a committed entry never changes, but by a fault at
rest (`Lose`); no entry bears a term above its member's; what a member holds beside its log is
above it; a member that becomes leader voted for itself; a leader keeps its log in its term and adds
only entries of its term. **Departure from §4.5:** these are the projections of `Next` on each
member's variables, not a search for a sequence of model actions from one abstract state to the
next; the model's messages are not abstracted (it keeps a member's last word, `says`, which the
implementation's messages in flight do not map onto one to one).

Every step of the default schedules is a model step: 244,088 steps over 24 group seeds, 24 fast and
12 pipelined, the liveness phases included (`every_step_of_the_default_schedules_is_a_step_of_the_model`),
and every step of every coverage campaign below.

**Coverage** is the abstract state's shape, bounded: per member whether it runs, its role, its
vote's kind, its term, log length, last log term and commit each as an offset below the group's
greatest capped at three (FastTrack.tla's largest `MaxTerm` and `MaxLen`), and its entries beside
its log capped at two (`HeldAt`'s most). **The campaign** (`guided_campaign`, Gulcan et al.'s
Algorithm 1): fresh tapes while no corpus entry has energy, then mutations of the entry with the
most; a run that reaches `m` new points joins the corpus with energy `m`, one run a unit; the corpus
holds 64 tapes, the weakest giving way. A tape's first step is the group's seed, so a mutation may
draw another group's member randomness as it draws another schedule.

**The tape** (`strategy::tape`). Every word a schedule draws, grouped by step, each step tagged as
a delivery, a crash or other. A word is read as the harness reads any draw (the high half of its
product with the bound), so any tape is feasible, and a tape recorded from a seed is that seed's run
(SplitMix64 read the same way). Mutations are Gulcan et al.'s three over steps: a step's words
redrawn (which link or member it chose), two steps of a kind exchanged, a step dropped or repeated
(one delivery fewer or more). The tape's bound is four times the most words any default seed drew
(25,358) and a step for the seed.

### 15.5 Implementation-level exhaustive search at a tiny scope (§4.5)

`explore::rounds` searches every sequence of a few rounds of a real system depth first, the
systems alive at once one per round of the path (§7's bound for forks), each round's resulting key
held in a fingerprint set within the budget, a prefix not followed when its key was explored with as
many rounds left. hyper-raft's `Tiny` is three members, all voters, on focal's rules with one
entry an append, no pre-vote, no check of quorum, and refusing what arrives ahead of a hole. A
round is one of 144 scenarios (Twins §4.2): a leader (3), its election's partition (3), its
replication's partition (4), whether its replication carries entries of its new term (2), and
whether it then takes a proposal it sends to no one (2). Every member restarts at a round's start,
so a round's state is the members' disks and the judge's oracles, which the key holds: the
reduction's claim (two prefixes with one key have the same futures) holds by construction.
**Departure from §4.5:** no symmetry reduction; members are renamed by nothing, since the key
holds the oracles' ghosts, which name members.

| Rounds | Rounds played | Distinct states | Prefixes pruned | Result |
|---|---|---|---|---|
| 2 | 12,240 | 85 | 60 | every oracle keeps (default suite) |
| 3 | 249,840 | 1,735 | 10,506 | every oracle keeps, with the older-term commit planted too |
| 4 | 4,481,280 | 31,120 | 218,721 | every oracle keeps |
| 4, the older-term commit planted | 33,621 to the catch | — | — | caught: Leader Completeness |

The search found the commit counted from an older term's replicas in four rounds — a shorter
history than the thesis's Figure 3.7, at index 1: member 1 leads by {1, 2} and replicates its
first entry to no one; member 2 leads by {2, 3}, likewise; member 1 leads again by {1, 3} and gives
member 3 its old entry and nothing of its term, so the defect commits it; member 2 leads by {2, 3}
without it. The figure's own path is a directed test (`figure_three_seven_is_a_path_of_four_rounds`).
2,096 random group schedules never reached either (§14.6).

### 15.6 Crash enumeration by fork (§3.7, §11 item 2)

**The core made `Clone`.** `RawNode`, `Raft`, `Log`, `Unstable`, `Page`, `Early`, `Outgoing` and
`Given` derive `Clone` (every other type a member holds already did): a derive adds code only where
a clone is asked for, so production pays nothing. The harness's `New`, `Lagged`, `Cluster` and
`Member` derive it too.

`strategy::fork::each_point` runs a schedule once and, after each step that returns a point, clones
the world, runs the clone's branch to its end, and drops it before the trunk's next step: one child
alive at a time. hyper-raft's `pipeline.rs` now enumerates its crashes by it (`forked`): at each
persistence step that did something, the group and its draws are cloned, the member crashed in the
clone, and the clone run through the rest of the schedule and its liveness phase. **The gate**
(`a_crash_point_forked_ends_as_its_replay_ends`): every crash point of every schedule the
enumerations take — 1,964 without faults at rest and 2,016 with, over 4 settings × 11 seeds × 400
steps — ends forked exactly as the replay with the crash there ends: every member's disk and view,
what was committed, the persistence steps' coverage, faults, waits, repairs and incarnations. The
trunks took 17,600 steps where the replays' prefixes took 785,600 and 806,400.

### 15.7 Shrinking (§4.5)

`strategy::tape::shrink` removes chunks of steps, halving them, then single steps until a whole
pass removes none (Zeller and Hildebrandt's `ddmin`, complement steps): the result is 1-minimal, or
the run budget ended first and it says so. The random walk's catch of a read before the term's first
commit (seed 0 of the group schedules), recorded through a tape — which replays the seed's run
exactly, the same steps and the same violation — shrinks from 944 steps to 30 in 471 runs, 1-minimal,
and still fails with a stale read
(`a_failing_run_shrinks_to_a_minimal_tape_that_fails_the_same_way`).

### 15.8 The runs each strategy needed (§4.6, §11 item 1)

Each campaign runs its strategy from its first seed until a run catches the defect, within the
random walk's own count (§14.6), so that a strategy not catching within it has not beaten the
random walk on that defect, and at least the 2,096 runs the random walk was given for the defect it
never caught. Each harness and its mix is `tests/check.rs`'s; each catch's run without the defect
keeps every oracle (§15.9). Release, the campaigns' processes four to six at once beside other
sessions' work, load 9–72 (`docs/benchmarks.md`, "hyper-check's strategies and searches (S-5)").

| Defect (§4.6) | Random walk (§14.6) | Swarm | PCT, depth 1 | PCT, depth 2 | Coverage-guided | Exhaustive rounds (rounds played) |
|---|---|---|---|---|---|---|
| a commit counted from an older term's replicas | not in 2,096 | **172** | not in 2,096 | not in 2,096 | not in 2,096 | **33,621** (four rounds) |
| a read served before the term's first commit | 4 | 18 | 8 | 8 | 20 | outside its scenarios (no reads) |
| a vote sent before it is durable | 1 | 1 | 1 | 1 | 1 | outside its scope (no lagging writes) |
| the fast track without its first rule | 102,775 | **3,313** | 22,066 | 38,861 | not in 102,775 | outside its scope (no fast track) |
| the fast track without its second rule | **8,868** | not in 8,868 | not in 8,868 | not in 8,868 | not in 8,868 | outside its scope |

The counts are those on the core that counts by the newest configuration in its log (§15.12) and
holds what it holds until a classic commit (§15.13), 2026-10-05; the campaigns were run again on it
from their first seeds, and each catch is its defect's (`each_catch_by_seed_is_its_defects`,
`each_catch_by_coverage_is_its_defects`). On the core of §15.12 the read before the first commit took
13, 13 and 8 runs of PCT and coverage. On the core before the release (§15.12 alone) the two fast-track
rows were 67,843, 2,881, 26,306, 28,115, 6,655 and 1,484, 571, not in 2,096, not in 2,096, 1,496;
before that, the second rule's swarm campaign met the core's open defect first (§15.9), and the first
rule's counts were 47,819 (random walk), 706, 25,324, 21,181 and 4,787.

The guarantees the PCT campaigns' run counts carry, by Theorem 9 at their `k`, five members: for a
defect of depth one, 1 − (4/5)^R, above 0.9999 at every count here but the first runs; for depth two,
0.925 at 26,306 runs and 0.937 at 28,115 (the first rule, on the core of §15.12), 0.194 at 2,096
(the group's) and 0.185 (the second rule's); for depth three, at most 1.4·10⁻³ (the campaigns' own
report, 2026-10-05). **What the counts say.** On the first fast-track rule the swarm beats the
random walk by 31 times and PCT by 2.7 to 4.7, and coverage guidance does not catch it within the
random walk's count; on the second, under the
release (§15.13), only the random walk catches it within its count: a member holds a vote now
until a classic commit covers it, so fewer schedules leave a fast quorum counted across a change. The swarm alone catches the older-term
commit, which needs an order of elections and partitions rather than of deliveries. The exhaustive round
search is the only strategy that guarantees its result within its scope: it plays every four-round
scenario, and the older-term commit is among them.

### 15.9 What the strategies found

The swarm with no defect planted, every step held to the model and every group required to settle
(`the_swarm_with_no_defect_keeps_every_oracle_and_the_model`, 5,000 seeds of each of the group,
fast and pipelined harnesses), and the swarm's campaigns, found two defects of the core, two of the
harness and three of the judges. Each was traced to its cause; all but the core's second are
corrected, each with a directed test that fails without its correction, and the second is open (below). With them corrected the hunt ends with no failure over its 15,000 seeds, and every catch of
§15.8 is its defect's: the same run without it keeps every oracle (`each_catch_by_seed_is_its_defects`,
`each_catch_by_coverage_is_its_defects`). The round search to four rounds and the conformance check
over the default schedules found nothing.

**In the core: a leader of an older term never told of the newer one** (fast seed 3,112: four
members, neither check-quorum nor pre-vote, changes on). A change made member 1 a learner in its
group's later configuration while member 1, not yet having applied it, led term 24 by an older one;
member 3, in the later configuration's joint form, campaigned without end (its log too short for the
others), reaching term 3,898; member 1 sent its heartbeats of term 24 to member 3, which dropped them.
A member answered an append or heartbeat of an older term only under check-quorum or pre-vote, as
raft-rs does, leaving the rest to its vote requests, and a member's vote requests go only to the voters
its configuration names: member 1 led its old term for ever and the group never converged. It now
answers whatever the settings (thesis Figure 3.1, "reply false if term < currentTerm"; `Raft::step_older_term`,
`docs/raft.md` §3.3), and the leader steps down
(`a_member_of_a_later_term_answers_a_leader_of_an_earlier_one`, which fails without it; the seed,
`the_swarm_seeds_whose_groups_never_converged_settle`). The differential with raft-rs loses such a
message for both cores, a third decided difference, and holds that its runs reach it.

**Fixed in the core: a fast-committed entry lost with the record of a vote** (swarm campaign against
the second rule, fast seed 41,345, no defect planted: four voters, check-quorum on, pre-vote off, no
changes, restarts or partitions). Index 10 was committed by the fast quorum in term 13, member 4's
vote among the three. Member 4 led term 18, recovered the entry into its log and so released its
vote (`Raft::release_proposals`: a member holds only above its log); term 23's leader cut member 4's
log at index 6 with an append that carried entries short of index 10, member 4 then held another
proposal there, and member 4 was elected in term 34 by members 1 and 3, neither of which held anything at index 10 by
itself: the most held of the reports was the other value. The recovery argument (`track.rs`, after Fast Paxos's
condition O4) takes a member that holds the entry from a leader to vote for no candidate lacking it,
which fails once a later leader's recovery stamps its term on entries below the index. slates'
search refuses the same rule (`Variant::DropCovered`, §15.1) and releasing at a commit that counts
fast commits (`Variant::PruneAtFastCommit`); its design releases a vote only under a classic commit.
`FastTrack.tla` keeps hyper-raft's release, and its `Replicate` always sends through the leader's
end, so it never cuts a log short of an index and cannot reach the run. The fix changes the core's
release rule, what a follower learns of the classic commit, the storage's contract for what it holds
and the model together, and is its own series (`docs/raft.md` §3.5, §15.13 here); the seed's test
(`a_fast_committed_entry_outlives_a_vote_its_log_covered_and_then_lost`) failed before it and passes
with it.

**In the harness:**
- *A snapshot lost at the network's bound was never reported to its sender* (fast seed 2,396). The
  network drops its oldest message to take a new one; a dropped snapshot left its leader waiting on
  it for ever, the member it was for caught up but never again sent to. An owner whose transport
  drops a transfer reports it failed; `Cluster` now does (`Cluster::report`; the seed's test fails
  without it).
- *The liveness phase had no bound* (§4.2: a group that keeps moving without converging is ended by
  the run's budget, never passed). Seed 3,112's phase ran for hours, its terms moving and its group
  never converging, so the quiet period never passed. The strategies' groups now state their bound,
  four times the most operations any of the swarm's 15,000 seeds took in its phase (11,292; the
  default seeds took at most 1,556), and a run past it is reported unconverged
  (`Cluster::liveness_bound`). The schedule tests that do not state one keep none: S-6's, when they
  move onto the world (§8).

**In the judges** (pipelined seeds of the first hunt of 2,000 seeds, eight failures, and a fast
seed of the swarm's campaign against the second fast-track rule, whose catch was no catch: the run
failed the same way with the defect taken out; held by
`the_swarm_seeds_that_found_the_judges_wanting_keep_every_oracle_and_the_model`):
- *The durability oracle's I1 refused a request the core may send* (seeds 123, 522, 967). A member
  campaigned with a log of an older term (seed 123: twenty entries of term 1); before its write's
  notice, term 3's leader cut its log to two entries, and that write was durable too, writes being
  durable in order and their notices later (I7). The request, released at its notice, named index
  20 of term 1, which the device no longer held. `docs/durable.md` §3 said a request "names only a
  last entry `D` holds", which the core never promised against later writes of the term. A request
  is judged by the last entry it names (thesis §3.6.1), and the device's log, (term 3, index 2), is
  more up to date than (term 1, index 20): every entry a voter granting the request could require is
  held. The oracle and §3 now state that (`a_request_from_a_log_its_terms_leader_cut_is_judged_by_its_claim`,
  which refuses a claim more current than the device as before).
- *The abstraction read a leader whose term was not yet durable as the model's leader* (seeds 299,
  585, 1,159, 1,580, 1,633, each a member that applies before durability, alone a voter after a
  change, elected at once). Such a member sends nothing until its term and vote are durable (I1), and
  leads in the model once that write is (`docs/models/README.md`, "Readies ahead of their
  persistence"); the abstraction now counts a member as leading only in a term its device holds.
- *Log Matching refused a term falling after a member's committed prefix* (fast seed 34,957). Index
  40 was committed under term 33 at member 3 (term 33's leader took it again at its election and
  stamped it, as the fast track does); member 1, leading term 37, held it under term 32 and sent its
  entry of term 32 at index 41 after it. A member keeps its committed prefix as it holds it and takes
  its leader's entries after it, whatever the two stamped the committed entries with
  (`Log::append_after`; `FastTrack.tla`'s `Replicate`, from `Max(p, commit[m]) + 1`), so member 3's
  log held term 32 after term 33. The election restriction is not weakened: an entry is committed
  only by holders whose last entry bears the committing leader's term (classically its own entry
  after it; by the fast quorum, the first rule), and a log's last term bounds what it may lack only
  above its commit. The oracle now lets a term fall in a fast group only where the entry before is
  committed at the member (`LogMatching::holds_at`, `a_term_falls_only_after_a_fast_members_committed_prefix`).
  `FastTrack.tla`'s own `LogMatching` compares whole entries, terms included, and would refuse that
  state; its scopes (at most two indexes or three terms) do not reach it. Whether the invariant
  should compare what entries state below a member's commit is the model's to decide in CI (§15.11).

**slates' prefix model's representative** is not one key an orbit (§15.1, `docs/research/sim.md`
§7): its counts are 0.02 % to 0.06 % above the classes'. No verdict of slates' changes. slates is
not edited here; the finding is reported to its owner.

### 15.10 Costs

`docs/benchmarks.md`, "hyper-check's strategies and searches (S-5)". In brief, CPU a run, p50 /
p99, load 17–20: the random walk 14.2 / 32.5 ms (group), the swarm 8.9 / 114.8, PCT 15.1 / 35.0,
the walk with every step held to the model 20.1 / 51.1 (the abstraction's cost, §11 item 3: 1.4
times the walk), the coverage campaign 50.9 / 63.6; a round of the round search 37 µs; crash
enumeration by fork 7.1 / 14.8 ms a schedule against the replay's 12.4 / 22.3; the full-scope model
searches 14.7 min, 2.48 GB resident at most.

### 15.11 Open in S-5

- **The fast track's lost entry** (§15.9, swarm fast seed 41,345): fixed (§15.13).
- **`FastTrack.tla` as a Rust `Model`** (§4.5's last paragraph): built (§15.13).
- **The conformance check is by projection** (§15.4): a step is held to every relation the model's
  actions keep, not shown to be a sequence of model actions.
- **The round search's scope** leaves out reads, lagging writes and the fast track (§15.8), and
  applies no symmetry (§15.5); three of the five planted defects lie outside it.
- **PCT's chains** (§11 item 7): per-member priorities were built; Ozkan et al.'s construction is
  still unread.
- **The strategies in the schedule tests**: the swarm, PCT and coverage run as campaigns here;
  moving `tests/group.rs`, `fast.rs` and `pipeline.rs` onto them, and hyper-durable's crash
  enumeration onto forks, is S-6's (§8).
- **slates' prefix model's representative** (§15.9) is slates' to correct; reported, not edited.
- **`FastTrack.tla`'s `LogMatching`** compares terms only where neither member has committed, and a
  scope reaches a restamped prefix (§15.13).

### 15.12 The configuration a member counts by (2026-10-05)

The random walk, re-run on the fast schedules, elected two leaders of a term (fast seed 135,923): a
member whose log held a change past its commit counted elections by the configuration it had applied
(`docs/raft.md` §3.4). The core now counts elections and commitment by the newest configuration in
its log. The schedules then found three more of the rule's consequences, each corrected at its cause
with a directed test (§3.4's list): a newcomer refused the snapshot that would seed it (group seed
0), a leader the newest configuration left out answered a read alone (hostile seed 47), and the
members holding the entry that leaves a joint configuration had no one they could elect (group seed
11). The harness held three things to the configuration applied that now count by the newest: the
durability oracle's configuration a commit was decided by, the members the stop rule and the
electability check read, and hyper-durable's I3; each now reads what the core counts by. A member that
restarts on a snapshot its disk holds is credited with it by the exactly-once oracle (pipelined seed
21, where the snapshot was durable and the member stopped before it heard so).

The swarm with no defect planted, re-run on this core, found a defect that is not the rule's and
that the core had before it (the directed test fails on the core before either): a stale read (group
seed 4,521). An asker asked a read again under the context an earlier read round had carried, so the
read waited at the queue's tail behind reads asked since; a late, repeated answer to that earlier
round named the context, and confirmed the read and every one before it, all asked after the round
left. A deposed leader answered at its commit of 48 a read asked when 220 was committed. A round now
carries its number after the context, and an answer confirms a read only if its round was sent while
the read waited (`ReadOnly::ack_round`; `docs/raft.md` §3.3,
`a_late_answer_to_a_round_confirms_no_read_asked_after_it_under_the_same_context`).

What the counts moved. The read before the first commit is first caught at group seed 3 (it was 0),
the first fast-track rule at seed 67,842, by the fast agreement oracle (a member voted two values at
one index in one term, where the leader's wrong count led; the run without the defect keeps every
oracle), and the second at seed 1,483. The floors of `tests/check.rs` are the counts on this core
(§15.2); a write displaced by another entry is reached about once a pipelined seed, and is held to
being reached.

### 15.13 Holdings kept until a classic commit (2026-10-05)

The fix of §15.9's lost entry (`docs/raft.md` §3.5) changes what a member holds and for how long, so
every campaign was run again on it from its first seed (release, beside other sessions' work):

- **No defect planted.** The swarm, 5,000 seeds a harness: group, fast and pipelined keep every
  oracle and the model, and settle within 9,039, 8,016 and 3,396 operations of a liveness phase
  (`LIVENESS` is four times the most, unchanged).
- **The planted defects.** The first fast-track rule's random walk first catches at seed 102,774
  (Leader Completeness); the second rule's at seed 8,867. The table of §15.8 gives the rest. Each
  catch's run without its defect keeps every oracle.
- **Its store, end to end.** `hyper-durable-e2e`'s `fast-kill-*` scenarios run the kill points of
  the classic scenarios on a group whose members each take writes and propose them by the fast
  track, on hyper-log over real fully flushed files: every answered write reads back and every
  member applies the same history. They found two faults, each fixed with a directed test that fails
  without it: hyper-log refused a write whose entries reached a proposal it carried and fenced the
  member (`docs/raft.md` §3.5, "Storage"; `tests/hyperlog.rs`,
  `a_holding_the_same_write_reaches_by_an_append_is_kept`; the scenarios fence a member without the
  fix), and the shell dropped `Ready::displaced`, so a member never learned that another entry took
  its proposal's index and its client waited on an entry no member applies (`Output::displaced`;
  `tests/shell.rs`, `a_fast_proposal_another_entry_took_the_index_of_is_given_back`). On this
  machine's disks no index of those runs was committed by a fast quorum: with three voters the fast
  quorum is all three, and the last holder's write lands behind the commit write it had out, after
  the leader's append round. The scenarios report the counts and assert none, for which path wins
  is a race; the swarm's fast harnesses decide the fast commit exactly.
- **`FastTrack.tla` as a Rust `Model`.** `tests/models/fasttrack.rs` is the specification's `Next`
  under `hyper_check::explore`, every constant a field of its `Scope`, and searches each
  configuration of `docs/models/` (`tests/fasttrack.rs`). It reaches TLC's count at every
  configuration it shares with CI's model job: with the rules before the fix, the eight of one
  value at the counts TLC published before (`the_model_counts_what_tlc_counted`); with the fix,
  `one` 573,160, `round` 3,974,278, `four` 3,602,968, `change` 4,067,274, `grow` 9,551,710, `classic`
  841,050, `joint` 2,626,941, `marked` 303,762, `markedchange` 431,168 and `markedjoint` 1,182,158
  (CI run 37336523719), and the scenario from the end of its term 1 at 3,870,308. Its scripts replay
  the swarm's run in the scenario (`the_entry_the_swarm_lost_is_lost_by_each_rule_before_the_fix`,
  `with_the_fix_the_same_run_keeps_the_entry`). CI's explore job runs its whole scopes.
- **`LogMatching` on restamped prefixes.** A member keeps the stamp of an entry it committed that a
  later leader's election took again under its own term, and takes that leader's entries after it,
  so two logs can hold an entry of one term at one index above entries of different terms; the
  invariant compares terms only where neither member has committed. No configuration reached such
  a pair (each searched with the claim `NoRestamp`, the scenario from its term 1 too); the restamp
  scope does, three voters at two terms and three indexes, the entry held at index 2
  (`FastTrackRestampReached.cfg`, refused; the Rust search finds the pair after 1,195,591 classes),
  and keeps every invariant at 3,808,625 states (`FastTrackRestamp.cfg`,
  `the_restamp_scope_reaches_a_restamped_prefix_and_keeps_log_matching`).
