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
  where the requirements come from ("could be given by the application"); and when to reconfigure,
  which the adaptive detector does as its estimates are recomputed, each heartbeat.

**When to renew an estimate's configuration (derived here, 2026-10-02).** For an estimator that is
a mean over a growing history (sample means, the sample variance through its squares, a count's
posterior), the estimate over `n` observations with the one over the first `n_k` nested in it
differs from it by `((n − n_k)/n)(x̄_new − θ̂_k)`, whose variance is `σ²(1/n_k − 1/n)` for
observations of variance `σ²`; correlated observations multiply both this and the estimate's own
variance `σ²/n` by the same integrated autocorrelation time (Sokal 1997, §3). The configuration made
at `n_k` is as stale as the current estimate is uncertain when the two are equal, at `n = 2n_k`,
whatever the distribution. For a mean over a sliding window of `n`, the estimates `m` observations
apart share `n − m` and differ by a variance `2mσ²/n²`, equal to `σ²/n` at `m = n/2`. The variance of
a sample variance is `(μ₄ − σ⁴)/n` (Cramér, *Mathematical Methods of Statistics*, 1946, §27.4), so
a rule on an estimated standard error of `V̂` needs the fourth moment, which a heavy tail makes as
uncertain as it makes `V̂`; a rule on the expected staleness needs nothing of it. hyper-swim
(`crates/hyper-swim/src/detector.rs`, `Stream::due`) renews its verdict once a window's worth of
round trips have come, over estimates of the whole history.

**Where a freshness detector errs: one bound a heartbeat taken (derived here, 2026-10-03).**
NFD-E trusts at `t ∈ [τ_i, τ_{i+1})` iff some heartbeat numbered `i` or more has arrived (Chen et
al.'s rule). Let `h` be the latest heartbeat taken; the freshness point in force is
`τ_{h+1} = EA_{h+1} + α`. A suspicion of a live sender (an S-transition) happens at `τ_{h+1}` exactly
when no heartbeat numbered past `h` has arrived by then, and it ends at the next heartbeat taken,
`h' > h`, which arrived past `τ_{h+1}`: so each mistake is matched to the one heartbeat taken that
ends it, and a heartbeat taken `h'` ends a mistake exactly when its lateness
`ℓ = A_{h'} − EA_{h+1}` passes `α`. The count of mistakes is the count of heartbeats taken with
`ℓ > α`, and by linearity of expectation, which asks nothing of the dependence between them, its
expectation over `N` heartbeats taken is at most `N·β` for any `β` that bounds each one's
`Pr(ℓ > α)`. Cantelli's one-sided inequality gives such a `β` from the latenesses' mean and variance
alone, and Rényi's record law adds what the variance cannot know of (below). At most one heartbeat
is taken an interval, so mistakes recur no oftener than every `η/β`: Theorem 7's form, with no
product over the heartbeats in the margin and no loss term. A heartbeat the network lost, or a slot
the sender skipped while it stalled, is no heartbeat taken: it puts the next one taken an interval
or a stall later past `EA_{h+1}`, which its lateness measures. Chen et al.'s model has a sender
that is slow and not crashed send late (their delay `D` is measured from the sender's schedule,
`σ_i = iη`), and its messages lost or delayed independently; a sender whose stall makes it skip the
slots it was behind for sends none of them, and the per-arrival view needs neither the
independence nor the distinction. Detection is untouched: a sender that stops for good is
suspected at the freshness point after its last heartbeat, `E(D) + α + η` past its schedule.

**Little, "A proof for the queuing formula L = λW", Operations Research 9(3), 1961, pp.
383–387.** For any queueing system in a steady state, the long-run mean number in the system is
the arrival rate times the mean time each spends in it, whatever the distributions. With the
elections a group runs as the system, a crash of the leader's node entering once every MTBF and
staying `E(D) + α + η + T_E` (detection, then the election), and a mistake entering at most once
every `η/β` and staying `T_E`, the mean number of elections in progress is
`U = (E(D) + α + η + T_E)/MTBF + T_E·β/η`. The share of time one or more is in progress is
`Pr(N ≥ 1) ≤ E[N] = U` (Markov's inequality): `U` bounds the time a group cannot commit from
above, and at one or more bounds nothing.

**Chan, Golub and LeVeque, "Algorithms for computing the sample variance: analysis and
recommendations", The American Statistician 37(3), 1983, pp. 242–247.** The means and sums of
squared deviations of two sets combine exactly: `n = n_a + n_b`, `δ = x̄_b − x̄_a`,
`x̄ = x̄_a + δ·n_b/n`, `M₂ = M₂,a + M₂,b + δ²·n_a·n_b/n`, as stable as Welford's update. The
sliding window measured below kept its latenesses in half-window blocks, Welford's moments each,
and combined the two it held, so it slid with no ring of its samples.

**The arrivals' window: derived, built, measured and not kept (2026-10-03).** A configuration's
estimate should average over as much of the link as is stationary, and no more. The Allan deviation of the link's window means
falls with every doubling of the window while averaging still removes noise, and rises where the
series has moved (Allan 1966, above); so the levels give the horizon past which a longer window
averages over a link that changed: the longest level, from the least deviation's on, within the
least's statistical tolerance `1/√(2(K−1))` before a longer level with its windows rises past it.
Where no level rises, the whole history is stationary as far as it reaches, and the window is all
of it. The horizon is read on the offsets' levels, which a drift between the two clocks also
raises at long windows, so a drifting pair slides its window sooner than its latenesses (which
carry no drift) need: a shorter window, a larger unseen share, a looser bound, never a tighter one.
The window slides in blocks of half the horizon, the last two closed, and its configuration is
renewed at each close: the staleness rule above for a window sliding at `n` asks it every `n/2`.
Growing, it is renewed once it has doubled, the same rule for a history. Chen et al.'s adaptive
detector re-estimates over the `n` most recent heartbeats and leaves `n` open; Hayashibara et al.
keep a sliding window of inter-arrival times of a fixed size, picked. Measured against the history
in the simulation's worlds that change (`docs/timing.md` §3, item 11) it lost on every count: the
window's unseen share, `1/(m_w + 1)` over its own arrivals only, floors the bound at every margin,
and its horizon, read on levels that keep the whole history at the interval, never lets the window
grow back once a change has shown in them. The estimator keeps the history.

**Page, "Continuous inspection schemes", Biometrika 41 (1954).** The CUSUM test for a change in a
process's mean: the cumulative sum of deviations from a reference, a change declared at a
threshold `h` chosen for the average run length to a false alarm. The threshold is a design choice
the test does not give; it is not used here (`docs/timing.md` §3, item 11).

**Hayashibara, Défago, Yared, Katayama, "The φ accrual failure detector", SRDS 2004.** Outputs a
continuous suspicion level adapted to observed inter-arrival times instead of a boolean;
evaluated over an intercontinental link. Leaves the threshold on φ to the application. Its level is
a function of the time since the latest arrival against the distribution of the gaps between
arrivals, kept over a sliding window of a fixed size: each arrival judged against the one before it, as
the per-arrival bound above judges each heartbeat taken, but through a normal distribution fitted
to the gaps where Cantelli's inequality assumes none.

**Das, Gupta, Motivala, "SWIM: Scalable Weakly-consistent Infection-style Process Group Membership
Protocol", DSN 2002 (read 2026-10-01).**
- §3.1: each protocol period `T'` a member pings one member and waits for an ack "within a
  prespecified time-out (determined by the message round-trip time, which is chosen smaller than the
  protocol period)", then asks `k` members to ping it (ping-req); with no ack, direct or indirect,
  by the period's end the member is declared failed. "Properties of the protocol hold if `T'` is the
  average protocol period." The time-out "is based on an estimate of the distribution of round-trip
  time ... e.g. an average or 99th percentile"; "`T'` has to be at least three times the round-trip
  estimate". The experiments set the time-out from the average measured round trip.
- §4.1: infection-style dissemination piggybacks updates on ping, ping-req and ack, each element at
  most `λ log n` times. From Bailey's epidemic, with contact rate `1 − (1 − 1/n)²` a period, after
  `λ log n` periods the expected infected number is at least `n(1 − n^{−((2−4/n)λ−1)})`, so at most
  `n^{−((2−4/n)λ−2)}` members are uninfected in expectation; "setting `λ` to a small constant
  suffices". The experiments used `3⌈log(N + 1)⌉`.
- §4.2: suspicion: an unanswered member is suspected and the suspicion gossiped; a member that
  pings a suspect successfully un-marks it; the suspect refutes with a higher incarnation; a
  suspicion that times out is confirmed. The time-out "trades off an increase in failure detection
  time for a reduction in frequency of false failure detections"; its value is "prespecified".
- §4.3: round-robin target selection over a list re-permuted after each traversal, new members
  inserted at random positions: "successive selections of the same target are at most
  `(2·n_i − 1)` protocol periods apart."
- Left open: the time-out and the suspicion time-out are picked; `λ` is "a small constant".

**Dadgar, Phillips, Currey, "Lifeguard: Local Health Awareness for More Accurate Failure
Detection", DSN 2018 (arXiv:1707.00788; §IV read 2026-10-01).** SWIM marks healthy members failed
when the *detecting* member is slow (CPU exhaustion, pauses). §IV: "missing expected responses
could indicate a member is experiencing slow message processing, and ... an episode of slow message
processing at a given member is likely to impact multiple of its interactions with other members
in a short period of time"; the authors call this the member's *local health*.
- LHA-Probe: a saturating counter `LHM ∈ [0, S]`, raised by a failed probe, a refutation of a
  suspicion about oneself and a missed nack, lowered by a successful probe; the probe interval and
  time-out are the base values times `LHM + 1`. memberlist's bases are 1 s and 500 ms, `S = 8`.
- LHA-Suspicion: time-out `max(Min, Max − (Max − Min)·log(C + 1)/log(K + 1))` over `C` independent
  suspicions, `K = 3` by default; the time-out "will fall to its minimum level as long as the local
  member is receiving and processing gossip messages in a timely manner".
- Buddy system: a suspected member is told of the suspicion first, to shorten the time to refute.
- The constants were chosen by trying combinations; the mechanism carries over, the constants do
  not. `docs/timing.md` §2.7 keeps the buddy system and measures local health instead of counting
  it.

**Fréchet, "Sur les tableaux de corrélation dont les marges sont données", Ann. Univ. Lyon A 14,
1951, pp. 53–77.** For any two events, `Pr(A ∩ B) ≤ min(Pr(A), Pr(B))` whatever their dependence:
the bound `docs/timing.md` §2.7 uses for two consecutive missed probes when the pair's probes are
not measured independent.

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
- Figure 2 (Followers): "If election timeout elapses without receiving AppendEntries RPC from
  current leader or granting vote to candidate: convert to candidate": granting a vote resets the
  timer, which `docs/timing.md` §2.9 keeps as a round before a member that granted competes.
- §3.10, leadership transfer: the leader sends `TimeoutNow`, the target campaigns at once; "if the
  transfer does not complete within an election timeout, the prior leader aborts the transfer".
  §2.9 uses the order to hand over a lead that ends in its term.
- §4.2.2, removing the current leader: the leader steps down once the change commits, and "a
  server in Cnew will time out and become the new leader"; by suspicion nothing times out, so the
  leader hands over (§2.9).
- §4.2.3 and §9.6, disruptive servers and pre-vote: a server grants a pre-vote only if the
  candidate's log is up to date and it has not heard from a leader within the minimum election
  timeout; a server that has neither updates its term nor grants a vote. §2.9 reads "heard" as
  "trusts".
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

The node-pair stream (`docs/timing.md` §2.8) takes store liveness's rule that a heartbeat requires a
durable write, so a stalled disk loses support as a crash does, and carries the evidence (the count
of durable writes and the age of the latest) for the receiver to check, rather than a lease epoch:
support and leases are the core's and the shell's (L-2), not the detector's.

**RFC 3550 (Schulzrinne et al.), RTP, §6.4.1 (read 2026-10-02).** A receiver report carries the
middle of the NTP timestamp of the last sender report received (LSR) and the delay since receiving
it (DLSR); the sender computes the round trip as `A − LSR − DLSR` on its own clock, so neither
clock's offset enters. The node-pair stream's echo is the same three fields (`docs/timing.md` §2.8),
with each side's lateness past its schedule added back to bound the delay NFD-E measures.

**CockroachDB, `pkg/kv/kvserver/replica_raft_quiesce.go` (master, read 2026-10-02).** A leader
quiesces a range, sending nothing, once its live replicas are caught up: "A range may be quiesced
in the presence of non-live replicas if the remaining live replicas all meet the quiescence
requirements"; a non-live node is skipped and recorded as lagging. "When a node considered non-live
becomes live, the node liveness instance invokes a callback which causes all nodes to wake up any
ranges containing replicas owned by the newly-live node that were out-of-date at the time of
quiescence" (`Store.nodeIsLiveCallback`). `docs/timing.md` §2.9's leader beats only while its group
has work in flight, leaves out members it suspects, and sends a heartbeat to one trusted again.

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

## Estimating from traces

**Allan, "Statistics of atomic frequency standards", Proc. IEEE 54(2), 1966, pp. 221–230; IEEE Std
1139-2008.** The two-sample (Allan) variance `σ²_A(m) = ½ E[(ȳ_{k+1} − ȳ_k)²]` over consecutive
means of `m` samples separates noise that averaging removes from drift that it does not: for
uncorrelated stationary samples it falls as `σ²/m`, and where it stops falling, a longer average
tracks a quantity that has moved. The window at its minimum is the averaging length past which the
estimate gets no better. The relative uncertainty of `σ_A` from `K` windows is about
`1/√(2(K−1))`.

**Madras and Sokal, "The pivot algorithm", J. Stat. Phys. 50, 1988; Sokal, *Monte Carlo Methods
in Statistical Mechanics: Foundations and New Algorithms*, Cargèse lectures, 1997, §3.** A
correlated series' mean has variance `σ² τ_int / n`, `τ_int = 1 + 2 Σ_{k≥1} ρ_k`; summing the
sample autocorrelation over the self-consistent window `M ≥ c τ_int(M)`, `c ≈ 6`, keeps the
estimator's bias and variance both small.

**Box, Jenkins and Reinsel, *Time Series Analysis*, 4th ed., 2008, §2.1.6.** For white noise the
sample autocorrelation at any lag is within `±1.96/√n` with 95 % probability (Bartlett).

**Ferro and Segers, "Inference for clusters of extreme values", JRSS B 65(2), 2003, pp. 545–556.**
Exceedances of a high threshold by a stationary series come in clusters whose mean size is the
inverse of the extremal index `θ`; the intervals estimator gives `θ` from the times between
exceedances alone, and the `⌊θN⌋ − 1` longest of those times separate the clusters.

**Rousseeuw and Croux, "Alternatives to the median absolute deviation", JASA 88(424), 1993.**
`1.4826 · MAD` estimates `σ` consistently for normal data; for a distribution with a heavy tail it
estimates the spread of the body, not the variance. The MAD has the best possible breakdown point,
50 %, as the median has.

**Hampel, "A general qualitative definition of robustness", Annals of Mathematical Statistics
42(6), 1971, pp. 1887–1896.** Defines the breakdown point, the share of a sample that can be moved
arbitrarily far before the estimate is; the median's is one half. `PathRtt`'s window is the
shortest whose median the late probes of one stall cannot move: `2k + 1` for `k` late.

**RFC 9002, §5.1 and §5.3 (read 2026-10-02).** "An endpoint generates an RTT sample on receiving an
ACK frame that meets the following two conditions: the largest acknowledged packet number is newly
acknowledged, and at least one of the newly acknowledged packets was ack-eliciting"; "on the first
RTT sample after initialization, smoothed_rtt = latest_rtt, rttvar = latest_rtt / 2". The
handshake's packets carry CRYPTO frames, which are ack-eliciting, so a path has a sample before any
application data: the ballot's first (`docs/timing.md` §3, item 10).

**RFC 9002 (Iyengar, Swett), QUIC Loss Detection and Congestion Control, Appendix A.2 and §6.1.2
(read 2026-10-01).** "kGranularity: Timer granularity. This is a system-dependent value, and
Section 6.1.2 recommends a value of 1 ms." The 1 ms is a recommendation for a timer the RFC cannot
see; hyper-timing's tails take the owner's measured lateness instead.

**RFC 5905, §8, On-Wire Protocol (read 2026-10-01).** The offset `θ = ½[(T2 − T1) + (T3 − T4)]` and
round-trip delay `δ = (T4 − T1) − (T3 − T2)`: halving the round trip is exact only on a path whose
two directions take equally long, which the formula takes without stating. The ballot's one-way
latency is half a measured round trip on the same footing.

**Lindley, "The theory of queues with a single server", Proc. Cambridge Philos. Soc. 48(2), 1952,
pp. 277–289.** A single server fed at regular intervals `η` with independent service times `S`
reaches a stationary waiting time iff `E[S] < η`; otherwise the wait grows without bound. A sender
that must wake and flush before each heartbeat is such a server.

**Jeffreys, "An invariant form for the prior probability in estimation problems", Proc. R. Soc.
A 186, 1946, pp. 453–461; Brown, Cai and DasGupta, "Interval estimation for a binomial
proportion", Statistical Science 16(2), 2001, pp. 101–133.** The Jeffreys prior is the
parameterization-invariant prior; for a proportion it is `Beta(½, ½)`, posterior mean
`(k + ½)/(n + 1)` after `k` events in `n` trials, and its interval has the coverage Brown, Cai and
DasGupta recommend; for a Poisson rate it is `∝ λ^{−½}`, posterior `Gamma(k + ½, T)` after `k`
events in exposure `T`, mean `(k + ½)/T`.

**Brown, Cai and DasGupta, "Interval estimation in exponential families", Statistica Sinica 13,
2003, pp. 19–49.** The score interval for a Poisson mean from a count `k`,
`k + z²/2 ± z√(k + z²/4)`, has close to nominal coverage down to small counts.

**Rényi, "Théorie des éléments saillants d'une suite d'observations", Ann. Fac. Sci. Univ.
Clermont-Ferrand 8, 1962, pp. 7–13; Arnold, Balakrishnan and Nagaraja, *Records*, Wiley, 1998,
ch. 2.** For independent, identically distributed continuous observations the record indicators
are independent and the `n`-th observation is a record (larger than all before it) with
probability exactly `1/n`, whatever the distribution; the argument needs only exchangeability (each
of the `n` is equally likely to be the largest). The estimator uses it for the chance that the next
heartbeat is later than every one in a history of `m` independent ones, `1/(m + 1)`.

**Welford, "Note on a method for calculating corrected sums of squares and products",
Technometrics 4(3), 1962, pp. 419–420.** The running mean and sum of squared deviations updated
one observation at a time, without the cancellation of the sum-of-squares formula.

**RFC 5905 (Mills, Martin, Burbank, Kasch), Network Time Protocol Version 4, §7.2, Figure 6
(read 2026-10-01).** `TOLERANCE`, "frequency tolerance PHI (s/s)", 15e-6: the frequency error NTP
assumes of a clock. Two clocks within it of true time drift apart by up to 30 ppm; the estimator's
window bound takes it as the drift a link's expected arrival must follow, and a suspicion's bound
as the drift between the echo that bounded the clocks' offset and the suspicion
(`crates/hyper-liveness/src/bound.rs`).

## The machines' timers, from their sources

**XNU `osfmk/kern/timer_call.c`, `timer_call_slop` and `timer_compute_leeway`
(apple-oss-distributions/xnu, main, read 2026-10-01).** A user thread's timer is given a leeway of
`min((deadline − now) >> shift, max)` unless coalescing is off or the timer is critical; the shift
and the cap come from the thread's class: real-time and critical urgency get shift 0 and cap 0 (no
leeway), time-share threads `timer_coalesce_ts_shift`, and a thread with a latency-QoS tier that
tier's scale. **`osfmk/arm/arm_timer.c`, `tcoal_prio_params_init`**: on macOS, time-share shift 3
with a 1 ms cap; latency tiers 0–5 scale `{3, 2, 1, −2, 3, 3}`, caps `{1, 5, 20, 75, 1, 1}` ms.
`sysctl kern.timer_coalesce_*` on the machine reports the same values.

**XNU `bsd/netinet/ip_input.c`, `ip_savecontrol`**, called from `udp_input`
(`bsd/netinet/udp_usrreq.c`): with `SO_TIMESTAMP_MONOTONIC` set the kernel attaches
`mach_absolute_time()` when UDP input queues the datagram on the socket (`SCM_TIMESTAMP_MONOTONIC`,
a `uint64_t`; `<sys/socket.h>`); `SO_TIMESTAMP` attaches `getmicrotime()`, microseconds of wall
time. macOS's `setsockopt(2)` page documents neither.

**Linux `socket(7)`, `SO_TIMESTAMPNS`**: an `SCM_TIMESTAMPNS` control message with a
`struct timespec` of `CLOCK_REALTIME` at the datagram's reception. **The kernel configuration**
(`/proc/config.gz`) of Docker Desktop's linuxkit 6.12.76 has `CONFIG_HZ=1000`, `CONFIG_NO_HZ_IDLE=y`
and `# CONFIG_HIGH_RES_TIMERS is not set`; `/proc/timer_list` reports a resolution of 1,000,000 ns
for every clock base, so every timed wait in that VM ends on a 1 ms tick whatever the 50 µs timer
slack would allow.

**Microsoft, "Winsock timestamping" (learn.microsoft.com/windows/win32/winsock/winsock-timestamping,
updated 2025-03-11; read 2026-10-02).** Receive timestamps are enabled per socket with the
`SIO_TIMESTAMPING` IOCTL (`TIMESTAMPING_CONFIG`, `TIMESTAMPING_FLAG_RX` 0x1; minimum Windows 10
build 20348, client and server) and returned by `WSARecvMsg` as an `SO_TIMESTAMP` (0x300A) control
message, a `UINT64`; UDP only. They are "a software timestamp at the interface between the miniport
and NDIS, and a hardware timestamp in the NIC hardware"; software stamps are QPC values. "In addition
to socket-level configuration ... system-level configuration is also needed." **"Attaching
timestamps to packets" (Windows driver documentation, updated 2025-03-25)**: the miniport driver
attaches software stamps (`KeQueryPerformanceCounter`) only when it reports the `AllReceiveSw`
capability and it is enabled; nothing in the stack stamps for a driver that does not. Microsoft Q&A
(question 5790088) reports `WSAEOPNOTSUPP` from `SIO_TIMESTAMPING` on Windows 10 21H2 for that
reason, and that virtual NICs almost never report the capability. So no receive stamp is available
on a loopback path or a virtual machine's NIC, CI's runners among them: `docs/timing.md` §3 item 5.

**Linux `net/core/dev.c` (torvalds/linux master, read 2026-10-02), `net_enable_timestamp`,
`netstamp_clear`, `net_timestamp_set` and `net_timestamp_check`.** With jump labels, the first
socket to ask for stamps increments `netstamp_needed_deferred` and schedules `netstamp_work`, whose
`netstamp_clear` enables the static key `netstamp_needed_key`; only once it is enabled does the
receive path set `skb->tstamp = ktime_get_real()`. A datagram received before the work ran carries
no stamp, and the socket layer (`__sock_recv_timestamp`, `net/socket.c`) stamps it when it is read:
late, never early. Observed in Docker's linuxkit 6.12.76 on the first datagram after the option;
hyper-tokio's stamp test sends one datagram through first (`docs/transport.md` §4b).

## What the sources leave to us

1. Where the QoS requirements come from. Chen et al. take them from the application; etcd,
   Dynatune and Lifeguard pick constants.
2. The estimation window `n` of NFD-E.
3. How a split-vote rate turns into a randomization range without picking a multiple of the
   latency (Ongaro gives the probability, and a rule of thumb for the range).
4. How heartbeat cost bounds `η` once a node has many peers.
5. The first estimates, before a link has history.

`docs/timing.md` answers 1–3 (and, for SWIM in §2.7, its time-outs, suspicion, `λ` and `k`),
answers 5 for the loss, the mean delay and the MTBF from the traces
and the sources above (§3), answers the part of the first variance a history has not yet seen by
Rényi's record probability (§2.6), and states 4 and the rest of the first variance as measurements
still to make.
