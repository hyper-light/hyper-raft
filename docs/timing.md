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
time `EA_i`, estimated from recent arrivals. Its quality follows from the measured loss
probability `p_L` and delay variance `V(D)` without knowing the delay distribution (Theorems 7–8):
detection within `E(D) + α + η`, and mistakes no more often than a bound in `η`, `α`, `p_L`,
`V(D)`.

Chen et al. configure `η` and `α` from requirements an application states. hyper-raft has no
such application, and a requirement it chose would be a constant it picked. Instead it chooses
`η` and `α` to minimize what the requirements stand for, the time a group cannot commit:

    U(η, α) = (E(D) + α + η + T_E) / MTBF  +  T_E / E(T_MR)(η, α)

The first term is a crash: detected within `E(D) + α + η`, then an election of `T_E`, once per
`MTBF` of the leader's node. The second is a false suspicion: an election for nothing, once per
mistake recurrence time, bounded by Theorem 7. Every input is measured: `E(D)`, `V(D)`, `p_L`
from the link's heartbeats, `T_E` from elections (§2.3), `MTBF` from the membership's failure
history. `η` has floors, each measured (§2.6): the timer granularity `G` (§2.4); the sender's
stability, `η > E[flush] + G`; and Chen et al.'s independence assumption, which holds only for
heartbeats at least the link's correlation time `T_c` apart, so `η ≥ T_c` whenever the margin
holds more than one heartbeat.
The minimum is found numerically each time the estimates move, as Chen et al.'s adaptive scheme
does.

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
- **The span** `W` is `election_span` on the ballot, searched to within `G`. Its `T_E` is the
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
  repair is that refusal reaching the leader and the resend reaching the follower, and no shorter
  or longer one exists. RFC 9002's gains and its `4·rttvar` are the standard's constants, cited.

On the comparison workload (five voters, four WAN paths of 40–58 ms, a 10 ms heartbeat, macOS's
`G` and `T_c`), the law before L-1 gave a base of `10 × tail` and a span of `10 × spread`; the law
now gives the base from the detector and the span from the ballot (`docs/benchmarks.md`,
"hyper-timing against focal-timing and slates' timing", has the values and the costs).

### 2.4 The local clock and the local machine

- **Granularity.** A timed wait ends on the OS's timer, not when asked. `G` is the mean lateness
  of the detector's own waits, measured: the mean because the waits feed queues (the sender's
  schedule, the detector's checks) whose stability and expected delay depend on it (§2.6). It
  floors `η`, `α` and the estimates, as RFC 6298 floors its variance term by `G`. Measured on
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
interval on every run.

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
memory and bounded work a heartbeat, and the trace analyser now takes its window from it:
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
  Theorem 7's factor with `p` for `p_L`. The estimator feeds the configurator `p` as the loss, so a
  young link's margin cannot promise better than its history excludes, and the configurator spreads
  the margin over more heartbeats until the history has shown more. It refuses to configure until
  it has the evidence `m` needs: two prediction errors (a variance) and a measured `τ_int` (an Allan
  level holding Madras and Sokal's window), a few dozen heartbeats at the correlation-time floor.
  What stays open is §3, item 3.

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

**Death.** A suspected peer is told by the member's next probe of it, which carries the suspicion
(Lifeguard's buddy system); if that probe too goes unanswered, the peer is condemned at the next
answer the member has from another member. Lifeguard found that "an episode of slow message
processing at a given member is likely to impact multiple of its interactions" (§IV) and counted
independent suspicions as evidence the local member processes messages in time; an answer from
another member is that evidence, measured on the member's own round trips. A member whose own
network has failed suspects everyone and condemns nobody. Gossiped suspicions are hints: only a
member's own probes condemn. A suspicion re-adopted at a newer incarnation keeps the probes that
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
ran rounds of one, while the victim's last probe had been in a round of three).

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
unless its answer comes later; it judges nothing, and its wake measures `G`. The very first probe,
before any round trip, waits on an answer or another member. An earlier rule ended an unanswered
measurement period only when another member was next heard from, assuming no time at all; in Linux
at two CPUs with four busy loops, a throttled container dropped a burst of datagrams, every member's
probe was lost at once, and all four waited for one another for ever
(`a_lost_measurement_probe_ends_at_its_expected_arrival`). A member that hears nothing has nothing
to judge with and nobody to probe usefully, and waits on the network.

**The member's own lateness** (Lifeguard's local health) is measured, not multiplied:
- every wake it asked for and got late is a sample of `G` ([`Lateness`]), which floors `α`;
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
(`codec::gossip_capacity`): 60 entries at QUIC's 1,200-byte minimum.

**Memory.** Each peer's estimator holds a ring of `G/(PHI·η) − 1` sums at the pair's interval `η =
m·T̄` (§2.6, the drift bound), so a member's rings together hold about `G/(PHI·T̄)` whatever its
membership, and the pool as many again: at most `2/PHI` slots (1.1 MB) when `T̄` is at its floor
`G`. Each estimator is boxed, so the fields a period reads stay together. A period allocates nothing
once each peer has answered once (`docs/benchmarks.md`, "hyper-swim").

**What the cluster test asserts** (`crates/hyper-swim/tests/cluster.rs`, §2.5). Four member
processes run the detector as the library configures it. The supervisor waits on facts: every member
judging every peer by a configured verdict (the pair's own, or the pool's while the pair's estimator
refuses), then it kills one; every survivor holds it dead, each within the bound its detector
stated, measured on the member's clock from the victim's last answer. Waiting for every pair's own
estimator was not a fact to wait on: under a CPU throttle a pair's round trips can stay too
correlated for `τ_int` to be measured, and the estimator rightly refuses for as long as that lasts.
Of live members it asserts what the configuration promises: Theorem 7 bounds the expected number of
suspicions and of condemnations by `Σβ`, and a run refutes that only when the 95 % lower limit of
its count (the Poisson score interval, as the replay and the trace analyser use, §2.6) passes it. A
first form asserted the count itself within `Σβ`, which no detector can promise of one run: at an
allowance of 1.8, two mistakes are ordinary, and twelve runs in a hundred at one CPU failed so while
the series as a whole kept far inside its bound. The rule itself refutes a bound that holds with
probability at most 2.5 % an assertion, under the Poisson model the replay uses; on unloaded hosts
the runs' counts are far below their allowance and it does not arise. A member whose supervisor is
gone ends when its report cannot be written.

**Measured** (`docs/benchmarks.md`, "hyper-swim"): on loopback a period is 0.1–1 ms, `μ` 60–100 µs
and `α` growing from about 0.3 ms with the MTBF; a period costs no allocation and less time than
slates' at every point; 2,000 runs of the cluster test on macOS and on Linux at one, two and four
CPUs with busy loops beside them all passed, detection a median 4 ms on macOS and 16–18 ms in
Docker's VM after the victim's last answer. Open: §3, item 1 governs the probe rate too, since a
period is its probe's deadline and nothing yet prices a probe, and as the MTBF grows the margins and
so the periods grow with it; two members cannot condemn each other, as neither can tell its own
failure from the other's; the pool's mean is wrong for a pair far from the member's others until
that pair configures; and the allowance is loose while a history is young (§3, item 3).

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
(`LinkEstimator`), the configurator (`qos`), the timer fold (`Wakes`, moved out of hyper-swim for
this), the MTBF fold (`Exposure`) and the refutation rule (`poisson95`, which was written out three
times). The wire is the crate's own (`codec.rs`, one plane message, its first byte `KIND`), so an
owner multiplexing the plane tells it apart; the kernel stamps come from the owner's socket
(hyper-tokio's `PlaneSocket`, §2.4; slates' runtime its own).

**The stream.** A node sends each peer heartbeat `k` due at `σ_k = σ_{k−1} + η`, carrying its run
(`boot`), `k`, `η`, its stability floor `E[flush] + G`, the interval it asks of the peer, its send
time and lateness past `σ_k`, and the flush proof. A sender behind its schedule sends the latest
heartbeat due; those it skipped are losses to the receiver, which they are.
- **The interval** is the receiver's: its configurator's best (`Configuration::best`), asked in its
  own heartbeats (Chen et al.'s adaptive scheme), never below the sender's floor, nor below the
  interval its own evidence needs (below, "The interval the evidence needs"). Where the floor binds
  (before the receiver asks, or past what it asked) the interval is the floor, followed up and not
  down: an interval below the floor is unstable (Lindley 1952), but a floor that fell is a mean that
  moved with a sample, and following it down started the receiver's estimator again at every move
  (`LinkEstimator::retime`), which kept a link whose flushes stalled now and then unconfigured for
  thousands of heartbeats (§2.9). A change within `G`, the configurator's resolution, is none.
  Bootstrap: the first heartbeat waits on the first flush, whose time is the first `E[flush]`; the
  first wake measures `G`.
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
  The receiver asks at least that interval from then on (`max(best, evidence)`), never one it showed
  it cannot measure at. On an AR(1) delay of correlation time 199 ms with heartbeats at 1 ms, which
  refuses at that interval past a thousand heartbeats, 64 seeds configured within 576 heartbeats, at
  a final interval of 166 ms at the median and 1.26 s at the most (`link::tests`). The margin still
  holds one heartbeat: the measured `T_c` is the evidence's floor, not a licence for Theorem 7's
  product, which a young link's history cannot vouch for (below).
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
  heartbeat came is a suspicion in whatever order the owner feeds messages and polls. Configured
  through `qos::configure` with `Floors { G: the receiver's measured G, sender: the sender's
  advertised floor, correlation: unbounded }`, so the margin holds one heartbeat (`α < η`) and
  Theorem 7's bound is a single Cantelli factor that assumes no independence, as hyper-swim judges
  each probe (§2.7). `T_c` is the spacing past the longest stall a run saw (item 6), which a young
  link has not seen; a first form took the link's own `τ_int·η` for it, and under a one-CPU throttle
  the many-heartbeat margins it allowed broke their allowance in most runs (`docs/benchmarks.md`,
  "hyper-liveness"). `T_c` measured online (above) is where a link's evidence is resolved, not the
  spacing past every stall its host will see; whether a link may count on the product past it stays
  open. `Costs { election: T_E from the owner (set_election, the election law's span over the
  groups the pair shares), mtbf: the Jeffreys posterior over
  the node time the pairs watched and the restarts and abandoned suspicions seen, seeded with the
  fleet's history }`.
- **Renewal on a doubling schedule.** The configurator runs when never configured, when the peer
  moved to the interval asked, when the heartbeats taken have doubled since the last configuration
  (the history the inputs are estimated over, and the exposure the MTBF is, doubled with them), or
  when `β` at the margin in force has doubled past the configured one. The estimator's window
  cadence configured every few heartbeats at short intervals and spent five times the stream's own
  work on the configurator (`docs/benchmarks.md`, "hyper-liveness"). The allowance is kept sound
  between configurations: each freshness point is charged `β` at the margin in force from the
  estimates as they stand, not as the configuration assumed.
- **The echo** (RFC 5905 §8's on-wire round trip; RFC 3550 §6.4.1's LSR and DLSR). Each heartbeat
  echoes the latest heartbeat its sender had from the receiver: that one's send time and lateness on
  the receiver's clock, and the hold since its arrival. The receiver gets the network round trip on
  its own clock, `A − s_echo − hold` (a path for the election law's ballot, `round_trip`), and the
  sum of the two directions' delays from their schedules, `round trip + late_echo + late`, which
  bounds this heartbeat's delay since both are positive.
- **Judged before its own evidence: the node's pool** (§3, item 10). A link whose own estimator
  has not configured is judged by the margin its node's pool configures for it. The pool is one more
  `LinkEstimator`, fed the prediction errors `A − EA` of every link without a configuration of its
  own, and of every link until the pool has its evidence (hyper-swim's rule, §2.7; a pool fed by the
  young links alone, whose links all configured before it could measure, never measured, and a peer
  never heard from was never judged: found by `a_peer_never_heard_from_is_suspected`), numbered by
  the heartbeats due across the links so a heartbeat lost on any is a loss to it
  (`LinkEstimator::on_offset`). The errors carry no clock offset, so links whose clocks differ pool.
  The pool's behaviour for link `L` is its own with the deviation scaled by `√(1 + 1/n_L)` for `L`'s
  window `n_L`: the prediction errors' variance at a window of `n` is `V(D)(1 + 1/n)` for independent
  delays and the pool's is at least `V(D)`, so the scaled variance bounds `L`'s from above, the side
  Cantelli's inequality may err on. Its margin is `detector_at` at `L`'s interval, costs and floors,
  imposed on `L`'s estimator (`LinkEstimator::impose`), renewed on the doubling schedule, at a poll
  as soon as the pool can give one, and charged to the allowance at `β` from the scaled behaviour. What
  the pool measured stands when a later stretch makes its `τ_int` unmeasured again
  (`pool_measured`), as hyper-swim's verdict and `configure`'s margin stand. A peer from which no
  heartbeat has come is judged from the node's first poll with the pair attached: one interval at
  the node's own floor (the pool's premise is that the stalls, and the floors, are the hosts') and
  the pool's margin for a window of one; its suspicion states that time as its bound, from the
  attach. `PairReport::judged` says a margin judges, `PairReport::freshness` the `η + α` in force.
- **A restart** (§3, item 10). A heartbeat of a new run (`boot`) is reported as
  `Change::Restarted { peer, at_ns }` before the trust the heartbeat leaves, for the owner to call
  the core's `restarted` (trusted, and leading nothing it led); the owner is then told a suspicion
  only if the heartbeat leaves the peer suspected. Changes are reported only where what the owner
  was told differs: the owner trusts a peer until told otherwise, so trust after a suspicion is told
  and trust after nothing is not.
- **The detection bound** each suspicion states: NFD-E suspects at `τ_{h+1} = EA_{h+1} + α`, which is
  `η + α + mean(D)` past the last heartbeat's schedule over the expected arrival's window, whatever
  the clocks' offset; the mean of the echoed sums over the same window bounds `mean(D)`, so
  `η + α + ⌈mean of the sums⌉` bounds the time from the sender's last schedule, and so from its
  crash, to the suspicion. No clock synchronization or path symmetry enters it. Unstated while a
  heartbeat in the window carried no echo.

**The API** the core's suspicion-started elections (L-2) and the shell consume (`src/lib.rs`):
`attach`/`detach` a group's peer; `on_durable(write, started, durable)`; `on_heartbeat(from, message,
arrival_ns, out)`; `poll(now, out)` and `wake()`, with `Output::{heartbeat, flush, change}`;
`Change::Suspected(Suspicion { peer, at_ns, noticed_ns, last: { seq, arrival_ns, due_ns, sent_ns },
detection, detector })`, `Change::Trusted { peer, at_ns }` and `Change::Restarted { peer, at_ns }`;
`trust(peer)`, `suspected()`,
`configuration(peer)` (the election law's base is `current.interval + current.margin`),
`round_trip(peer)`, `report(peer)` (`sent`, `taken`, `unproven`, `suspicions`, `allowance`,
`configurations`, `configured`, `judged`, `freshness`), `set_election(peer, T_E)`, `flush_mean()`, `granularity()`, `floor()`, `mtbf()`.
Every refusal is typed. The owner's contract: feed every message stamped before a time before
polling at it (hyper-tokio's `PlaneSocket::receive_ready`); the core takes `suspect(node)`,
`trust(node)` and `restarted(node)` from the changes (hyper-durable's `Owner::believe`).

**Bounds.** Pairs at most `Settings::max_peers` (placement's), typed refusal past it; groups per pair
a `u32`; one liveness write out at a time; per pair one boxed estimator and a ring of delay sums the
estimator's own size (the drift bound, §2.6), both resized in place for a longer interval; per node
one pool, an estimator at the first fed link's interval, boxed. Once each pair is configured, a
heartbeat sent and one taken allocate nothing.

**Tests.** `tests/sim.rs`, one clock, seeded delays, stalls, losses, flushes and wake lateness, the
owners computing `T_E` from the library's law: live peers keep Theorem 7's allowance (8 seeds); no
heartbeat leaves without a newer flush made after the previous was due (with and without the
groups' own writes); a killed peer is suspected by every survivor within the bound each states from
the peer's last schedule (16 seeds); a stalled disk is suspected so too, and the stalled node still
trusts its live peers (8 seeds); a thousand groups send what one does and an unshared pair is
silent; a restarted peer is reported restarted once by each other node, trusted again by the
detector in force and counted; every refusal. Problem 1 and item 10, over seeds 0 to 31 in each of
three worlds, a LAN, one whose hosts freeze for up to 50 ms about every 250 ms (macOS's measured
`T_c`, a hundred heartbeats at the floor in each freeze), and one whose groups write every few
milliseconds to a device that stalls one flush in fifty for up to 60 ms
(`every_link_configures_or_suspects_a_crash_within_its_bound`): every pair configures, none taking
more heartbeats unconfigured than any window holds (`WINDOW_LIMIT`, the drift bound's ceiling: a link
whose estimator has not measured its correlation in as many heartbeats as any window could average
is one no window resolves), at most 302 heartbeats; then one node is killed after a share of the
heartbeats it sent before every pair configured in the same seed's run, drawn between none and twice
as many, and every survivor suspects it, each suspicion within the bound it states, by its own
configuration's margin (169), the pool's (117), or, for a node never heard from, from the attach (2).
Before the fix, the stalling world kept a link unconfigured for 5,775 heartbeats (its floor
followed down with each decaying mean, the estimator started again at each move), and a world
that is too correlated at its floor is refused by the estimator however long it runs
(`link::tests`). A peer never heard from is suspected by every other within the bound from the
attach (16 seeds).
Property tests: the codec reads back what it writes and refuses every truncation, extension, kind
and version; the bound is its window's mean. `tests/processes.rs`, real processes over UDP on the
sealed plane through hyper-tokio's kernel-stamped socket, each liveness write a real write and
platform flush of a real file: one member's disk is stalled (its device thread stops completing
flushes) and every other suspects it within its stated bound from the last schedule, on the host's
monotonic clock, which the processes share; then one is SIGKILLed and every survivor suspects it so;
live members keep the allowance. The test derives nothing; it waits on facts.

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
- `RawNode::set_timing(Timing { span, round })`: `Timing::of(&ballot, &span)` from the law of §2.3,
  `span` the `W` the ballot chose and `round` the ballot's `broadcast_tail` (the slowest voter path's
  tail over `G`, plus the mean flush). Given again whenever the ballot moves.
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
  attempt)` uniform on `[0, W)` (`ElectionTiming::delay` is the same draw). It trusts a leader while
  it knows one, its detector does not suspect it, and the configuration it applied names it a voter
  (a leader that is none steps down once it applies that, and a member that applied it knows it was
  committed). A suspicion withdrawn before the delay ends cancels the campaign.
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
  hold as much, to each in turn, one a hand-over (`Watch::attempt`): one that restarted knows nothing
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
sends no Raft message while idle but after a detector's change (2,156 over 7,142 changes), its
leader's node killed, every survivor suspects it within the bound the suspicion states and the
survivors elect and commit, and the killed node started again on its store, a new run of its
stream, every survivor's stream reports its restart to its core and the node catches up. The quiet
period its waits use is the stream's own: the longest `η + α` in force (`PairReport::freshness`,
which counts an interval asked and not yet taken), the election's span and rounds, a flush and a
network round.

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
after a quiet period of the members' own law (the longest `η + α` any member states, its election's
span and three rounds, an ask's three), never less than its own retransmission timeout (RFC 6298:
one second before a round trip is measured and never less after, §2.1, §2.4). An ask is resent at
that timeout. A member just started is waited on while its process runs. Its count bounds are the
protocol's: a member passes each durability point at least once for each turn of writes it takes
(the most a turn takes, `max_pending + voters + 1`), so a target that has not stopped after that
many answered writes is a failure. New scenarios: a stalled disk (the member's file stops
completing flushes, so its heartbeats stop; every other member suspects it and the group elects
without it) and, at every restart, every member that heard the last run and shares a group with
the restarted one reports the restart its stream saw. The member's shell writes its commit alone at
the first moment no write is out (`Settings::quiet` zero): an owner woken by events has no period.
Measured in `docs/benchmarks.md`, "hyper-durable-e2e on its own detectors".

**The model.** These rules only bring forward or refuse a campaign, or forget a leader, which is
volatile; the TLA+ model's `Elect` may be taken at any time with any quorum the log comparison
admits, so it needs no change (`docs/models/README.md`).

**Evidence.** `crates/hyper-raft/tests/suspicion.rs`, a group in time on one-way latencies: an idle
group sends nothing and is due for nothing for a simulated day; elections start only on suspicion;
the delay is the law's draw exactly and uniform over 20,000 members; split votes resolve, the first
rounds of 1,000 crashes of a five-voter leader splitting 135 times against Ongaro's 110.8 (`split`
of `election_span` on the same latency and round, inside the 99.9 % interval); a withdrawn
suspicion cancels; pre-vote refused by members that trust the leader; step-down on a suspected
majority of either half of a joint configuration; hand-overs; a held member campaigns for nothing;
a restarted leader forgotten; a leader that beats while work is in flight and then sleeps.
`tests/pipeline.rs` runs the R-4 durability oracle with suspicion-driven elections at its four
settings, the fast track and the crash at every persistence step, its detectors right nine times in
ten about a member that is down or cut off and wrong one time in ten about one that is not
(`crates/hyper-raft/ORIGIN.md`, "L-2", has the counts).

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
  estimate can know of, is counted with the loss at its distribution-free probability
  `1/(m + 1)` (§2.6), and the estimator refuses to configure before `τ_int` is measured. Open: the
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
  delay is counted as the sender's, in the delays the detector measures and so in its margin. Open:
  every measurement of §2.6 on Windows, that read delay among them.
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
  - *A link that stopped before its evidence is judged by its node's pool.* The evidence about a
    link's delays is not only the link's: §2.6 measured that the stalls are the hosts', and
    hyper-swim already judges a pair with no verdict of its own by its member's pool (§2.7), item 3's
    candidate. Until a link's own estimator configures, its trust is judged by the margin the node's
    pool configures, scaled to the link's window (the prediction errors' variance is
    `V(D)(1 + 1/n)` at a window of `n`), against the link's own expected arrival — anchored at its
    first heartbeat, or, for a peer from which none has arrived, one interval past the moment the
    node first attached the pair: built, with the pool's own behaviour scaled by `√(1 + 1/n)`, an
    upper bound on the link's (§2.8, "Judged before its own evidence"). That suffices, by this
    argument: a group elects only with a live
    majority of its voters. For `n ≥ 3` a live majority holds at least two members, so every live
    voter that must detect its leader's crash has a live peer in the group, and the node-pair
    stream between them (L-3 runs one between every two nodes that share a group) feeds its node's
    pool. The pool reaches its evidence — two prediction errors and an Allan level of seven windows
    at Madras and Sokal's `m ≥ 6τ_int`, at least 56 heartbeats, at `T_c` or more apart, sooner
    when several live links feed it — and from then the dead link, unconfigured, is suspected at its
    next freshness point. Two voters cannot elect without both, one never suspects, and a node whose
    pool has no live link has no peer to elect with. A crash in a link's first heartbeats is so
    suspected within the later of the pool's evidence and the link's freshness point, both measured.
    The pool's evidence is itself bounded by the interval the evidence needs (§2.8): a live link
    too correlated at its floor moves to where its heartbeats are independent instead of refusing,
    so the pool, which needs the same evidence, reaches it. Held in hyper-liveness's simulation
    (`every_link_configures_or_suspects_a_crash_within_its_bound`, `a_peer_never_heard_from_is_suspected`).
  - *A restart is not a crash the detectors can miss.* The stream carries the sender's run
    (`boot`, §2.8), and a new run is an incarnation's end: told to the core (`restarted`), the node is
    trusted and leads nothing it led before. hyper-liveness reports it, `Change::Restarted`, and
    hyper-durable's owner takes it to the core (§2.8, §2.9), as the E2E's members do.
  What stays open is §2.7's for the pool: its mean is wrong for a pair far from the node's other
  peers until that pair configures, and while a history is young the allowance is loose (item 3).

## 4. Steps

- **L-1** in `hyper-timing`, sans-io, done. `qos.rs` holds the Theorem 7 bound, the configurator
  and the split-vote span, each checked against a brute-force search and the split probability
  against a Monte Carlo; the configurator takes the measured floors (`Floors`: `G`, `E[flush] + G`,
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
  macOS, read-time on Windows, item 5); each pair's NFD-E estimator configured by `qos::configure`
  from measured floors on a doubling schedule; each suspicion stating its bound from the echoed
  round trips. A heartbeat allocates nothing once configured and costs 0.7 to 1.0 µs of the crate's
  work; per node, the stream's cost does not grow with the groups (`docs/benchmarks.md`,
  "hyper-liveness"). Its follow-ups (§2.9) done: every link configures or is judged by its node's
  pool, the interval its evidence needs measured online, a moved interval expected, a restart
  reported (`Change::Restarted`).
- **L-4** the E2E member and harness on L-1–L-3 (§2.5): hyper-raft-e2e's harness still computes its
  tick and budgets; hyper-durable-e2e's runs on its members' own detectors, its computed period and
  budgets gone (§2.9, "On real detectors").
- **L-5** mantle, focal and slates on it.
