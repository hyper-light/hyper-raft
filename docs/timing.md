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
priority and its window budget (`REPAIR_ROUND_TRIPS`, a protocol fact) stay. What goes are its
picked numbers: `ELECTION_MARGIN = 10` (Raft's "order of magnitude" read as a multiplier, for the
base and the span alike), `PATH_WINDOW = 16`, and `GRANULARITY_NS`, RFC 9002's 1 ms, which is
QUIC's assumption and not this machine's timer.

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
variance. `PathRtt`'s median and MAD stay where they are, in the election timing, whose purpose is
the opposite (one late answer must not move a group's timeout); the detector's estimator is a
separate `PathEstimate` over mean and variance.

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
- `V(D)`, still open: the first heartbeats underestimate it badly. Over consecutive blocks of `m`
  heartbeats, the median block's deviation is 1–18 % of the run's on every run with a tail, for `m`
  from 2 to 2,048 (78–83 % on idle Linux from `m = 8`): the variance lives in stalls a short window
  has not yet seen. A link's `V(D)` is therefore the variance over its whole history, not over the
  window; what a link with no history uses until it has seen its node's stalls is §3, item 3.

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

## 3. Open, to be measured before it is fixed

Items 2, 6 and 7 and part of 3 are settled by the traces (§2.6); the remaining items keep their
numbers.

- **1. Heartbeat cost.** `η` minimizing `U` ignores what heartbeats cost, and the configurator
  shows what that means: on a LAN-like link (0.2 ms mean delay, 0.1 ms deviation, 1 % loss,
  10 ms elections, a month's MTBF) the optimum interval is the timer floor itself, whatever the
  floor (`qos::tests`). On the measured links the optimum is again the floor, now the correlation
  time. At one heartbeat per peer per floor, a node with many peers spends its network on
  liveness. Placement bounds the peers per node, and the cost per heartbeat must be measured
  against the data path and enter `U` before the fleet step.
- **3. The first variance.** A link with no history underestimates `V(D)` by an order of
  magnitude until it has seen a stall (§2.6), and Theorem 7's bound with too small a variance is
  not a bound. The candidate, to be measured: seed a new link's variance from its node's other
  links, since the stalls measured here are the hosts', not the links'. The fleet's first link
  has nothing to seed from.
- **4. A group stalled on a live node.** Node-pair detection does not see one group wedged while
  its node is healthy; groups with work still exchange appends, and a follower whose forwarded
  proposals make no progress needs a rule that is not a timer constant.
- **5. Kernel receive timestamps on Windows**, to confirm against Microsoft's documentation, and
  every measurement of §2.6 on Windows.
- **8. Strict timers.** A finer `G` shortens detection and costs power; on a laptop on battery
  that trade is measured, not assumed. On Windows the request is `timeBeginPeriod`; on macOS a
  timer with no leeway (real-time or critical urgency, or a strict dispatch timer), without which
  `G` is half of every wait. Neither is measured yet, nor a Linux host with high-resolution
  timers.
- **9. Bare Linux.** Every Linux number here is from Docker Desktop's VM: 1 ms ticks, and a disk
  image that is a file on the Mac. A Linux host with its own disk is to be traced the same way.

## 4. Steps

- **L-1** in `hyper-timing`, sans-io (in part: `qos.rs` holds the Theorem 7 bound, the configurator
  and the split-vote span, each checked against a brute-force search and the split probability
  against a Monte Carlo; the traces settled the estimator's inputs, window and floors (§2.6), which
  it is still to implement, and the first variance stays open): the NFD-E estimator as a
  `PathEstimate` over mean and variance with the window `min(n_G, n_A)`, the floors `G` and
  `E[flush] + G` and the independence rule `η ≥ T_c` when `α ≥ η`, the Theorem 7 bounds, the
  configurator minimizing `U`, the split-vote model and `W` replacing `ELECTION_MARGIN`'s base and
  span, the granularity probe replacing `GRANULARITY_NS`. Unit and property tests; benchmarks
  under the allocation law against the current derivation (`hyper-timing-compare`).
- **L-2** the core: elections started by suspicion with the randomized delay of §2.3,
  check-quorum from the detectors, no per-group timers, idle groups silent.
- **L-3** the transport: one heartbeat stream per node pair on the datagram plane, stamped on
  receipt by the kernel, carrying proof of a recent log flush.
- **L-4** the E2E member and harness on L-1–L-3 (§2.5); the harness's computed tick and budgets
  go.
- **L-5** mantle, focal and slates on it.
