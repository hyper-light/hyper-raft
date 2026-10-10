# Timing: failure detection and elections from measurement

How hyper-raft decides when a leader is gone and how long an election waits, on one laptop and
on a fleet with millions of groups, from what it measures and nothing it picks. Sources and what
each establishes are in `docs/research/timing.md`.

## 1. What was wrong

The core inherited raft-rs's shape: every group ticks, every leader heartbeats every follower
every `heartbeat_tick`, every follower runs an election timer of `election_tick` ticks, and the
tick is a duration the application chooses. Each of those is a constant someone picks. The E2E
harness made it worse by computing a tick and budgets for one test on one loopback interface.
None of it survives a fleet: links differ by orders of magnitude, conditions change by the
minute, and per-group heartbeats grow with the number of groups, not the number of machines.

`hyper-timing` (from focal-timing and slates' timing law) already measures each path and derives
a group's timing from it, and is the place this design lands: its `PathEstimate` trait, the
durable-acknowledgement tail it adds to a path's round trip (mantle audit §11.7), its election
timer counted in the owner's periods so a starved node waits instead of campaigning, its election
priority and its window budget (`REPAIR_ROUND_TRIPS`, a protocol fact) stay. Its picked numbers
went in L-1 (§2.3, "The election law"): `ELECTION_MARGIN = 10` (Raft's "order of magnitude" read
as a multiplier, for the base and the span alike, and a cap on a round's extensions),
`PATH_WINDOW = 16`, and `GRANULARITY_NS`, RFC 9002's 1 ms, which is QUIC's assumption and not this
machine's timer.

## 2. The design

### 2.1 Failure detection belongs to node pairs, not groups

One detector per ordered pair of nodes that share a group, shared by every group they share
(CockroachDB's coalesced heartbeats and store liveness; TiKV's hibernated regions). A group runs
no election timer and its leader sends no heartbeat of its own: a follower campaigns when its
detector of the leader's node suspects it, and a leader steps down when its detectors suspect a
majority of a group's followers (check-quorum). The cost is per peer, bounded by placement, and a
group with nothing to replicate sends nothing.

A heartbeat is sent only after the sender's log device took a write and flush within the period
(CockroachDB's store liveness): a node whose disk stalls stops being trusted, as a crashed one
does.

### 2.2 Each detector is NFD-E, configured to minimize unavailability

Each detector is Chen, Toueg and Aguilera's NFD-E: heartbeats every `η`; the receiver trusts the
sender while a heartbeat is fresh, freshness point `τ_i = EA_i + α` from the expected arrival
time `EA_i`, estimated from recent arrivals. A crash is detected within `E(D) + α + η`
(Theorem 4), whatever delays and losses do.

**Where it errs.** After heartbeat `h`, the latest taken, the freshness point in force is
`τ_{h+1} = EA_{h+1} + α`, and the next heartbeat taken ends the gap it judges. The receiver
suspects a live sender exactly when that heartbeat comes past `τ_{h+1}`: each heartbeat taken is
the one mistake its predecessor's freshness point can make, and its lateness
`ℓ = A − EA_{h+1}` past the expected arrival the point was set from (with any move of the
sender's interval expected, §2.8) is the one quantity the detector compares with `α`. A slot the
sender skipped while it was stalled, or one the network lost, is no heartbeat taken: it lengthens
the next one's lateness and is nothing else. A sender's stall is delay, as Chen et al.'s model has
it (a process that is slow is not crashed, and its messages are delayed), not a run of losses.

**The bound, per arrival.** For latenesses of mean `μ` and variance `V`, Cantelli's one-sided
inequality bounds `Pr(ℓ − μ ≥ x) ≤ V/(V + x²)` for any distribution with those moments, and a
lateness past every one the estimate has seen, which no variance can know of, is counted at its
distribution-free chance `u = 1/(m + 1)` over the estimate's `m` independent arrivals (Rényi's
record law; §2.6, item 3):

    β(α) = u + (1 − u)·V/(V + (α − μ)²)   past the mean, and 1 at or below it.

One factor, whatever the heartbeats the margin holds: it assumes no independence between
heartbeats, so any margin is admitted and no correlation time floors the interval, and it has no
loss term, a lost heartbeat being the next one's lateness. The bound on detection still holds for a
sender that stops for good: its last heartbeat's freshness point passes `E(D) + α + η` after its
schedule, whatever the latenesses were.

Theorem 7 bounded the mistakes through a product over the heartbeats still fresh at a freshness
point, each factor `(V + p_L·x_j²)/(V + x_j²)`, `x_j = α − jη`, with the loss `p_L`: the detector
judged at every freshness point, its heartbeats lost independently of their delays. On a stream
from a host that stalls, two things defeat it (§2.9, "On real detectors", and `docs/benchmarks.md`,
"The detector model, at its causes"). A stall is one late sender, a burst of consecutive slots it
skipped, and counted as independent losses it fed `p_L` at 23–45 % on Linux and 6–8 % on macOS, of
which 1–4 % was lost on the wire; every factor is at least `p_L`, so margin bought nothing. And the
product's independence, refuted on every trace below the correlation time `T_c` (§2.6, item 6),
held the margin to one heartbeat, `α < η − G`, a cap that left no margin at all where `G ≥ η`. Of
3,398 configured Linux pairs, 268 got `α = 0`, a detector that suspects at every expected arrival,
and 1,475 a `U` above one; on macOS 1 and 312 of 608. Each cause holds its share of them: with the
wire's loss in place of the skipped slots and the cap kept, `U`'s mistake term stayed at one or more
in 1,294 of the 1,475 Linux pairs (283 of 312 on macOS); without the cap, a margin brought it below
one in 534 of them with the skipped slots as losses (253 on macOS) and in 1,377 with the wire's loss
(306). The per-arrival bound has neither: each freshness point is judged by the one heartbeat that
ends its gap. SWIM's probe detector (§2.7) keeps Theorem 7's product for its probes,
each answered or not within its period (`qos::detector_at`).

**What it minimizes.** Chen et al. configure `η` and `α` from requirements an application states.
hyper-raft has no such application, and a requirement it chose would be a constant it picked.
Instead it chooses `η` and `α` to minimize what the requirements stand for, the time a group cannot
commit:

    U(η, α) = (E(D) + α + η + T_E) / MTBF  +  T_E · β(α) / η

The first term is a crash: detected within `E(D) + α + η`, then an election of `T_E`, once per
`MTBF` of the leader's node. The second is a false suspicion: an election for nothing, at most once
a heartbeat taken, so at most once every `η`, each with chance at most `β(α)`. Every input is
measured: the latenesses from the link's heartbeats (§2.8, over its history, §3 item 11),
`T_E` from elections (§2.3), `MTBF` from the membership's failure history. `η` has floors, each
measured (§2.6): the timer granularity `G` (§2.4) and the sender's stability, `η > E[flush] + G`.
At an interval, past the mean `U` is `y/MTBF + c·V/(V + y²)` and a constant, `y = α − μ`,
`c = T_E(1 − u)/η`: falling while `c·2Vy/(V + y²)²`, which rises to its peak at `y = √(V/3)` and
falls after, is past `1/MTBF`, and rising again from where it falls back, one valley, whose floor
is found by bisection on the falling side; the better of it and `α = 0` is the margin. The interval
is searched above its floors by golden section. The configuration is renewed as its estimates renew
(§2.8), as Chen et al.'s adaptive scheme reconfigures.

**A `U` of one or more is no configuration.** By Little's law (Little 1961: the mean number in a
system is its arrival rate times the mean time in it, for any system in a steady state), `U` is the
mean number of elections in progress, crash-caused and mistaken, and so bounds from above the share
of time one is. At one or more it bounds nothing: the evidence says the group may never commit, and
a detector configured there promises nothing at all. So the configurator admits a detector only
below one. The detector in force is the best at the interval the link is at where its `U` is below
one; where it is not, no margin at that interval is a configuration, and the detector in force is
the best over every interval the floors allow, whose interval the receiver asks of the sender and
expects at once (`LinkEstimator::expect_interval`); where even that one's `U` is one or more, there
is none (`Refusal::Unavailable`): the detector in force stays, and the configurator is asked again
once the estimate has renewed. A pair with no detector in force yet is judged by its node's
evidence meanwhile (§2.8, "Judged before its own evidence").

An availability target, where an operator states one, is a check on the result (`U ≤ 1 − A`),
reported when it fails, never a parameter.

### 2.3 Elections

A suspicion starts an election after a randomized delay, so that the followers that suspected
together do not all campaign at once. Ongaro gives the split-vote probability for a
randomization range `W`, `s` servers available of `n` and one-way latency `l`:
`Pr(split) = Pr(D_{c,s} < l)` with `c = s − ⌊n/2⌋ + 1` (dissertation §9.2), and elections needed
are geometric (§9.3). The expected time to a leader is therefore

    T_E(W) = (E[first timeout](W) + vote rounds) / (1 − Pr(split)(W))

which falls and then rises in `W`: a narrow range splits votes, a wide one waits longer for the
first candidate. hyper-raft takes the `W` that minimizes it, with `l` and the vote round (a
broadcast: round trip and flush) measured per link. Ongaro's "10–20× the one-way latency" is a
rule of thumb from the same formula; L-1 computes the optimum on his assumptions and checks it
against his figures before any measured link relies on it.

**The election law** (`crates/hyper-timing/src/election.rs`) is the detector and the span, one
design, with nothing picked between them:
- **The ballot.** From a voter's measured paths to the others: `l` is half the slowest path's mean
  round trip (a round trip is what one clock measures; half of it is a symmetric path's one-way
  delay, the half NTP's offset takes, RFC 5905 §8) plus the mean flush, since a candidate's request
  leaves only once its own term and vote are durable and a member that turns to campaign in that
  time splits the vote as surely as one within the path's delay (L-2, §2.9), and the vote round is the candidate's quorum,
  the `⌊n/2⌋`-th smallest mean round trip, plus the mean flush a voter makes its vote durable in (Raft's
  `votedFor` is persistent, Figure 2; `Flushes`). Means, because `T_E` is an expectation. The span
  is chosen for `s = n − 1`, the voters left when the leader's node crashed, which is the case the
  detector exists for; a group of two, whose survivor is no majority, for `s = n`. No ballot before
  a quorum's paths are measured, and none for a sole voter.
- **The span** `W` is `election_span` on the ballot, searched to within `G`, from the narrowest
  span to `(s + 1) · T_E(W₀)` of a span `W₀` with a finite time (the narrowest, or `(s + 1)` times
  it where every attempt at the narrowest splits), past which `T_E(W) ≥ W / (s + 1)` costs more
  than `W₀`; a fixed widening of `(s + 1)²` there cut the best span off once the vote round passed
  about ten latencies, which the ballot's latency, holding the flush, keeps it below. Its `T_E` is the
  election cost the detector's configurator is charged (`Costs::election`), so `η` and `α` are
  chosen knowing what an election costs this group, and the election waits only as long as that
  choice assumed.
- **The base** is when the follower's detector of the leader's node suspects it: NFD-E trusts the
  sender until the next heartbeat's freshness point, `η + α` past the latest heartbeat (exactly, on
  a link whose delay does not vary: `the_base_lapses_where_the_link_suspects`), for the detector
  in force on the link (`Configuration::current`). A follower silent that long has lost its leader,
  as its detector says; a leader judges its quorum on the same cadence. A crash is detected within
  `E(D) + α + η`.
- **Periods.** A core that counts its owner's periods takes the base and the span rounded up to
  whole periods, so its base never ends before the detector would suspect, and a starved owner
  waits instead of campaigning. An owner whose period is longer than `W` cannot resolve the span
  in periods (one period of jitter; the owners' unsynchronized phases spread their campaigns), so
  the same draw is given in time, uniform on `[0, W)` (`ElectionTiming::delay`), for the core of
  L-2 that campaigns on its detector's suspicion and waits to the owner's `G`. A core whose tick
  counts are fixed when it opens draws its timeout from `[election_tick, 2·election_tick)` ticks,
  one count for both base and span, so its period covers the longer of the two (`TickPace`).
- **The granularity** under every tail (`PathRtt`, `ExchangeRtt`: RFC 9002 §6.2.1's
  `max(4·rttvar, kGranularity)`, whose Appendix A.2 defines `kGranularity` as "Timer granularity.
  This is a system-dependent value" and §6.1.2 recommends 1 ms for it) is the owner's measured
  `G`, passed in. hyper-transport's `exchange_tail` takes it from its caller, and its receive
  windows are tuned under it (`Endpoint::set_granularity`, which hyper-tokio's driver feeds from its
  timer; RFC 9002's 1 ms until the first report).
- **A round's extensions** were capped at `ELECTION_MARGIN`, "so a round never outlasts the
  election timeout it would displace a leader over". The ceiling already bounds a round, so a round
  with that purpose takes the base as its ceiling and the cap goes.
- **What stays.** `REPAIR_ROUND_TRIPS = 2` is a protocol fact, not a tunable: Raft's consistency
  check (§5.3) makes a follower refuse the batch after a lost one rather than buffer it, so the
  repair is that refusal reaching the leader and the resend reaching the follower and its answer the
  leader, and no shorter or longer one exists. The window a leader keeps in flight is what the path
  carries over that repair (`inflight_window`, mantle note 32 R16): `window_budget` is it in whole
  batches for a sender that sends a batch a period, and twice the transport's congestion window for
  one the transport clocks (`docs/raft.md` §3.2). RFC 9002's gains and its `4·rttvar` are the standard's constants, cited.

On the comparison workload (five voters, four WAN paths of 40–58 ms, a 10 ms heartbeat, macOS's
`G` and `T_c`), the law before L-1 gave a base of `10 × tail` and a span of `10 × spread`; the law
now gives the base from the detector and the span from the ballot (`docs/benchmarks.md`,
"hyper-timing against focal-timing and slates' timing", has the values and the costs).

### 2.4 The local clock and the local machine

- **Granularity.** A timed wait ends on the OS's timer, not when asked. `G` is the mean lateness
  of the detector's own waits, measured: the mean because the waits feed queues (the sender's
  schedule, the detector's checks) whose stability and expected delay depend on it (§2.6). It
  floors `η`, `α` and the estimates, as RFC 6298 floors its variance term by `G`. A wait counts
  when the owner began it before its deadline and it ended at or past it, whatever ended it: the
  deadline, or a message or completion that came after it, the stream then polled that late past
  its wake while its owner waited for it (`hyper_liveness::Liveness::on_wait`). A wait that ended
  before its deadline reached nothing, and a wake the owner came to late because its one thread
  was in its own write is the write's lateness, which the sender's floor counts as `E[flush]`, not
  the timer's. Taken from every poll past a wake, `G` was the owners' own stalls: 32–65 ms on macOS
  and 15–76 ms on Linux through the stall `stalled-devices` orders, and 4.8–7.4 s in a run whose
  timer was late by milliseconds (§2.9). Counted only where the deadline ended them, as the trace
  recorder's are (nothing else wakes it), the waits of an owner woken past its wakes by messages
  before its timer fired counted nothing: hyper-durable-e2e's members, asked for reports every few
  hundred microseconds on Linux's 1 ms ticks, went without `G` and refused every heartbeat as
  unmeasured, a group that never formed or a stalled member a peer never suspected (§2.9). `G` is
  never stated below the resolution `r` of the clock the waits are read on, which the owner states:
  a wait read exactly on time was late by less than `r`, and a mean below `r` is lateness the clock
  cannot tell from none. As a plain mean, an owner whose every wait read on time (a simulation, a
  busy-polling owner, a coarse counter) had `G` of zero, which floors nothing, and its detectors
  configured nothing and suspected no one: slates' harness, which wakes its detectors when they
  ask. hyper-tokio's `Clock::resolution` states `r` for the host's monotonic clock: a tick of
  `mach_absolute_time` on macOS (`mach_timebase_info`, 125/3 ns on Apple silicon), a count of
  `QueryPerformanceCounter` on Windows (a second over `QueryPerformanceFrequency`), and on Linux,
  which states no step for its readings (`clock_getres(2)` reports its timers' resolution, a jiffy
  without high-resolution timers, which a wait's lateness does not stay above), a nanosecond, the
  readings' unit. A stop
  of the process or a
  frozen host inside a wait does count: the OS ran nothing then, and the queues the waits feed saw
  the delay; one such wait weighs `1/n` of `n`. Measured on
  2026-10-01 (`docs/benchmarks.md`, "Heartbeat traces"):
  - **macOS 26.4.1** (Apple M5 Max). A user thread's wait ends late by half its length plus about
    10 µs: 1 ms waits 0.51 ms late at the median, 10 ms waits 2.6–5 ms. This is XNU's timer
    coalescing, a leeway of `min(wait >> shift, cap)` (`timer_call_slop`), here with shift 1;
    `taskpolicy -l 0` did not change it. `G` on macOS is therefore not a constant but half the
    wait: 45 µs for the 40 µs waits of a 100 µs heartbeat, 1.6 ms for the 4 ms waits of a 10 ms
    one. Real-time and critical-urgency timers get no leeway (`arm_timer.c`: shift 0, cap 0); the
    detector's thread has to ask for one (§3, item 8).
  - **Linux in Docker Desktop's VM** (linuxkit 6.12.76, aarch64). The kernel is built without
    high-resolution timers (`CONFIG_HZ=1000`, `CONFIG_HIGH_RES_TIMERS` unset; `/proc/timer_list`
    reports a 1 ms resolution), so every wait ends on the next 1 ms tick: `G` = 0.90–0.97 ms. A
    Linux host with high-resolution timers, where the 50 µs default slack bounds the lateness, was
    not available and is not measured.
  - **Windows**: not measured (§3, items 5 and 8). Its waits end on the 15.625 ms clock interrupt
    unless the process asks for finer (`timeBeginPeriod`, per process since Windows 10 2004).
- **Local health.** A detector that wakes late must not blame its peers (Lifeguard). Arrivals are
  stamped by the kernel when the datagram is received (`SO_TIMESTAMPNS` on Linux,
  `SO_TIMESTAMP_MONOTONIC` on macOS, which XNU stamps with `mach_absolute_time()` as UDP input
  queues the datagram), so a heartbeat that arrived before its freshness point is fresh however
  late the process read it; only the send side of a slow node looks slow, which is true. The gap
  the stamp removes was measured: between the kernel's stamp and the process's read, macOS
  p99.9 3.8–4.7 ms and up to 101 ms (100 µs heartbeats, load 17–32), p99.9 73 ms and up to
  125 ms at load 41; Linux p99.9 0.09–0.15 ms, up to 22 ms.

### 2.5 What a test does

A test runs this mechanism and asserts what it promises: every suspicion of a node that was
killed comes within the detection bound the configurator computed for that link; elections end;
nothing answered is lost. It waits on facts — a leader, a term, a commit — and fails when the
mechanism's own stated bound passes without them. It computes no tick and no budget of its own.

### 2.6 What the traces settle

`crates/hyper-timing-trace` records two processes exchanging sequence-numbered heartbeats over UDP
on loopback: the sender's schedule `σ_i = iη`, when it woke and sent, the kernel's receive stamp
and when the receiver read each, and both processes' timed waits (`docs/benchmarks.md`,
"Heartbeat traces", has the runs, the commands and every table). The delay the detector sees is
NFD-E's, `D_i = A_i − σ_i` from the kernel's stamp: the sender's timer lateness, its flush, the
network and the kernel. Seven runs: macOS at 100 µs heartbeats (twice, the second under a parallel
cargo build) and at 50 µs (load 41), macOS with a write and `F_FULLFSYNC` before each heartbeat
at 10 ms (twice, the second loaded), and Linux in Docker at 2 ms with and without `fdatasync`;
five to ten minutes each, 30,000 to 6,000,000 heartbeats, no loss.

**The recording interval and the stability floor.** A sender that wakes and then, in the flush
variant, writes and flushes before each heartbeat is a single server fed every `η`; it reaches a
stationary delay only while its mean service time is below `η` (Lindley 1952). The recorder ran
at the finest interval on the 1-2-5 grid whose measured mean service (timer lateness, plus the
mean write and flush) is below it: 100 µs on macOS (mean lateness 49 µs; at 50 µs and load 41
the mean was 111 µs and 66 % of heartbeats left behind schedule, the delay a random walk with
lag-1 autocorrelation 0.999), 10 ms with `F_FULLFSYNC` (mean write and flush 5.9 ms in the
sweep), 2 ms on Linux (1 ms ticks) with or without `fdatasync` (0.09 ms). The same inequality is
a floor on the detector's `η`: **`η > E[flush] + G`**, measured per node.

**Item 7: mean and variance, not median and MAD, feed Theorem 7.** Cantelli's inequality, and so
Theorem 7, holds for any distribution with the stated mean and variance; it says nothing for a
spread estimated from the body. The traces' delays are heavy-tailed in every run that touches a disk
or a loaded scheduler: `sd / (1.4826·MAD)` is 24 (macOS 100 µs), 24 (loaded), 49 (50 µs at load 41),
13 and 17 (macOS flush, plain and loaded), 22 (Linux flush), and 1.8 only on the idle Linux link;
the mean is 1.2–1.9 times the median, 4 times at load 41. Replaying the configured NFD-E over each
trace (the detector's own rule: at `t ∈ [τ_i, τ_{i+1})` trust iff some `m_j`, `j ≥ i`, has arrived)
and comparing its mistake rate with Theorem 7's bound: every detector configured from mean and
variance kept its bound, in every run; detectors configured from median and MAD broke theirs in
every run with a tail, making mistakes 26 (macOS flush) to 880 (macOS at 50 µs) times more often
than their bound promised (the table in `docs/benchmarks.md`). The late answers are delay the
detector must cover in its accounting: a suspicion during a stall costs an election whether or not
the stall is later called a failure, so it belongs in `U`'s second term, whose bound needs the true
variance. `PathRtt`'s median and MAD stay where the purpose is the opposite, ordering voters (the
election priority), where one stall must not reorder a group; the base and the span now come from
the detector and from means (§2.3, "The election law"). Its window is the shortest whose median one
stall cannot move: a stall shorter than `T_c` (item 6) makes at most `k = max(1, ⌈T_c/p⌉)`
consecutive probes at interval `p` late, and the median of `2k + 1` outvotes `k` (its breakdown
point is one half: Hampel 1971; Rousseeuw and Croux 1993), so three at `p ≥ T_c` and eleven for
macOS's 50 ms at a 10 ms probe. A path that moved is followed in `k + 1` probes, about one
correlation time, as fast as any estimate one stall cannot move.

Its samples are fresh and of one endpoint (focal's audit F44: an estimate with no age, kept across
an endpoint's change under one id, and a sparsely probed path whose window is hours old). Each
sample carries when it came and the generation of the peer's endpoint it measured. A sample of a
new generation starts the window again, as QUIC resets its round-trip estimator on a path it has
validated (RFC 9000 §9.4). A sample older than the window's span at its probe interval,
`(2k + 1)·p`, about two correlation times, the time the window is derived to cover, is dropped as
newer ones come or when the owner asks: it measures a path that may have moved further than the
window can see, cached path state that "could also become invalid over time" (RFC 9040 §8.1). No
constant enters: the span is the window's own. A path probed more sparsely than its interval holds
fewer samples, and its readers see how many and the oldest's age. With fewer than `k + 1` held, one
stall can move its median, so an owner asks a quorum path in that state for fresh probes first. A
path whose samples have all aged out contributes nothing to the election law, as one never probed.

**Item 6: the correlation time, and the rule it gives.** Theorem 7's `β` is a product over the
heartbeats inside the margin, each factor a Cantelli bound: it takes them as independent. In the
traces they are not: a stall delays every heartbeat it covers. The probability that two
heartbeats `k` apart both exceed the 99.9th percentile is 250–1,000 times the square of one's at
`k = 1` in every run, and the extremal index of the exceedances (Ferro and Segers 2003) puts the
mean cluster at 2 (idle Linux) to 1,100 heartbeats. The correlation time `T_c` is measured as the
least spacing from which the replay no longer refutes Theorem 7's product at any margin holding
two or more heartbeats (the trace's 99th to 99.99th percentiles, and margins of 2, 3, 5, 10 and 20
heartbeats; refuted when the 95 % lower limit of the replayed mistake rate exceeds `β`), taken
as the next spacing on the 1-2-5 grid past the last refuted one. Measured: 20 ms on idle Linux,
50 ms on macOS (plain and loaded), 250 ms at 50 µs and load 41, and 200 ms with a flush, macOS
and Linux alike (100 ms refuted; their flushes stall up to 117 ms and 139 ms). Each `T_c` is the
spacing past the longest stall the run saw; a longer run can only raise it. The rule:
**the heartbeats inside a margin are at least `T_c` apart: `η ≥ T_c` whenever `α ≥ η`** (a
margin under one interval holds one heartbeat, a single Cantelli bound, which needs no
independence). The configurator searches with the floors `G` and `E[flush] + G`, and searches
again with `T_c` added when its answer has `α ≥ η` and `η < T_c`. Every configured detector
below kept its bound on replay with this rule; without it, the bound failed at the trace's own
interval on every run. The rule is the product's, and stands for SWIM's probes (§2.7); the
node-pair stream bounds each freshness point by the one heartbeat that ends its gap (§2.2), which
asks no independence, so no `T_c` floors its interval.

**Item 2: NFD-E's window is `n = min(n_G, n_A)` at the configured interval.** The window trades
the expected arrival's error against how fast it follows the link. Two measurements bound it:
- `n_G = ⌈τ_int · V(D) / G²⌉`: the window whose estimate of the mean is within `G`, the timer's
  resolution, which nothing finer can observe. `τ_int` is the integrated autocorrelation time
  (Madras and Sokal 1988), which turns `n` correlated samples into `n / τ_int` independent ones.
- `n_A`: the window at which the Allan deviation of the delay's window means stops falling (Allan
  1966; the first window within the statistical tolerance `1/√(2(K−1))` of the minimum). Past it, a
  longer window averages over a link that has moved. Both are computed on the heartbeats at the
  configured spacing, not the trace's. Measured: 128 on macOS (`n_A` binds; `n_G` 3,219), 256 under
  the cargo build (`n_A`; `n_G` 646), 79 and 64 with `F_FULLFSYNC` (`n_G` 79 under `n_A` 128; `n_A`
  64 under `n_G` 155), 77 with Linux's `fdatasync` (`n_G` under `n_A` 128), and 1 on idle Linux
  (`n_G`: `V(D)` is below `G²`, so one heartbeat already places the mean within the 1 ms tick): 6.4,
  12.8, 15.8, 12.8, 15.4 s and 20 ms of history. On the fine-grained traces the Allan deviation
  rises from the first window to about 50–300 ms, the stall time scale, and falls again past a
  second (macOS 100 µs: 64 µs at two heartbeats, 300 µs at 51 ms, 109 µs at 1.6 s), so a window that
  straddles a stall averages it in; at the configured spacing the stalls are single heartbeats and
  the curve falls.

The estimator (`crates/hyper-timing/src/link.rs`, `LinkEstimator`) computes both online, in bounded
memory and bounded work a heartbeat, and the trace analyser now takes its window from it (a move of
`G` alone, which comes with nearly every heartbeat a detector takes, places the window from the
`n_A` the levels last gave, since only an offset moves them and `G` reaches `n_A` only through the
drift bound's power of two):
- `n_A` from Allan levels at windows `1, 2, 4, …`, each holding its unfinished window's sum and the
  running sum of squared differences of consecutive window means: one step a level a heartbeat. A
  level enters the comparison at seven windows, where its relative uncertainty `1/√(2(K−1))` is
  finer than the fall `1 − 1/√2` a doubling makes for white noise; the analyser had used 16.
- `τ_int` from the same levels: the variance of an `m`-mean is `V·τ_int/m` once `m` is long against
  the correlation (Sokal 1997, §3), and the Allan variance of non-overlapping means is that variance
  less the neighbouring means' covariance, which vanishes there; so `τ̂(m) = m·σ²_A(m)/V` at the
  shortest `m ≥ 6·τ̂(m)`, Madras and Sokal's self-consistent window. The analyser had summed the
  autocorrelation by FFT. The Allan form is one a drift does not inflate.
- `V` is the variance of the prediction errors `A_i − EA_i` over the link's history: what the
  freshness test compares with `α`, `V(D)(1 + 1/n)` for independent delays, and free of the two
  clocks' offset and drift, which `EA` follows and a plain variance of `A_i − iη` would count as
  delay (15 ppm is 54 ms an hour).
- **The window's bound.** `EA` is a mean centred `(n − 1)/2` heartbeats back predicting `(n + 1)/2`
  ahead of it; two clocks within RFC 5905's frequency tolerance `PHI = 15 ppm` (§7.2) drift apart by
  up to `2·PHI·η` a heartbeat, so `EA` lags a drifting pair by up to `PHI·η·(n + 1)`. A lag past `G`
  is a link that moved by more than the timer can observe, which `n_A` exists to stop, so
  `n + 1 ≤ G / (PHI·η)`. With `η ≥ G` (the configurator's floor) no window passes `1/PHI − 1 =
  66,665`. Each link's ring holds `G/(PHI·η) − 1` eight-byte prefix sums, sized when it is built:
  3,333 slots (27 KiB) for Linux's 1 ms tick at 20 ms, 33,333 (267 KiB) for macOS's half-the-wait
  `G` at 50 ms, 533 KiB at most. A pair drifting faster than `PHI` shows it in the Allan deviation,
  and `n_A` binds first. The bound is not loose on the traces: with the receiver's `G` of 45 µs
  measured at 100 µs waits, a 50 ms interval allows 59, under the 128 recorded above for a shared
  clock.
On a 120 s macOS trace at 100 µs recorded with the estimator in place (2026-10-01, load 32–37,
`docs/benchmarks.md`, "The detector's estimator"), the window at 50 ms is 64: `n_A` 64 under
`n_G` 73, which is the drift bound.

**Item 3, in part: first estimates.**
- `E(D)`: NFD-E's estimator over the heartbeats received so far, its window filling to `n`.
- `p_L`: the Jeffreys posterior mean `(k + ½)/(m + 1)` after `k` losses in `m` heartbeats
  (Jeffreys 1946; Brown, Cai and DasGupta 2001): `1/(2(m+1))` before any loss, never zero, so
  Theorem 7's factors stay below one only by what the link has shown. No run lost a heartbeat;
  the estimates were 1.7·10⁻⁷ to 1.7·10⁻⁵.
- `MTBF`: the Jeffreys posterior for a Poisson rate, mean `(k + ½)/T` over the fleet's node
  exposure `T` with `k` failures, so `MTBF = 2T` before the first failure: a fleet that has run
  little is treated as failing as often as its exposure cannot exclude, and detection is fast
  until history says otherwise.
- `V(D)`, in part: the first heartbeats underestimate it badly. Over consecutive blocks of `m`
  heartbeats, the median block's deviation is 1–18 % of the run's on every run with a tail, for `m`
  from 2 to 2,048 (78–83 % on idle Linux from `m = 8`): the variance lives in stalls a short window
  has not yet seen. A link's `V(D)` is therefore the variance over its whole history, not over the
  window. What a short history has not seen is counted, not guessed: for exchangeable delays the
  chance that the next is later than all `m` before it is exactly `1/(m + 1)` whatever their
  distribution (the first record indicator; Rényi 1962), with `m = count / τ_int` independent
  heartbeats. A heartbeat that late is to Theorem 7 as good as lost:
  `Pr(lost or later than x) ≤ p + (1 − p)·V/(V + x²)`, `p = 1 − (1 − p_L)(1 − 1/(m + 1))`, which is
  Theorem 7's factor with `p` for `p_L`. The estimator feeds a probe detector `p` as the loss
  (`LinkEstimator::behaviour`, SWIM's, §2.7), so a young link's margin cannot promise better than
  its history excludes. The node-pair stream's configurator takes the same count over its
  latenesses, `u = 1/(m + 1)` beside Cantelli's factor on the lateness (§2.2), with no loss: a lost
  heartbeat is the next one's lateness. It refuses to configure until it has the evidence `m`
  needs: two latenesses (a variance) and a measured `τ_int` (an Allan level holding Madras and
  Sokal's window), a few dozen heartbeats at the correlation-time floor. What stays open is §3,
  item 3.

**The configurator on the measured inputs.** With `p_L`, `E(D)` and `V(D)` from each trace, the
floors above, `T_E` from `election_span` (three voters, two up, one-way latency the measured mean
send-to-read time, vote round twice that plus the mean flush) and an MTBF of 30 days:

| run (load) | `η` | `α` | detection bound | `T_MR` bound | `U` | window | `T_E` (3, 2 up) | replay |
|---|---|---|---|---|---|---|---|---|
| macOS 100 µs (17–32) | 50 ms | 65.8 ms | 115.9 ms | 8.4 d | 4.5·10⁻⁸ | 128 | 0.38 ms | holds |
| macOS 100 µs, cargo build (22–33) | 50 ms | 64.2 ms | 114.3 ms | 1.1 d | 5.0·10⁻⁸ | 256 | 0.56 ms | holds |
| macOS 50 µs (41–43) | 250 ms | 301.9 ms | 552.3 ms | 2.0 d | 2.2·10⁻⁷ | 64 | 1.85 ms | holds |
| macOS `F_FULLFSYNC`, 10 ms (15–30) | 200 ms | 451.0 ms | 658.0 ms | 49.3 d | 2.6·10⁻⁷ | 79 | 5.67 ms | holds |
| macOS `F_FULLFSYNC`, cargo build (32) | 200 ms | 437.6 ms | 645.1 ms | 2.6 d | 2.7·10⁻⁷ | 64 | 5.29 ms | holds |
| Linux 2 ms (4–6) | 20 ms | 27.9 ms | 48.9 ms | 2.6 d | 2.1·10⁻⁸ | 1 | 0.49 ms | holds |
| Linux `fdatasync`, 2 ms (4–6) | 200 ms | 248.8 ms | 450.2 ms | 1.7 d | 1.8·10⁻⁷ | 77 | 0.69 ms | holds |

"Replay" is the detector run over its own trace: no mistake at these margins on any run. With an
MTBF of an hour the margins are tighter and mistakes happen, still within the bound (macOS
100 µs: one mistake every 566 s replayed against a bound of 110 s; Linux `fdatasync`: 157 s
against 49 s). `T_E` is far below `η` everywhere, so detection, not the election, dominates a
crash's cost. The macOS rows would not shrink much with strict timers (item 8): their floor is
the stalls' correlation time, not the timer's `G`.

### 2.7 SWIM: each member's probes of a peer are that pair's NFD-E detector

`crates/hyper-swim/src/detector.rs`. SWIM (Das, Gupta and Motivala 2002) and Lifeguard (Dadgar,
Phillips and Currey 2018) stay: the probe, the indirect probe through relays, suspicion before
death, infection-style dissemination, the randomized round-robin, the buddy system and local health.
What goes is every number hyper-swim's caller picked: the protocol period (40 ms in the cluster
test), the acknowledgement timeout (half of it), the suspicion window in periods (3), its floor (2),
Lifeguard's confirmations `K` (2) and multiplier cap (2), the dissemination multiplier `λ` (3), the
relay fan-out (every member) and the gossip entries a message carries (8).

**What failed.** CI run 36960011977 (commit 63cb65d, windows-2025): "member 4 saw live member 3 as S
in period 7". A timed wait on Windows ends on the 15.625 ms clock interrupt (`timeBeginPeriod`), so
on a loaded runner a live member's acknowledgement missed a picked 20 ms window. A bigger period
would have moved the threshold, not removed it.

**The stream.** Member `p`'s probes of `q` and `q`'s acknowledgements are a heartbeat stream on
`p`'s own clock: probe `k` sent at `s_k`, answered at `A_k`. NFD-E's delay `A_k − σ_k` is then the
probe's round trip, with `q`'s handling and both hosts' stalls in it and no second clock: `E(D)` is
measured, not only `V(D)`. Each pair has a `LinkEstimator` fed the round trips as heartbeat `k`'s
offset on a schedule at the pair's interval, so everything §2.6 settled holds for it: mean and
variance (item 7), the window `min(n_G, n_A)` (item 2), the loss by Jeffreys and the unseen-delay
chance (item 3). SWIM probes a peer once a round of `m` periods in a fresh permutation (§4.3), so
the spacing varies; NFD-E's offset does not depend on it, and SWIM's own analysis needs only the
average period (§3.1). The pair's interval is the mean round, `η = m·T̄`, with `T̄` the member's
mean period.

**Each period's probe.** The acknowledgement of a probe sent at `s` is due at `s + μ + α`: `μ` the
window's mean round trip, `α` the margin `detector_at` chooses at `η`. Unanswered then, the relays
are asked, and their answers are due after the slowest relay's own span `μ + α` (the leg to it and
back) plus the target's (the relay's leg to the target, whose stalls are the target's own, §2.6). A
probe answered by neither suspects its target. The period ends at the direct deadline, or the
indirect one when that was needed: each period is what its probe needs, which replaces SWIM's "at
least three round trips" rule of thumb (§3.1) by the measured deadlines.

**The margin.** `α` minimizes `U` (§2.2) at the pair's interval, with:
- a false suspicion costing the time until it is refuted: the member's next probe of the peer
  carries it (the buddy system) and its answer carries the refutation, `η + μ`;
- the MTBF from the member's `Exposure` fold, the node time it watched and the deaths it learned,
  seeded with the fleet's history by the owner (Jeffreys' `2T` before the first failure);
- the floors `G` (§2.4, measured) and the sender's `E[flush] + G = G`, since an acknowledgement
  is not flushed;
- one probe in the margin (`α < η`): SWIM judges each probe on its own, and Theorem 7's single
  factor `β = (V + p·α²)/(V + α²)` needs no independence between probes.

`β` is the configured bound on a live peer missing a probe's deadline; the member reports `Σβ` over
its judged probes as the expected number of suspicions of a live peer it allows.

**Indirect probes** are not counted in that bound. Their paths share the target's host and its
stalls, which the traces found dominate, so they are not independent of the direct one, and the
bound holds whatever they add. Their number: the fewest relays whose paths together fail no more
often than the direct probe did. A relayed probe is two round trips, so with the configured loss `p`
one fails with `1 − (1 − p)²`, and `k` relays are asked where `(1 − (1 − p)²)^k ≤ p` (two for any
loss below 0.38, where `p(2 − p)² ≤ 1`), nearest the target in Vivaldi coordinates.

**The coordinates** (`crates/hyper-swim/src/coordinates.rs`, `docs/research/swim.md`) rank those
relays: the predicted round trip between two peers is the distance between their learned coordinates.
The engine is Dabek, Cox, Kaashoek and Morris's (SIGCOMM 2004) as the paper gives it: Fig. 3's update
whole, `w = e_i/(e_i + e_j)`, the sample's relative error `|‖x_i − x_j‖ − rtt|/rtt`, the error a
moving average of those with weight `c_e·w`, and the step `c_c·w` along the unit vector, in §5.4's
height vectors, two dimensions and a height, the height moving inside the step. `c_c = 0.25` is
§4.1's. `c_e`, which neither Dabek nor Ledlie, Gardner and Seltzer give, is `2/(m + 1)`: the estimate
tracks the node's error over its links, which SWIM samples once each a round of `m` probes, and that
weight's moving average has the variance of an `m`-sample mean. The engine it replaced (hyperscale's,
copied) folded the error in seconds into an estimate documented and floored as a relative one, so on a
LAN every node's error sat at its floor of 0.05, 50 ms, and the confidence weights did nothing; it had
eight dimensions against §5.2's finding that extra dimensions past three add nothing, a separate share
of each step for the height, an adjustment term with its smoothing and an uncommented ±1 s clamp, and a
gravity that multiplied every coordinate by 0.99 an update. There is no gravity: Ledlie's
`G = (‖x_i‖/ρ)² × u(x_i)` is a dimensionless magnitude applied as a displacement in milliseconds, the
unit its paper measured in, so no `ρ`, derived from a measured diameter or otherwise, makes it
unit-free; and drift, a rigid motion of every coordinate, leaves every prediction between coordinates
refreshed each round unchanged, which are the only predictions the engine makes. A sample's error is
floored at the nanosecond a round trip is measured in, which keeps the estimate positive, and a height
at it, which keeps the height positive (§5.4). Two fresh nodes at the origin separate along `u(0)`,
drawn at random from each node's own seeded stream. A peer's coordinate that is not a number, or has
a negative height or error, is not learned and moves nothing, and a sample that would leave the
coordinate infinite or not a number (a peer's point so far out that the distance overflows) is not
taken.

**Death.** A suspected peer is told by the member's next probe of it, which carries the suspicion
(Lifeguard's buddy system); if that probe too goes unanswered, the peer is condemned at the next
answer the member has from another member. Lifeguard found that "an episode of slow message
processing at a given member is likely to impact multiple of its interactions" (§IV) and counted
independent suspicions as evidence the local member processes messages in time; an answer from
another member is that evidence, measured on the member's own round trips. A member whose own
network has failed suspects everyone and condemns nobody. So a member of a two-member view never
condemns: the one other member is the suspect, there is no third to answer, and a lone survivor
cannot tell its peer's death from its own network's failure, the case local health exists for; each
side of a partition of two would otherwise condemn the other. An owner that must act on a death in
a pair takes the evidence from outside the detector, a quorum or its supervisor's word that the
process ended. Gossiped suspicions are hints: only a member's own probes condemn. A suspicion re-adopted at a newer incarnation keeps the probes that
already told the peer and its pending condemnation: they carried a suspicion and went unanswered all
the same, and resetting them had a crashed member told again from the start. Any answer from a peer,
even one too late to be measured, is evidence of its life when it arrives. A member with nobody
alive or suspected left in its view probes the members it holds dead, and each ping tells its target
so, as a suspicion is told: the member is likelier the one cut off than everyone else dead, and a
live target refutes in its answer. Without it, members that had condemned one another under a
one-CPU throttle, each holding the rest dead, never sent again (found in the Linux runs,
`docs/benchmarks.md`). The bound on condemning a live peer at a probe is the bound on it and the
previous probe both missing, the lesser of the two (Fréchet's bound, which needs no independence).
Not their product: a pair's probes are a few periods apart, inside the stalls' correlation time of
§2.6 (20 to 250 ms), and a short history's `τ_int` of one has not yet seen a stall; the product,
taken where `τ_int` was one, was refuted twice in three hundred runs at one CPU. This replaces the
suspicion window, its floor, `K`, the confirmation curve and the multiplier cap. A witnessed
extension (§S13 of mantle note 32) grants one more told probe, the base window's worth.

**The detection bound** the member states has two parts. To the condemnation pending, with `m` the
members its view holds besides itself (no round is larger): a peer's next probe is at most `2m − 1`
periods after its last answer (§4.3), unanswered it suspects, and the told probe starts at most as
far again, and its own period resolves it: `2(2m − 1) + 1` periods. A period is at most the longest
the member has run or, where longer, what an unanswered probe's deadlines allow: its target's span
`μ + α`, then the slowest relay's and the target's again, at most three times the longest span any
of its verdicts has had, plus the latest the member has woken past a wake it asked; the period in
progress, and how late the member is now for the wake it asked, count as measured. Then the wait for
an answer from another member, the evidence that the member's own network works, which nothing
bounds in advance (a member truly cut off waits for ever, by design): the member measures it and
adds it. Earlier forms failed under load and are recorded so they are not tried again: the longest
period run so far (one macOS run in a hundred: early on every period had been answered, and the
first unanswered one, waiting on its relays, was longer than any yet), and one more period for the
answer from another member (four Linux runs in four hundred under a CPU throttle, where the next
probes missed too); periods already ended only (seven runs in three hundred at one CPU, where a
throttle's freeze of some 40 ms was inside the period still running when the death was noted); and
the current round's size for `m` (a member that had condemned another, falsely, under the throttle
ran rounds of one, while the victim's last probe had been in a round of three). The member states
the bound only while every probe it makes is judged, by its pair's verdict or the pool's: an
unjudged probe that goes unanswered suspects nobody, so before then the member detects nothing by
its own probes, and a death it holds is another member's condemnation, adopted on that member's
timeline. Stating its own bound for one was the last form to fail: with the directed gossip below,
three runs in 2,000 noted a death 0.2 to 2.2 ms past a bound of 2.3 to 3.3 ms, and a build that kept
a ring of each member's probes, answers and views, dumped at the overshoot, showed why: a live
member falsely condemned in the run's first milliseconds, adopted by gossip at a member whose probes
were still measurement only (`a_member_that_judges_nothing_states_no_bound`).

**The owner's detection budget** (slates' A-125). A judged period ended at its probe's deadline,
which tracks the round trip, so an idle member probed at its round trip's pace: in slates' two-member
fleet on loopback each member sent about 10,000 pings in 6 s. SWIM sets the protocol period for load
and bounds it below by the round trip (§3.1); memberlist probes once a second on a LAN, every 5 s on a
WAN (`docs/research/swim.md`). The owner states its detection budget `D` (`Detector::new`), the
longest it may take this member to declare a dead member: Chen, Toueg and Aguilera's requirement
`T_D^U`, here a statement of the owner's. A dead member is declared within `2(2m − 1) + 1` periods of
its last answer (above), so a judged period lasts at least `D/(2(2m − 1) + 1)`, `m` the members a
round probes, and an idle member probes no faster than detection within `D` needs. The probe's
deadline and its relay request are unchanged: only an end that would come sooner waits for the
floor. Where every period's deadlines fit within it, the declaration to pending is within `D`; a
period whose deadlines run past the floor is as long as they are, and the stated bound counts it.
Measurement and provisional periods are not floored: they judge nothing, or nothing yet, and
flooring them left a lone peer unsuspected 9 s into its silence in slates' runs. A budget of zero
leaves every period at its probe's deadline. In the unit test three near peers answering in about
1 ms drew 138 probes in one 900 ms budget without the floor and at most 12 with it
(`an_idle_member_probes_no_faster_than_its_detection_budget_needs`); in the simulation each member's
probe judged at the floor is followed by its next no sooner than the floor, on every seed
(`a_member_under_a_detection_budget_probes_no_faster_than_its_floor`: 1.8 ms after it before);
slates measured pings fall from 10,264 to 134 a member in 6 s.

**Before a pair can be judged.** A pair's estimator refuses until it has two prediction errors and a
measured `τ_int` (`Refusal::TooFewHeartbeats`, `CorrelationUnmeasured`). Until then its probes are
judged by the member's pool: one more `LinkEstimator`, fed every round trip the member measured, its
sequence the member's probe count so an unanswered probe to anyone is a loss. The pool is the
candidate of §3, item 3 (the stalls are the host's); it configures within a few dozen round trips of
the member's first, and a pair then within a few dozen of its own. The pool is fed while it judges:
by pairs with no verdict of their own, and by every pair until it has one; its verdict is renewed
when a probe needs it. A renewal the estimator refuses leaves the verdict in force, as
`LinkEstimator::configure` leaves its margin: a stall can make `τ_int` unmeasured again, and the
first form, which dropped the verdict then, left a probe of a crashed member unjudged (one Linux run
in three hundred at one CPU). While the pool refuses too, nothing is judged: a probe is measurement
only, and its period ends when it is answered or at its expected arrival from the latest round trip
(NFD-E over a window of one), whichever is first. Unanswered then, it is a loss to the estimators
unless its answer comes later; it judges nothing, and its wake measures `G`. Each measurement period
that ends unanswered doubles the next one's wait, as a retransmission timer backs off (RFC 6298
§5.5), up to the 60 s at which §2.5 lets the doubling be capped, and a measured round trip ends the
backing off. An answer is measured only while its probe is outstanding, the latest three of its
peer's; without the backing off, round trips that lengthened past that, under load, came each for a
probe written over, none was measured, the latest round trip never lengthened, and the member probed
on at the stale pace with nothing judged, slow to answer the others, who condemned it: a member
43,557 periods into a cluster run, condemned 95 times
(`measurement_periods_follow_round_trips_that_lengthen`, which reproduces it in the detector alone,
taking no round trip in 2,000 periods without it). An earlier rule ended an unanswered
measurement period only when another member was next heard from, assuming no time at all; in Linux
at two CPUs with four busy loops, a throttled container dropped a burst of datagrams, every member's
probe was lost at once, and all four waited for one another for ever
(`a_lost_measurement_probe_ends_at_its_expected_arrival`). The very first probe, before any round
trip, has no round trip to expect its answer from: it waits as a retransmission timer does before
its first measurement, 1 s (RFC 6298 §2.1, "Until a round-trip time (RTT) measurement has been made
... the sender SHOULD set RTO <- 1 second"), backed off as above, and ends sooner when another member
is heard from. An owner that measured a round trip to the peer outside the detector, the handshake
that keyed its session, gives it at the join (`Detector::join_measured`), and the first probe waits
on that instead: 1 s is some 10^4 times a LAN's round trip. It only times the first waits; a
handshake is not a probe, so no estimator takes it and it judges nothing. It once waited on its answer or another member alone, and members whose first probes
were all lost waited on one another for ever: slates' daemons, three in one process, dropped the
datagrams queued while a peer's address was re-resolved at a re-key, sent five datagrams in all and
then nothing, in 4 runs of 6 (`a_lost_first_probe_ends_at_the_initial_wait`, and in the simulated
cluster `members_whose_first_probes_are_all_lost_probe_again`).

**A pair the pool does not fit.** The pool's verdict suits the paths the member mostly probes. For a pair much farther away, it is wrong until that pair's own estimator configures. Found by slates on two networks 100 ms apart: a far member's pool, fed by its same-side peers' 1 ms round trips, put each crossing probe's deadline 1.9 to 2.7 ms after its send on a 200 ms path. Each far member condemned each live near member 76 to 115 times in 30 s, from the first second to the last. Every answer arrived after its probe's record had been reused (three a peer, above), so the pair never took a sample and never configured.

The rule:

- A pair is **misfit** (`misfits_pool`) when it has no verdict of its own and any of these holds:
  - an answer came back past the pool's span `μ + α`;
  - an answer arrived after its probe's record was reused;
  - the handshake that keyed its session measured a round trip longer than that span (`join_measured`).
- The pool is configured, if due, **before** the handshake is compared with it. Comparing first read no verdict on the probe that configured the pool, and that probe was judged by the pool; on slates' harness a near member was condemned in 5 of 40 runs.
- A misfit pair is never judged by the pool. Until its own estimator configures it is judged **provisionally** (`misfit_verdict`):
  - **Expected arrival `μ = R`**, the pair's own latest measured round trip: its latest answer however late, else its handshake's. An answer that finds its probe's record reused gives one too. Each peer keeps the latest record a later probe wrote over while it was unanswered (`Peer::reused`): an answer to it is timed exactly, and an answer to an earlier probe, sent before it since nonces only increase, has at least that record's age, a lower bound. Without it, a pair made misfit by such an answer, with no handshake round trip, had no `R`, stayed measurement only, and a member that then died was never condemned by it (`a_far_peer_that_answers_only_after_its_records_are_reused_and_dies_is_condemned`, failing first: alive after 600 s simulated).
  - **Margin `α = max(2R, α_pool)`.** RFC 6298 §2.2's rule for a path with one measured round trip `R` sets `SRTT = R` and `RTTVAR = R/2`, so `RTO = SRTT + 4·RTTVAR = 3R`, and the margin is `3R − R = 2R`. The pool's margin is kept where it is wider: the host's own stalls (§2.6) are in every pair's round trip, near or far.
  - **Interval `η` and loss `p`** are the pool's. The pair is probed at the member's rounds like any other, and the loss sizes the relays an unanswered probe asks.
- Before any round trip of the pair is measured, which needs a handshake that measured none and no answer yet, its probes are measurement only, as before.

**What `mistake = 1` means.** Theorem 7's bound `(V + p·α²)/(V + α²)` needs the pair's own delay variance `V`, which a pair with one or two round trips has not measured. So the provisional verdict claims no bound, and the allowance it adds is the whole probe. The condemnation rule does not change:

- an unanswered provisional probe suspects its target, as any judged probe does;
- the next probe, which carries the suspicion, going unanswered past any extension makes the condemnation pending;
- the pending condemnation is confirmed by an answer from another member (§2.7, above).

So a member that never answers is condemned after two unanswered provisional probes and the pending wait, the same count as for a configured pair. Only the deadline that decides "unanswered" differs.

**The bound for a member that never answered.** `detection_bound` counts from the peer's last answer. A member killed before any survivor heard from it has no last answer: every survivor's time to hold it dead is counted from the kill instead, on each survivor's own clock, and must be within the bound that survivor stated plus its measured pending wait. Counting from the kill is the stricter choice, since the last answer can only come before it.

**Why a provisional probe's period ends at its answer.** A configured verdict's period ends at its deadline, so the pair is probed at its interval `η`. An answered provisional probe instead ends its period at the answer, as a measurement probe does. Waiting out `3R` on every answered far probe lengthened the member's periods past the interval its pairs' estimators were built at. Their samples stopped counting, and pairs near and far stopped configuring: the kill case on slates' harness failed 33 of 40 runs that way, and passes 40 of 40 with the period ending at the answer. An unanswered provisional probe runs to its deadline and is judged.

**The pool is fed only by pairs it fits.** A misfit pair's round trips belong to another path, so they are not the pool's evidence (`on_ack`). The first form fed the pool with every pair's answers. A far member's pool then mixed its 200 ms crossings with its 0.4 ms same-side round trips, and its mean rose to 75–100 ms. That verdict judged the member's same-side probes, and a judged probe's period runs to its deadline, so the member's mean period grew from 18–54 ms to 95–167 ms. Every pair then sampled at about half the rate (far samples by 30 s: 615 against 1,480 on seed 0), and far pairs took 116–281 s to configure. Fed only by the pairs it fits, the pool keeps the member's own side's paths: far pairs configure in 33–155 s and a far member's death is detected in 0.55–3.54 s (tables below).

**When it ends.** The pair leaves provisional judgment at the probe after its own estimator first configures: `judging` returns the pair's own verdict whenever it has one. `Detector::verdict` reports the same rule. `no_live_member_is_condemned_across_a_lossless_far_link` (`tests/sim.rs`) checks that every far pair was judged provisionally and, by the end, by its own verdict.

**Measured** (`tests/sim.rs`, seeds 0 to 15 and the stale-pool seeds, each through the run-twice check):

Three trees, the same harness and seeds (each row one seed's run; `never` = not within four times the
branch's most steps):

| tree | false condemnations / min (30 s) | far kill detected | all-far kill detected | far pairs configured |
|---|---|---|---|---|
| `756bfaa` (before the branch) | 0 to 214 (a lower bound: most runs spent the step budget inside the 30 s) | never | 59 to 493 ms, past the stated bound | never |
| `f129a55` (misfit pairs measured, not judged) | 0 | 0.36 to 2.05 s | never | 30 to 123 s |
| this branch, the pool fed by every pair | 0 | 1.25 to 5.03 s | 2.45 to 4.07 s | 116 to 281 s |
| this branch, the pool fed by the pairs it fits | 0 | 0.55 to 3.54 s | 2.45 to 4.07 s | 33 to 155 s |

The per-seed rows are in `docs/benchmarks.md`. On `756bfaa` the all-far kill is detected fast only
because the far pair is judged by the near pool's 2 ms deadline: the same deadline condemns live far
members, and the detection overran the bound the detector stated.

**A peer with no evidence of its path** (no handshake round trip, never answered: one learned by gossip
before it was spoken to, or re-learned after it was forgotten) is judged provisionally at RFC 6298
§2.1's initial RTO: "until a round-trip time (RTT) measurement has been made for a segment sent
between the sender and receiver, the sender SHOULD set RTO <- 1 second". It takes the provisional
verdict with `R = 1/3 s`, so its deadline `3R` is that second (`FIRST_CONTACT_RTT_NS`). Its first
answer, measured or bounded by a reused record, replaces it with the path's provisional verdict, or
with the pool's once the pair is shown to fit; a first answer slower than a second backs off per pair
(§5.5, above). Judged by the pool instead, a live far peer was suspected, condemned and forgotten
before its first 200 ms answer could return (`a_peer_joined_without_a_round_trip_is_not_suspected_before_its_first_answer`,
failing first). Requiring a measured round trip at `join` instead would refuse every member a node
learns by gossip before it has spoken to it. The cost falls on a peer that never answers: each
unanswered judged probe waits out its direct deadline and then its relays', which includes the
target's own span, two initial RTOs a probe, and a condemnation takes two probes, so its first
detection moves by at most four initial RTOs: 4,076 ms against 108 ms with a 1 ms handshake
(`a_near_peer_joined_without_a_round_trip_is_condemned_at_most_four_initial_rtos_later`). In the
far-link record one seed of 22 moved: seed 10's far kill held dead in 5.51 s rather than 3.54 s, a
survivor that had forgotten the victim having re-learned it by gossip with no evidence of its path.

Open: until a far pair configures, the far member's own detection of it runs at the provisional
`3R` deadline, so a far member's death is held a few hundred milliseconds to seconds later than a
configured pair would hold it; and configuration itself is paced by the far pairs' one sample a
round, minutes on this topology.

**The member's own lateness** (Lifeguard's local health) is measured, not multiplied:
- every wake it asked for and got late is a sample of `G` ([`Lateness`]), which floors `α`, never
  below the resolution of the clock its owner reads (`Detector::new`, §2.4);
- its delay in reading acknowledgements is in the round trips it measures, so a slow member's own
  `V(D)` and `α` grow with it;
- a probe is resolved when the member wakes, with every acknowledgement delivered by then, so a
  member that wakes late does not blame its peers for its own lateness (a kernel receive stamp,
  §2.4, removes the read delay where the caller has one).
Lifeguard's multiplier (`S = 8`, "chosen by trying combinations") and hyper-swim's own lag in
whole periods are gone.

**Dissemination.** SWIM §4.1: after `λ·ln n` periods of infection-style dissemination at most
`n^{−((2−4/n)λ−2)}` members are uninfected in expectation, which is below one member once `λ > n/(n
− 2)`. Each update is piggybacked the least whole number of times past `n·ln n/(n − 2)`: 3 at four
members, 6 at 256. With two members every message reaches the only other one. The logarithm is
computed in fixed point, so every host gets the same budget. A message carries the gossip that fits
its datagram beside the largest message, an acknowledgement with its coordinate
(`codec::gossip_capacity`): 63 entries at QUIC's 1,200-byte minimum (60 while the coordinate carried
eight dimensions and an adjustment).

**Memory.** Each peer's estimator holds a ring of `G/(PHI·η) − 1` sums at the pair's interval `η =
m·T̄` (§2.6, the drift bound), so a member's rings together hold about `G/(PHI·T̄)` whatever its
membership, and the pool as many again: at most `2/PHI` slots (1.1 MB) when `T̄` is at its floor
`G`. Each estimator is boxed, so the fields a period reads stay together. A period allocates nothing
once each peer has answered once (`docs/benchmarks.md`, "hyper-swim").

**The view's bound, and the dead forgotten** (`docs/research/swim.md`). The view held every member
gossip named and never forgot one: a fleet's churn and any peer's gossip grew every member's view,
its peers' estimators, coordinates and gossip reports without bound. Now:
- the view holds at most the members the owner's placement says this node can know, itself
  included (`Detector::new`'s `members`), and refuses an update about one more, typed (`Full`) and
  counted (`Detector::refused`); every map keyed by a member holds only members the view holds;
- a dead member's record is kept while gossip of it from before its death can still arrive, and then
  forgotten with everything held of it. That gossip, at an incarnation at or below the death's, would
  otherwise add the member back: probed, suspected and condemned again, and passed to members that
  forgot it too. A report from before the death survives only at members the death has not reached,
  one pending report a member being replaced by the death; by SWIM §4.1 the death reaches all but
  `n^{−((2−4/n)λ−2)}` in expectation, below one, within `λ ln n` periods of its first adoption, and an
  earlier report's own epidemic ends within as many of its start. So the record is kept the
  dissemination budget `T` (the largest this member's reports have had) times the longest a period of
  this member lasts, the period the detection bound uses, past this member's adoption: a peer slowed
  by its own host is slow in this member's round trips to it too, which that period covers. What is
  left is the expected straggler fraction: a straggler's stale update adds the member back, and this
  member's own probes condemn it again within its bound. memberlist keeps a dead node 30 s, a chosen
  number, and adds any node an alive message names;
- a record past its window makes room for a newcomer at once; a member with nobody alive or suspected
  left keeps its dead, which are the members it probes;
- a death of a member the view does not hold changes nothing: there is no state of it to override.
  With anti-entropy (below) the rule is needed: a member past its window took a death back from one
  still inside its own as a newcomer's, restarting its window, and pushed it on in turn, so a record
  went round the members for as long as any held it, as Demers et al.'s death certificates do when
  each site's threshold starts at its own receipt (§2); a certificate older than the time to reach
  every site is one whose obsolete copies are unlikely anywhere (§2.1), which is the window's
  premise (`an_exchange_does_not_bring_back_a_forgotten_record`);
- the detection bound counts the most members the view has held at once, not the members it holds:
  a member forgotten since was in the rounds before.

**A refutation a rumor missed.** A refutation is an update like any other, gossiped `T` times by
each member that adopts it and then dropped, and a rumor can end known to some members and not all
(Demers et al. 1987, §1.4–§1.5; `docs/research/swim.md`): a member it missed holds the refuted
member dead, past the record's window forgets it, and probes it no more. The cluster test found it
three times in 1,119 runs (`docs/benchmarks.md`, "The cluster test"). Lifeguard's buddy system
(§IV-C) makes a probe of a suspected member carry the suspicion whatever the rumor's budget, so the
suspect hears it at the first probe. Now:
- a probe also carries the prober's own state, alive at its incarnation
  (`Detector::ping_gossip_into`), so a member a refutation missed hears it from the refuted
  member's next probe: the refuted member holds it alive and probes it every round, held dead or
  forgotten by it as it is;
- an answer carries the answering member's suspicion or death of the prober
  (`Detector::ack_gossip_into`), so a member held dead at the incarnation it died at, which never
  heard so, is told by the answer to its probe, since nobody probes the dead; it refutes, and its
  next probe revives it there;
- the two take their room in a message's gossip before the ordinary batch does, which is drained
  only into the room left, so no rumor is counted sent that was not: one entry on every probe, and
  one on an answer only to a member held suspected or dead.

Between two members that probe each other these two entries reconcile the two states that concern
them. Two live members that each hold the other dead, both refutations missed, probe neither each
other nor anyone about each other; Demers et al. back a rumor up with anti-entropy for this, each
site resolving every difference with another chosen at random (§1.5), and memberlist exchanges its
whole state with one member every 30 s, a chosen number.

**Anti-entropy.** Each member reconciles its whole view with one other, push and pull, once a
dissemination window (`Detector::sync_into`, `Detector::on_sync`):
- **Why the window.** A rumor is sent on its adopter's next `T` messages, at least one a period, so
  past `W = T` of the member's longest periods (the window its dead records are kept for) after its
  last adoption it reaches nobody new, and a member it missed then never hears it by rumor. For a
  rumor sent a fixed count, Demers et al.'s relationship between traffic and residue (§1.4,
  `s = e^{−m}` with `m = T(1 − s)` sent a member) gives the fraction it misses: `s = e^{−T(1−s)}`,
  6.0 % at the five-member cluster's `T = 3`, 0.09 % at `T = 7`, a thousand members'. An exchange
  each `W` is anti-entropy at the rumor's own pace.
- **With whom.** The next partner of a shuffled cycle of the members it holds alive, rebuilt when it
  runs out, as the probe order is: over a cycle of `m` exchanges, `m` the members it holds alive,
  it exchanges with each.
- **What.** The exchange opens with the digest of the view (`Membership::digest`): the wrapping
  sum, over every member it holds, of SplitMix64's output function (Steele, Lea and Flood 2014)
  applied to the member, its incarnation and its liveness in turn, kept as the view changes, so
  two views of the same states have the same digest whatever order they changed in, and two that
  differ in any have the same with odds of one in 2⁶⁴. "Only if the checksums disagree do the sites
  compare their entire databases" (Demers et al., §1.3): a partner whose digest is the same answers
  nothing; one whose digest differs answers with its view, and the opener answers that with its
  own. A view goes in id order, every member it holds, alive, suspected or dead inside its
  record's window, its own state included, in chunks of what a datagram holds beside a bare chunk
  (`SwimMessage::Sync`), each entry applied as gossip is. A member owes one push at a time: a pull
  from another while it owes one is refused and counted (`Detector::pulls_refused`), and one
  opening is let go before the next, so the exchanges hold a cursor, an opening and a cycle no
  larger than the view, and allocate nothing once grown.
- **How long a split lasts.** A member a rumor missed learns the update at its first exchange with a
  member that holds it, which each partner does unless the rumor missed it too: the split outlasts
  the rumor's window by more than `k` windows with probability at most `s^k`, and by one in
  expectation `1/(1 − s)`. Deterministically, it ends within one cycle, `W + m·W` past the rumor's
  last adoption, when the rumor reached any member it holds alive, and within two when only the
  update's origin holds it, whose own cycle carries it to every member it holds alive first.
- **What it costs.** Views that agree exchange one opening of 30 bytes a member a window; views
  that differ, `⌈n/r⌉` datagrams each way, `r` the entries a chunk holds (66 in a 1,200-byte
  datagram). A first form pushed whole views at every exchange, `2⌈n/r⌉/T` datagrams a period
  beside the two a member sends probing, and its period cost 7.5 entries a member a period at 64
  members and 25 at 256; with the digest, none in a quiet cluster, and 5.1 and 16.4 where a member
  refutes a suspicion every period (`docs/benchmarks.md`). A stale alive state an exchange carries
  can add back a dead member already forgotten, as a late rumor could (above): it is probed and
  condemned again within the detection bound, the cost probes, not safety.

Both change what the wire carries: a probe and an answer carry entries of the existing gossip
encoding, which a receiver of the earlier form applies as any gossip, and the exchanges a new
message, `Sync`, with its own golden vector (`docs/research/swim.md`, "The wire, and its
argument"). No consumer runs hyper-swim yet, so the wire changed in place.

**What the cluster test asserts** (`crates/hyper-swim/tests/cluster.rs`, §2.5). Five member
processes run the detector as the library configures it, in two phases. The supervisor waits on
facts: every member judging every peer by a configured verdict (the pair's own, or the pool's while
the pair's estimator refuses), then it kills one; every survivor holding it dead; then every surviving
pair judged by its own estimator, then it kills another; every survivor holding that one dead too.
Each survivor holds each victim dead within the bound its detector stated, measured on the member's
clock from the victim's last answer: the death that stands, since a member notes every death it
comes to hold and a live member falsely condemned and alive again dies afresh. A first form noted
only the first, which could be such a false death, held before the member's probes judged
anything. The first kill comes within a few hundred milliseconds of the
start, when most pairs are judged by the pools, and the test had only it: three local runs ended with
none of the twelve pairs judged by its own estimator, so the end-to-end test never killed under a
pair's own detector. The second phase is that kill; it is a longer run, which waits for the pairs' own
evidence rather than a duration. Each wait goes on while the members move toward its fact, as
hyper-liveness's process test does: a pair's judgement, its own configuration and the round trips it
takes toward it, a victim's state in each survivor's view. Once a quiet period passes with nothing
moving, the longest detection bound a live member states and never less than RFC 6298's one-second
retransmission timeout, the wait fails with every member's last line: every step a wait waits on is
stated within one probe spacing and two periods of the one before, and the bound spans two spacings
and a period. A pair that takes more round trips without its own configuration than any window of
its estimator holds (`WINDOW_LIMIT`) fails the wait at once, so a throttle that keeps a pair's round
trips too correlated for `τ_int` to be measured no longer holds it for as long as that lasts; so does
a member whose output ends, its process exited, unless the supervisor killed it. Before, a wait ended
only on its fact, and nothing but CI's job limit bounded one that never came. A peer a member forgot
after its death reports as forgotten, and a member keeps what its detector reported of each peer
across the peer's being forgotten and adopted again.

Every suspicion and condemnation it checks exactly, from the members' own records, against the
detector's rule. A probe states its deadline as it is sent (`Ping::due_ns`), and a poll what the
member's own probes found, each finding with its evidence (`Detector::findings`: a suspicion, a
condemnation made pending, a condemnation). Each member records every probe it sends with that
deadline and whether it carried its suspicion of the target, every relay it asks, every answer it
hands its detector, direct or relayed, every ping it answers, and every finding, in the order it
made them. A suspicion, or a condemnation made pending, traces to its probe: sent when and to whom
the finding says, with the deadline it states; its period ended no earlier than that deadline nor,
where relays were asked (at or past it, for that target, none of them the target), than the
relays' deadline; no answer, direct or relayed, handed to the detector between the probe and the
end; and a pending one's probe carried the suspicion. Then the answer that missed it is found:
handed over after the end, late, or never, lost, the target's record saying whether the ping
reached it. A condemnation traces to the pending one it follows, ended when it says, and to the
probe of another member answered before it, and every count a member reports equals its record's
findings at every line it writes. Theorem 7's allowance, `Σβ` over the judged probes of live
members, is printed beside their counts, a report: it bounds an expectation, and a run's count is
what the rule found, not a draw. Two forms before asserted the count: within `Σβ` (twelve runs in
a hundred failed at one CPU while the series kept far inside it), then refuting `Σβ` only when the
count's 95 % Poisson lower limit passed it, a test that fails by chance; the owner's rule
(2026-10-03) is that no test passes or fails by chance. A member whose supervisor is gone ends when
its report cannot be written.

**Measured** (`docs/benchmarks.md`, "hyper-swim"): on loopback a period is 0.1–1 ms, `μ` 60–100 µs
and `α` growing from about 0.3 ms with the MTBF; a period costs no allocation and less time than
slates' at every point; 2,000 runs of the one-kill cluster test on macOS and on Linux at one, two and
four CPUs with busy loops beside them all passed, detection a median 4 ms on macOS and 16–18 ms in
Docker's VM after the victim's last answer; of the two-phase form on macOS, 300 runs with two
failing and 819 traced with one, each on a refutation a rumor missed; with probes stating their
sender, three forms of 1,000 each, 1,000, 999 and 998 passing, no split, the three failures each
the first victim held dead past a bound its member's unjudged probes could not keep (above); and
with the split closed, the soak: 2,000 runs on macOS and 500, 655 and 638 on Linux at one, two and
four CPUs with busy loops, every one passing, no split and no overshoot; and with every finding
traced, 2,000 runs on macOS and 454, 475 and 468 on Linux at one, two and four CPUs under ambient
load, every one passing, every answer a live member's probe missed late and none lost. Open: §3, item 1
governs the probe rate too: the owner's detection budget floors a judged period, but nothing yet
prices a probe, and as the MTBF grows the margins and so the periods grow with it; two members cannot condemn each
other, as neither can tell its own failure from the other's; a pair far from the member's others is
judged provisionally, at `3R`, until it configures ("A pair the pool does not fit", above); the
allowance is loose while a history is
young (§3, item 3); and views that differ are pushed whole, so under heavy churn the exchanges
carry `2⌈n/r⌉` datagrams a member a window, where digests of ranges of the view would push only
the ranges that differ.

### 2.8 The node-pair stream (L-3)

`crates/hyper-liveness`, sans-io: §2.1's one stream per pair of nodes that share a group, shared by
every group they share, each heartbeat proving a recent durable flush, each pair judged by §2.2's
NFD-E detector.

**Where it lives, and why a crate of its own.** The heartbeat is a message on the sealed datagram
plane (`hyper-datagram`), which escapes QUIC's congestion window (RFC 9221 §5) so a heartbeat never
waits behind bulk; not on `hyper-transport`'s streams for that reason. Not in `hyper-datagram`
either: the plane seals, authenticates and replays-checks opaque messages for Raft control, SWIM
and this alike, and slates uses it without consensus groups; putting a detector in it would tie the
seal to `hyper-timing` and to the shell's flush evidence. Not in `hyper-timing`, which holds the
laws and estimators both detectors use and no wire or per-peer protocol state. Not in `hyper-swim`:
a SWIM member probes a random peer each period and measures a round trip on its own clock, its
acknowledgements unflushed, with indirect probes, gossip and the membership's suspicion and death;
this stream is one-way, scheduled, flushed, per pair of consensus nodes, and its consumers are the
core (L-2) and the shell. What the two share is in `hyper-timing` and both use it: the estimator
(`LinkEstimator`), the configurator (`qos`: the stream's bound per arrival, SWIM's probes'
Theorem 7 product, §2.2), the timer fold (`Wakes`, moved out of hyper-swim for this) and the MTBF
fold (`Exposure`). The wire is the crate's own (`codec.rs`, one plane message, its first byte `KIND`), so an
owner multiplexing the plane tells it apart; the kernel stamps come from the owner's socket
(hyper-tokio's `PlaneSocket`, §2.4; slates' runtime its own).

**The stream.** A node sends each peer heartbeat `k` due at `σ_k = σ_{k−1} + η`, carrying its run
(a count raised at every start, below), `k`, `η`, its stability floor `E[flush] + G`, the interval it asks of the peer, its send
time and lateness past `σ_k`, and the flush proof. A sender behind its schedule sends the latest
heartbeat due; those it skipped are never sent, and the receiver takes them as that heartbeat's
lateness, the stall's delay (§2.2; `PairReport::skipped` counts them).
- **The interval** is the receiver's: its configurator's best (`Configuration::best`), asked in its
  own heartbeats (Chen et al.'s adaptive scheme), never below the sender's floor, nor below the
  interval its own evidence needs (below, "The interval the evidence needs"), nor, while its node's
  evidence judges the link and its margin at the link's interval promises nothing, below the best
  interval that evidence gives ("Judged before its own evidence"). Where the floor binds
  (before the receiver asks, or past what it asked) the interval is the floor, followed up and not
  down: an interval below the floor is unstable (Lindley 1952), but a floor that fell is a mean that
  moved with a sample, and following it down started the receiver's estimator again at every move
  (`LinkEstimator::retime`), which kept a link whose flushes stalled now and then unconfigured for
  thousands of heartbeats (§2.9). A change within `G`, the configurator's resolution, is none.
  Bootstrap: the first heartbeat waits on the first flush, whose time is the first `E[flush]`; the
  first wait for a wake that the owner reports measures `G` (§2.4).
- **The interval the evidence needs** (`T_c`, measured online: §2.6 item 6 left it open). The
  estimator configures only once it has measured `τ_int` (§2.6, item 3), at Madras and Sokal's
  self-consistent window `m ≥ 6·τ̂(m)` among Allan levels of at least seven windows; heartbeats so
  correlated at their interval that no level the history holds reaches that window are refused
  (`Refusal::CorrelationUnmeasured`) for as long as the correlation outgrows the levels, and the
  receiver, which asked for a longer interval only once configured, never asked. Now the estimator
  says where its heartbeats would be independent (`LinkEstimator::independent_interval`): `τ_int`
  heartbeats at `η` carry what one independent one does (an `m`-mean's variance is `V·τ_int/m`,
  Sokal 1997 §3), so at `η·τ̂` consecutive heartbeats are independent and the window is met at the
  first level that can hold it, eight windows of eight, 56 heartbeats. `τ̂` is the longest qualified
  level's, which sees the most of the correlation and is low, never high, where it is shorter than
  the correlation; it is taken at the low end of its uncertainty (the level's Allan variance to
  within `(1 + 1/√(2(K−1)))²` for `K` windows, the tolerance `n_A` takes), and a refusal within that
  uncertainty waits for longer levels instead of moving. A significant refusal has `τ̂` past `8/6`
  there, so each move lengthens the interval by more than a third, and a link still too correlated
  at the interval given moves again: the moves end at the first interval its history can tell apart.
  The receiver asks at least that interval until the link configures (`max(best, evidence)`); from
  the first configuration on it asks the best alone, and a refusal of a configured link moves
  nothing: the configuration in force judges the link while its levels at the interval grow until
  they measure `τ_int` again. Held past the first configuration, the interval asked was the longest
  estimate every later refusal drew, a maximum of noisy estimates that grows with the refusals
  sampled: in the simulation's world whose host freezes begin once its links configure it held a
  link at 31 s against its configuration's best of 0.57 s, its pairs took a heartbeat every 5.6 s
  on average against 0.32 s without it, and survivors noticed a killed peer up to 23.8 s after the
  kill against 1.9 s (`docs/benchmarks.md`, "The detector model, at its causes"). On an AR(1) delay of correlation time 199 ms with heartbeats at 1 ms, which
  refuses at that interval past a thousand heartbeats, 64 seeds configured within 576 heartbeats, at
  a final interval of 166 ms at the median and 1.26 s at the most (`link::tests`). The measured
  `T_c` is the evidence's floor and nothing else: the margin's bound asks no independence of the
  heartbeats it holds (§2.2), and the unseen share counts the window's arrivals in `τ_int`'s units.
- **A moved interval.** A receiver that asks for a longer interval expects the next heartbeat that
  much later (`LinkEstimator::expect_interval`): until a heartbeat at the new interval comes, each
  freshness point is put back by the difference, and the stated bound counts it. Before, the
  freshness point at the old spacing passed before the first heartbeat at the new one came, and the
  receiver suspected its peer for having done as asked (in `crates/hyper-durable/tests/liveness.rs`,
  seed 56: a 2 ms margin against a 73 ms move, a false suspicion and an election in an idle group).
- **The flush proof** (§2.1; CockroachDB's store liveness). A heartbeat leaves only once a write on
  the sender's log became durable after the previous heartbeat to that peer was due and is newer
  than the one the previous heartbeat carried. The owner reports every durable completion
  (`on_durable`, `Write::Log` for the shell's, the log's completions of `docs/durable.md` §8); when
  none came in time the stream asks for one (`Output::flush`, a liveness write the owner makes on
  its log device) and sends on its completion. A heartbeat carries the sender's count of durable
  writes and the age of the latest; the receiver takes it only if the count moved and the age is at
  most `late + η`, so the flush came after the previous heartbeat was due. A node whose disk stalls
  stops heartbeating and is suspected as a crashed one is; a sender that heartbeats without
  flushing is not trusted either (`Refusal::Unproven`). Idle, a node makes at most one liveness
  write per smallest interval among its pairs, shared by every pair; a node whose groups write makes
  none.
- **The receiver** feeds each heartbeat to the pair's `LinkEstimator` at the sender's interval (a
  new interval or a new run re-anchors it; a restarted peer's heartbeats are numbered on from its
  last run's, so the link's history, the hosts' and the path's, stays and the peer is judged at
  once). It judges the peer at the arrival first, so a freshness point that passed before a
  heartbeat came is a suspicion in whatever order the owner feeds messages and polls. Each
  heartbeat taken has its lateness past the expected arrival the freshness point in force was set
  from (§2.2; none for a peer's first, nor for the first of a new run, whose gap was its absence),
  kept over the link's history (§3, item 11). Configured through `LinkEstimator::configure`, which
  runs `qos::configure_arrivals` on those latenesses at intervals above the receiver's measured `G` and
  the sender's advertised floor, any margin admitted, the bound one Cantelli factor on a lateness,
  which assumes no independence: the detector in force is the best at the link's interval where
  its `U` is below one and the best over the intervals otherwise, and there is none where that
  one's `U` is one or more (§2.2, `Refusal::Unavailable`). Theorem 7's product, which a first form
  allowed past the link's own `τ_int·η`, broke its allowance under a one-CPU throttle in most runs
  (`docs/benchmarks.md`, "hyper-liveness"): its factors took the heartbeats in a margin as
  independent, which a stall makes them not; the per-arrival bound asks it of none. `Costs
  { election: T_E from the owner (set_election, the election law's span over the groups the pair
  shares), mtbf: the Jeffreys posterior over the node time the pairs watched and the restarts and
  abandoned suspicions seen, seeded with the fleet's history }`.
- **Renewal once a configuration is as stale as its estimates are uncertain.** The configurator
  runs when never configured, when the peer moved to the interval asked, and when the latenesses
  have doubled since it last ran (`LinkEstimator::reconfigure_due`); a configurator that found no
  availability (`Refusal::Unavailable`) is asked again on the same schedule. The doubling is
  derived, not picked: every input is estimated over the link's whole history (the latenesses'
  moments and their unseen share, `τ_int`'s levels, the exposure the MTBF is), and an estimate
  over `n` arrivals, with the one over the first `n_k` nested in it, has moved since by a variance
  `σ²(1/n_k − 1/n)` while its own variance is `σ²/n` (`σ²` per arrival, `τ_int` the same factor
  in both). The two meet at `n = 2n_k`. Renewed sooner, a configuration follows moves smaller than
  the estimates' own error; later, it lags the evidence by more than that error. The rule asks nothing of the delays' distribution, which a heavy tail
  makes hard to estimate (a rule on an estimated standard error would need the errors' fourth
  moment). Measured against the alternatives (`docs/benchmarks.md`, "The renewal schedule"):
  hyper-swim's once a window, half a window (the same rule for an estimate over a sliding window,
  where the move after `m` new heartbeats has variance `2mσ²/n²` against `σ²/n`, `m = n/2`), and
  Chen, Toueg and Aguilera's continuous re-estimation (§6), every heartbeat. In the simulation's
  macOS world they configured 15, 24 and 45 times a thousand heartbeats against the doubling's
  11 and moved the link's interval 23, 24 and 28 times against its 22, each move restarting the
  evidence at the new interval; they cost up to half again a heartbeat, allocated nothing either
  way, and detected and erred alike (suspicions of live peers 1,160 to 1,235 against allowances of
  8,915 to 9,072 over 40 seeds; every detection test the same). The rule that stood beside the
  doubling, `β` at the margin in force doubled past the configured one, had a factor no rule gave;
  it is gone, and in the simulation's worlds that change the detectors keep their allowance
  without it (§3, item 11). **The allowance** is the sum, over every heartbeat taken that ended a
  gap a freshness point judged, of the bound that point put on the heartbeat's coming past it: `β`
  at the margin in force on the arrivals as they stood when it came (the link's latenesses; the
  behaviour a margin of the node's evidence was imposed from, while that margin judged; for a
  peer's first heartbeat, the bound its judgment from the attach put on it), not as the
  configuration assumed. Each heartbeat taken is the one mistake its predecessor's freshness point
  can make, so the allowance is the expected number of suspicions of a peer alive throughout,
  wherever the bound holds.
- **The echo** (RFC 5905 §8's on-wire round trip; RFC 3550 §6.4.1's LSR and DLSR). Each heartbeat
  echoes the latest heartbeat its sender had from the receiver: that one's send time and lateness on
  the receiver's clock, and the hold since its arrival. The receiver gets the network round trip on
  its own clock, `A − s_echo − hold` (a path for the election law's ballot, `round_trip`), and the
  sum of the two directions' delays from their schedules, `round trip + late_echo + late`, which
  bounds this heartbeat's delay since both are positive. The hold runs from the echoed heartbeat's
  arrival stamp. A kernel stamp is when it reached the host, so the hold covers any time it sat in
  the socket, a stopped receiver's included, and the round trip is the path's. A stamp taken at
  the read is when it left the socket: the hold misses the time it sat there, and the round trip
  counts it as the path's. The E2E harnesses' members stamped at the read on every platform, and
  a member stopped and let go left its peers' `T_E`, which their round trips to it enter, at up to
  1.5 s on macOS and 2.1 s on Linux (§2.9); they read the kernel's stamps on Linux and macOS since, as
  hyper-tokio's plane socket does, through its `Stamped` (a standard socket's receive with the
  stamps, for an owner that runs no tokio). On Windows the read is all there is (§3, item 5).
- **Judged before its own evidence: what the node measured of its links** (§3, item 10). A link
  whose own estimator has not configured is judged by the margin its node's evidence configures for
  it. That evidence is of two kinds, and the link takes the wider of them, each measure the larger
  (`Liveness::renew_evidence`, kept where either kind moves and read only for a link with no
  configuration of its own: computed at every heartbeat, it cost every configured pair three maxima
  it threw away, `docs/benchmarks.md`, "The node's evidence, kept"):
  - *the node's pool*, one more `LinkEstimator`, fed the lateness of every heartbeat taken on a
    link without a configuration of its own, and on every link until the pool has its evidence
    (hyper-swim's rule, §2.7; a pool fed by the young links alone, whose links all configured
    before it could measure, never measured, and a peer never heard from was never judged: found by
    `a_peer_never_heard_from_is_suspected`), one an arrival (`LinkEstimator::on_lateness`). The
    latenesses carry no clock offset, so links whose clocks differ pool. What it measured stands
    when a later stretch makes its `τ_int` unmeasured again (`pool_measured`), as hyper-swim's
    verdict and `configure`'s margin stand;
  - *the widest configured link*: the arrivals each of the node's links configured its own
    detector from (`Configuration::link`), each measure the largest over the pairs that have one,
    kept at each configuration made and each pair let go.

  The young link's margin is `qos::arrival_detector_at` at its interval and costs, from those
  arrivals with the deviation scaled by `√(1 + 1/n_L)` for its window `n_L` (a lateness's variance
  at a window of `n` is `V(D)(1 + 1/n)` for independent delays and the measured latenesses' is at
  least `V(D)`, so the scaled variance bounds `L`'s from above, the side Cantelli's inequality may
  err on), widened by what the link's own latenesses show so far (their mean and deviation, at its
  own window already); put in force as a configuration of the link's own is: the best at its
  interval while its unavailability is below one, the best over every interval the floors allow
  where it is one or more (which promises nothing), that interval asked of the peer, none where
  that is one or more too. Imposed at the link's interval alone, 8 of the simulation's 6,786 such
  margins had a `U` past one, at most 1.6; in a pair's unit test, elections of a second and one
  lateness in fifty unseen at a 10 ms interval, the margin so imposed was 1.28 s and still promised
  nothing, where the best's is 121 ms at its longer interval
  (`a_margin_of_the_nodes_evidence_that_promises_nothing_at_the_links_interval_is_not_its_margin`).
  It is imposed on the link's estimator (`LinkEstimator::impose`), renewed on the doubling
  schedule, at a poll as soon as the node has evidence, and charged to the allowance at `β` from
  the arrivals it was imposed from.

  *Why the widest, and why it keeps the bound.* `β = u + (1 − u)·V/(V + (α − μ)²)` grows with the
  unseen share `u`, the variance `V` and the mean `μ` at every margin (its derivatives in them are
  `1 − V/(V + x²)`, `(1 − u)x²/(V + x²)²` and `(1 − u)·2Vx/(V + x²)²`, `x = α − μ`, none negative),
  so a margin configured from any componentwise upper bound on the young link's `(u, μ, V)`
  promises a mistake bound no lower than the truth's: the bound holds wherever the evidence bounds
  the link. What makes a node's evidence a bound on a link it has not measured is the pool's
  premise, measured in §2.6: the stalls are the hosts', the receiver's and the senders' alike, not
  the paths'. The widest configured link needs no premise beyond the pool's, and dominates the pool
  over the same links: the pool's latenesses are a mixture of its links', so their chance of
  passing a margin is a weighted mean of the links' chances, each at most its link's bound and so
  at most the widest's. It is the better-founded measurement besides: a configured link measured
  itself at one interval, its `τ_int` within Madras and Sokal's window and its unseen share counted
  (§2.6, item 3), the conditions its estimator refuses to configure without; the pool's levels mix
  links and intervals. The young link's own latenesses widen it again, so it is never judged
  narrower than it has shown itself to be: in the simulation's former stalling world they were
  wider than the node's evidence often enough to move the survivors' 90th-percentile detection
  from 997 ms to 1,141 ms.

  *What the pool alone missed* (the Windows E2E, every platform). With three members, a survivor
  whose leader died in its links' first heartbeats has one live link, the only feed of its pool.
  That link configures once it has its own evidence, and its configuration asks the peer to send at
  its best interval, 0.25–7.4 s on Windows' 15.625 ms timer; from then the pool's errors came at
  that rate, and the dead leader's pair, unconfigured, was trusted (`Trust::Unconfigured`), its
  survivor refusing the other's pre-vote, until the pool had its own evidence: 43–118 s to elect on
  macOS, up to 29 minutes on Windows. Now the dead pair is judged at the first poll once its
  freshness point has passed and the node holds any link's own evidence. Held in the simulation
  (`a_peer_dead_before_its_links_have_evidence_is_suspected_once_a_sibling_has_its_own`: three
  nodes, the victim killed after a seeded share of the heartbeats it sent before any pair
  configured, seeds 0–31 of four worlds, Windows' timer and flush among them, every survivor
  noticing the death no later than its first poll past both its freshness point and its live
  link's configuration), in the real processes (`tests/processes.rs`) and in hyper-durable's shell
  (`a_leader_killed_before_its_links_have_evidence_is_replaced`). The time from the kill to every
  survivor's suspicion, median, 90th percentile and most over the 64 survivors of each world, in the
  worlds the simulation then had, whose parameters were picked:

  | world | the pool alone | the node's evidence |
  |---|---|---|
  | LAN | 33 / 349 / 576 ms | 23 / 39 / 86 ms |
  | hosts frozen up to 50 ms | 6 / 63 / 831 ms | 5 / 32 / 207 ms |
  | flushes stalled up to 60 ms | 89 / 1,199 / 13,829 ms | 107 / 1,141 / 3,919 ms |
  | Windows' timer and flush | 578 / 4,468 / 33,828 ms | 578 / 2,080 / 5,867 ms |

  In the worlds the traces measured (`docs/benchmarks.md`, "The simulation's worlds"), the node's
  evidence: macOS 198 / 606 / 1,404 ms, macOS with its log busy 436 / 1,312 / 3,302 ms, Linux in
  its VM 37 / 304 / 1,363 ms, Windows' timer and flush 677 / 1,990 / 2,301 ms; over a hundred times
  the seeds the most reaches 13–35 s, each the time a survivor took to hold evidence of its own (its
  live link's configuration or its pool's measure), every freshness point long passed.

  What remains is the live link's own evidence, which no rule can lend it: in the slow worlds it is
  the link's correlation, which the moves of "The interval the evidence needs" resolve.

  A peer from which no heartbeat has come is judged from the node's first poll with the pair
  attached: one interval at the node's own floor (the pool's premise is that the stalls, and the
  floors, are the hosts') and the evidence's margin for a window of one; its suspicion states that
  time as its bound, from the attach. `PairReport::judged` says a margin judges,
  `PairReport::freshness` the `η + α` in force, and `PairReport::interval` the interval the peer's
  heartbeats come at, or a longer one asked, judged or not: what an owner waiting on a young link's
  evidence waits past.
- **A restart** (§3, item 10). Runs are ordered: a node's run (`Settings::run`) is a count it keeps
  durably and raises at every start, before its stream's first heartbeat. A heartbeat of a later
  run than the latest taken from the peer is reported as `Change::Restarted { peer, at_ns }` before
  the trust the heartbeat leaves, for the owner to call the core's `restarted` (trusted, and leading
  nothing it led), and counted once in the MTBF's evidence; one of an earlier run is refused
  (`Refusal::Stale`). The plane keeps two epochs a peer (`hyper-datagram`, `epochs_per_peer`), so a
  heartbeat the old run sent can open after the new run's first: with a run that had no order (a
  boot nonce from the OS's random source, a process id) it was taken as a restart back to the old
  run, and the new run's next as another, two spurious restarts and two failures, each making the
  followers of the restarted node's groups drop their leader and campaign (the wire is version 2
  since; `a_superseded_runs_heartbeat_is_stale_and_its_restart_counts_once` counts three restarts
  and three failures under the old rule, one and one now). A run number reused across starts (a
  process id the operating system gave again) made the new run's heartbeats, numbered from zero,
  stale for good. How an owner keeps it: as a record of its node written whole before the run is
  used (to a temporary name, the platform's full flush, a rename, its directory's flush, its
  CRC-32C checked on read: `hyper_block::record`), as the E2E harnesses' members keep theirs beside
  their logs (`hyper_raft_e2e::run`) and hyper-liveness's real-process members beside their files; a start that
  crashed before its run was durable sent nothing under it, so the next may take the same number.
  The run is the node's, not a group's, so it is not the shell's to keep: hyper-durable's `Kind::Start`
  is a compaction's new start of a group's log, written when the log is compacted, not when the
  node starts, and a node's replicas come and go. mantle keeps it with its node's other records
  (`crates/node/src/layout.rs`, written whole the same way); its random `Incarnation` has no order
  and cannot be the run. The owner is then told a suspicion only if the heartbeat leaves the peer
  suspected. Changes are reported only where what the owner
  was told differs: the owner trusts a peer until told otherwise, so trust after a suspicion is told
  and trust after nothing is not. Both ways, at every heartbeat and every poll:
  - a peer no margin judges is one the owner trusts, so a heartbeat that leaves a peer told
    suspected judged by no margin (its first, from a peer suspected from the attach, when the
    node's evidence went with a detach or gives no margin at its interval) tells it trusted;
  - a margin imposed at a poll (the node's evidence's) that finds a young link's latest heartbeat
    already past the next freshness point leaves the peer suspected with no freshness point passing
    then, and the poll tells it, from that point (`LinkEstimator::freshness`). Untold, the detector
    held the peer suspected while its owner trusted it, and a peer that died in its links' first
    heartbeats was never reported (a pair's unit test; the simulation's worlds had not reached it).
  The simulation holds every node's owner to it after every step (`told_is_believed`).
- **The detection bound** each suspicion states: NFD-E suspects at `τ_{h+1} = EA_{h+1} + α` on the
  receiver's clock, and the sender's last heartbeat was due at `σ_h` on its own; the time between
  is `τ_{h+1} − σ_h − θ`, `θ` the clocks' offset. Each echoed heartbeat `j` bounds `θ` from below:
  its delay `A_j − σ_j − θ` is at most the sum of the two directions' delays the echo gives,
  `S_j`, so `θ ≥ A_j − σ_j − S_j` when it came, and the clocks drift apart by at most RFC 5905's
  `PHI` each since. So `(τ_{h+1} − σ_h) − (A_j − σ_j − S_j) + PHI/(1 − PHI)·((τ_{h+1} − A_j) +
  (σ_h − σ_j))` bounds the time from the sender's last schedule, and so from its crash, to the
  suspicion, for every echoed `j` of the run; the tightest is kept, its order among the `j`s the
  same at every suspicion (`bound`). No clock synchronization or path symmetry enters it, and
  heartbeats that carried no echo (a peer's first, before it heard from this node) leave it
  stated. Its form before, `η + α` and the mean of the echoed sums over the expected arrival's
  window, was unstated while any heartbeat in the window carried no echo: a survivor's suspicion of
  a stalled member at one CPU stated none, once in twenty runs (`docs/benchmarks.md`, "Quiet only
  while every member is heard"; `a_suspicion_states_its_bound_once_any_heartbeat_was_echoed` fails
  on it). And it kept a ring of the window's sums a pair, the estimator's own size, which the
  running best replaces.

**The API** the core's suspicion-started elections (L-2) and the shell consume (`src/lib.rs`):
`Liveness::new(Settings { local, run, max_peers, history, resolution })`, `run` the node's durable
count of its starts, `resolution` its clock's (§2.4); `attach`/`detach` a group's peer; `on_durable(write, started, durable)`; `on_heartbeat(from, message,
arrival_ns, out)`; `on_wait(deadline, woke)`, a wait for `wake()` the owner began before it and that
ended at or past it, whatever ended it (§2.4: what `G` is made of); `poll(now, out)` and `wake()`, with `Output::{heartbeat, flush, change}`;
`Change::Suspected(Suspicion { peer, at_ns, noticed_ns, last: { seq, arrival_ns, due_ns, sent_ns },
detection, detector })`, `Change::Trusted { peer, at_ns }` and `Change::Restarted { peer, at_ns }`;
`trust(peer)`, `suspected()`,
`configuration(peer)` (the election law's base is `current.interval + current.margin`),
`round_trip(peer)`, `report(peer)` (`sent`, `taken`, `unproven`, `suspicions`, `allowance`,
`configurations`, `configured`, `judged`, `freshness`, `interval`), `set_election(peer, T_E)`,
`flush_mean()`, `granularity()`, `floor()`, `mtbf()`.
Every refusal is typed. The owner's contract: feed every message stamped before a time before
polling at it, reading the clock for that time before the socket, which then holds every datagram
stamped before it (hyper-tokio's `PlaneSocket::receive_ready`; read after the socket, a stop
between the two left the stop's datagrams unread, and the poll suspected a peer whose heartbeat had
come: traced in the process test once its mistakes were checked exactly); the core takes `suspect(node)`,
`trust(node)` and `restarted(node)` from the changes (hyper-durable's `Owner::believe`).

**Bounds.** Pairs at most `Settings::max_peers` (placement's), typed refusal past it; groups per pair
a `u32`; one liveness write out at a time; per pair one boxed estimator (its ring the drift bound's
size, §2.6, resized in place for a longer interval) and the echoes' best bound on the clocks'
offset; per node
one pool, an estimator at the first fed link's interval, boxed. Once each pair is configured, a
heartbeat sent and one taken allocate nothing.

**Tests.** `tests/sim.rs`, one clock, and a world as a host's traces measured it
(`tests/support/worlds.rs`; `docs/benchmarks.md`, "The simulation's worlds"): one-way delays,
flushes and an owner's timer lateness drawn by the inverse transform from measured quantiles, and
the host's measured freezes replayed from a point of the trace each node draws; the owners compute
`T_E` from the library's law. The worlds had been picked (uniform delays, a stall chance and
length, a loss rate, hosts frozen up to 50 ms about every 250 ms); macOS's trace has 103 freezes in
300 s, 67 of them in one 31 s burst, the longest 575 ms and 1.21 s, and no loss. An owner's timer is
set to the stream's wake after every call and fires late by a lateness drawn once, when its
deadline is set, as hyper-sim's world fires a timer (`World::wake`); the harness had drawn the
lateness again at every turn of its loop and anchored it at the present, so while other nodes'
events kept coming a due wake was pushed past the world's bound (a death noticed 84 µs past its
freshness point where wakes are at most 80 µs late), and the soak at ten times the seeds failed two
tests under that model and passes all under this one. A frozen node's owner does nothing, and at
the thaw takes what arrived, each datagram judged at its kernel stamp, and what completed, then
polls once, as `LinkEstimator::on_heartbeat` asks of its caller: a poll after each held datagram
found the freshness point of one whose successor it had not yet taken, a false suspicion a
datagram (59 of live peers against an allowance of 10, one seed). The draws are hyper-sim's
SplitMix64, a stream a source (a node's timer, disk, groups' writes and host, a directed link), so
a change in one node's timing moves no other source's draws. Each test's seeds are a space of its
own, `HYPER_LIVENESS_SEEDS` of them for a soak (from the `HYPER_LIVENESS_SEED`-th); each runs until
the facts it asserts on hold (every pair configured, every live pair through a renewal of its
configuration, every survivor holding the victim suspected), never to a picked horizon; and every
node's owner is held to the contract after every step. No test decides by a confidence level. Each
node keeps a record (`tests/support/record.rs`, the process test's members too): every heartbeat it
fed its stream, with its run, number, kernel stamp, schedule and send, what the stream made of it
and the trust it held of the peer after; every poll that told a change, moved a trust or came at or
past a point a peer was held trusted to; every change told; every heartbeat sent. Every suspicion
in the records is traced to NFD-E's rule exactly: its heartbeat is the latest the node took from
the peer before the call that told it (or the one that call took, where it came at or past its own
successor's point, a configuration the take made having restated it); its freshness point is the one
the stream held the peer trusted to before that call (where it held none, the point the margin
given in that call set); the call came at or past the point and noticed it then; and no heartbeat of
the peer's taken after was stamped before the point. Every heartbeat and every poll at or past the
point a peer was held trusted to told its suspicion there, so none is missed. Every count a node's
report states of a peer (the suspicions, the heartbeats taken, refused for their proof and sent, the
slots skipped) is its record's, after every step of the simulation and at every state line of the
process test. What became of the heartbeat each point awaited is reported: taken late and how late,
its slot skipped by its sender, refused, lost, a new run's, none sent. The suspicions of live peers
against their allowance are the model's figures, reported (`docs/benchmarks.md`), never asserted,
since no run's count tests a bound on an expectation. A stream made to suspect 20 ms early, to
judge at half its polls, to drop one heartbeat in five it says it took, or to leave a suspicion
uncounted fails the trace or the count at once. Live peers configure and are
trusted (8 seeds); no heartbeat leaves without a newer flush made after the previous was due (with
and without the groups' own writes); a killed peer is suspected by every survivor within the bound
each states from the peer's last schedule (16 seeds); a stalled disk is suspected so too, and the
stalled node still hears its live peers (8 seeds; the disk stalls while every peer trusts the node,
since a peer that suspected it in a freeze just before held that suspicion through the stall, seed
86 of 800); a sender behind its schedule for every other slot, at a receiver whose timer is late
past the interval, has a margin past the skipped slot configured, an unavailability below one, and
no suspicion once configured (`a_sender_that_skips_slots_is_late_not_lost`: under the product
bound it was fed half its slots as losses and configured `α = 0`, `U` 10); an owner whose every
wait for a wake a message ends 300 µs past it, before its 1 ms timer, has `G` 300 µs from the first
and takes every heartbeat, where counted only as the timer ended them none counts and every
heartbeat is refused (`a_wait_a_message_ends_past_its_wake_measures_the_wake`); a thousand groups
send what one does and an unshared pair is silent; a restarted peer is reported restarted once by each other node, trusted again by
the detector in force and counted, its old run's last heartbeat refused as stale; every refusal.
Problem 1 and item 10, over seeds 0 to 31 in each of three worlds, macOS, macOS with its groups
keeping its log busy (its flushes back to back) and Linux in its VM
(`every_link_configures_or_suspects_a_crash_within_its_bound`): every pair configures, none taking
more heartbeats unconfigured than any window holds (`WINDOW_LIMIT`, the drift bound's ceiling: a link
whose estimator has not measured its correlation in as many heartbeats as any window could average
is one no window resolves), at most 486 heartbeats; then one node is killed after a share of the
heartbeats it sent before every pair configured in the same seed's run, drawn between none and twice
as many, and every survivor suspects it, each suspicion within the bound it states, by its own
configuration's margin (156), the node's evidence's (129), or, for a node never heard from, from the
attach (3). Before the fix, the former stalling world kept a link unconfigured for 5,775 heartbeats (its
floor followed down with each decaying mean, the estimator started again at each move), and a world
that is too correlated at its floor is refused by the estimator however long it runs
(`link::tests`). A peer never heard from is suspected by every other within the bound from the
attach (16 seeds). A peer dead in its links' first heartbeats, in a group of three, is suspected by
both survivors once they hold evidence of their own (above, "Judged before its own evidence"; 32
seeds of four worlds). At ten and at a hundred times every test's seeds, each passes: links
configured within 1,113 heartbeats, freezes of up to 1.21 s replayed (`docs/benchmarks.md`).
Property tests: the codec reads back what it writes and refuses every truncation, extension, kind
and version; the bound is its window's mean. `tests/processes.rs`, real processes over UDP on the
sealed plane through hyper-tokio's kernel-stamped socket, each liveness write a real write and
platform flush of a real file: one member's disk is stalled (its device thread stops completing
flushes) and every other suspects it within its stated bound from the last schedule, on the host's
monotonic clock, which the processes share; then one is SIGKILLed and every survivor suspects it so;
each member traces every suspicion it makes to the detector's rule as it happens, from its kernel
stamps (the first heartbeat it takes from the peer after the one the suspicion judged from was
stamped no earlier than the freshness point), and one that does not trace fails the test; the live
members' suspicions against their allowance are reported. A second group of three kills a member as soon as it has heard
every peer and sent to each, and each survivor suspects it no later than its first poll past its
freshness point (plus the latest lateness of its wakes) and the state line in which it stated its
live link configured. The test derives nothing; it waits on facts, each for as long as the members
move toward it: a quiet period derived from their states (their pairs' `η + α` or intervals, their
longest flush, their wakes' lateness, their reporting period) that passes with nothing moving fails
the wait with every member's last state. Without that end, a stall the stalled member's device
thread never took (its queue held a flush and refused the stall) left its peers trusting it and the
supervisor waiting: 1 h 45 min on CI's ubuntu-24.04-arm before it was cancelled. The test looped on
that runner reproduced it in its 31st round, the dump naming the refused stall; the queue now has
room for every request ever outstanding, and each member binds its own port (a port picked and
released for it was taken by the other test's member as both started).

### 2.9 Elections by suspicion in the core (L-2, as built)

`crates/hyper-raft/src/watch.rs` and `src/raft.rs` (`Config::elections`). A member that elects by
suspicion (`Elections::Suspicion`) takes no ticks: `RawNode::tick` is refused. `Elections::Ticks`
stays, raft-rs's rule, for the raft-rs differential (which compares against raft-rs's tick timer)
and the owners that still tick.

**What the owner gives the core.**
- `RawNode::suspect(member)` and `RawNode::trust(member)`: what its detector of `member`'s node now
  believes. Per member of a group; the owner fans each node's event out to the groups with a member
  on that node, which is per group only when a node's standing changes. A member is trusted until
  its owner says otherwise, so a member opened with no word from any detector trusts everyone.
- `RawNode::restarted(member)`: the node came back as a new incarnation. It is trusted, and leads
  nothing it led before. A leader probes it from what it is known to hold, its window emptied: what
  was in flight went with the incarnation that stopped (core step R-7; with a window of one byte, a
  leader waiting on ten such messages freed one a beat, `HeartbeatAnswers::Bare`, and the group
  looked stalled for longer than the schedules' quiet period, seed 478 of the faults at rest).
- `RawNode::set_timing(Timing { span, round, election })`: `Timing::of(&ballot, &span)` from the
  law of §2.3, `span` the `W` the ballot chose, `round` the ballot's `broadcast_tail` (the slowest
  voter path's tail over `G`, plus the mean flush) and `election` the `T_E` the law expects at that
  span, against which a learner's catch-up round is judged (`RawNode::catch_up`, `docs/raft.md`
  §3.2, R13). Given again whenever the ballot moves.
- `RawNode::wake(now)`, the owner's monotonic clock in nanoseconds, after each call it makes and at
  `RawNode::deadline()`; the core reads no clock, and what a call arms is timed at the next wake.
  A group with nothing timed has no deadline and is woken for nothing.
- `RawNode::hold_campaigns(held)`: the owner holds the member's campaigns (a write that waits for
  room; a log that may lack what it acknowledged the core judges itself, `docs/durable.md` §5.2).
  Everything else goes on.

`Config::validate` refuses elections by suspicion without pre-vote and check-quorum: a member that
opens knowing no leader campaigns, and without pre-vote one that restarted in an idle group would
spend a term and depose a leader that never left.

**The rules**, each with its source and, where the schedules found it, the run that did:
- **A follower campaigns when it trusts no leader**, after the law's draw, `election_delay(W, seed,
  index)` uniform on `[0, W)` (`ElectionTiming::delay` is the same draw). It trusts a leader while
  it knows one, its detector does not suspect it, and the configuration it counts by, the newest its
  log states, names it a voter (a leader that is none leads only until that configuration is
  committed, and hands over once it applies it; `docs/raft.md` §3.4). A suspicion withdrawn before the delay ends cancels the campaign.
- **Every arming draws anew** (`Watch::draws`, the next draw's index): a member's delays are
  independent across elections, as Raft's randomized timeout is drawn anew at every reset (§5.2,
  §9.3) and as the law's split probability takes them. A draw kept until it fired, as the campaign
  count once indexed it, is not: a member whose delay never fired keeps a long one while those
  whose delays fired draw again, so the delays an election runs on lean long. The split test below
  found it, 135 first rounds of 1,000 splitting against the law's 110.8 with the draw kept, and 99
  with a draw at every arming (`every_arming_draws_anew` fails on the kept draw). Where detectors
  are often wrong the long delays had a use the law does not count: they outlasted short wrong
  suspicions, so fewer campaigns ran on them, and a campaigning member knows no leader to forward a
  proposal to. The schedules, wrong one time in ten, commit 5.5 % fewer fast-track entries by
  suspicion with the draw anew (`crates/hyper-raft/ORIGIN.md`, "A draw at every arming"); the cost
  of a campaign on a wrong suspicion is not in the law's expected time, which takes the suspicion
  as right (§3 item 12).
- **When the delay is counted from.** From now where the event is common to the followers: the
  detectors suspected the leader, or the members opened together. After any other reset — a new
  term, a campaign lost or refused, a vote granted, a leader that stopped leading — a round
  (`Timing::round`) and then the draw, as a reset gives a whole election timeout on ticks (Raft
  Figure 2: granting a vote resets the timer). A candidate whose campaign is unresolved draws again
  a round after it began. Without the round, five-voter fast-track schedules held a leader in 5 %
  of their steps against 17 % on ticks, their candidates rising within one another's rounds; with
  it, and a round tail that covers the schedules' vote round, 18 % (`tests/pipeline.rs`;
  `crates/hyper-raft/ORIGIN.md`, "L-2").
- **A member campaigns only while it and the members it trusts are a quorum** of each half of its
  configuration: a campaign that cannot win is not run, and costs nothing while a partition lasts.
- **A leader steps down** once it and those it trusts are no quorum of either half (check-quorum
  from the detectors, Raft §6.2), at once: the detector already waited its `η + α`.
- **A leader that stops leading in its term hands over**: a transfer's order (`MsgTimeoutNow`,
  dissertation §3.10) to the voter it trusts that holds the most of its log, and among those that
  hold as much, to each in turn, one a hand-over (`Watch::handovers`): one that restarted knows nothing
  of what its followers hold, and the first it named may be one that cannot campaign. A leader that
  restarted marked and could not campaign named a member that was a learner by its own
  configuration, again and again, while its followers kept their lease on its node (core step R-7,
  seed 75 of the faults at rest by suspicion). Its followers trust
  its node, which lives, and on ticks their lease would have run out; nothing else would make them
  campaign. The schedules found it twice: a leader that removed itself with no voter holding its
  whole log (seed 7 of the four settings), and one that stepped down for want of a quorum. The
  heir's campaign moves its voters to a later term, where they know no leader, and an heir that
  cannot win leaves them there all the same. Until its term moves or it follows another leader of
  it, a member that led its term and cannot campaign hands over again a round and a draw after the
  last, as a candidate asks again, for the order may be lost; a member of a later term answers an
  old order (as it answers an old leader's heartbeat), which moves the sender's term.
- **What ends a follower's trust in its leader**, besides its detector: a request for votes from
  that leader (a member asks only while it does not lead, and terms only rise at it); its
  incarnation's end (`restarted`); a configuration that names it no voter; and a request from a
  member already in a later term than the follower's (`message.term > term + 1`: with pre-vote a term
  moves only by a campaign a majority let through, and the leader of the follower's term is deposed
  once it and the asker speak). The schedules found each: a leader stepped down and its followers
  kept refusing its own campaign (seed 1); a removed leader's follower trusting it (seed 7); a new
  voter with an empty log trusting a leader of an older term and refusing the one candidate that
  could win (seed 2313 of 10,000).
- **A member that voted for itself in its term may have led it.** It opens treating the term as
  one it led — it campaigns, or hands over if held or not a voter — until its term moves or it
  follows another leader of the term (a term has one leader). Messages it sent before it stopped
  may still arrive and make a follower trust it again; its followers' detectors seeing its restart
  cannot undo a heartbeat delivered after (seed 1322).
- **Pre-vote keeps its role.** A member that trusts its leader neither grants a pre-vote nor moves
  its term for a vote request (dissertation §9.6: no pre-vote is granted while a leader is heard;
  here, while it is trusted). A leader asked for a vote by a member of its group answers with a
  heartbeat: that member knows no leader, as one that restarted while its group was idle does, and
  nothing else would tell it.
- **A leader beats only while its group has work in flight**: a transfer, a read, or a member it
  trusts that is behind it, has not said it holds the commit, is probed, sent a snapshot or has
  messages out. Its beat is a round of heartbeats once a round tail after the last (the
  retransmission timeout RFC 6298 §2 computes, mean plus four deviations over the granularity, as
  RFC 9002 §6.2.1's), which recovers what was lost. A group with nothing in flight is sent nothing
  and woken for nothing. A member the leader suspects is left out, as CockroachDB quiesces a range
  whose behind replicas are on non-live nodes, and a leader that trusts a member again sends it a
  heartbeat, as CockroachDB wakes such ranges when the node becomes live
  (`replica_raft_quiesce.go`, `Store.nodeIsLiveCallback`; `docs/research/timing.md`).
- **A transfer** is given up `TRANSFER_ROUNDS` (two) rounds after it began, the order reaching the
  transferee and the new leader's append coming back with the transferee's vote round between
  (dissertation §3.10: given up when not finished within an election), or at once when the
  transferee is suspected.
- **A sole voter** campaigns at once, timed or not: it has no one to split a vote with.

**Time** is the owner's monotonic clock in nanoseconds, a `u64`, for the core, the durable shell
(`Replica::drive`, `deadline`, `Driven::wake` and `flushed`) and hyper-liveness alike. The core
must not read a clock, so it takes a number; a `std::time::Instant` has no value of its own a
simulated world can make without reading the host's clock, and each crate that took one anchored on
`Instant::now()` in its tests. One integer type across the sans-io crates lets one simulated clock
drive them all (`docs/sim.md`), and an owner with an `Instant` converts at its edge, once.

**The law's latency counts the candidate's flush.** hyper-durable's processes found it: two voters
whose vote flush was two orders of magnitude past the path's one-way delay split round after round
(terms in the thousands, no leader), because a candidate's request leaves only once its term and
vote are durable, and the other, turning to campaign in that time, had voted for itself before the
request reached it. Ongaro's model has the request leave at once; with durable votes the split's
latency is the path's delay and the flush, which `Ballot::latency` now is. The core's own
schedules persist at once and never saw it.

**Driven by hyper-liveness (L-3).** `crates/hyper-durable`'s owner keeps each replica's node pairs
attached to the node's stream from its configuration (`Owner::pairs`), takes each `Change` to every
replica with a member on that node (`Owner::believe`: `suspect`, `trust`, and `restarted` for
`Change::Restarted`; a fenced replica reopened is told the stream's beliefs,
`Replica::believe_all`), derives each group's timing by this law from what the stream measured, its
echoed round trips, mean flush and granularity (`Replica::measure`, `Owner::measure`), charges every
pair the mean `T_E` of the groups it shares (`Liveness::set_election`), and hands the stream each
replica's durable writes as its flush proof (`Driven::flushed`). Every pair, not only the leaders':
a mistake about any member costs its group an election at most (a follower that suspects its leader
campaigns, a leader that suspects too many followers steps down), and a pair never charged never
configured, so a leader's detectors of its followers, which check-quorum reads, judged nothing.
Held to it in `crates/hyper-durable/tests/liveness.rs`, three nodes on one simulated clock with
seeded delays, flushes and wake lateness, 1,000 seeds (64 in the gates): a group elects from nothing,
sends no Raft message while idle but after a detector's change (2,150 over 7,146 changes), its
leader's node killed, every survivor suspects it within the bound the suspicion states and the
survivors elect and commit, and the killed node started again on its store, a new run of its
stream, every survivor's stream reports its restart to its core and the node catches up; and,
in a second test, the leader's node killed as soon as the group elects it, before any survivor's
link to it has evidence of its own, every survivor suspects it and the survivors elect and commit
(`a_leader_killed_before_its_links_have_evidence_is_replaced`: over the 1,000 seeds replaced
within 855 ms of the kill at the most, against 243.8 s judged by the pool alone, §2.8). The quiet
period its waits use is the stream's own: the longest `η + α` in force (`PairReport::freshness`,
which counts an interval asked and not yet taken) or, for a pair no margin judges, the interval its
heartbeats come at (`PairReport::interval`), the election's span and rounds, a flush and a network
round.

What the first form of this wiring found of the stream, each closed in hyper-liveness (§2.8):
- **A link could stay unconfigured for good.** The receiver asked a longer interval only once its
  estimator configured, and until it asked the sender sent at its floor; where the heartbeats at the
  floor were too correlated for `τ_int` to be measured (item 6), or the floor itself moved with
  each stall of the flushes it is the mean of, so that the estimator started again at each move, the
  link was refused for as long as either lasted. In this test, seed 0, one node's link to another
  took 3,000,000 heartbeats unconfigured; in hyper-durable-e2e's processes on this machine, both
  followers' links to one member took 2,700 each. Closed at both causes: the floor is followed up
  and not down, and a link that refuses for want of `τ_int` asks the interval its own levels say its
  heartbeats would be independent at (§2.8, "The interval the evidence needs"); meanwhile it is
  judged by its node's pool, and a peer never heard from is judged from the attach.
- **A new run was not surfaced.** Now `Change::Restarted`, to the core's `restarted`.
- **The pool** of item 10: built (§2.8).
- **A false suspicion at every move the receiver asked.** Found once the intervals moved: a
  receiver that asked a longer interval suspected its peer at the old spacing (§2.8, "A moved
  interval"); the receiver now expects the move.
- **A link that stopped before its evidence waited on its sibling's interval.** Found by the
  Windows E2E: with three members a survivor's pool is fed by its one live link, which, once
  configured, asks its best interval, seconds on Windows, so the dead leader's pair stayed
  unjudged, trusted, for minutes, and its survivor refused the other's pre-vote. The young link is
  now judged by the widest of what its node measured, the pool's and its configured links' (§2.8,
  "Judged before its own evidence").
- **Its kill test killed during a mistake.** Seed 26 of
  `a_killed_peer_is_suspected_within_the_stated_bound` killed the victim while a survivor suspected
  it by a mistake; the test now kills once every survivor trusts the victim. In this test, likewise,
  a suspicion held from before the kill is not one the kill's bound applies to (seed 607).

**On real detectors** (L-4's harness half, `crates/hyper-durable-e2e`). Each member process runs
the stream itself, wired as the owner wires it for one replica: heartbeats travel as the harness's
datagrams, stamped when read (as hyper-tokio stamps where the kernel cannot, item 5); the stream's
own writes go to a group of their own on the member's log, on the same device, so a disk that stops
stops the heartbeats with it. The test tells no member what to believe and derives nothing: its
period measurement, its 95/95 tolerance bounds and its count caps are gone. It waits on facts and
goes on while the group moves — any member's term, commit, applied index, last index or restarts
seen, or, while a member has a pair no margin judges, the heartbeats it has taken — and gives up
after a quiet period of the members' own law (the longest `η + α` any member states, or the longest
interval at which a pair no margin judges takes heartbeats, since the wait goes on while those
move; its election's span and three rounds; an ask's three), never less than its own
retransmission timeout (RFC 6298:
one second before a round trip is measured and never less after, §2.1, §2.4). Quiet is only time in
which the test heard every member, by hyper-raft-e2e's rule, one module both harnesses' waits keep
(`hyper_raft_e2e::quiet`; "The same rule in the other harnesses", below). An ask is resent at
that timeout. A member just started is waited on while its process runs. Its count bounds are the
protocol's: a member passes each durability point at least once for each turn of writes it takes
(the most a turn takes, `max_pending + voters + 1`), so a target that has not stopped after that
many answered writes is a failure. New scenarios: a stalled disk (the member's file stops
completing flushes, so its heartbeats stop; every other member suspects it and the group elects
without it), devices held while the survivors of a stalled leader elect, a member stopped (both
below), and, at every restart, every member that heard the last run and shares a group with the
restarted one reports the restart its stream saw. A member waits for its datagrams by a peek and
takes them without waiting, for a receive that times out on Windows can lose the datagram arriving
as it does, which lost the test's asks on the windows-11-arm runner (`docs/raft.md`, "The harness's
receive"). A wait that gives up prints why, what its looks saw, and each member's last report (its
suspicions, the peers heard, its unjudged pairs and the longest interval they take heartbeats at,
heartbeats taken, stated detection, span and round, the time it has had a write of its log out, how
long its oldest write still out has been, its longest write and its longest time between two reads
of its socket) with the quiet period in force. The member's shell writes its commit
alone at the first moment no write is out (`Settings::quiet` zero): an owner woken by events has no
period. Measured in `docs/benchmarks.md`, "hyper-durable-e2e on its own detectors" and "A link
younger than its evidence" (the stalled member's scenarios, 0.7–112 s on main across the six
targets, 0.3–18.8 s with a young link judged by what its node measured).

hyper-raft-e2e's members (`crates/hyper-raft-e2e`, a `RawNode` over a file log of the harness's
own) run the same wiring on the core directly: `Config::elections = Suspicion` with pre-vote and
check-quorum, the pairs attached from the configuration (each told to the core as the stream
believes it when attached, as hyper-durable's `Owner::pairs` tells a replica), each `Change`
taken to `suspect`, `trust` or `restarted`, its run (`Settings::run`) the count of its starts kept
beside its log and raised before its stream's first heartbeat (`src/run.rs`, which hyper-durable-e2e's
members share; §2.8, "A restart"), the timing derived by this law from the stream's
echoed round trips, mean flush and granularity (`stream::timing`, as `Replica::measure` derives
it) and every pair charged the span's `T_E`, each log write handed to the stream as a flush proof,
and, where the group wrote none in time, the log's hard state written again and flushed on the
same file (`Wal::prove`). The member wakes at the earlier of the core's deadline and the
stream's, waiting by a peek and taking without waiting as hyper-durable-e2e's members do; a member
cut off drops its heartbeats with its Raft messages. Its test is hyper-durable-e2e's in shape: no
tick, no `--tick-ms`, no 95/95 bounds of a flush, a datagram or a wake, no count of elections per
wait or per run; waits go on while any member's term, commit, applied index, last index or
restarts seen moves (or, while a pair is unjudged, its heartbeats), and fail once a quiet period of
the members' stated law passes with nothing moved (the longer of a member's stated detection and
an unjudged pair's interval, then its election's span and rounds and an ask's, never below RFC
6298's one second). Quiet is time in which the test saw the group and nothing moved: a look begun
before the period ended counts as movement unseen; a look in which a member did not answer decides
nothing (its answer after counts as movement); and the time the members report their one thread
spent in their logs' writes since the watch last heard them extends the watch by the most any one
spent. A member's silence is excused only as far as the members' own measures go: the longest one
write of a log any member has reported (or the stall the test ordered) and the quiet period,
against its silence counted from its first unanswered ask to its latest, less the retransmission
timeout the test waited on the latest (a lost ask costs the test, not the member). Past it the wait
fails, naming the member, with the group's state: `member-stopped` stops a follower (`SIGSTOP`
on Unix; on Windows, which has no stop signal, the member holds its thread outside any write) and
asserts the wait fails naming it, then lets it go and the group converges. The rule is one module,
`hyper_raft_e2e::quiet`, which hyper-durable-e2e's waits keep as well. The partition is also seen by the
detectors: the member cut off suspects every other and every other suspects it. Its bounds stay the
scenario's: the keys it writes, the asks it keeps waiting, and a log of one entry a write and one a
term, for a leader proposes no write its log already holds and each term's leader appends one
empty entry; and every report holds the member to its bookkeeping: each write it keeps waiting has
an entry above what it applied in the log of the term it leads (`stray`, asserted zero). A phase
writes one entry more than an append carries on the platform's datagram, so a member that missed
one catches up over more than one append. Measured in `docs/benchmarks.md`, "End to end".

**The stall it found, and its cause.** About one run in forty on Linux in Docker stopped with the
group judged stuck while the test still had writes to land; once a dump was printed, the group a
look later was healthy and idle, its leader having taken none of the writes resent to it. The
cause was the test's, not the group's. A member's one thread answers nothing while it is in a
write of its log; on Docker Desktop's virtual machine, whose disk is one file on the host shared
by every container, a flush took up to 1.8 s, the same 1,807 ms on members of groups in different
containers at once (each member's longest time between two reads of its socket was its longest
flush, to within a millisecond, whenever that flush passed half a second): the device held them all
together; macOS's own disk, under that load, held one flush 1.9 s. The test then heard no one (the
last look before one failure was empty), its quiet period (one second, RTO's floor, above the
law's) passed with nothing seen to move, and it called the group stuck; the group was waiting on
its device, and went on with it. The bookkeeping lead (a write kept waiting
that no apply would answer) was checked and is not it: the invariant never broke in any run. The
fix is the rule above, which counts as quiet only time in which the test saw the group. The
`stalled-devices` scenario holds it: every member's device answers no flush for the quiet period in
force and two looks' worth of retransmission timeouts, a write is sent into the stall, and the test
waits for every member to apply it. Under the old rule the test failed there with the same empty
last look in each of five runs on macOS; under the new it waited through the silence and the write
committed once the devices went on, in each of five.

**The same rule in the other harnesses.** Two more harnesses called silence quiet as
hyper-raft-e2e's had, each failing on CI once in a way a re-run passed:
- **hyper-durable-e2e.** `stall-leader` on ubuntu-24.04 ("a write was never answered; quiet 1s": the
  stalled leader's two survivors heard a moment before, in term 4 with no leader, nothing moved for
  a second) and `stall-follower` on windows-11-arm ("the stalled member was not suspected by every
  other; quiet 1s": every pair still unjudged, the two members up last heard 1.01 s before). Its
  waits now keep hyper-raft-e2e's rule, moved where both harnesses read it
  (`hyper_raft_e2e::quiet`), with its heartbeat framing (`hyper_raft_e2e::stream`), its run's record
  (`hyper_raft_e2e::run`) and its parent watch and hold (`hyper_raft_e2e::parent`). Its members'
  threads do no write of their own (the log's threads do), so a member held in a write still
  answers: the fault file held every member's flushes seven seconds (`file::hold_flushes`, ordered
  by `hyper_raft_e2e::stream::put_stall`) and each answered every look within 120 µs, its longest
  time between two reads of its socket 7 ms, while its group moved nothing through it. The time a
  member reports in its writes is therefore the time it had a write of its log out, the replica's or
  the stream's, measured at its turns, which on macOS ran to 37–90 % of a scenario's (the stream's
  own writes every `η`). Since a write that never ends would extend the watch for good, a member
  heard with its oldest write out past the members' longest write (or the hold the test ordered) and
  the quiet period ends the wait, named (`Stuck::Held`), as a silent one does, its write's time
  counted less the timeout a look waits for an answer, as a silent member's silence is: on CI's
  windows-2025 every member of a fresh group had its first writes out 1.0–1.3 s while the longest
  any had finished took 154 ms, which the bound without the timeout called held. The members' longest
  write is learned only from writes that finished, so the first write a busy device slows past
  every one before it was still called held: at load averages of 50 to 100, other processes' builds
  on the same disk, `stalled-devices` failed once in twenty with all three members' writes out
  2.1 s together against a 97 ms longest write (2026-10-04, before any hold), and once with a
  follower's write out 9.0 s past its 7 s hold. A slow device and a stuck member look alike to that
  rule and not to the device: before a member is judged silent, the test flushes a file of its own
  on the members' device with their flush (`hyper_raft_e2e::device::Probe`) and its time joins the
  excuse. A member with a write in progress is not judged by any other flush: one write on a
  contended device, its thread competing for the CPU, stays out for seconds while another returns at
  once. `kill-durable-leader` at load 60 to 78 (2026-10-04) held single writes out 1.3 to 3.5 s while
  the longest any member had finished took 83 ms and the test's own flush was faster still, which
  the rule that judged a write by the test's flush called held. Twelve runs at load 60 to 65 passed
  only because no write outran it. A write in progress is the kernel's until the kernel's own bound
  on a flush passes, and past that it is held. The
  test's stalls are made inside a member's process (`FaultFile`), so an injected stall still fails
  named; a device that answers no flush of the test's within the kernel's own bound on a flush, 60 s
  (Linux's SCSI disk driver: `SD_TIMEOUT` 30 s × `SD_FLUSH_TIMEOUT_MULTIPLIER` 2; its NVMe driver's
  `nvme_io_timeout`, 30 s), has failed, and the wait says so (`Stuck::Device`). A fact no longer
  holds of a member that did not answer: the stalled member's suspicion and a restart's report were
  true of a look that heard no one. `stalled-devices` (the leader's disk stalled, the survivors'
  devices held the quiet period and two looks' timeouts, 7 s, as they elect, once every pair is
  judged) failed under the old rule in 3 of 3 runs on macOS with CI's message, the survivors heard
  14–88 µs before, each suspecting the other and the leader, and passes under the new;
  `member-stopped` (`SIGSTOP`; on Windows the member holds its thread until released over stdin)
  fails the wait naming the stopped member after 2.0–3.0 s of its silence against 1.04–1.05 s
  excused, then converges. CI's windows-11-arm form, the survivors silent while the test waits, was
  reproduced by a helper that stopped one of them 2.5 s during stall-follower's wait (not
  committed): 5 of 5 failed under the old rule with CI's message, its last report 1.0–2.0 s old, and
  5 of 5 passed under the new. Held on young links, the survivors trusted the stalled leader 44 s: a
  link younger than its evidence, its heartbeats stopped by the hold, is judged only once its node's
  links have evidence again (§2.8), so the scenario holds once every pair is judged.
- **hyper-liveness's process test.** The young victim's test,
  `a_node_killed_in_its_first_heartbeats_is_suspected_once_a_sibling_has_its_evidence`, on
  windows-2025 ("the victim heard every peer: nothing moved for 1s", every member with a flush in
  flight, its longest 372–467 ms; CI run 37104550192, its first attempt). A member's heartbeats wait
  on its flushes: its first, proved by a first flush of 372–467 ms, were refused by peers whose own
  wakes had not yet measured their timers (`Refusal::Unmeasured`), and the next waited a floor and a
  second flush, so the first heartbeat taken came about three flushes in, past one quiet second,
  through flushes each begun after the quiet period did, which the old rule excused only for a flush
  begun before it (and waited on without bound). Its members push lines rather than answer asks, so
  the rule is brought there in those terms. A member's line is due once its statement period (its
  shortest interval, its floor before any) and its wakes' lateness have passed since its latest, and
  it now states one at each flush completed; a line past its due by a retransmission timeout, the
  time an E2E harness waits for an answer, is a member unheard: in a write, held, or not scheduled.
  A check while a member is unheard decides nothing, and its line after counts as movement; a check
  is a look: a member whose line is past its due when it begins, not yet by that timeout, is awaited
  once, for that line or that timeout, as an ask waits for its answer, so a check ends within a
  timeout of its beginning; the time each member's writes took since the last check (its run's
  record, kept durably before its stream sent anything, then its flushes) extends the wait by the
  most any member took; and a member silent past its due by more than the longest write any member
  stated and the quiet period ends the wait, named, whatever else moves (`Stuck::Silent`). A member
  that has stated nothing yet is still waited on while its process runs. With every flush held 450
  ms more by the member's device thread (the flushes CI's runner took; not committed), the
  young-victim test failed under the old rule in 3 of 3 runs on macOS as on CI ("nothing moved for
  1s", every member with a flush in flight, its longest 459–468 ms), and with 1.2 s more in 2 of 2
  ("nothing moved for 2.4 s", once waiting on a first flush); under the new it passed 3 of 3 and 2
  of 2, in 43–78 s and 75–197 s, the victim noticed dead 28–193 s after the kill at that pace of
  flushes. A new test stops a member once every pair is configured (`SIGSTOP`, the stop taken from
  the system's word that the process stopped, `ps`'s state `T`; on Windows it holds its thread until
  a byte comes on its standard input, and says when it holds): the wait for a line only it can state
  fails naming it, and let go, its first line awaited while its process runs (what it stated before
  it stopped is no word of it since), it is trusted again. A member's silence counts only time the
  supervisor listened for lines, as an E2E harness's lost ask costs the test and not the member:
  the supervisor reads every line already come before it judges, waits for a line no longer than
  it must listen before the first member would be silent past the excuse, and is deaf from when a
  wait for a line ends (or the time it asked to end, if it woke past it) to when the next begins,
  lines that come meanwhile waiting unread. Counted against the wall clock, its own lag was the
  members': in Docker's virtual machine (four CPUs and four busy loops, eight more in a container
  beside, every line traced to the run's file) 9 of 40 runs failed `Stuck::Silent` with every
  member's latest line 2.08–2.26 s old at once, where the rule before passed 40 of 40 beside it, and
  a supervisor held three seconds once every member had stated (a scratch hold, not committed)
  failed 5 of 5 that way on macOS; counted as listened, the held supervisor passes 5 of 5, deaf
  3.01 s at most, and beside the same load 40 of 40 passed, the supervisor deaf up to 6.08 s at
  once (`docs/benchmarks.md`, "Quiet only while every member is heard").
- **hyper-durable-e2e's `kill`, no `G` where messages end the waits** (2026-10-03, over
  `dc42e0e`). In Docker's Linux, one scenario a run failed at one, two and four CPUs ("a write was
  never answered" or "the stalled member was not suspected by every other"; "nothing moved for 1s"),
  each time with a member at its start that had taken no heartbeat, its pairs unjudged. The owners
  reported to the stream only the waits their deadline ended, as `on_wait` asked (§2.4). The test
  asks each member for its report back to back, and on the VM's 1 ms tick a timer ends a wait
  0.12 ms to past 1 ms late (`worlds::LINUX_TIMER`), so nearly every wait for the stream's wake
  ended on an ask, before or past its deadline. With no wait counted, `G` stayed unmeasured, and the
  stream refuses every heartbeat until it is measured (`Refusal::Unmeasured`: the estimator's ring
  and window are `G`'s): nothing taken, nothing judged, no timing derived, no campaign. A diagnostic
  build (counters in the member, not committed) ran the test three times at two CPUs and failed all
  three (`stall-leader`, `stalled-devices`, `flush-leader`): 11 of 59 member processes went a second
  or more without `G`, one through 21,901 waits for its wake, none of them ended on its timer, and
  the runs refused 4,020, 2,575 and 2,370 heartbeats as unmeasured. On main every poll past a wake
  was a sample. A wait the owner began before the wake and that ended at or past it, whatever ended
  it, now counts (§2.4): the E2E members report it as the wait ends, before what it brought is fed
  (`receive_until`), the process test's member likewise, and the simulation's owner reports a wait
  an arrival or a completion ended past its wake. With it the same build passed three runs of
  three, `G` measured a median 108–143 ms into a member's start, 51, 9 and 18 heartbeats refused in
  all (`docs/benchmarks.md`, "`G` from every wait that reached its wake").
- **The E2E members polled their streams past what they had read** (2026-10-03, over `09e8316`).
  hyper-raft-e2e's `cluster` on that head, once in Docker's Linux at each of four, two and one CPUs
  beside main's runs, counted 465 and 392 suspicions in `stalled-devices` at four and two CPUs,
  against main's 19 and 124, and the leader killed in `leader-killed` was in term 19 and 29, against
  main's 9 and 13. Both members polled the stream at the top of their loop, after their drive, which
  a write of the log holds as long as the device takes, and again after a drain that a turn's budget
  may have cut with datagrams left. `Liveness::poll` asks that every datagram stamped before the
  time it is polled at be fed first (§2.8): stamped as they were read (main), unread heartbeats had
  no stamp yet and the rule held of itself; stamped by the kernel (`dc42e0e`), a heartbeat that came
  in time and sat unread was a suspicion. A scratch diagnostic on macOS (prints in the member, not
  committed) showed the suspicions told 113–203 ms after the member's last drain, the next heartbeat
  taken stamped before the suspicion's point. The members now read the clock, drain the socket, and
  poll at that clock only when the drain emptied it; a drain cut at its budget polls nothing, and
  the next that empties the socket does. Each member counts the suspicions it told while a heartbeat
  stamped before their point sat unread, found as that heartbeat is taken (`Report::unread`), and
  both tests hold every report's count to zero, exactly. On `09e8316`'s loops the count failed both
  tests on macOS in `member-stopped`: 35 in hyper-raft-e2e's member stopped 3 s, 4 in
  hyper-durable-e2e's; with the loops so, both tests passed whole, every count zero.

**What the runs measured of the detectors** (every scenario prints, for each member, its floors,
longest flush and longest time between two reads of its socket, and for each pair what the
configurator was fed, the detector in force and its suspicions against Theorem 7's allowance,
`PairReport::allowance`). The runs are `docs/benchmarks.md`'s, "End to end", on `c4530be`: ten on
macOS under load 25–66 from other sessions and Docker's Linux runs beside them, and fifty in
Docker's Linux at four, two and one CPUs with as many busy loops (the virtual machine's load
4.0–11.0). Of macOS's 760 pairs 608 were configured (`own`), 52 judged by their node's pool and 100
still unjudged when their scenario ended; of Linux's 3,800, 3,398, 168 and 234. Over the configured
pairs, macOS first:
- **Floors.** In the scenarios without a stall or a stop, `G` 0.06–11.4 ms (median 1.6–6.8 ms by
  scenario) on macOS and 0.2–11.3 ms (median 1.5–3.5 ms) on Linux; `E[flush]` 3.7–20.3 ms (median
  9.6–13.5 ms; `F_FULLFSYNC`) and 0.35–19.5 ms (median 0.8–1.2 ms); so the sender's floor
  `E[flush] + G` near 20 ms on macOS and 4 ms on Linux. The expected election `T_E` charged to every
  pair, where a member's law was derived, 30–155 ms (median 61–99 ms) and 3.3–217 ms (median
  19–51 ms).
- **`G` takes in the owner's own stalls.** `G` is the mean lateness of every wake the stream asked
  (`hyper_timing::Wakes::granularity`), and a wake that falls while the member's one thread is in a
  flush comes late by the rest of the flush. The stall `stalled-devices` orders took `G` to 32–65 ms
  on macOS (median 45 ms) and 15–76 ms on Linux (median 29 ms), against medians of 5.4 ms and
  2.6 ms in commits-3; a member stopped took its own to 30–62 ms on macOS and 9.5–47 ms on Linux,
  its peers' staying at 0.9–8 ms. The sender's floor follows `G`, and a sender's interval follows
  its floor up and not down (§2.9, "Every link judged"). In one run over `ecbf071`, of a set
  `docs/benchmarks.md` does not count (two matrices ran at once in the virtual machine),
  stalled-devices' members ended with `G` 4.8–7.4 s, every pair unjudged after 12–18 heartbeats
  sent each in the scenario's 94 s, and the quiet period their law stated before the test's order
  was 76 s, so the test ordered a stall of 82 s, and waited it out.
- **What the pairs measured.** `p_L` 0.9–24 % (median 6–8 % by scenario) on macOS, 67 % on one pair
  through the ordered stall, and 0.6–87 % (median 23–45 %) on Linux, where 1,121 of the 3,398
  configured pairs were fed more than 40 %; none of it refused for its proof. Most of it is not the
  link's: the stream numbers each heartbeat slot, and a sender held past its slots (in a flush, or
  unscheduled) skips them, which its receiver counts as lost. From each pair's two ends, one minus
  what the receiver took over what the sender sent, in commits-3, commits-5, stalled-devices and
  member-stopped, is 0.4–12.5 % (median 1.8–4.2 %) on macOS and 0.2–43 % (median 1.1–2.0 %) on
  Linux. `E(D)` enters the configurator as zero (the expected arrival absorbs the mean delay). The
  deviation of the prediction errors 3.3–22.5 ms on macOS and 0.1–37 ms on Linux (611 ms on
  stalled-devices, 1,020 ms on member-stopped).
- **What was configured.** `η` 7.3–121 ms (median 16–22 ms) and `α` 3.5–84 ms (median 10–14 ms) on
  macOS, `η` 0.36–385 ms (median 1.5–55 ms) and `α` 0–323 ms (median 0.6–50 ms) on Linux: mostly one
  heartbeat in the margin, so a single skipped slot is a suspicion. The mistake recurrence each
  detector promises, `η/β`, median 52–116 ms on macOS (11 ms–7.5 s) and 2–218 ms on Linux (0.4 ms
  to 28 s), shorter than an election in many pairs; the unavailability `U` it was chosen for exceeds
  1 (a model that charges more election time than there is time) in 312 of the 608 configured pairs
  on macOS and 1,475 of the 3,398 on Linux. On Linux 268 pairs were configured with `α = 0`, a
  detector that suspects at each expected arrival (`η` median 1.8 ms, its recurrence `η` itself: a
  mistake promised at every freshness point), fed `p_L` of 5–72 % (median 51 %), `U` median 6.1; on
  macOS one, the pair fed 67 % through the stall.
- **Mistakes against the allowance.** 2,846 suspicions against 26,424 allowed on macOS, 65,834
  against 446,673 on Linux. No pair's count refutes its allowance: the 95 % lower limit of every
  count (the Poisson score interval, as the hyper-swim test above judges) is below its `Σβ`. Four
  Linux pairs' counts passed `Σβ` itself: three counted the true suspicion of a member killed (2
  against 1.2, 3 against 2.2, 2 against 1.7), and one was of a pair whose ends were both up, 51
  against 39.6 (lower limit 38.8); none on macOS. The suspicions of commits-3 and commits-5 are all
  mistakes (no member is down or cut off there); the partition's include the true ones of and by
  the member cut off, and those of leader-killed, follower-restarts, all-killed and member-stopped
  the true ones of the members killed or stopped.
- **A member stopped and let go.** The heartbeats that waited in a socket's buffer through the stop
  are stamped as they are read (the kernel's stamps since, §2.8, "The echo"), so the round trips
  both ends measured took in the stop, and their
  law's `T_E` at the scenario's end stood at 0.06–1.5 s on its peers and 2.9–5.7 s on the member
  stopped on macOS, 0.01–2.1 s and 0.4–6.6 s on Linux.
The detectors keep their bound, and the bound is loose; the configurator, minimizing `U` with
elections this cheap and losses this high, accepts detectors that mistake many times a second, and
each mistake about a leader can cost an election (the leader's term at leader-killed's kill reached
22 on Linux and 2 on macOS, where the scenario's first election makes term 1). Nothing here was
tuned to hide it. What it asks is §3's items 1 and 3 of the configurator (the cost of a mistake
beyond `T_E`: a leader change moves every client), a loss estimate that tells a sender's skipped
slots from the link's, and a `G` that does not count the owner's own stalls as its timer's lateness.

**The model.** These rules only bring forward or refuse a campaign, or forget a leader, which is
volatile; the TLA+ model's `Elect` may be taken at any time with any quorum the log comparison
admits, so it needs no change (`docs/models/README.md`).

**Evidence.** `crates/hyper-raft/tests/suspicion.rs`, a group in time on one-way latencies: an idle
group sends nothing and is due for nothing for a simulated day; elections start only on suspicion;
the delay is the law's draw exactly, and every arming draws anew; split votes resolve, and each of
1,000 crashes of a five-voter leader splits its first round exactly when the law's event holds on
the delays its members armed (three of the four starting within the latency of the first), which
is predicted before the crash runs. No count is judged against a level: 99 first rounds split,
reported beside the law's expectation of 110.8 (`split` of `election_span` on the same latency and
round); how the draws spread over the span is the law's to show (hyper-timing). A withdrawn
suspicion cancels; pre-vote refused by members that trust the leader; step-down on a suspected
majority of either half of a joint configuration; hand-overs; a held member campaigns for nothing;
a restarted leader forgotten; a leader that beats while work is in flight and then sleeps.
`tests/pipeline.rs` runs the R-4 durability oracle with suspicion-driven elections at its four
settings, the fast track and the crash at every persistence step, its detectors right nine times in
ten about a member that is down or cut off and wrong one time in ten about one that is not
(`crates/hyper-raft/ORIGIN.md`, "L-2", has the counts).

### 2.10 Histograms for an owner's metrics

`Histogram` (`src/histogram.rs`) is what an owner's metrics keep of a latency in fixed room: a log's
flushes and group-commit waits (hyper-log's `LogStats`, `docs/durable.md` §13.1), and an owner's
queue, quorum and apply delays (focal's metrics view, F26). A value below eight nanoseconds has a
bucket of its own; from eight, each doubling `[2^e, 2^(e+1))` is cut into eight equal buckets:
bucket `8 + 8·(e − 3) + ((v >> (e − 3)) & 7)`, 496 buckets covering `u64`, 3,968 bytes. A bucket is
at most an eighth of its least value wide, so a value is placed within 12.5%. The precision is
focal's, from what the numbers are used for: its measured claims compare runs that differ by 10–30%,
and the smallest regression worth paging on is a quarter of p99, which at an eighth always lands a
bucket apart, where in `log2` buckets (a factor of two) it can sit in one.

A histogram counts, sums (saturating) and merges bucket by bucket, so an owner gathers several logs'
or owners' histograms without a second copy of the bucket arithmetic. A quantile is read by the
nearest rank as the largest value its bucket holds: the quantile is at most that, and less by an
eighth at most. A metrics page renders count, sum and fixed quantiles (p50, p90, p99, p99.9 and the
most), which keeps its cardinality fixed; tools read the raw buckets.

## 3. Open, to be measured before it is fixed

Items 2, 6 and 7 and part of 3 are settled by the traces (§2.6) and implemented in L-1's estimator
and election law; the remaining items keep their numbers, and item 10 is what L-1 left to L-2.

- **1. Heartbeat cost.** `η` minimizing `U` ignores what heartbeats cost, and the configurator
  shows what that means: on a LAN-like link (0.2 ms mean delay, 0.1 ms deviation, 1 % loss,
  10 ms elections, a month's MTBF) the optimum interval is the timer floor itself, whatever the
  floor (`qos::tests`). On the measured links the optimum is again the floor, now the correlation
  time. At one heartbeat per peer per floor, a node with many peers spends its network on
  liveness. Placement bounds the peers per node, and the cost per heartbeat must be measured
  against the data path and enter `U` before the fleet step. Measured for the node-pair stream
  (§2.8): 0.7 to 1.0 µs of the crate's work a heartbeat sent or taken, configurations included, two plane messages a pair an
  interval, and, on an idle node, one liveness write and flush an interval shared by all its pairs.
  The flush makes the floor `E[flush] + G` the bootstrap interval, until the receiver's first
  configuration asks for its optimum: in the simulation, a few tens of milliseconds once one
  heartbeat holds the margin (§2.8), and the floor itself while the many-heartbeat margins were
  allowed, a flush every half millisecond. What a flush costs a device's other work is the
  measurement to make before `U` prices it.
- **3. The first variance, in part.** A link with no history underestimates `V(D)` by an order of
  magnitude until it has seen a stall (§2.6), and Theorem 7's bound with too small a variance is
  not a bound. Closed by L-1: a delay past everything the history has seen, which no variance
  estimate can know of, is counted at its distribution-free probability `1/(m + 1)` (§2.6; for the
  node-pair stream a lateness past every one in its window, beside Cantelli's factor, §2.2), and
  the estimator refuses to configure before `τ_int` is measured. Open: the
  sampling error of the variance within the range seen, which a heavy tail skews low. On synthetic
  traces of the recorded macOS shapes (`crates/hyper-timing/tests/replay.rs`), 40 seeds of 2, 10
  and 60 minutes each kept Theorem 7's bound with the unseen term and also without it, the term
  halving the first two minutes' mistakes (macOS 100 µs shape: 11 with it, 46 allowed, against 22
  without, 51 allowed; flush shape: 2 with it, 15 allowed, against 8 without, 37 allowed): the models do not show the residual error
  mattering, and they are models. The candidate stands, to be measured on real links: seed a new
  link's history from its node's other links, since the stalls measured here are the hosts', not
  the links'. The fleet's first link has nothing to seed from.
- **4. A group stalled on a live node.** Node-pair detection does not see one group wedged while
  its node is healthy; groups with work still exchange appends, and a follower whose forwarded
  proposals make no progress needs a rule that is not a timer constant.
- **5. Kernel receive timestamps on Windows.** Confirmed against Microsoft's documentation
  (`docs/research/timing.md`, "Winsock timestamping"): `SIO_TIMESTAMPING` exists from build 20348,
  but its receive stamps are attached by a NIC miniport driver that reports timestamping
  capabilities, with system configuration, and on no loopback path or virtual NIC. hyper-tokio
  therefore stamps a Windows datagram when it is read (§2.8, `docs/transport.md` §4b): its read
  delay is counted as the sender's, in the delays the detector measures and so in its margin, and
  in the round trip of an echo of it (§2.8, "The echo": a receiver stopped with heartbeats in its
  socket gives its peers a round trip that holds the stop). Open: every measurement of §2.6 on
  Windows, that read delay among them.
- **8. Strict timers.** A finer `G` shortens detection and costs power; on a laptop on battery
  that trade is measured, not assumed. On Windows the request is `timeBeginPeriod`; on macOS a
  timer with no leeway (real-time or critical urgency, or a strict dispatch timer), without which
  `G` is half of every wait. Neither is measured yet, nor a Linux host with high-resolution
  timers.
- **9. Bare Linux.** Every Linux number here is from Docker Desktop's VM: 1 ms ticks, and a disk
  image that is a file on the Mac. A Linux host with its own disk is to be traced the same way.
- **10. A link younger than its evidence. Resolved; the core's half built in L-2, the detector's
  half in hyper-liveness (§2.8).** The election law has nothing to run on
  before a quorum's paths have answered (no ballot) or before the leader's link has configured its
  detector (a few dozen heartbeats at `T_c`, §2.6, item 3), and a link that stopped receiving never
  gathers its evidence. The cause is that a link's detector judges only from its own history. The
  resolution, with no startup timeout:
  - *The first election needs no detector.* A member that knows no leader campaigns after its draw
    (§2.9): a group opened on links with no history elects as followers that suspected together do.
    It needs only `W`, and the ballot needs one measured round trip on each path of a candidate's
    quorum. QUIC gives every path its first sample in the handshake, before any group message: an
    RTT sample is taken on each ACK that newly acknowledges an ack-eliciting packet (RFC 9002 §5.1),
    the handshake's CRYPTO frames are ack-eliciting, and the first sample sets `smoothed_rtt` and
    `rttvar` (§5.3); the node-pair stream's echo gives one with its first answered heartbeat
    (`Liveness::round_trip`, §2.8). Before a path has a sample no member campaigns, and none could
    hear its vote.
  - *A link that stopped before its evidence is judged by what its node measured.* The evidence
    about a link's delays is not only the link's: §2.6 measured that the stalls are the hosts', and
    hyper-swim already judges a pair with no verdict of its own by its member's pool (§2.7), item
    3's candidate. Until a link's own estimator configures, its trust is judged by the margin the
    node's evidence configures — the wider of its pool's measure and the widest behaviour its
    configured links measured, scaled to the link's window (the prediction errors' variance is
    `V(D)(1 + 1/n)` at a window of `n`) and widened by the link's own errors — against the link's
    own expected arrival, anchored at its first heartbeat, or, for a peer from which none has
    arrived, one interval past the moment the node first attached the pair: an upper bound on the
    link's behaviour wherever the pool's premise holds, and so on its `β` (§2.8, "Judged before its
    own evidence"). That suffices, by this argument: a group elects only with a live majority of
    its voters. For `n ≥ 3` a live majority holds at least two members, so every live voter that
    must detect its leader's crash has a live peer in the group, and the node-pair stream between
    them (L-3 runs one between every two nodes that share a group) gathers its node's evidence: it
    feeds the node's pool, and it configures its own detector — two prediction errors and an Allan
    level of seven windows at Madras and Sokal's `m ≥ 6τ_int`, at least 56 heartbeats, at `T_c` or
    more apart. From the first of the two, the dead link, unconfigured, is judged and suspected at
    its next freshness point. The live link's own configuration is the one to count on: once it
    configures it asks its best interval, seconds on a coarse timer, and the pool, fed at that rate,
    could take minutes more (found by the Windows E2E: 43–118 s to elect on macOS, up to 29 minutes
    on Windows, §2.9). Two voters cannot elect without both, one never suspects, and a node with no
    live link has no peer to elect with. A crash in a link's first heartbeats is so suspected
    within the later of its node's first evidence and the link's freshness point, both measured.
    That evidence is itself bounded by the interval the evidence needs (§2.8): a live link too
    correlated at its floor moves to where its heartbeats are independent instead of refusing, and
    configures there. Held in hyper-liveness's simulation
    (`every_link_configures_or_suspects_a_crash_within_its_bound`,
    `a_peer_never_heard_from_is_suspected`,
    `a_peer_dead_before_its_links_have_evidence_is_suspected_once_a_sibling_has_its_own`), its real
    processes and hyper-durable's shell (`a_leader_killed_before_its_links_have_evidence_is_replaced`).
  - *A restart is not a crash the detectors can miss.* The stream carries the sender's run, a
    count it raises at every start (§2.8), and a later run is an incarnation's end: told to the core (`restarted`), the node is
    trusted and leads nothing it led before. hyper-liveness reports it, `Change::Restarted`, and
    hyper-durable's owner takes it to the core (§2.8, §2.9), as the E2E's members do.
  What stays open is §2.7's for the pool, and so for the widest configured link: its mean is wrong
  for a pair far from the node's other peers until that pair configures (the young link's own
  errors widen the margin as they come, but a peer that dies first has shown few), and while a
  history is young the allowance is loose (item 3).

- **11. A link that changes. Measured: the history stays, the `β` rule goes.** Every estimate the
  configurator reads is over the link's whole history, which is what makes the doubling the right
  renewal (§2.8) and its configurations few, and what makes a link that changes slow to be
  followed: once `V` has a history of `n` heartbeats, a regime of `V' = rV` from heartbeat `n_c` on
  moves it to `(n_c V + (n − n_c)V')/n`, so the change counts fully only once the new regime is
  most of the history. The `β`-doubled rule stood for this case, with a factor no rule gave
  (`V̂`'s relative standard error is `√((κ − 1)τ_int/n)` for errors of kurtosis `κ`: at
  `n = 1,000` independent heartbeats a doubling of `V` is 1.7 standard errors for the kurtosis of
  the 1 ms macOS trace, 357, and 22 for Gaussian delays). Chen, Toueg and Aguilera re-estimate from
  the `n` most recent heartbeats and leave `n` open (`docs/research/timing.md`); the estimator's own
  Allan levels measure the horizon past which averaging stops helping; and the staleness rule, for
  an estimate over a sliding window of `n`, renews every `n/2`. That design was built with the
  per-arrival bound (§2.2) and measured on 2026-10-03: the latenesses over a window sliding at the
  stationarity horizon the link's levels show (the longest level within the least deviation's
  tolerance before a longer one rises past it; the whole history where none rises), kept in
  half-window blocks combined by Chan, Golub and LeVeque's formulas, renewed at each block's close,
  the `β` rule dropped. Against it, the history with the doubling, the `β` rule dropped too, both
  with the evidence interval held until the first configuration (§2.8). In the simulation, worlds
  that change once every pair has configured (a calm macOS host whose freezes then begin; Linux's
  one-way delays stepping tenfold), 16 seeds of four nodes, every node's MTBF held at an hour or a
  day by a seeded fleet history of a thousand failures (left to the run's own exposure, the MTBF
  grew with each run's length and the comparisons followed it), after the change
  (`docs/benchmarks.md`, "The detector model, at its causes"):

  | world, MTBF | estimate | suspicions of live peers / allowance | a pair's an hour | mean spacing | a killed peer noticed (median / 90th / most) | `U` realized |
  |---|---|---|---|---|---|---|
  | freezes begin, an hour | history | 111 / 1,285 | 1.92 | 0.90 s | 1.52 / 1.86 / 2.09 s | 4.5·10⁻⁴ |
  | | sliding window | 614 / 15,563 | 2.40 | 1.65 s | 2.92 / 5.81 / 22.6 s | 1.1·10⁻³ |
  | delays step, an hour | history | 10 / 1,185 | 0.32 | 0.43 s | 0.65 / 1.08 / 1.40 s | 2.1·10⁻⁴ |
  | | sliding window | 38 / 6,906 | 0.45 | 0.63 s | 1.02 / 1.86 / 2.83 s | 3.0·10⁻⁴ |
  | freezes begin, a day | history | 0 / 1,482 | 0 | 2.95 s | 4.55 / 7.14 / 8.16 s | 5.7·10⁻⁵ |
  | | sliding window | 365 / 24,611 | 0.16 | 6.58 s | 10.3 / 26.6 / 101 s | 1.6·10⁻⁴ |
  | delays step, a day | history | 16 / 1,481 | 0.06 | 1.42 s | 2.54 / 4.04 / 5.20 s | 3.0·10⁻⁵ |
  | | sliding window | 25 / 19,116 | 0.02 | 3.23 s | 5.15 / 9.41 / 45.9 s | 7.4·10⁻⁵ |

  "`U` realized" is the measured unavailability: the elections the mistakes caused
  (`T_E` times the suspicions a second) and those a crash costs (the mean time to notice it plus
  `T_E`, once an MTBF). The history did better on every row: fewer suspicions in all four, a crash
  noticed 1.6 to 2.3 times sooner at the median and 2 to 12 times at the most, and `U` 1.5 to 2.8
  times lower, its allowance 12 to 119 times its suspicions (one row had none) against the
  window's 25 to 765.
  The window's cause: its unseen share counts only the window's arrivals, `u ≥ 1/(m_w + 1)`, which
  floors `β` at every margin, and the configurator bought that floor down with longer intervals and
  wider margins. And its horizon is read on levels that keep the whole history at the interval, so
  once a change has shown in them the window never grows back while the link stays at its interval,
  however long the new regime holds. The history's promise just after a change is the looser
  argument (its old latenesses are not exchangeable with the new ones, so its unseen share
  understates a record's chance until the history has doubled past the change), but it kept its
  allowance on every row, and the doubling renews it as the new regime fills the history. So the
  estimate stays over the history, renewed at each doubling, and the `β` rule is gone. A change-point
  test on the latenesses (Page's CUSUM, Biometrika 41, 1954) would need a stated average run length,
  a picked number. hyper-swim's verdict, renewed once a window over estimates of the whole history,
  is as slow to follow a change.
- **12. A campaign on a wrong suspicion.** The election law's expected time to a leader takes the
  suspicion as right: the span minimizes it for a leader that is gone (§2.3). A campaign on a wrong
  suspicion costs what the law does not count: the member knows no leader from its pre-vote until
  its leader answers, so it forwards no proposal and casts no fast-track vote, and the delay that
  outlasts a short wrong suspicion runs no campaign at all. With the detector's mistakes measured
  (Theorem 7's rate and duration), the span could minimize the expected cost over both cases; how
  the measured mistake durations compare with the span decides whether it would move.
  `crates/hyper-raft/ORIGIN.md`, "A draw at every arming", has the first numbers, from schedules
  whose detectors are wrong one time in ten.

## 4. Steps

- **L-1** in `hyper-timing`, sans-io, done. `qos.rs` holds the Theorem 7 bound, the configurator
  and the split-vote span, each checked against a brute-force search and the split probability
  against its order statistic's law, exactly (rationals, every group of two to seven voters); the
  configurator takes the measured floors (`Floors`: `G`, `E[flush] + G`,
  `T_c`) and searches both regimes, one heartbeat in the margin at the base floors and any margin at
  `η ≥ T_c`, keeping the better, and `detector_at` gives the best margin at the interval a link
  sends at now. `link.rs` holds the estimator the traces specified (§2.6): `LinkEstimator`, NFD-E's
  expected arrival over the window `min(n_G, n_A)` under the drift bound, the mean and the variance
  of the prediction errors, `p_L` by Jeffreys, the unseen-delay term, freshness and suspicion events,
  feeding the configurator when its estimates have renewed, refusing it without evidence;
  `folds.rs` the timer-lateness fold for `G`, the flush fold for the sender's floor and the
  exposure fold for the MTBF. `election.rs` holds the election law (§2.3): the ballot from the
  paths' means, the span `W` charged to the detector as `T_E`, the base from the configured
  detector's `η + α`, the delay drawn in time beside the period-counting timer, the priority over
  `PathRtt`'s median with its window from `T_c` (§2.6, item 7); every tail takes the measured `G`.
  `ELECTION_MARGIN`, `PATH_WINDOW` and `GRANULARITY_NS` are gone. Each is bounded and nothing
  allocates once built: a heartbeat costs 77–129 ns, and the law's operations are measured against
  the law before it, focal-timing and slates' (`docs/benchmarks.md`, "hyper-timing against
  focal-timing and slates' timing"); a replay of synthetic traces of the recorded shapes keeps
  Theorem 7's bound, and the trace analyser takes its window, loss and bound from the crate.
- **L-2** the core, done (§2.9): elections started by suspicion with the randomized delay of §2.3,
  check-quorum from the detectors, no per-group timers, idle groups silent, item 10 closed.
- **L-3** the transport, done (§2.8): `crates/hyper-liveness`, one heartbeat stream per node pair on
  the datagram plane, shared by every group the pair shares and silent for a pair that shares none;
  each heartbeat proving a durable flush made after the previous was due; stamped on receipt by the
  kernel through hyper-tokio's plane socket (`SO_TIMESTAMPNS` on Linux, `SO_TIMESTAMP_MONOTONIC` on
  macOS, read-time on Windows, item 5); each pair's NFD-E estimator configured from measured
  floors, by `qos::configure_arrivals` over its latenesses since 2026-10-03 (§2.2, §3 item 11),
  renewed as the window renews; each suspicion stating its bound from the echoed
  round trips. A heartbeat allocates nothing once configured and costs 0.7 to 1.0 µs of the crate's
  work; per node, the stream's cost does not grow with the groups (`docs/benchmarks.md`,
  "hyper-liveness"). Its follow-ups (§2.9) done: every link configures or is judged by its node's
  pool, the interval its evidence needs measured online, a moved interval expected, a restart
  reported (`Change::Restarted`).
- **L-4** the E2E member and harness on L-1–L-3 (§2.5), done: hyper-durable-e2e's and
  hyper-raft-e2e's members run on their own detectors, and neither test computes a period, a tick, a
  tolerance bound or a count of elections (§2.9, "On real detectors").
- **L-5** mantle, focal and slates on it.
