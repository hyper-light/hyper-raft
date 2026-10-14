# The shard loop in a consumer: what a switch costs there, and the designs that cut it

The question: why hyper-rt's loop, measured faster in its own benches (rt-2x: a yield 1.65–2× faster, a
spawn 1.3–1.6×), was no faster in mantle at 100M and its fill's put p99 slower, and which loop designs
cut the cost a consumer actually pays. The decisions this note founds are written in
`docs/runtime.md` §3.10 (the shard wanted; the switch's lines) and §3.11 (a long poll); this note keeps
the measurements and the sources' own words.

## 1. What a consumer pays: measured in mantle's ranges bench

Linux VM on the M5 Max host, host load 7–14, 100M keys, the client on its range's shard
(`--shard-client`), every operation inline on the lent engine (fill 100,000,000 of 100,000,000,
gets 999,998 of 1,000,000, seeks 1,000,000 of 1,000,000). Runs and scripts:
`benchmark-results/mantle-splinterdb-20261013` (`sweep-100m.txt`, `serve-15.txt`, `bisect-15.txt`).

- **The client's turn is the quantum, and a yield ends it.** At a fixed 15 µs quantum a fill yields
  1.5–1.7 M times; a yield's median is 375–541 ns and its mean 1.5–1.8 µs, where hyper-rt's own bench,
  every line hot, measures 15 ns.
- **Every yield runs the range's task.** Its polls were within 0.5% of the yields in every round: the
  fill leaves the engine maintenance owed at every turn, and the range task lends, yields and reclaims
  while it is (`crates/engine/src/ranges.rs`, `serve`). So no yield of the fill is needless; the loop's
  own share of one, the yields' time less the range task's polls', was 189–265 ns, 1.2–1.8% of the fill.
- **The quantum is the boot probe's, and swings.** A busy shard never parks, so its wake estimate is
  the 40 ms boot probe's for the whole run: 9.9–66.8 µs, and twice 188 and 424 µs, across 26 runs. The
  put p99 follows how many puts yield: 0.88 µs at a fixed 5 µs (4.8 M yields), 0.72 at 15 (1.6 M), 0.67
  at 45 (0.5 M); the fill's rate 3.32 M, 3.82 M and 3.91 M puts a second.
- **rt-2x's regression was lines, not instructions.** At a fixed quantum rt-2x's yield cost 105–187 ns
  more at the median than its base's, at 100M and not at 30M, and the per-commit bisect showed a creep
  over the commits that won hyper-rt's own benches (the poll inlined into the batch, the batch's
  thread-local lookup, the channel's hop), with no single step. The desk's fields a switch reads were
  spread over six 128-byte lines by the compiler's order, with the 728-byte spawn frames rt-2x added
  between them.
- **Long-poll attribution judged correct turns.** A poll past one quantum was long; the client's turns
  and the range's maintenance slices are a quantum by design, so about half of them were, each judged
  with readings of the thread's account: 860–2,240 pairs of `clock_gettime(CLOCK_THREAD_CPUTIME_ID)` and
  `getrusage` a second on the shard (`perf trace`), and no output read but four counters.

## 2. Sources, read at their primary text

**Concord** (R. Iyer, M. Unal, M. Kogias, G. Candea, "Achieving Microsecond-Scale Tail Latency
Efficiently with Approximate Optimal Scheduling", SOSP '23, https://dslab.epfl.ch/pubs/concord.pdf).
- §2.2.1: preemption by IPIs "lead[s] to an overhead that grows linearly as the scheduling quantum
  decreases: 33% at 2µs and 6% at 10µs"; rdtsc() probes "a uniform ≈ 21% overhead across all scheduling
  quanta, since the probes are inserted approximately every 200 instructions".
- §2.2.2: a synchronous hand-off "subsumes at least two cache coherence misses, which add up to ≈ 400
  cycles in total".
- §3.1: decisions go "via a per-core dedicated cache line L_i, instead of IPIs"; "unlike an rdtsc call,
  which always costs ≈ 30 cycles, the cache line L_i is in the L1 cache of worker W_i for all but the
  final check, so most checks consist of an L1 cache hit plus a compare, i.e., 2 cycles"; the final check's
  coherence miss "≈ 150 cycles", "1/8 the cost of a Shinjuku IPI (which costs ≈ 1200 cycles)"; overhead
  "near-constant at around 1-1.5%"; cooperative yielding switches "between requests within ≈ 100ns".

**IX** (A. Belay et al., "IX: A Protected Dataplane Operating System for High Throughput and Low
Latency", OSDI '14), §3, "Run to completion with adaptive batching": "(i) we never wait to batch requests
and batching only occurs in the presence of congestion; (ii) we set an upper bound on the number of
batched packets. Using batching only on congestion allows us to minimize the impact on latency, while
bounding the batch size prevents the live set from exceeding cache capacities and avoids transmit queue
starvation."

**Seastar** (github.com/scylladb/seastar).
- `include/seastar/core/preempt.hh`: `need_preempt()` reads a `preemption_monitor` of two counters,
  "We preempt when head != tail", a layout that "happens to match the Linux aio completion ring, so we
  can have the kernel preempt a task by queuing a completion event to an io_context"; two relaxed loads
  and a compare, "We aren't reading anything from the ring, so we don't need any barriers".
- `src/core/reactor.cc`: the task-quota timer is a `timerfd` (`CLOCK_MONOTONIC`); stall reports come from
  a `timer_create(CLOCK_THREAD_CPUTIME_ID, …)` timer signalling the thread (`SIGEV_THREAD_ID`), so no task
  run reads the thread's clock; `task_queue_group::account_runtime` counts a task that ran past
  `2 * task_quota` as a quota violation.
- `doc/tutorial.md`, "Preemption and Task Quota": "The task quota controls how long tasks may run before
  yielding to the event loop and is set to 500 µs by default"; preemption "can occur only at specific
  preemption points".

**Tokio**, "Reducing tail latencies with automatic cooperative task yielding" (2020-04-01,
https://tokio.rs/blog/2020-04-preemption): "each Tokio task has an operation budget. This budget is
reset when the scheduler switches to the task"; "a budget of 128 operations per 'tick'"; "The number 128
was picked mostly because it felt good" — a count not derived, which hyper-rt's quantum is.

## 3. What follows for the loop

1. **Yield only where the shard is wanted** (IX's batching only under congestion; Seastar's and
   Concord's one-line test): `ShardContext::wanted` and `futures::maybe_yield`, and an input read that
   touches no inbox the same signals say is empty (docs/runtime.md §3.10).
2. **Price a switch in lines.** In a consumer the runtime runs between bursts of the consumer's work and
   pays per line it touches (§1: 375–541 ns where its bench pays 15), so what a switch reads is packed
   into the desk's first two lines and the loop's first (§3.10).
3. **Judge a long poll against the cooperation it asks for.** A task that asks once a quantum and yields
   when told to holds a wanted shard less than a quantum and an operation; past twice the quantum since
   its last go-on is the contract broken (Seastar's own violation threshold), and only such a poll is
   judged (§3.11).
4. **Open:** the boot probe's quantum for a shard too busy to park (§1's swings), and contention among
   many threads waking one shard (rt-2x put the bitmap's pending flag, summary and first words on one
   line, best for one waker); each is measured before it is changed.
