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
history. `η` has a floor: the timer granularity `G` (§2.4), and Chen et al.'s independence
assumption that heartbeats further apart than the link's correlation time behave independently.
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

- **Granularity.** A timed wait ends on the OS's timer, not when asked: Windows on its
  15.625 ms clock interrupt unless the process asks for finer (`timeBeginPeriod`, per process
  since Windows 10 2004), Linux up to the thread's timer slack late (50 µs default), macOS by
  the timer's leeway. `G` is measured as the lateness of the detector's own waits and floors
  `η`, `α` and the estimates, as RFC 6298 floors its variance term by `G`.
- **Local health.** A detector that wakes late must not blame its peers (Lifeguard). Arrivals are
  stamped by the kernel when the datagram is received (`SO_TIMESTAMP`-class socket options), so a
  heartbeat that arrived before its freshness point is fresh however late the process read it;
  only the send side of a slow node looks slow, which is true.

### 2.5 What a test does

A test runs this mechanism and asserts what it promises: every suspicion of a node that was
killed comes within the detection bound the configurator computed for that link; elections end;
nothing answered is lost. It waits on facts — a leader, a term, a commit — and fails when the
mechanism's own stated bound passes without them. It computes no tick and no budget of its own.

## 3. Open, to be measured before it is fixed

1. **Heartbeat cost.** `η` minimizing `U` ignores what heartbeats cost, and the configurator
   shows what that means: on a LAN-like link (0.2 ms mean delay, 0.1 ms deviation, 1 % loss,
   10 ms elections, a month's MTBF) the optimum interval is the timer floor itself, whatever the
   floor (`qos::tests`). At one heartbeat per peer per floor, a node with many peers spends its
   network on liveness. Placement bounds the peers per node, and the cost per heartbeat must be
   measured against the data path and enter `U` before the fleet step.
2. **NFD-E's estimation window `n`.** Larger `n` estimates `EA` more exactly (standard error
   `√(V(D)/n)`) and adapts more slowly. Candidates: the `n` whose error falls below `G`, which
   nothing finer can observe, bounded by the time over which the link's measurements stay
   stationary — to be measured.
3. **First estimates.** A link with no history and a fleet with no failure history need
   starting values for `E(D)`, `V(D)`, `p_L` and `MTBF`; the measured startup probes give the
   first three; `MTBF` needs a stated prior or a bound from exposure without failure.
4. **A group stalled on a live node.** Node-pair detection does not see one group wedged while
   its node is healthy; groups with work still exchange appends, and a follower whose forwarded
   proposals make no progress needs a rule that is not a timer constant.
5. **Kernel receive timestamps on Windows**, to confirm against Microsoft's documentation.
6. **The link's correlation time**, below which heartbeats are not independent (Chen et al.'s
   assumption behind Theorem 4), as a second floor on `η`: measured from the autocorrelation of
   the link's delays.
7. **Robust estimates against the bounds' assumptions.** `hyper-timing` estimates a path by the
   median and median absolute deviation of its latest round trips, so a peer answering seconds
   late while it starts or stalls does not move the timing; Chen et al.'s Theorem 7 needs the
   delay's mean and variance, which such answers inflate. Whether those answers are delay the
   detector must cover (a stalled disk is a failure, §2.1) or outliers it must not, decides
   which estimate feeds the bound; to be settled on measured traces of both.
8. **Whether to ask Windows for a finer timer.** A finer `G` shortens detection and costs power;
   on a laptop on battery that trade is measured, not assumed.

## 4. Steps

- **L-1** in `hyper-timing`, sans-io (in part: `qos.rs` holds the Theorem 7 bound, the
  configurator and the split-vote span, each checked against a brute-force search and the split
  probability against a Monte Carlo; the estimator and the granularity fold wait on traces for
  open items 2, 3 and 7): the NFD-E estimator as a `PathEstimate`, the Theorem 7
  bounds, the configurator minimizing `U`, the split-vote model and `W` replacing
  `ELECTION_MARGIN`'s base and span, the granularity probe replacing `GRANULARITY_NS`. Unit and
  property tests; benchmarks under the allocation law against the current derivation
  (`hyper-timing-compare`).
- **L-2** the core: elections started by suspicion with the randomized delay of §2.3,
  check-quorum from the detectors, no per-group timers, idle groups silent.
- **L-3** the transport: one heartbeat stream per node pair on the datagram plane, stamped on
  receipt by the kernel, carrying proof of a recent log flush.
- **L-4** the E2E member and harness on L-1–L-3 (§2.5); the harness's computed tick and budgets
  go.
- **L-5** mantle, focal and slates on it.
