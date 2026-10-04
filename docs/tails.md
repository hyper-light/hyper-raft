# Tails under the worst conditions

The owner's rule, 2026-10-04: a busy machine is the norm; the p99 tails under load and horrific
network conditions are what every crate targets and tests, and under the worst conditions possible
they must be beyond industry leading. Every crate in this repository is held to it, not one layer.

This document is the design of how that is measured and shown: the conditions (§2), the
measurement (§3), the harnesses (§4), the comparators for each crate (§5), what each crate has
today and lacks (§6), and the order of work (§7). Sources are in `docs/research/tails.md`; a
number below is a source's, a measurement's, or a stated derivation's, as CLAUDE.md requires.

## 1. What "beyond industry leading" is checked as

For each crate and each condition of §2, the crate and every comparator of its class (§5) run the
same workload, under the same load, through the same impairment, from the same seed or script,
interleaved run by run. The claim holds for a condition when the crate's p99 and p99.9 are at or
below every comparator's, each quantile's interval (§3.3) clear of the comparator's; anything else
is recorded as a loss in `docs/benchmarks.md` with its traced cause, and is open work. A crate is
done under this rule when no condition of its grid has a loss left, or a loss left is shown to be
a cost of a guarantee the comparator does not give (durability, checksums, bounded memory), named
and measured.

Medians are reported and never decide. Throughput is reported beside each tail, since a tail
bought by refusing work is not a win: a run's offered and served rates are both stated, and a
crate that sheds load says how much.

## 1a. Efficiency, the other half of the bar

The same binary runs on one laptop and across a Meta-sized fleet, and the bar at the laptop is to
be maximally efficient in what the crate uses: CPU, memory, wakeups, energy, device writes and
network bytes (the owner, 2026-10-04). The two ends measure the same quantities. On a laptop they are
the battery, the heat and the user's other work; across a fleet, each cost an operation has times
the fleet's rate of operations is machines and power, and each cost an idle group has times the
groups a node holds is what a fleet that is mostly quiet pays to stand still. So every benchmark
under this rule reports, beside its tails and under the same condition:
- **per operation**: CPU time (user and system), instructions and cycles, allocations (CLAUDE.md
  §1a's law), energy where the platform measures it, bytes written to the device per byte the
  caller stored (write amplification, which is also an SSD's wear), and network bytes;
- **at rest**: an idle member's wakeups a second, CPU time and energy a second, and its memory
  footprint, alone and per group it holds;
- **the peak**: the most memory the run held (its high-water footprint).
An efficiency comparison is against the same comparators as the tails (§5), under the same load and
network. A crate wins the bar when it meets §1 and uses no more of each resource than the best
comparator, or the excess is shown to buy a guarantee the comparator lacks.

Each platform's own accounting is the source, read per process without privilege where it can be:
- **macOS**: `proc_pid_rusage` with `RUSAGE_INFO_V6` (`<sys/resource.h>`, the SDK on this machine):
  `ri_energy_nj` and `ri_penergy_nj` (energy, all cores and performance cores), `ri_instructions`
  and `ri_cycles`, `ri_pkg_idle_wkups` and `ri_interrupt_wkups`, `ri_phys_footprint` and
  `ri_lifetime_max_phys_footprint`, `ri_diskio_bytesread` and `ri_diskio_byteswritten`, the user
  and system times. The heartbeat cost runs already read `RUSAGE_INFO_V4`.
- **Linux**: `getrusage` and `/proc/<pid>/io` (`write_bytes`), `perf_event_open` for instructions
  and cycles, `/proc/<pid>/status` (`VmHWM`) for the peak; energy from the powercap interface
  (RAPL) where the hardware has it and the host lets it be read, which is per package, not per
  process, so it is read on a host running one member. Where no energy counter exists (most aarch64
  Linux hosts), instructions, cycles and wakeups are reported and energy is marked unmeasured.
- **Windows**: `GetProcessTimes`, `QueryProcessCycleTime`, `GetProcessIoCounters`,
  `GetProcessMemoryInfo` (`PeakWorkingSetSize`); energy marked unmeasured unless the platform's
  estimate is read and its method cited.
Each interface is verified at its primary documentation in T-0 before its numbers are reported.

## 2. The conditions

A condition is a host state and a network state at once; the grid of each crate crosses the
classes below that reach it. "Worst" is each class's measured or published envelope edge, and past
it the point where the crate stops meeting its bounds, found by sweeping (a breaking point is a
result, not a failure of the method).

### 2.1 The host

- **CPU saturated**: more runnable threads than cores, by real work beside the run (a parallel
  workspace build, as the existing loaded runs use) and, in Linux containers, by `--cpus N` with
  busy loops beside, as the hyper-swim and hyper-liveness soaks do. The load average is recorded
  before and after each run.
- **CPU throttled**: a cgroup's CPU bandwidth quota (`cpu.max`), whose throttling periods stall a
  process for the rest of a period; the Linux kernel's CFS bandwidth documentation states the
  mechanism.
- **Stopped processes**: `SIGSTOP`/`SIGCONT` of a member for a drawn span (a VM's steal, a page
  storm), as the hyper-liveness reproductions already use.
- **Memory pressure**: a cgroup memory limit near the working set (`memory.high`), so reclaim runs
  on the request path; macOS and Windows by a co-resident allocator, recorded.
- **Device contention and fail-slow**: other flush streams on the same device; a device that
  slows (fail-slow, Gunawi et al., FAST 2018) or stalls (hyper-durable-e2e's `StallFlush`); a flush
  that fails.

### 2.2 The network

Each property has a model shared by the simulation and the real-socket relay (§4.2), so a
condition is one specification in both:
- **Delay and jitter**: hyper-sim's `Path` (one-way delay, jitter), LAN, regional and geographic
  points, and measured distributions (`Measured`) from published or recorded traces.
- **Loss**: independent (`Loss::random`) and bursty (`Loss::bursty`, the Gilbert–Elliott model,
  Gilbert 1960, Elliott 1963), its parameters fitted to published measurements.
- **Reordering and duplication**: `Path::reordering` and the network's duplication.
- **Capacity and queues**: drop-tail links with deep buffers (bufferbloat) and CoDel (RFC 8289),
  as the Copa harness already runs; competing flows (NewReno, CUBIC) where the crate shares a link.
- **MTU**: paths whose MTU is below the datagram size (a black hole), and changes of it.
- **Partitions**: complete, partial and simplex (one-way), flapping, healed; hyper-sim's
  `Partitions`. Alquraan et al. (OSDI 2018), over 136 partition failures in 25 systems, found 29%
  caused by partial partitions, 88% triggerable by isolating a single node and 62% deterministic.
- **NAT rebinding**: hyper-sim's `Nat` idle timeout.
- **Gray failure**: a link or process that answers some peers and not others, or answers late
  (Huang et al., HotOS 2017).

### 2.3 At fleet scale, the worst is the background

A failure rare at one node is constant across a fleet: with `N` components each failing at rate
`λ`, the fleet sees `Nλ` failures a unit of time, each lasting its repair time, so at a Meta-sized
fleet some partition, fail-slow device and gray link is always in progress somewhere. Meta's scale
is the bar for everything distributed here (the owner, 2026-10-04: "Meta scale is what we aim for
with distributed. Period."). The worst condition of every distributed crate is therefore not one
event in a quiet run but that background,
at the rates the fleet's own failure data give, with bursts on it (microbursts at machine speed,
the agentic load the project is built for). Each crate's grid holds the background condition:
failures arriving as a process at the rates derived from per-component rates and a Meta-sized fleet,
both from the production studies, repaired at the measured repair times, while the crate's tail is measured.
Failure-mode studies (Alquraan et al., Huang et al.) choose which kinds of fault to inject; the
rates and sizes come from hyperscaler production studies (`docs/research/tails.md`, "At fleet
scale").

### 2.4 The grid's values

No value of the grid is chosen. Each is one of:
- a published envelope's edge (a cited measurement study or a standard's class), with the source
  and its table named in `docs/research/tails.md`;
- a value recorded on the hosts this repository runs on (the heartbeat traces' method,
  `docs/benchmarks.md`, "Heartbeat traces"); or
- a sweep's breaking point, found by the sweep and reported as such.

Step T-0 (§7) derives the grid and records each value's source before any comparator is run.

## 3. The measurement

### 3.1 Open loop

A closed-loop driver sends its next request when the last one answers, so a stall stops the
offered load and hides itself from the tail: coordinated omission (Tene). Every latency benchmark
under this rule is open loop: requests are scheduled ahead at the offered rate, and each latency is
measured from the request's scheduled time, not its send. Where a comparator's own tool is closed
loop, it is driven by this repository's open-loop driver instead, or its numbers are labelled
closed loop and never compared with open-loop ones. Open and closed systems differ in their tails
even at equal load (Schroeder, Wierman and Harchol-Balter, NSDI 2006), so the arrival process is
stated for every run: they found an open system's mean response time can exceed a closed one's by
an order of magnitude or more at the same load.

### 3.2 Histograms

Latencies are recorded in `hyper_timing::Histogram` (log-linear, 8 sub-buckets an octave, 496
buckets), whose quantile is its bucket's inclusive upper bound: a reported quantile is high by at
most one bucket's width, 12.5% of its octave. A comparison whose quantiles fall in one bucket is
reported as a tie at that resolution, never as a win.

### 3.3 Enough samples

A quantile's interval is distribution-free: the order statistics that bracket the `q`-quantile with
a stated coverage come from the binomial distribution of how many samples fall below it (David and
Nagaraja, *Order Statistics*). A run reports p50, p99, p99.9, p99.99 and the maximum, each with its
interval; a quantile whose interval reaches the maximum has too few samples and is reported as
unresolved, not as a number. The run length follows: the samples needed above `q` for an interval
of the stated coverage, divided by `1 − q`.

### 3.4 Tests stay exact

These are benchmarks. Tests stay exact (no statistical pass/fail): a test under a hostile condition
asserts a bound the code states (a deadline, a detection bound, a queue's limit), and under hyper-sim
it holds for every seed named, replayed exactly.

## 4. The harnesses

### 4.1 The simulation

hyper-sim's world and network (S-1, S-2) carry every network condition of §2.2 exactly and at
scale, and run every crate's sans-io core under them with seeds that replay. A condition is first
run there, where a tail's cause can be traced to the step.

### 4.2 Real sockets: the impairment relay

The end-to-end crates run real processes on real sockets. Nothing in them impairs a socket yet:
they kill members, stall flushes and partition by filtering. A relay process between the members
(each member's peers addressed through it) applies the same models as hyper-sim's network (§2.2),
seeded, on every one of the six targets, and counts what it did as `NetStats` does. Its own delay
is measured and reported, so the relay's cost is not charged to the crate. On Linux, `tc netem` in
a container is a second, kernel-level impairment, used to check the relay and to impair
comparators whose transport the relay cannot carry (TCP).

### 4.3 The host conditions

Scripts run each host condition of §2.1 beside a run and record it: the load average, the cgroup's
throttled time, the stopped spans, the memory limit, the device's other streams. Anything a script
starts it ends, by exact pid.

## 5. The comparators

The owner allowed (2026-10-04) industry implementations in any language as comparison-only
references in the `*-compare` workspaces, pinned by revision, never shipped. Each crate is compared
with the leaders of its class and with the project implementations it replaces:

| Crate | Industry comparators |
|---|---|
| hyper-raft, hyper-multilog | etcd-io/raft, tikv/raft-rs, openraft; Multi-Raft as TiKV runs it |
| hyper-log, hyper-durable, hyper-block | RocksDB's WAL, etcd's WAL; fio as the device's ceiling |
| hyper-quic, hyper-tls, hyper-transport, hyper-tokio | quinn, quiche, msquic, s2n-quic; rustls and BoringSSL handshakes |
| hyper-datagram | WireGuard (boringtun) |
| hyper-swim, hyper-liveness, hyper-timing | hashicorp/memberlist, foca; φ-accrual (Hayashibara et al., SRDS 2004) |
| hyper-check | Porcupine, Elle: checking time per history, its tail |
| hyper-sim, hyper-measure | the tools of the others' measurement; their own cost is reported, not compared |

A sans-io comparator (etcd raft, raft-rs) runs in this repository's harness over the same network;
a comparator with its own transport runs as its own processes, impaired by netem (§4.2).

## 6. Each crate today

From `docs/benchmarks.md` at 2e9fd39. "Tails" means p99 or beyond reported; "load" the load average
recorded; "hostile" a network condition of §2.2 measured, not only tested.

| Crate | Tails | Load | Hostile network | Industry comparator |
|---|---|---|---|---|
| hyper-raft | R-5, R-6, L-2, R16, R17 sections; not the core tables | yes | loss, jitter in R16/R17; partitions in E2E | none |
| hyper-multilog | not yet (means only) | yes | not yet | none |
| hyper-log | throughput, small appends | partly | n/a (device: no fail-slow tails) | none |
| hyper-durable | the shell against mantle's | yes | loss in traces | none |
| hyper-block | not reported | — | n/a (device conditions) | none |
| hyper-quic | Copa's competing mode only | yes | loss, delay, queues (Copa) | none |
| hyper-tls | none | — | — | none |
| hyper-transport | none (means, allocations) | yes | none measured | none |
| hyper-tokio | none | partly | loss in its E2E, not measured | none |
| hyper-datagram | none | yes | none | none |
| hyper-swim | cluster test: max, not p99 | yes | loss, delay | none |
| hyper-liveness | simulation worlds | yes | loss, delay, partitions | none |
| hyper-timing | heartbeat traces, timer sweeps | yes | n/a | none |
| hyper-check | not yet | — | n/a | none |
| hyper-sim, hyper-measure | tools | — | — | — |

No crate is yet compared with an industry implementation, and no real-socket run is impaired by a
model; those two are the common gaps.

## 7. The order of work

Each step is gated commits, results in `docs/benchmarks.md` with hardware, date, load, condition
and command.

- **T-0 The grid**: `docs/research/tails.md` verified; each condition's values derived (§2.3, §2.4).
- **T-1 The measurement kit**: an open-loop driver and the quantile intervals of §3.3 in
  hyper-measure, with exact tests of the interval arithmetic; the per-process resource accounts of
  §1a on the six targets, at rest and per operation.
- **T-2 The relay**: the impairment relay on the six targets, sharing hyper-sim's models, its own
  delay measured; netem scripts for Linux; the host-condition scripts.
- **T-3 onward, every crate class at once in turn**, not one layer first: for each class of §5,
  its comparators built and pinned, the grid run, the tails tabled, each loss traced to its cause
  and fixed or shown to be a guarantee's cost. The order follows the classes' dependencies (the
  timing and the device under the log; the log under the replica; the transport under the
  consensus), so a cause found low is fixed once for every layer above it.

The work in flight (hyper-multilog's steps 4–6, hyper-check's S-4) is measured under this rule as
it lands: tails, load and hostile networks in its own benchmarks.
