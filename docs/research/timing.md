# Research: Raft timing from measurement, at any scale

Source notes for `docs/timing.md`. Each entry says what the source establishes, verified against
the source text on 2026-10-01, and what it leaves open.

## Failure detection as a quality of service

**Chen, Toueg, Aguilera, "On the Quality of Service of Failure Detectors", DSN 2000, pp. 191–200;
extended in IEEE Transactions on Computers 51(5), May 2002, pp. 561–580.**

- A failure detector's quality is specified by primary metrics: the detection time `T_D` (crash to
  permanent suspicion), the mistake recurrence time `T_MR` (between false suspicions) and the
  mistake duration `T_M`; the query accuracy probability is `P_A = 1 − E(T_M)/E(T_MR)`
  (Theorem 1 relates all six metrics).
- NFD-S: `p` sends heartbeat `m_i` at `σ_i = iη`; `q` trusts `p` at time `t` iff it received a
  heartbeat still fresh at `t`, freshness point `τ_i = σ_i + δ`. Theorem 4: `T_D ≤ δ + η`
  whatever delays and losses do; `E(T_MR) = η / p_S`; `E(T_M) = ∫₀^η u(x) dx / p_S`, with `p_S`
  and `u` from the loss probability `p_L` and the delay distribution (Proposition 3).
- Theorem 5: among all detectors that send heartbeats every `η` and bound detection by `T_D^U`,
  NFD-S with `δ = T_D^U − η` has the best query accuracy probability.
- Configuration (Section 4, Theorem 6): given requirements `(T_D^U, T_MR^L, T_M^U)` and the
  heartbeat behaviour, find the largest `η` meeting them by a numerical search, then
  `δ = T_D^U − η`.
- Distribution-free configuration (Section 5, Theorems 7–8): with only `p_L`, `E(D)` and `V(D)`,
  the one-sided (Cantelli) inequality `Pr(D > t) ≤ V(D) / (V(D) + (t − E(D))²)` for `t > E(D)`
  gives bounds on `E(T_MR)`, `E(T_M)`, `P_A`; the procedure computes `η` and `δ` from them and
  meets the requirements provided `T_D^U > E(D)`.
- Estimation: `p_L` from heartbeat sequence numbers; `E(D)`, `V(D)` from timestamps.
- Adaptive (Section 6): re-estimate `p_L`, `E(D)`, `V(D)` from the `n` most recent heartbeats
  and feed them back into the configurator.
- Unsynchronized clocks (NFD-E; Toueg's DSN 2002 workshop slides, `dependability.org/wg10.4/
  timedepend/02-Toueg.pdf`, and the 2002 journal version): freshness points are shifted from
  *expected arrival times*, `τ_i = EA_i + α`, with
  `EA_{n+1} ≈ (1/n) Σ_{i=1..n} (A_i − iη) + (n+1)η` over the arrival times `A_i` of the `n` most
  recent heartbeats; the QoS analysis is NFD-S's with `δ = E(D) + α`, and the configurator needs
  only `p_L` and `V(D)`.
- Left open: the window `n` ("with appropriate n, the estimates can be very accurate"), and
  where the requirements come from ("could be given by the application").

**Hayashibara, Défago, Yared, Katayama, "The φ accrual failure detector", SRDS 2004.** Outputs a
continuous suspicion level adapted to observed inter-arrival times instead of a boolean;
evaluated over an intercontinental link. Leaves the threshold on φ to the application.

**Dadgar, Phillips, Currey, "Lifeguard: Local Health Awareness for More Accurate Failure
Detection", DSN 2018 (arXiv:1707.00788).** SWIM marks healthy members failed when the *detecting*
member is slow (CPU exhaustion, pauses). Lifeguard makes the detector account for its own
health: a local health multiplier raises probe interval and timeout while the member misses acks
or must refute suspicions of itself; a suspicion's timeout falls logarithmically as independent
suspicions arrive; a suspected member is told first. Its constants (multiplier cap `S = 8`,
`K = 3` confirmations, `α`, `β` of the suspicion bounds) were chosen by trying combinations; the
mechanism carries over, the constants do not. hyper-swim already applies it with measured
parameters (`crates/hyper-swim/src/detector.rs`).

## Raft's timing

**Ongaro, "Consensus: Bridging Theory and Practice", Stanford PhD dissertation, 2014.**

- §3.9, timing requirement: `broadcastTime ≪ electionTimeout ≪ MTBF`; broadcast time an order of
  magnitude below the election timeout so heartbeats keep followers from campaigning and split
  votes are unlikely; election timeout a few orders below MTBF since a leader crash costs about an
  election timeout of unavailability. Broadcast time 0.5–20 ms with storage, so election
  timeouts of 10–500 ms.
- §6.3, leases for reads: a leader whose heartbeats a majority acknowledged assumes no other leader
  for an election timeout divided by a clock-drift bound; safe only under a drift bound, which
  scheduling pauses, VM migration and clock slewing make hard to maintain.
- Chapter 9: with no split vote an election ends about a third of the way into the timeout range;
  a split vote occurs when too many servers time out within the one-way latency `l` of the first:
  with `s` servers available of `n`, a split needs `c > s − ⌊n/2⌋` of them within `l`, so
  `Pr(split) = Pr(D_{c,s} < l)` where `D_{c,s} = T_(c) − T_(1)` over the sorted timeouts (§9.2,
  CDF derived there for uniform timeouts, then for variable latency). Elections needed are
  geometric with mean `1 / (1 − split rate)` (§9.3). Recommendation: a range 10–20× the one-way
  latency keeps split rates under 40 %.

**etcd tuning guide (etcd.io/docs/v3.5/tuning).** Heartbeat about the maximum average round trip
between members (0.5–1.5×); election timeout at least 10× the round trip and 5–10× the
heartbeat; defaults 100 ms and 1000 ms. Rules of thumb, not derivations.

**Shiozaki, Nakamura, "Dynatune: Dynamic Tuning of Raft Election Parameters Using Network
Measurement", IEEE Access 2026 (arXiv:2507.15154).** Per leader–follower path, measured from
heartbeats: election timeout `Et = μ_RTT + s·σ_RTT` with `s = 2`; heartbeats per timeout
`K = ⌈log_p(1 − x)⌉` for loss rate `p` so at least one arrives with probability `x = 0.999`;
interval `h = Et / K`; tuning starts after 10 samples and resets to defaults on any election.
On etcd it cut detection time 78–81 % and out-of-service time 33–45 %. Confirms per-path
measurement pays; its `s`, `x` and sample count are picked, which the QoS configurator replaces.

**Wang, "BALLAST: Bandit-Assisted Learning for Latency-Aware Stable Timeouts in Raft", arXiv
2512.21165, December 2025.** Contextual bandits choose among a few timeout arms per node and
term; evaluated in discrete-event simulation only. The arms are picked; not adopted.

## Many groups per node

**Darnell, "Scaling Raft", Cockroach Labs blog, 2015-06-12.** With hundreds of thousands of
groups per node, MultiRaft coalesces heartbeats: each node pair exchanges one heartbeat per tick
for all the groups they share, on a constant number of goroutines (3), not one per group.

**Kettaneh, Radeva, Ajmani, Bhola, Taft, VanBenschoten, "Scalable Leader Leases For Multi
Consensus Groups in CockroachDB", SIGMOD 2026** (read through the review at emptysqua.re). Store
liveness: stores heartbeat each other and grant support for an epoch with an expiration; a
heartbeat requires a disk write, so a stalled disk cannot be supported. A Raft leader is
*fortified* by its followers' support and elections follow store liveness instead of per-group
timers. Reported: up to 85 % less lease-maintenance CPU at scale.

**TiKV, "Best Practices for TiKV Performance Tuning with Massive Regions" (docs.pingcap.com).**
Hibernate Region: idle regions get no ticks, so their leaders send no heartbeats; enabled by
default; `peer-stale-state-check-interval` sets the check between hibernated peers.

## Timers

**RFC 6298 (Paxson, Allman, Chu, Sargent), Computing TCP's Retransmission Timer.**
`SRTT`, `RTTVAR` with gains `α = 1/8`, `β = 1/4`; `RTO = SRTT + max(G, K·RTTVAR)`, `K = 4`; `G`
the clock granularity, a floor under the variance term.

**Microsoft, `timeBeginPeriod` (learn.microsoft.com).** The default timer resolution is
15.625 ms (64 interrupts a second). Since Windows 10 2004 a request applies to the calling
process only; a process that asks nothing gets no finer than the default.

**Linux, `PR_SET_TIMERSLACK(2const)` (man7.org).** A thread's timer expirations, including
`epoll_wait`, `poll`, `nanosleep` and futex waits, may be late by its timer slack, 50 µs by
default; never early.

**Apple, Dispatch `DispatchSourceTimer` (developer.apple.com).** Timers carry a leeway the system
may delay them by to coalesce wakeups; the default depends on the timer and QoS class;
`DISPATCH_TIMER_STRICT` asks the system to keep a smaller leeway.

## What the sources leave to us

1. Where the QoS requirements come from. Chen et al. take them from the application; etcd,
   Dynatune and Lifeguard pick constants.
2. The estimation window `n` of NFD-E.
3. How a split-vote rate turns into a randomization range without picking a multiple of the
   latency (Ongaro gives the probability, and a rule of thumb for the range).
4. How heartbeat cost bounds `η` once a node has many peers.
5. The first estimates, before a link has history.

`docs/timing.md` answers 1–3 and states 4–5 as measurements still to make.
