# Tails under the worst conditions: sources

The sources `docs/tails.md` rests on. Each is marked **verified** once read at its primary source,
with what was checked, or **UNVERIFIED** where its primary text could not be reached, with what stands
in for it. Step T-0 (2026-10-13) read every one before a value of the grid was derived from it.

## Measurement

- **Tene, "How NOT to Measure Latency"** (talks, 2013–2016; the HdrHistogram documentation's
  account of coordinated omission). Taken: a closed-loop driver hides stalls from its tail;
  measure from the scheduled time. Its written form, **wrk2's README** (github.com/giltene/wrk2),
  **verified** (2026-10-13): wrk measures each request from its send, so a connection whose last
  answer was slow sends late and the slow period is under-sampled; wrk2 holds a constant rate and
  measures from when each request should have been sent. Its own runs: p99 10.52 ms corrected
  against 5.43 ms uncorrected on a quiet server, and with a 1.4 s stall (Apache suspended) 1.27 s
  against 6.04 ms. NON-PEER-REVIEWED, a tool's documentation.
- **Schroeder, Wierman and Harchol-Balter, "Open Versus Closed: A Cautionary Tale"**, NSDI 2006,
  from p. 239. **Verified** (the USENIX PDF, 2026-10-04): a closed model's new arrivals are
  triggered only by completions, an open model's arrive independently (§2); "for a fixed load, the
  mean response time for an open system model can exceed that for a closed system model by an order
  of magnitude or more", and the difference persists at an MPL of 1,000 (§1); a partly-open model,
  where an arriving user submits again with probability p, lies between the two (§2, Figure 1c).
- **Dean and Barroso, "The Tail at Scale"**, CACM 56(2), February 2013, from p. 74 (DOI
  10.1145/2408776.2408794). **Verified** (the authors' PDF, 2026-10-13): a request that collects from
  100 servers, each slow (1 s) once in 100, is slow 63% of the time (p. 76); a real fan-out's p99 is
  10 ms for one leaf, 70 ms for 95% of the leaves and 140 ms for all of them (Table 1); a hedge sent
  after the 95th percentile's latency adds about 5% load, and in a BigTable read of 1,000 keys over 100
  servers a hedge after 10 ms cut the p99.9 from 1,800 ms to 74 ms with 2% more requests (p. 77).
- **David and Nagaraja, *Order Statistics*** (3rd ed., Wiley, 2003). Taken: distribution-free
  confidence intervals for quantiles from order statistics and the binomial distribution.
  **UNVERIFIED**: the book was not reachable (2026-10-13). The interval rests on its own argument,
  stated in `hyper_measure::quantile` (the count below a quantile is binomial whatever the
  distribution, DERIVED), whose ranks are checked against exact integer binomial sums there.

## Network conditions

- **Gilbert, "Capacity of a burst-noise channel"**, Bell System Technical Journal 39, 1960, and
  **Elliott, "Estimates of error rates for codes on burst-noise channels"**, BSTJ 42, 1963. Taken:
  the two-state burst-loss model hyper-sim's `Loss::bursty` and `Loss::bursty_in_time` implement.
  Its form as fitted to Internet traces, and the measured traces its parameters come from (Bolot,
  SIGCOMM 1993; Jiang and Schulzrinne, NOSSDAV 2000), are read and quoted in
  `docs/research/burst-loss.md`.
- **RFC 8289, Controlled Delay Active Queue Management** (Nichols, Jacobson, McGregor, Iyengar,
  2018). Already used by the Copa harness (`docs/research/congestion.md`). **Verified** (2026-10-13):
  INTERVAL "SHOULD have a value of 100 ms" for the terrestrial Internet (§4.2, for round trips of
  about 10 ms to 1 s); TARGET "SHOULD be set to 5 ms" (§4.3), 5% of INTERVAL (§4.5).
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
- **ITU-T Y.1541 (12/2011), "Network performance objectives for IP-based services".**
  **Verified** (the ITU PDF, 2026-10-13), Table 1: upper bounds on the mean one-way delay of 100 ms
  (classes 0 and 2), 400 ms (1 and 3) and 1 s (4); on the delay variation, the 1 − 10⁻³ quantile above
  the minimum, of 50 ms (classes 0 and 1, unbounded in the others); on loss of 10⁻³ (classes 0 to
  4); errors 10⁻⁴; class 5 unspecified. The objectives are for public IP networks, judged over a
  minute. ITU-T G.1050's impairment profiles: not read.
- **RFC 4787, NAT behavioral requirements for unicast UDP.** **Verified** (2026-10-13), §4.3: a UDP
  mapping timer "MUST NOT expire in less than two minutes" (REQ-5) but for well-known ports (REQ-5a);
  "five minutes or more" recommended (REQ-5c); "great variation in the values used by different NATs".
- **Hätönen, Nyrhinen, Eggert, Strowes, Sarolahti and Kojo, "An Experimental Study of Home Gateway
  Characteristics"**, IMC 2010, from p. 260. **Verified** (the conference PDF, 2026-10-13): 34
  gateways. A UDP binding kept by one packet out: median 90 s, mean 160 s, shortest 30 s, longest
  691 s, more than half under RFC 4787's 120 s; refreshed by inbound packets, least 54 s and median
  180 s; traffic both ways, median about 181 s, mean 226 s (§4.1). TCP bindings: median 59.98 min,
  shortest 239 s. Queuing under load: 74 ms rising to 291 ms in one gateway, 110 ms to 400 ms in
  another, about 2 ms in the best (TCP-3). Keepalives as short as 15 s are in use (§4.4).
- **RFC 8200, IPv6.** **Verified** (2026-10-13), §5: "every link in the Internet have an MTU of 1280
  octets or greater".
- **Bennett, Partridge and Shectman, "Packet Reordering is Not Pathological Network Behavior"**,
  IEEE/ACM ToN 7(6), 1999, pp. 789–798. **UNVERIFIED**: no copy reachable. The grid derives
  reordering from Y.1541's delay variation instead.

## At fleet scale

The rates and sizes of the background condition (`docs/tails.md` §2.3) come from production studies
at hyperscale, each read at its primary source before a rate is taken from it:
- **Meza, Xu, Veeraraghavan and Mutlu, "A Large Scale Study of Data Center Network Reliability"**,
  IMC 2018 (DOI 10.1145/3278532.3278566). Scope **verified** (Facebook Research and the IMC
  proceedings, 2026-10-04): seven years of operational data from Facebook's intra-data-center
  networks, thousands of incidents across a cluster design and a fabric design, and eighteen months
  of repair tickets for its WAN backbone. Body **verified** (the conference PDF, 2026-10-13): incident
  rates vary three orders of magnitude across device types; mean time between incidents in 2017 from
  39,495 device-hours (core) to 9,958,828 (rack switches), and p75 resolution near 300 h (§5.5). The
  backbone (§6, Table 4), each an exponential fit over the percentile `p` of nodes or vendors: edge
  node MTBF 462.88·e^(2.3408p) h (half fail less than once in 1,710 h, 90% less than once in 3,521 h;
  the least reliable once in 253 h), MTTR 1.513·e^(4.256p) h (half within 10 h, 90% within 71 h, the
  slowest 608 h); fiber vendors' link MTBF 336.51·e^(3.4371p) h (half 2,326 h, 90% 5,709 h; the worst
  every 2 h), MTTR 1.1345·e^(4.7709p) h (half within 13 h, 90% within 60 h). By continent (Table 5),
  edge MTBF 1,579–5,400 h and MTTR 2–22 h.
- **Zhang, Liu, Zeng and Krishnamurthy, "High-Resolution Measurement of Data Center Microbursts"**,
  IMC 2017. Abstract **verified** (University of Washington copy, 2026-10-04): rack-level traffic in
  a Facebook production data center measured at tens to hundreds of microseconds; more than 70% of
  bursts last at most tens of microseconds, and congestion seen by coarser measurement is likely
  collections of such microbursts. Body **verified** (the same copy, 2026-10-13): a burst is a run of
  25 µs samples whose egress utilization passes 50%; 90% of bursts last at most 200 µs, the web
  racks' at most 50 µs (§5.1); as a two-state chain each 25 µs, from idle and from busy, 0.003 and
  0.359 (web), 0.016 and 0.721 (cache), 0.042 and 0.655 (Hadoop) (Table 2); 40% of the gaps between
  bursts under 100 µs on web and cache racks, idle spans up to hundreds of milliseconds, the arrivals
  not Poisson (§5.2); Hadoop racks busy 15% of the time.
- **Roy, Zeng, Bagga, Porter and Snoeren, "Inside the Social Network's (Datacenter) Network"**,
  SIGCOMM 2015, from p. 123 (DOI 10.1145/2785956.2787472). **Verified** (a course copy, 2026-10-13):
  traffic neither rack-local nor all-to-all, links lightly used (§4); demand wide and stable, its
  heavy hitters changing quickly (§5); outside Hadoop the median packet is under 200 bytes, with no
  on/off arrivals, and a server talks to hundreds of hosts and racks within one 5 ms interval (§6).
- **Dixit, Pendharkar, Beadon, Mason, Chakravarthy, Muthiah and Sankar, "Silent Data Corruptions
  at Scale"** (Facebook, arXiv:2102.11245, 2021). **Verified** (arXiv, 2026-10-13): tests across
  hundreds of thousands of machines found hundreds of CPUs that compute wrongly without reporting it
  (abstract), reproducible and not transient; fault-injection studies put a CPU's silent corruption at
  one in a million, the fleet's at orders of magnitude more (§1). A fleet rate of the order of 10⁻³ of
  machines is DERIVED from the abstract's counts.
- **Govindan, Minei, Kallahalla, Koley and Vahdat, "Evolve or Die: High-Availability Design
  Principles Drawn from Google's Network Infrastructure"**, SIGCOMM 2016, from p. 58 (DOI
  10.1145/2934872.2934891). **Verified** (a course copy of the CC-licensed PDF, 2026-10-13): over 100
  post-mortems of distinct high-impact failures in two years; comparable failure rates across Google's
  three networks and across the data, control and management planes; 80% of the failures last 10 to
  100 minutes, nearly 90% are high-impact, and most began while a management operation was under way
  (§1).
- **Gill, Jain and Nagappan, "Understanding Network Failures in Data Centers"**, SIGCOMM 2011
  (Microsoft). **Verified** (the authors' copy, 2026-10-13): more than four nines for about 80% of
  links and 60% of devices; a ToR fails in a year with probability about 5%, an aggregation switch
  10%, one load balancer in five (§1.1, §4.3); devices that fail at all, a mean of 2.5 failures a year
  and a p99 of 11 (Table 5); most link failures over within about five minutes, inter-data-centre
  links the longest; load balancers' median repair 3.7 and 4.5 minutes and time between failures 8.6
  and 16.4 minutes (§4.5.1–§4.5.2); 41% of link failure groups hold more than one failure, 10% more
  than four, the largest 180 links (§4.6); redundancy cuts a failure's median impact by up to 40%
  (§5.1).

## Host conditions

- **Gunawi et al., "Fail-Slow at Scale: Evidence of Hardware Performance Faults in Large Production
  Systems"**, FAST 2018. Abstract **verified** (USENIX page, 2026-10-04): 101 reports of fail-slow
  incidents from 12 institutions; every hardware type (disk, SSD, CPU, memory, network) shows
  performance faults; faults convert from one form to another and cascade. Body **verified** (the
  USENIX PDF, 2026-10-13): disk throughput down three orders of magnitude to 100 KB/s (vibration), SSD
  operations stalled for seconds (firmware), memory at 25% of its speed, CPUs at 50% (power), NICs
  collapsed to Kbps (§1); SSD I/Os throttled in multiples of 250 µs, up to 2–3 ms (§4.1); loss and
  corruption of 1–50% (§3.2); symptoms by type, permanent, transient, partial and transient stop
  (Table 4); time to detect, minutes for 1%, hours 13%, days 13%, weeks 11%, months 17% (§3.5). Its 12
  institutions are not named as including Meta: a fleet-scale rate needs the studies above as well.
- **The Linux kernel's CFS bandwidth control documentation**
  (`Documentation/scheduler/sched-bwc.rst`) and **cgroup v2** (`Documentation/admin-guide/cgroup-v2.rst`:
  `cpu.max`, `memory.high`). Taken: throttling stalls a group for the rest of its period; reclaim
  above `memory.high`. **Verified** (docs.kernel.org, 2026-10-13): the period defaults to 100 ms, and
  a quota or period is at least 1 ms, a period at most 1 s; "Throttled threads will not be able to run
  again until the next period when the quota is replenished"; `cpu.max` is `$MAX $PERIOD`, default
  `max 100000`; above `memory.high` "the processes of the cgroup are throttled and put under heavy
  reclaim pressure", never the OOM killer, which `memory.max` invokes. The kernel version the
  containers run is stated with each run.

## Efficiency accounting

- **macOS `<sys/resource.h>`** (the SDK on this machine, read 2026-10-04): `struct rusage_info_v6`
  holds `ri_energy_nj`, `ri_penergy_nj`, `ri_instructions`, `ri_cycles`, `ri_pinstructions`,
  `ri_pcycles`, `ri_pkg_idle_wkups`, `ri_interrupt_wkups`, `ri_phys_footprint`,
  `ri_lifetime_max_phys_footprint`, `ri_interval_max_phys_footprint`, `ri_diskio_bytesread`,
  `ri_diskio_byteswritten`, `ri_billed_energy`, `ri_serviced_energy`, `ri_runnable_time`;
  `rusage_info_current` is v6. **Verified** that the fields exist; the header states no units. The
  times are Mach absolute-time units, **verified by measurement** (`hyper_measure::usage`'s test
  holds the converted user time against getrusage(2)'s); the energy's unit is the field's name,
  nanojoules, its cadence unmeasured (a known load against a known duration is owed with T-1).
- **Linux**: `proc(5)` (`/proc/<pid>/io`, `/proc/<pid>/status`), `perf_event_open(2)`, the kernel's
  powercap documentation (`Documentation/power/powercap/powercap.rst`) and the access restriction on
  RAPL energy counters after CVE-2020-8694. **Verified** (man7.org and docs.kernel.org, 2026-10-13):
  `stat`'s `utime` and `stime` are "measured in clock ticks (divide by sysconf(_SC_CLK_TCK))"; `io`'s
  `write_bytes` is "the number of bytes really sent to the storage layer", truncation not deducted
  (`cancelled_write_bytes`); `status`'s `VmRSS` and `VmHWM` are inaccurate by "a kernel-internal
  scalability optimization", `smaps_rollup` being the exact, slower read (a peak needs the cgroup's
  own account); powercap's `energy_uj` is a zone's "current energy counter in micro joules", a zone a
  package or its parts, not a process; the kernel made it readable only by root ("powercap: restrict
  energy meter to root access", commit 949dd0104c49), so a process's energy on Linux needs a host
  given to the run, as `docs/tails.md` §1a says. `perf_event_open(2)`: not read.
- **Windows**: the Win32 references for `GetProcessTimes`, `QueryProcessCycleTime`,
  `GetProcessIoCounters`, `GetProcessMemoryInfo`. **Verified** (Microsoft Learn, 2026-10-13):
  `GetProcessTimes`' kernel and user times are FILETIMEs, "a 64-bit count of 100-nanosecond time
  units", summed over the threads; `QueryProcessCycleTime` gives "the number of CPU clock cycles used by
  the threads of the process", user and kernel; `IO_COUNTERS` counts read, write and other operations
  and bytes. `GetProcessMemoryInfo`: not read.

## Comparators

- **φ-accrual: Hayashibara, Défago, Yared and Katayama, "The φ Accrual Failure Detector"**, SRDS
  2004, pp. 66–78. Its mechanism as `docs/research/timing.md` records it (a sliding window of a fixed
  size, a normal fit to the gaps between heartbeats, the threshold left to the application);
  **UNVERIFIED** here: the authors' report copy has moved, and the proceedings are behind IEEE's login.
- The comparator implementations of `docs/tails.md` §5, each pinned by revision in its `*-compare`
  workspace when it is added, with its license checked by `cargo deny` or, outside Rust, recorded
  here.
