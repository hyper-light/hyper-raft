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
4. **Measured, and left as they are:** the boot probe's quantum for a shard too busy to park (§1's
   swings), and many threads waking one shard (rt-2x put the bitmap's pending flag, summary and first
   words on one line); §4.

## 4. The two left open, measured

**A busy shard's quantum.** Across 55 of the 100M runs (rounds 201–234: mantle main's runtime, 767627f's,
rt-2x's and rt-pending's), the boot probe's quantum ran from 9.8 to 424 µs. The fill's rate and put p99
show no dependence on it beyond the runs' own spread: 767627f's two runs at 188 and 424 µs filled at
3.26 M and 3.44 M puts a second with p99s of 0.67 and 0.59 µs, where its runs at 10–22 µs filled at
2.61–3.95 M with p99s of 0.67–1.60 µs. §1's fixed-quantum sweep moved the fill 2.4 % between 15 and
45 µs, inside one run's spread. Nothing here to refine.

**Many threads waking one shard.** P threads each wake a task of their own on one shard, a million wakes
a round, five rounds; the figure is the median round's, two runs each, interleaved (`xwakemp`, Apple
M5 Max, the host busy with gates and the Linux VM's benchmarks). The measure is wakes delivered, the woken
tasks' polls, not the wakers' calls: a shard that drains sooner leaves a waker's next call more to do, so
a waker's nanoseconds a call could rise with rt-pending (73 against 22 with one waker, in one run) while
the wakes it delivered rose more.

| wakers | wakes delivered a second, 767627f → rt-pending | a waker's CPU a delivered wake |
|---|---|---|
| 1 | 2.2–3.4 M → 3.7–6.0 M | 294–460 → 166–270 ns |
| 2 | 3.5–3.8 M → 6.0–6.3 M | 522–567 → 318–325 ns |
| 4 | 3.9–5.1 M → 5.3–7.2 M | 784–948 → 546–728 ns |
| 8 | 5.4 M → 6.4–6.8 M | 1,437–1,478 → 1,142–1,188 ns |

The one line costs the wakers nothing at these counts; from two wakers on, the shard's drain, 6–7 M wakes
a second, is the limit. Mantle's cross-thread wakes, the issuer's completions and the workers' results,
come at 10⁴–10⁵ a second.

**Where a consumer's switch time goes.** With every operation through the range's channel (mantle's
`--no-inline`, a 10M fill, `perf` on the Linux VM), the shard's time is the consumer's: its serve loop
reads the OS's clock (`std::time::Instant`, 7.8 % of the process's samples, under `ranges::serve`) and
does its per-turn bookkeeping once an operation, while the loop's own frames, its polls, wakes and
channel hops, come to about 4 %.
