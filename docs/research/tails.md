# Tails under the worst conditions: sources

The sources `docs/tails.md` rests on. Each is marked **verified** once read at its primary source
with what was checked, or **to verify** with what `docs/tails.md` takes from it; step T-0 reads
every one marked to verify before a value of the grid is derived from it.

## Measurement

- **Tene, "How NOT to Measure Latency"** (talks, 2013–2016; the HdrHistogram documentation's
  account of coordinated omission). Taken: a closed-loop driver hides stalls from its tail;
  measure from the scheduled time. To verify, and to find the citable written form.
- **Schroeder, Wierman and Harchol-Balter, "Open Versus Closed: A Cautionary Tale"**, NSDI 2006,
  from p. 239. **Verified** (the USENIX PDF, 2026-10-04): a closed model's new arrivals are
  triggered only by completions, an open model's arrive independently (§2); "for a fixed load, the
  mean response time for an open system model can exceed that for a closed system model by an order
  of magnitude or more", and the difference persists at an MPL of 1,000 (§1); a partly-open model,
  where an arriving user submits again with probability p, lies between the two (§2, Figure 1c).
- **Dean and Barroso, "The Tail at Scale"**, CACM 56(2), 2013. Context for why the tail, not the
  median, governs a fan-out system's latency. To verify.
- **David and Nagaraja, *Order Statistics*** (3rd ed., Wiley, 2003). Taken: distribution-free
  confidence intervals for quantiles from order statistics and the binomial distribution. To
  verify the chapter and the interval's form.

## Network conditions

- **Gilbert, "Capacity of a burst-noise channel"**, Bell System Technical Journal 39, 1960, and
  **Elliott, "Estimates of error rates for codes on burst-noise channels"**, BSTJ 42, 1963. Taken:
  the two-state burst-loss model hyper-sim's `Loss::bursty` implements. To verify against
  hyper-sim's parameters (enter, leave, loss in the burst state).
- **RFC 8289, Controlled Delay Active Queue Management** (Nichols, Jacobson, McGregor, Iyengar,
  2018). Already used by the Copa harness (`docs/research/congestion.md`).
- **Alquraan, Takruri, Alfatafta and Al-Kiswany, "An Analysis of Network-Partitioning Failures in
  Cloud Systems"**, OSDI 2018, from p. 51. **Verified** (the USENIX PDF, 2026-10-04): 136 failures
  from 25 systems (88 from issue trackers, 16 Jepsen reports, 32 found by NEAT); 21% leave the system
  in a lasting erroneous state after the partition heals; 88% can be triggered by isolating a single
  node; 62% are deterministic; 29% are caused by partial partitions; 64% need no client access or
  access to one side only; most reproduce with three nodes. Three partition types (§2.1): complete,
  partial (a third group reaches both sides), simplex (traffic one way only). Its scope is failure
modes in open-source systems' bug reports, not scale: it chooses which partition kinds to test,
and none of its figures is a rate at fleet scale (the few frequencies it gives are others': Google,
Microsoft, CENIC).
- **Huang, Guo, Zhou, Lorch, Dang, Chintalapati and Yao, "Gray Failure: The Achilles' Heel of
  Cloud-Scale Systems"**, HotOS 2017. Abstract **verified** (Microsoft Research page, 2026-10-04):
  gray failure, not fail-stop, causes the major availability breakdowns and performance anomalies
  seen; its key feature is differential observability, "the system's failure detectors may not
  notice problems even when applications are afflicted by them".
- **Published delay, jitter and loss measurements** for LAN, regional and intercontinental paths,
  to fit the grid's values (§2.4 of `docs/tails.md`). To find: measurement studies with tables of
  loss-burst lengths and delay distributions; ITU-T G.1050 and Y.1541 for standard network classes.

## At fleet scale

The rates and sizes of the background condition (`docs/tails.md` §2.3) come from production studies
at hyperscale. Each to verify at its primary source before a rate is taken from it:
- **Meza, Xu, Veeraraghavan and Mutlu, "A Large Scale Study of Data Center Network Reliability"**,
  IMC 2018. Scope **verified** (Facebook Research and the IMC proceedings, 2026-10-04): seven years
  of operational data from Facebook's intra-data-center networks, thousands of incidents across a
  cluster design and a fabric design, and eighteen months of repair tickets for its WAN backbone.
  To take from its body: failure rates and repair times by device type, and the backbone links'
  time between failures and time to repair.
- **Zhang, Liu, Zeng and Krishnamurthy, "High-Resolution Measurement of Data Center Microbursts"**,
  IMC 2017. Abstract **verified** (University of Washington copy, 2026-10-04): rack-level traffic in
  a Facebook production data center measured at tens to hundreds of microseconds; more than 70% of
  bursts last at most tens of microseconds, and congestion seen by coarser measurement is likely
  collections of such microbursts. To take from its body: burst durations, sizes and gaps, the
  model of the burst condition.
- **Roy, Zeng, Bagga, Porter and Snoeren, "Inside the Social Network's (Datacenter) Network"**,
  SIGCOMM 2015: Facebook's traffic, its locality and burstiness.
- **Dixit et al., "Silent Data Corruptions at Scale"** (Meta, 2021): CPUs that compute wrongly
  without failing; its rate bears on the checksums every record and payload carries.
- **Govindan et al., "Evolve or Die: High-Availability Design Principles Drawn from Google's Network
  Infrastructure"**, SIGCOMM 2016, and **Gill, Jain and Nagappan, "Understanding Network Failures in
  Data Centers"**, SIGCOMM 2011 (Microsoft): the other hyperscalers' failure data, to cross-check.

## Host conditions

- **Gunawi et al., "Fail-Slow at Scale: Evidence of Hardware Performance Faults in Large Production
  Systems"**, FAST 2018. Abstract **verified** (USENIX page, 2026-10-04): 101 reports of fail-slow
  incidents from 12 institutions; every hardware type (disk, SSD, CPU, memory, network) shows
  performance faults; faults convert from one form to another and cascade. To verify in the body:
  the slowdowns' magnitudes and durations, which the grid's fail-slow values need. Its 12
  institutions are not named as including Meta: a fleet-scale rate needs the studies above as well.
- **The Linux kernel's CFS bandwidth control documentation**
  (`Documentation/scheduler/sched-bwc.rst`) and **cgroup v2** (`Documentation/admin-guide/cgroup-v2.rst`:
  `cpu.max`, `memory.high`). Taken: throttling stalls a group for the rest of its period; reclaim
  above `memory.high`. To verify at the kernel version the containers run.

## Efficiency accounting

- **macOS `<sys/resource.h>`** (the SDK on this machine, read 2026-10-04): `struct rusage_info_v6`
  holds `ri_energy_nj`, `ri_penergy_nj`, `ri_instructions`, `ri_cycles`, `ri_pinstructions`,
  `ri_pcycles`, `ri_pkg_idle_wkups`, `ri_interrupt_wkups`, `ri_phys_footprint`,
  `ri_lifetime_max_phys_footprint`, `ri_interval_max_phys_footprint`, `ri_diskio_bytesread`,
  `ri_diskio_byteswritten`, `ri_billed_energy`, `ri_serviced_energy`, `ri_runnable_time`;
  `rusage_info_current` is v6. **Verified** that the fields exist; their units and update cadence
  to verify in Apple's documentation and by measurement (a known load against a known duration).
- **Linux**: `proc(5)` (`/proc/<pid>/io`, `/proc/<pid>/status`), `perf_event_open(2)`, the kernel's
  powercap documentation (`Documentation/power/powercap/powercap.rst`) and the access restriction on
  RAPL energy counters after CVE-2020-8694. To verify.
- **Windows**: the Win32 references for `GetProcessTimes`, `QueryProcessCycleTime`,
  `GetProcessIoCounters`, `GetProcessMemoryInfo`. To verify.

## Comparators

- **φ-accrual: Hayashibara, Défago, Yared and Katayama, "The φ Accrual Failure Detector"**, SRDS
  2004. To verify; the detectors' own sources are in `docs/research/timing.md` and `swim.md`.
- The comparator implementations of `docs/tails.md` §5, each pinned by revision in its `*-compare`
  workspace when it is added, with its license checked by `cargo deny` or, outside Rust, recorded
  here.
