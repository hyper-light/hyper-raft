# Benchmarks: hyper-raft against the cores it replaces

`CLAUDE.md` §1a is the law for every crate here: allocations, reallocations and page faults are
measured on every hot path and driven down, and the crate is measured against each project's own
implementation it replaces, on the same workload, the same hardware and recorded commands. This
document is that record for `hyper-raft`.

## What is measured, and how

- **The tool** is `crates/hyper-raft-compare`, a workspace of its own (it builds other projects'
  cores from their repositories at pinned revisions, which this workspace's gates do not fetch).
- **The cores**, each driven through its own API:

  | Row | Source | How it is driven |
  |---|---|---|
  | hyper-raft | this branch, `crates/hyper-raft` | `RawNode::ready_in_place`, `advance_append_keeping` |
  | hyper-raft, copying Ready | this branch | `RawNode::ready`, `advance_append`, as focal's shell drives focal-raft |
  | focal-raft a8e95f7 | focal `origin/slates-port` `a8e95f71496461ebc8984926a65666746e946473` | as focal's shell drives it; the control: the source hyper-raft came from (R-1 changed its documentation and split `Raft::step` without changing what it does) |
  | focal-raft 1395e22 (mantle) | focal `1395e223c065e85158ec1611fdc72073d28fd8a0`, mantle `crates/range/Cargo.toml`'s pin | as focal's shell drives it |
  | slates 5cce86a | slates `main` `5cce86aa4cae58b421d171756b24e4305797a3e8`, `crates/cluster/src/raft.rs` | as slates' timed simulation drives it (`tests/support/timed.rs`), with its own `ElectionTimer`, publishing its retained state (`saved`) after each transition as its server does (`server/src/retention.rs`) |
  | slates 5cce86a, core alone | the same | the same, without the publication |
  | raft-rs 8e4cef1 | tikv/raft-rs `8e4cef172421bf77b2ae1c26628a9531b0be41f0`, the revision focal and mantle run | as focal's shell drove it before focal-raft |

- **slates' adapter** is the thinnest caller that sends what is owed (`src/slates.rs` says each
  choice): a leader sends a follower an append when the follower is owed entries or a commit, and
  every member's whole append once a period (slates' `lead`); an invitation to campaign is acted
  on when it arrives, as raft-rs's line acts on `MsgTimeoutNow`; the election timer runs slates'
  own `ElectionTimer` with base and span of `election_tick / heartbeat_tick` periods, since this
  in-memory network has no round trips for slates to derive them from; the batch budget and the
  window are focal's shell's `max_size_per_msg`, the bound every core runs with.
- **Settings** are focal's shell's for every core (`Settings::shell` in
  `tests/support/mod.rs`): election tick 10, heartbeat tick 2, 4 MiB + 1 KiB per message, 128
  messages in flight, pre-vote and check-quorum.
- **The network** is in memory and delivers in waves: every message in flight is stepped into its
  member, then each member that received one flushes once, as a shell steps what its socket holds
  before it asks for the next `Ready`. A member cut off loses what is sent to it and by it.
- **The owner's work** is the same for every core of raft-rs's line: a log in memory
  (`family.rs`, `Disk`), and an application whose digest reads each entry's index, length and
  first and last words, so that it costs the same at any entry size. slates' core keeps its log
  itself.
- **One measurement is two fresh processes** of the same seed, the cores interleaved run by run:
  - a timed run, whose allocator reads one word per call and counts nothing;
  - a counting run, whose counts are exact: a workload's allocations are fixed by its seed.
- **Columns**, all per operation (an entry committed by every member, an election, a snapshot):
  - time: the median of a row's runs (20 in the tables below), with the least and the most;
  - band: the least and most ratio of medians between two disjoint halves of the runs, over
    every halving (`hyper_measure::stats::band`): a difference inside both cores' bands is noise;
  - allocations, reallocations and bytes asked for, by the core's own calls, with the owner's work
    set aside (`hyper_measure::alloc::aside`);
  - allocations by the whole loop: the core, the owner's log and application, and the network;
  - page faults: minor and major from `getrusage(RUSAGE_SELF)`, and every fault the Mach VM layer
    took from `task_info(TASK_EVENTS_INFO)`, per thousand operations.

## The machine and the runs

- Apple M5 Max (`sysctl hw.model`: `Mac17,6`), 18 cores (6 super, 12 performance), 128 GiB.
- macOS 26.4.1 (kernel 25.4.0), rustc 1.98.0, release builds with `lto = true`.
- 2026-10-01, from 01:16 to 01:33 PDT. The machine was shared with other sessions' builds: the
  load average at each run's start and end is in the table headers below. The bands are what that
  noise did, measured; a difference is claimed only outside them.

## The workloads

Every workload is the same for every core: the same group, the same seeds, the same settings, the
same owner. "Ops" is what each row is per.

| Workload | What one run does | Op |
|---|---|---|
| steady | the leader is elected; each round, `batch` proposals of `bytes` each arrive at the leader at once, and the group is quiet when every member has committed and applied them; every member compacts its log each 4,096 entries (`COMPACT_ENTRIES`) | an entry committed and applied by every member |
| transfer | the leader hands the lead to the next member, which commits one entry of its term at every member | a transfer |
| failover | the leader is cut off; the others run their own timers (`period`) until one is elected, commits one entry, and the old leader is back and follows | an election |
| catchup | a follower is cut off while 1,024 entries are committed (`AWAY_ENTRIES`), then comes back and is caught up from the log | an entry caught up |
| snapshot | as catchup, with the leader compacted past what the follower holds, so it is sent a snapshot of the application (64 KiB) | a snapshot installed |
| fast | a follower proposes by the fast track; raft-rs has none | an entry committed and applied by every member |

A batch is proposed as each core takes several at once: one `MsgPropose` of them all for
raft-rs's line, an `append_command` each for slates'. Batch 1 is a lone proposal through each
core's single-proposal call.

## The batch size, by measurement

The steady workload at every power of two from 1 to 256 (3 voters, 64 B entries, 5 runs each,
nanoseconds per entry, median [min–max]):

Started Thu Oct  1 01:16:15 PDT 2026, load average { 9.26 8.56 8.94 }; ended Thu Oct  1 01:17:45 PDT 2026, load average { 14.22 10.45 9.62 }.

| batch | hyper-raft | hyper-raft, copying Ready | focal-raft a8e95f7 | focal-raft 1395e22 (mantle) | slates 5cce86a | slates 5cce86a, core alone | raft-rs 8e4cef1 |
|---|---|---|---|---|---|---|---|
| 1 | 1444 [1431–1798] | 1763 [1721–1791] | 1819 [1761–1912] | 1864 [1832–1972] | 279940 [265961–306276] | 1529 [1523–2051] | 1625 [1614–2184] |
| 2 | 1105 [828–1230] | 1458 [1064–1846] | 1650 [1181–1819] | 1727 [1179–1865] | 293057 [166964–377293] | 1252 [881–1501] | 1424 [988–2020] |
| 4 | 463 [451–480] | 666 [658–711] | 795 [754–802] | 750 [709–779] | 67472 [65658–68530] | 536 [519–564] | 632 [627–670] |
| 8 | 285 [284–294] | 475 [472–490] | 579 [564–588] | 561 [547–580] | 33193 [32748–35000] | 344 [329–361] | 458 [456–479] |
| 16 | 202 [192–207] | 354 [345–367] | 468 [452–493] | 449 [425–452] | 16499 [16314–17385] | 265 [252–282] | 356 [354–366] |
| 32 | 157 [155–168] | 304 [302–325] | 415 [412–435] | 401 [381–414] | 8551 [8372–8644] | 204 [201–216] | 315 [295–322] |
| 64 | 139 [136–148] | 295 [274–297] | 393 [383–413] | 377 [364–390] | 4409 [4326–4454] | 193 [183–197] | 298 [287–310] |
| 128 | 128 [124–133] | 270 [267–298] | 381 [378–401] | 359 [355–394] | 2239 [2128–2418] | 174 [168–176] | 285 [270–293] |
| 256 | 120 [118–122] | 263 [259–265] | 372 [369–403] | 349 [345–361] | 1231 [1191–1278] | 170 [162–190] | 282 [276–291] |

The table's batches are 1, a proposal alone, and 64, where the curves flatten: from 64 to 128
every core but slates' publication gains under 10 % per entry (3–10 %; slates' core alone gains
9.8 %), where from 32 to 64 hyper-raft still gained 11.5 %. The sweep above is the measurement
for every other batch from 1 to 256.

slates with its publication is in the sweep for completeness: it publishes its whole retained
state, the log included, after every transition, so its cost per entry falls with the batch
because the publications per entry do.

## Results

All numbers are per op. Allocations, reallocations and bytes are the core's own calls; "whole
loop" adds the owner's log and application and the network. Faults are per thousand ops; the Mach
count (`task_info`) and `getrusage`'s minor and major agreed on every run.

### Three voters (20 runs each)

Started Thu Oct  1 01:31:21 PDT 2026, load average { 4.32 7.86 9.44 }; ended Thu Oct  1 01:32:32 PDT 2026, load average { 4.82 7.37 9.13 }.

| workload | core | ns/op median [min–max] | band | ratio to hyper-raft | ops/s | allocs/op | reallocs/op | bytes/op | allocs/op whole loop | faults per 1k ops (minor+major) | Mach faults per 1k ops |
|---|---|---|---|---|---|---|---|---|---|---|---|
| steady 3v b1 64B | hyper-raft | 1457 [1416–1510] | 0.98–1.02 | 1.00 | 686187 | 11.0 | 0.00 | 7256 | 12.0 | 0.15 | 0.15 |
| steady 3v b1 64B | hyper-raft, copying Ready | 1778 [1727–1828] | 0.98–1.02 | 1.22 | 562370 | 23.0 | 0.00 | 8072 | 24.0 | 0.20 | 0.20 |
| steady 3v b1 64B | focal-raft a8e95f7 | 1830 [1806–1911] | 0.98–1.02 | 1.26 | 546366 | 32.0 | 0.00 | 9344 | 33.0 | 0.10 | 0.10 |
| steady 3v b1 64B | focal-raft 1395e22 (mantle) | 1859 [1836–1923] | 0.98–1.02 | 1.28 | 537807 | 32.0 | 0.00 | 11072 | 33.0 | 0.15 | 0.15 |
| steady 3v b1 64B | slates 5cce86a, core alone | 1554 [1484–1807] | 0.97–1.04 | 1.07 | 643633 | 51.0 | 0.00 | 3368 | 52.0 | 0.15 | 0.15 |
| steady 3v b1 64B | raft-rs 8e4cef1 | 1601 [1577–1648] | 0.98–1.02 | 1.10 | 624701 | 26.0 | 0.00 | 8696 | 27.0 | 0.25 | 0.25 |
| steady 3v b64 64B | hyper-raft | 139 [134–148] | 0.97–1.03 | 1.00 | 7210969 | 2.12 | 0.00 | 380 | 3.16 | 0.03 | 0.03 |
| steady 3v b64 64B | hyper-raft, copying Ready | 283 [278–305] | 0.98–1.02 | 2.04 | 3532828 | 8.22 | 0.00 | 1196 | 9.25 | 0.06 | 0.06 |
| steady 3v b64 64B | focal-raft a8e95f7 | 408 [390–428] | 0.98–1.02 | 2.94 | 2451572 | 11.3 | 0.00 | 1820 | 12.3 | 0.05 | 0.05 |
| steady 3v b64 64B | focal-raft 1395e22 (mantle) | 387 [377–443] | 0.98–1.03 | 2.79 | 2584595 | 11.3 | 0.00 | 1820 | 12.3 | 0.07 | 0.07 |
| steady 3v b64 64B | slates 5cce86a, core alone | 191 [185–210] | 0.98–1.02 | 1.38 | 5241662 | 5.72 | 0.00 | 407 | 6.74 | 0.07 | 0.07 |
| steady 3v b64 64B | raft-rs 8e4cef1 | 289 [281–323] | 0.96–1.04 | 2.08 | 3462142 | 11.2 | 0.00 | 1388 | 12.3 | 0.03 | 0.03 |
| steady 3v b1 4096B | hyper-raft | 2268 [2077–2503] | 0.90–1.11 | 1.00 | 440847 | 11.0 | 0.00 | 15320 | 12.0 | 0.70 | 0.70 |
| steady 3v b1 4096B | hyper-raft, copying Ready | 3041 [2944–4311] | 0.95–1.05 | 1.34 | 328861 | 23.0 | 0.00 | 40328 | 24.0 | 0.40 | 0.40 |
| steady 3v b1 4096B | focal-raft a8e95f7 | 3355 [3250–4668] | 0.95–1.05 | 1.48 | 298074 | 32.0 | 0.00 | 53696 | 33.0 | 0.20 | 0.20 |
| steady 3v b1 4096B | focal-raft 1395e22 (mantle) | 3425 [3340–5122] | 0.96–1.04 | 1.51 | 291928 | 32.0 | 0.00 | 55424 | 33.0 | 0.30 | 0.30 |
| steady 3v b1 4096B | slates 5cce86a, core alone | 2467 [2210–3644] | 0.91–1.10 | 1.09 | 405415 | 51.0 | 0.00 | 11432 | 52.0 | 0.70 | 0.70 |
| steady 3v b1 4096B | raft-rs 8e4cef1 | 3045 [2852–3351] | 0.95–1.05 | 1.34 | 328386 | 26.0 | 0.00 | 53048 | 27.0 | 0.50 | 0.50 |
| steady 3v b64 4096B | hyper-raft | 1021 [883–1400] | 0.78–1.28 | 1.00 | 979057 | 2.12 | 0.00 | 8444 | 3.16 | 2.00 | 2.00 |
| steady 3v b64 4096B | hyper-raft, copying Ready | 2335 [1985–3283] | 0.81–1.23 | 2.29 | 428183 | 8.22 | 0.00 | 33452 | 9.25 | 0.30 | 0.30 |
| steady 3v b64 4096B | focal-raft a8e95f7 | 3059 [2465–4247] | 0.86–1.16 | 2.99 | 326933 | 11.3 | 0.00 | 46172 | 12.3 | 0.20 | 0.20 |
| steady 3v b64 4096B | focal-raft 1395e22 (mantle) | 3060 [2395–4111] | 0.87–1.15 | 3.00 | 326809 | 11.3 | 0.00 | 46172 | 12.3 | 0.30 | 0.30 |
| steady 3v b64 4096B | slates 5cce86a, core alone | 1130 [974–1775] | 0.80–1.25 | 1.11 | 885034 | 5.72 | 0.00 | 8471 | 6.73 | 2.10 | 2.10 |
| steady 3v b64 4096B | raft-rs 8e4cef1 | 2934 [2336–3649] | 0.76–1.31 | 2.87 | 340860 | 11.2 | 0.00 | 45740 | 12.3 | 0.30 | 0.30 |
| transfer 3v b1 64B | hyper-raft | 4229 [4123–5632] | 0.97–1.03 | 1.00 | 236490 | 28.0 | 0.00 | 21288 | 29.0 | 3.00 | 3.00 |
| transfer 3v b1 64B | hyper-raft, copying Ready | 5059 [4860–6608] | 0.96–1.04 | 1.20 | 197659 | 46.0 | 0.00 | 22536 | 47.0 | 4.00 | 4.00 |
| transfer 3v b1 64B | focal-raft a8e95f7 | 5120 [4878–6678] | 0.96–1.04 | 1.21 | 195306 | 61.0 | 0.00 | 24888 | 62.0 | 3.25 | 3.25 |
| transfer 3v b1 64B | focal-raft 1395e22 (mantle) | 5047 [4930–6413] | 0.96–1.04 | 1.19 | 198154 | 61.0 | 0.00 | 28344 | 62.0 | 3.00 | 3.00 |
| transfer 3v b1 64B | slates 5cce86a, core alone | 3932 [3722–5271] | 0.92–1.08 | 0.93 | 254311 | 99.0 | 0.00 | 7120 | 100 | 7.50 | 7.50 |
| transfer 3v b1 64B | raft-rs 8e4cef1 | 4531 [4421–6235] | 0.95–1.06 | 1.07 | 220706 | 48.0 | 0.00 | 23304 | 49.0 | 4.50 | 4.50 |
| failover 3v b1 64B | hyper-raft | 6171 [5830–11329] | 0.94–1.06 | 1.00 | 162049 | 31.9 | 0.00 | 28208 | 32.9 | 35.0 | 35.0 |
| failover 3v b1 64B | hyper-raft, copying Ready | 6906 [6586–10986] | 0.93–1.07 | 1.12 | 144807 | 47.9 | 0.00 | 29456 | 48.9 | 35.0 | 35.0 |
| failover 3v b1 64B | focal-raft a8e95f7 | 6771 [6468–12849] | 0.92–1.09 | 1.10 | 147697 | 60.9 | 0.00 | 31520 | 61.9 | 40.0 | 40.0 |
| failover 3v b1 64B | focal-raft 1395e22 (mantle) | 6643 [6398–13213] | 0.93–1.07 | 1.08 | 150531 | 60.9 | 0.00 | 34184 | 61.9 | 30.0 | 30.0 |
| failover 3v b1 64B | slates 5cce86a, core alone | 6117 [5622–11860] | 0.90–1.11 | 0.99 | 163483 | 127 | 0.00 | 9164 | 128 | 65.0 | 65.0 |
| failover 3v b1 64B | raft-rs 8e4cef1 | 6377 [6001–13852] | 0.89–1.12 | 1.03 | 156817 | 49.7 | 0.00 | 29845 | 50.7 | 45.0 | 45.0 |
| catchup 3v b64 64B | hyper-raft | 33.1 [31.9–40.8] | 0.94–1.06 | 1.00 | 30225079 | 1.01 | 0.00 | 146 | 1.01 | 0.49 | 0.49 |
| catchup 3v b64 64B | hyper-raft, copying Ready | 84.5 [82.9–96.0] | 0.96–1.05 | 2.56 | 11829610 | 3.01 | 0.00 | 418 | 3.01 | 0.93 | 0.93 |
| catchup 3v b64 64B | focal-raft a8e95f7 | 123 [121–135] | 0.94–1.06 | 3.71 | 8146521 | 4.01 | 0.00 | 626 | 4.01 | 1.32 | 1.32 |
| catchup 3v b64 64B | focal-raft 1395e22 (mantle) | 123 [117–134] | 0.96–1.04 | 3.71 | 8146319 | 4.01 | 0.00 | 626 | 4.01 | 1.37 | 1.37 |
| catchup 3v b64 64B | slates 5cce86a, core alone | 25.6 [23.9–31.1] | 0.97–1.03 | 0.77 | 39015692 | 1.02 | 0.00 | 154 | 1.02 | 0.00 | 0.00 |
| catchup 3v b64 64B | raft-rs 8e4cef1 | 96.4 [89.5–143] | 0.96–1.05 | 2.91 | 10376516 | 4.01 | 0.00 | 486 | 4.01 | 1.12 | 1.12 |
| snapshot 3v b64 64B | hyper-raft | 4237 [3810–8387] | 0.73–1.37 | 1.00 | 236024 | 12.0 | 0.00 | 74031 | 14.0 | 100 | 100 |
| snapshot 3v b64 64B | hyper-raft, copying Ready | 9854 [8466–13804] | 0.91–1.09 | 2.33 | 101482 | 14.0 | 0.00 | 139615 | 18.0 | 220 | 220 |
| snapshot 3v b64 64B | focal-raft a8e95f7 | 10128 [8385–14381] | 0.77–1.30 | 2.39 | 98731 | 14.0 | 0.00 | 139615 | 18.0 | 200 | 200 |
| snapshot 3v b64 64B | focal-raft 1395e22 (mantle) | 9964 [8163–14837] | 0.84–1.19 | 2.35 | 100362 | 14.0 | 0.00 | 139615 | 18.0 | 195 | 195 |
| snapshot 3v b64 64B | slates 5cce86a, core alone | 3103 [2707–7193] | 0.88–1.13 | 0.73 | 322230 | 17.8 | 0.00 | 66531 | 17.8 | 70.0 | 70.0 |
| snapshot 3v b64 64B | raft-rs 8e4cef1 | 11762 [10468–18565] | 0.88–1.14 | 2.78 | 85019 | 24.2 | 7.98 | 143074 | 28.2 | 250 | 250 |
| fast 3v b1 64B | hyper-raft | 2854 [2822–3015] | 0.98–1.02 | 1.00 | 350387 | 45.0 | 0.00 | 14688 | 46.0 | 0.30 | 0.30 |
| fast 3v b1 64B | hyper-raft, copying Ready | 3178 [3109–3622] | 0.98–1.02 | 1.11 | 314626 | 57.0 | 0.00 | 15504 | 58.0 | 0.35 | 0.35 |
| fast 3v b1 64B | focal-raft a8e95f7 | 3249 [3184–3592] | 0.98–1.02 | 1.14 | 307815 | 66.0 | 0.00 | 16776 | 67.0 | 0.20 | 0.20 |
| fast 3v b1 64B | focal-raft 1395e22 (mantle) | 3317 [3221–3620] | 0.97–1.03 | 1.16 | 301522 | 66.0 | 0.00 | 19368 | 67.0 | 0.30 | 0.30 |
| fast 3v b1 64B | slates 5cce86a, core alone | 3211 [3114–3511] | 0.97–1.03 | 1.13 | 311398 | 103 | 0.00 | 6224 | 104 | 0.30 | 0.30 |
| fast 3v b1 64B | raft-rs 8e4cef1 | not implemented | | | | | | | | | |

### Five voters (20 runs each)

Started Thu Oct  1 01:27:15 PDT 2026, load average { 7.77 11.99 11.02 }; ended Thu Oct  1 01:29:09 PDT 2026, load average { 5.73 9.92 10.32 }.

| workload | core | ns/op median [min–max] | band | ratio to hyper-raft | ops/s | allocs/op | reallocs/op | bytes/op | allocs/op whole loop | faults per 1k ops (minor+major) | Mach faults per 1k ops |
|---|---|---|---|---|---|---|---|---|---|---|---|
| steady 5v b1 64B | hyper-raft | 2624 [2594–3571] | 0.99–1.01 | 1.00 | 381080 | 19.0 | 0.00 | 12136 | 20.0 | 0.12 | 0.12 |
| steady 5v b1 64B | hyper-raft, copying Ready | 3139 [3069–4540] | 0.98–1.02 | 1.20 | 318569 | 39.0 | 0.00 | 13496 | 40.0 | 0.10 | 0.10 |
| steady 5v b1 64B | focal-raft a8e95f7 | 3239 [3176–4822] | 0.97–1.04 | 1.23 | 308785 | 54.0 | 0.00 | 15616 | 55.0 | 0.10 | 0.10 |
| steady 5v b1 64B | focal-raft 1395e22 (mantle) | 3340 [3252–4883] | 0.98–1.02 | 1.27 | 299432 | 54.0 | 0.00 | 18640 | 55.0 | 0.15 | 0.15 |
| steady 5v b1 64B | slates 5cce86a, core alone | 3769 [3629–5417] | 0.95–1.05 | 1.44 | 265353 | 99.0 | 8.00 | 8376 | 100 | 0.10 | 0.10 |
| steady 5v b1 64B | raft-rs 8e4cef1 | 2897 [2850–4143] | 0.98–1.02 | 1.10 | 345152 | 44.0 | 0.00 | 14680 | 45.0 | 0.15 | 0.15 |
| steady 5v b64 64B | hyper-raft | 251 [236–380] | 0.78–1.28 | 1.00 | 3990207 | 4.22 | 0.00 | 724 | 5.25 | 0.05 | 0.05 |
| steady 5v b64 64B | hyper-raft, copying Ready | 500 [473–762] | 0.82–1.22 | 1.99 | 2000217 | 14.4 | 0.00 | 2084 | 15.4 | 0.07 | 0.07 |
| steady 5v b64 64B | focal-raft a8e95f7 | 722 [690–1042] | 0.85–1.18 | 2.88 | 1385613 | 19.5 | 0.00 | 3124 | 20.6 | 0.05 | 0.05 |
| steady 5v b64 64B | focal-raft 1395e22 (mantle) | 687 [649–999] | 0.88–1.13 | 2.74 | 1455939 | 19.5 | 0.00 | 3124 | 20.6 | 0.07 | 0.07 |
| steady 5v b64 64B | slates 5cce86a, core alone | 316 [301–468] | 0.88–1.13 | 1.26 | 3169101 | 8.44 | 0.12 | 816 | 9.45 | 0.10 | 0.10 |
| steady 5v b64 64B | raft-rs 8e4cef1 | 562 [488–799] | 0.81–1.24 | 2.24 | 1779274 | 19.4 | 0.00 | 2404 | 20.4 | 0.05 | 0.05 |
| steady 5v b1 4096B | hyper-raft | 4514 [4214–5924] | 0.89–1.13 | 1.00 | 221550 | 19.0 | 0.00 | 28264 | 20.0 | 0.90 | 0.90 |
| steady 5v b1 4096B | hyper-raft, copying Ready | 5861 [5546–7759] | 0.88–1.14 | 1.30 | 170619 | 39.0 | 0.00 | 69944 | 40.0 | 0.80 | 0.80 |
| steady 5v b1 4096B | focal-raft a8e95f7 | 6481 [6130–8929] | 0.89–1.13 | 1.44 | 154306 | 54.0 | 0.00 | 92224 | 55.0 | 0.70 | 0.70 |
| steady 5v b1 4096B | focal-raft 1395e22 (mantle) | 6592 [6225–8776] | 0.89–1.12 | 1.46 | 151691 | 54.0 | 0.00 | 95248 | 55.0 | 0.80 | 0.80 |
| steady 5v b1 4096B | slates 5cce86a, core alone | 5586 [5214–6822] | 0.94–1.06 | 1.24 | 179013 | 99.0 | 8.00 | 24504 | 100 | 0.80 | 0.80 |
| steady 5v b1 4096B | raft-rs 8e4cef1 | 5908 [5666–7973] | 0.97–1.03 | 1.31 | 169271 | 44.0 | 0.00 | 91288 | 45.0 | 0.80 | 0.80 |
| steady 5v b64 4096B | hyper-raft | 2073 [1804–3063] | 0.84–1.18 | 1.00 | 482362 | 4.22 | 0.00 | 16852 | 5.25 | 2.30 | 2.30 |
| steady 5v b64 4096B | hyper-raft, copying Ready | 3807 [3453–5434] | 0.86–1.17 | 1.84 | 262685 | 14.4 | 0.00 | 58532 | 15.4 | 0.90 | 0.90 |
| steady 5v b64 4096B | focal-raft a8e95f7 | 4864 [4426–7040] | 0.89–1.12 | 2.35 | 205574 | 19.5 | 0.00 | 79732 | 20.6 | 0.80 | 0.80 |
| steady 5v b64 4096B | focal-raft 1395e22 (mantle) | 4806 [4410–6436] | 0.85–1.18 | 2.32 | 208056 | 19.5 | 0.00 | 79732 | 20.6 | 0.90 | 0.90 |
| steady 5v b64 4096B | slates 5cce86a, core alone | 2150 [1981–2800] | 0.85–1.17 | 1.04 | 465031 | 8.44 | 0.12 | 16944 | 9.45 | 2.40 | 2.40 |
| steady 5v b64 4096B | raft-rs 8e4cef1 | 4718 [4313–7463] | 0.83–1.20 | 2.28 | 211973 | 19.4 | 0.00 | 79012 | 20.4 | 0.90 | 0.90 |
| transfer 5v b1 64B | hyper-raft | 7509 [7364–8387] | 0.97–1.03 | 1.00 | 133167 | 48.0 | 1.00 | 36456 | 49.0 | 3.50 | 3.50 |
| transfer 5v b1 64B | hyper-raft, copying Ready | 8658 [8450–8992] | 0.98–1.02 | 1.15 | 115503 | 78.0 | 1.00 | 38536 | 79.0 | 5.00 | 5.00 |
| transfer 5v b1 64B | focal-raft a8e95f7 | 8741 [8533–9362] | 0.98–1.02 | 1.16 | 114405 | 103 | 1.00 | 42456 | 104 | 5.50 | 5.50 |
| transfer 5v b1 64B | focal-raft 1395e22 (mantle) | 8765 [8499–9267] | 0.98–1.02 | 1.17 | 114087 | 103 | 1.00 | 48504 | 104 | 4.50 | 4.50 |
| transfer 5v b1 64B | slates 5cce86a, core alone | 7965 [7713–8443] | 0.97–1.03 | 1.06 | 125549 | 171 | 12.0 | 14976 | 172 | 8.50 | 8.50 |
| transfer 5v b1 64B | raft-rs 8e4cef1 | 7953 [7776–8396] | 0.97–1.03 | 1.06 | 125737 | 82.0 | 1.00 | 40296 | 83.0 | 5.00 | 5.00 |
| failover 5v b1 64B | hyper-raft | 11178 [10894–11919] | 0.98–1.02 | 1.00 | 89465 | 54.2 | 0.00 | 47158 | 55.2 | 32.5 | 32.5 |
| failover 5v b1 64B | hyper-raft, copying Ready | 12493 [12180–13004] | 0.98–1.02 | 1.12 | 80046 | 82.2 | 0.00 | 49238 | 83.2 | 30.0 | 30.0 |
| failover 5v b1 64B | focal-raft a8e95f7 | 12419 [12135–13337] | 0.98–1.02 | 1.11 | 80522 | 105 | 0.00 | 52870 | 106 | 45.0 | 45.0 |
| failover 5v b1 64B | focal-raft 1395e22 (mantle) | 12267 [12012–12714] | 0.98–1.02 | 1.10 | 81517 | 105 | 0.00 | 58126 | 106 | 35.0 | 35.0 |
| failover 5v b1 64B | slates 5cce86a, core alone | 13071 [12674–15175] | 0.95–1.06 | 1.17 | 76507 | 232 | 20.0 | 20885 | 233 | 65.0 | 65.0 |
| failover 5v b1 64B | raft-rs 8e4cef1 | 11601 [11200–13559] | 0.97–1.03 | 1.04 | 86196 | 85.9 | 0.00 | 50509 | 86.9 | 35.0 | 35.0 |
| catchup 5v b64 64B | hyper-raft | 33.0 [32.6–37.4] | 0.95–1.05 | 1.00 | 30320123 | 1.01 | 0.00 | 148 | 1.01 | 0.49 | 0.49 |
| catchup 5v b64 64B | hyper-raft, copying Ready | 83.1 [82.3–87.3] | 0.98–1.02 | 2.52 | 12034373 | 3.01 | 0.00 | 420 | 3.01 | 0.93 | 0.93 |
| catchup 5v b64 64B | focal-raft a8e95f7 | 122 [119–140] | 0.97–1.03 | 3.68 | 8230337 | 4.02 | 0.00 | 628 | 4.02 | 1.32 | 1.32 |
| catchup 5v b64 64B | focal-raft 1395e22 (mantle) | 120 [116–131] | 0.97–1.03 | 3.63 | 8346553 | 4.02 | 0.00 | 628 | 4.02 | 1.37 | 1.37 |
| catchup 5v b64 64B | slates 5cce86a, core alone | 26.8 [26.3–28.6] | 0.98–1.02 | 0.81 | 37297192 | 1.05 | 0.00 | 156 | 1.05 | 0.29 | 0.29 |
| catchup 5v b64 64B | raft-rs 8e4cef1 | 92.3 [89.5–98.1] | 0.98–1.02 | 2.80 | 10830257 | 4.01 | 0.00 | 488 | 4.01 | 0.98 | 0.98 |
| snapshot 5v b64 64B | hyper-raft | 4471 [4348–6677] | 0.97–1.03 | 1.00 | 223653 | 14.0 | 0.00 | 76591 | 16.0 | 90.0 | 90.0 |
| snapshot 5v b64 64B | hyper-raft, copying Ready | 9131 [8392–10303] | 0.95–1.05 | 2.04 | 109511 | 16.0 | 0.00 | 142191 | 20.0 | 210 | 210 |
| snapshot 5v b64 64B | focal-raft a8e95f7 | 9126 [8280–10079] | 0.94–1.06 | 2.04 | 109572 | 16.0 | 0.00 | 142191 | 20.0 | 190 | 190 |
| snapshot 5v b64 64B | focal-raft 1395e22 (mantle) | 9194 [8683–9683] | 0.96–1.04 | 2.06 | 108767 | 16.0 | 0.00 | 142191 | 20.0 | 200 | 200 |
| snapshot 5v b64 64B | slates 5cce86a, core alone | 4177 [3965–4575] | 0.96–1.04 | 0.93 | 239413 | 39.8 | 3.00 | 68736 | 39.8 | 70.0 | 70.0 |
| snapshot 5v b64 64B | raft-rs 8e4cef1 | 10989 [10326–12947] | 0.96–1.04 | 2.46 | 91004 | 32.2 | 8.98 | 147933 | 36.2 | 250 | 250 |
| fast 5v b1 64B | hyper-raft | 4991 [4903–5114] | 0.99–1.01 | 1.00 | 200360 | 73.0 | 0.00 | 24168 | 74.0 | 0.50 | 0.50 |
| fast 5v b1 64B | hyper-raft, copying Ready | 5580 [5479–5774] | 0.98–1.02 | 1.12 | 179223 | 93.0 | 0.00 | 25528 | 94.0 | 0.40 | 0.40 |
| fast 5v b1 64B | focal-raft a8e95f7 | 5630 [5541–5781] | 0.99–1.01 | 1.13 | 177607 | 108 | 0.00 | 27648 | 109 | 0.30 | 0.30 |
| fast 5v b1 64B | focal-raft 1395e22 (mantle) | 5703 [5637–6114] | 0.98–1.02 | 1.14 | 175341 | 108 | 0.00 | 31968 | 109 | 0.40 | 0.40 |
| fast 5v b1 64B | slates 5cce86a, core alone | 6390 [6248–7101] | 0.98–1.02 | 1.28 | 156495 | 173 | 8.00 | 12992 | 174 | 0.10 | 0.10 |
| fast 5v b1 64B | raft-rs 8e4cef1 | not implemented | | | | | | | | | |

### slates with its retained-state publication (4 runs, 64 B entries)

slates' server publishes the complete retained state after every transition, before any reply
(`server/src/retention.rs`); the adapter publishes it by `RaftNode::saved`, a copy, and does not
encode, hash or write it, so this is a floor under that cost. The log it copies is bounded by the
workload's compaction (4,096 entries). At 4 KiB entries a publication copies up to 16 MiB, and a
run of the steady workload took minutes; those rows are not run.

Started Thu Oct  1 01:19:23 PDT 2026, load average { 9.82 9.76 9.42 }; ended Thu Oct  1 01:25:58 PDT 2026, load average { 13.93 14.11 11.61 }.

| workload | core | ns/op median [min–max] | band | ratio to hyper-raft | ops/s | allocs/op | reallocs/op | bytes/op | allocs/op whole loop | faults per 1k ops (minor+major) | Mach faults per 1k ops |
|---|---|---|---|---|---|---|---|---|---|---|---|
| steady 3v b1 64B | slates 5cce86a | 293618 [277426–375974] | 0.82–1.22 | — | 3406 | 51.0 | 0.00 | 3368 | 12101 | 2.88 | 2.88 |
| steady 3v b64 64B | slates 5cce86a | 4607 [4573–4637] | 0.99–1.01 | — | 217069 | 5.72 | 0.00 | 407 | 198 | 1.21 | 1.21 |
| transfer 3v b1 64B | slates 5cce86a | 4687 [4634–4760] | 0.98–1.02 | — | 213339 | 99.0 | 0.00 | 7120 | 132 | 8.25 | 8.25 |
| failover 3v b1 64B | slates 5cce86a | 7060 [6643–7919] | 0.91–1.10 | — | 141647 | 127 | 0.00 | 9164 | 155 | 65.0 | 65.0 |
| catchup 3v b64 64B | slates 5cce86a | 35.3 [35.0–36.6] | 0.97–1.03 | — | 28352594 | 1.02 | 0.00 | 154 | 2.03 | 0.29 | 0.29 |
| snapshot 3v b64 64B | slates 5cce86a | 4388 [4346–4608] | 0.97–1.03 | — | 227898 | 17.8 | 0.00 | 66531 | 19.8 | 55.0 | 55.0 |
| fast 3v b1 64B | slates 5cce86a | 415788 [320582–462814] | 0.79–1.27 | — | 2405 | 103 | 0.00 | 6224 | 14869 | 7.10 | 7.10 |
| steady 5v b1 64B | slates 5cce86a | 608113 [466169–798832] | 0.73–1.36 | — | 1644 | 99.0 | 8.00 | 8376 | 20181 | 3.20 | 3.20 |
| steady 5v b64 64B | slates 5cce86a | 10553 [10092–11876] | 0.88–1.13 | — | 94760 | 8.44 | 0.12 | 816 | 329 | 6.00 | 6.00 |
| transfer 5v b1 64B | slates 5cce86a | 11070 [10322–13459] | 0.86–1.16 | — | 90333 | 171 | 12.0 | 14976 | 224 | 9.25 | 9.25 |
| failover 5v b1 64B | slates 5cce86a | 17558 [15190–17930] | 0.92–1.09 | — | 56954 | 232 | 20.0 | 20885 | 280 | 72.5 | 72.5 |
| catchup 5v b64 64B | slates 5cce86a | 48.5 [46.1–50.0] | 0.94–1.06 | — | 20637308 | 1.05 | 0.00 | 156 | 2.05 | 0.00 | 0.00 |
| snapshot 5v b64 64B | slates 5cce86a | 8370 [7818–10061] | 0.87–1.15 | — | 119468 | 39.8 | 3.00 | 68736 | 41.8 | 70.0 | 70.0 |
| fast 5v b1 64B | slates 5cce86a | 687158 [645743–783375] | 0.86–1.17 | — | 1455 | 173 | 8.00 | 12992 | 27859 | 20.9 | 20.9 |

## The noise band

`hyper_measure::stats::band` takes every division of a row's runs into two halves of equal size
and the ratio of the halves' medians; the band is the least and the most of them. A difference
between two cores is claimed only where the ratio of their medians is outside both their bands.

- The tables were first run at 10 runs a row (01:17 to 01:19, load average 9 to 14); bands reached
  0.83–1.21 on three voters and 0.45–2.21 on five, the five-voter rows having run as the load rose.
  Both group sizes were run again at 20 runs a row, the tables above (01:27 to 01:33, load
  average 4 to 12): bands up to 0.73–1.37 on three voters (one outlier run in the snapshot row)
  and 0.78–1.28 on five, and within 0.97–1.03 on most rows. Every claim below holds in both
  passes unless it says otherwise.
- Allocation counts have no band: a workload's allocations are fixed by its seed, and the timed
  and counting runs of a seed agree.
- The control (focal-raft a8e95f7) and focal-raft 1395e22 differ in their code (1395e22 lacks
  focal's allocation audit, F15 and F16), so they are not a noise pair; the control's own band is
  the noise of the code hyper-raft started from.

## Optimisations, before and after

"Before" is the control: focal-raft a8e95f7, the source hyper-raft came from, whose decisions R-1
kept (ORIGIN.md). Each change was proven to decide exactly as before: all tests, the raft-rs
differential, and every seed-fixed count at 1,000 seeds from seed 1,000 identical to R-1's record
(22,275,363 differential steps; ORIGIN.md, "Proof").

| Change | Commit | Measured on | Before | After |
|---|---|---|---|---|
| Entries move into the log uncopied (proposals and appends) | `5584c49` | steady, 3 voters, 64 B, batch 1, allocations per entry | 32.0 | 23.0 |
| | | the same, 4 KiB entries, bytes asked for per entry | 53,696 | 40,328 |
| | | catchup, allocations per entry | 4.01 | 3.01 |
| | | 16 lone proposals a round (before batches were proposed at once), reallocations per entry | 1.25 | 1.44 |
| A `Ready` given in place (`ready_in_place`, `advance_append_keeping`) | `62288fb` | steady, batch 1, allocations per entry, core / whole loop | 23.0 / 24.0 | 11.0 / 12.0 |
| | | steady, batch 64, allocations per entry, core / whole loop | 8.22 / 9.25 | 2.12 / 3.16 |
| | | catchup, allocations per entry | 3.01 | 1.01 |
| | | snapshot, bytes asked for per snapshot | 139,615 | 74,031 |
| A page's entries read once | `d913261` | catchup, ns per entry, 1,024 entries away (5 runs, interleaved) | 39.2 | 35.0 |
| | | the same, 16,384 entries away | 47.0 | 44.6 |

The one count that rose: a batch of lone proposals, or of appends, into a log that holds nothing
not yet durable grows the vector it adopted from its exact length, one reallocation more per
batch than growing a fresh one; each such batch saves an allocation and every entry's copy, so
allocator calls fell (16 lone proposals: 17.9 allocations and 1.25 reallocations per entry before,
11.75 and 1.44 after).

From the three-voter table (20 runs), end to end (control → hyper-raft copying → hyper-raft in place):

| Workload | ns per op | allocations per op | bytes per op | faults per 1k ops |
|---|---|---|---|---|
| steady batch 1, 64 B | 1,830 → 1,778 → 1,457 | 32.0 → 23.0 → 11.0 | 9,344 → 8,072 → 7,256 | 0.10 → 0.20 → 0.15 |
| steady batch 64, 64 B | 408 → 283 → 139 | 11.3 → 8.22 → 2.12 | 1,820 → 1,196 → 380 | 0.05 → 0.06 → 0.03 |
| steady batch 64, 4 KiB | 3,059 → 2,335 → 1,021 | 11.3 → 8.22 → 2.12 | 46,172 → 33,452 → 8,444 | 0.20 → 0.30 → 2.00 |
| transfer | 5,120 → 5,059 → 4,229 | 61.0 → 46.0 → 28.0 | 24,888 → 22,536 → 21,288 | 3.25 → 4.00 → 3.00 |
| failover | 6,771 → 6,906 → 6,171 | 60.9 → 47.9 → 31.9 | 31,520 → 29,456 → 28,208 | 40.0 → 35.0 → 35.0 |
| catchup, per entry | 123 → 84.5 → 33.1 | 4.01 → 3.01 → 1.01 | 626 → 418 → 146 | 1.32 → 0.93 → 0.49 |
| snapshot | 10,128 → 9,854 → 4,237 | 14.0 → 14.0 → 12.0 | 139,615 → 139,615 → 74,031 | 200 → 220 → 100 |
| fast | 3,249 → 3,178 → 2,854 | 66.0 → 57.0 → 45.0 | 16,776 → 15,504 → 14,688 | 0.20 → 0.35 → 0.30 |

Page faults: the copies' faults fell with them where they are counted above noise (catchup
1.32 → 0.93 → 0.49 per thousand entries; snapshot 200 → 220 → 100 per thousand snapshots). One
row rose: steady at batch 64 with 4 KiB entries, hyper-raft in place took about 20 faults per
9,984-entry run, against 3 copying and 2 for the control (0.002 per entry; 15 runs each: 4–21
against 3–19 and 2–18). They are the allocator's, in the first two compaction cycles: by run
length, in place against copying, 1 against 2 at 39 rounds, 19 and 2 at 78, 21 and 3 at 156, 37
and 3 at 312, and 37 and 3 at 624. Past the second compaction neither faults at all. Copying
recycles each round's 4 KiB buffers at once; in place the buffers live until the log is
compacted, and the allocator touches new pages for the second cycle's before the first cycle's
are reused.

Tried and not kept: a message queue given exactly what a burst asks for, in place of
`Outgoing::SMALLEST`'s four slots. It cut the bytes asked for per entry at batch 1 from 7,256 to
2,648, below slates' 3,368, but added a reallocation per op in the fast-track and failover
workloads (0 → 1.0), which §1a does not allow without a measured reason that outweighs it; focal's
shell also prices the queue by `SMALLEST` and `growth_of`.

## Where hyper-raft does not win, and why

hyper-raft in place allocates less than every other core in every row. It is faster than raft-rs
and both focal-raft revisions in every row but one: failover on three voters, where the ratios
(1.03 against raft-rs in the 20-run pass, 1.09 in the first) fall inside the bands. Against
slates' core alone it is faster in most rows; failover on three voters and steady at batch 64 with
4 KiB entries are inside the bands; and it is slower in these:

- **Catch-up from the log**: 3 voters, 33.1 against 25.6 ns per entry (0.77; first pass 0.74);
  5 voters, 33.0 against 26.8 (0.81). Both outside the bands. Measured on the same workload:
  - A raft-rs-line leader learns of a returned follower from a heartbeat's answer and probes it
    with an empty append, which is refused, before it sends the entries (traced: heartbeat, its
    answer, an empty append refused, then the 1,024 entries). slates' leader sends an append every
    period (`lead`) and backs up from the refusal's conflict hint. This is the protocol raft-rs
    runs, which the differential holds hyper-raft to.
  - Counting each entry's encoded length for the byte rule (prost's `encoded_len`) costs about
    4 % (removing every byte bound took 44.0 to 42.4 ns per entry); slates'
    `LogEntry::encoded_len` is arithmetic on one length.
  - The rest is spread, profiled with `sample(1)` at 16,384 entries away: the in-memory store's
    page, which clones each entry into the message (10 % in the store, 11 % in the clone, about
    20 % in malloc and free), the committed-range walk (3 %), and the queue's accounting (7.5 %
    before the single-walk change; removing it altogether moved the 1,024-entry run by under
    2 %). No single function of hyper-raft holds the gap; the largest pieces are the owner's.
  - Not closed. R-2's own message types could keep an entry's encoded length rather than
    recompute it.
- **Snapshot**: 3 voters, 4,237 against 3,103 ns per snapshot (0.73; first pass 0.77, outside the
  bands; in the 20-run pass one slow run widened hyper-raft's band to 0.73–1.37); 5 voters, 4,471
  against 4,177 (0.93, outside the bands). The same probe round before the leader learns it must
  send a snapshot, and the leader's copy of the snapshot out of storage into the message (one
  copy; slates' `install_snapshot_for` makes one too). Not closed.
- **Transfer, three voters**: 4,229 against 3,932 ns (0.93; outside the bands in the first pass,
  inside slates' in the second). A raft-rs-line leader appends and commits an empty entry of its
  term on election (raft-rs's behaviour, kept by the differential); slates' leader appends nothing
  on its classic path, so a transfer and one proposal is one append round fewer there. On five
  voters hyper-raft is faster (7,509 against 7,965 ns), where slates' core allocates 171 times and
  reallocates 12 times per transfer against hyper-raft's 48 and 1.
- **Bytes asked for at batch 1**: 7,256 against 3,368 per entry. raft-proto's `Message` is 288
  bytes against slates' `RaftMessage` of 112, and the message queue starts at four slots
  (`Outgoing::SMALLEST`), so each `Ready`'s queue asks 1,152 bytes. The exact-first queue above
  closes it at the cost of reallocations; R-2's own message types would shrink `Message`.

slates with its publication is slower than every other row by orders of magnitude at batch 1
(293,618 ns per entry on three voters): its durability copies the whole log after every
transition, where hyper-raft's owner writes each entry once, in place.

## End to end

`crates/hyper-raft-e2e` runs each member as a process (`hyper-raft-node`) on a UDP socket on the
loopback interface, with a log file it flushes with the platform's full flush before a `Ready` is
acted on (`F_FULLFSYNC` on macOS), driven in place. `tests/cluster.rs` measures the tick first:
twice the slowest of 16 flushes of 4 KiB, so that the election timeout (ten ticks or more) is far
above the broadcast time (Ongaro and Ousterhout 2014, §5.6). On this machine the tick came out at
10 to 52 ms across runs.

Scenarios and what they assert, with one run's output (2026-10-01):

| Scenario | Asserts |
|---|---|
| commits-3, commits-5 | 200 writes answered, each read back at once by a linearizable read; every member applied the same history to the same index |
| leader-killed | the leader is killed with `SIGKILL` (`TerminateProcess` on Windows) holding 5 writes in its log unanswered; the others elect in a later term; every answered write reads back, every unanswered one reads as written or as never written; the killed member restarts on its log and applies the same history |
| follower-restarts | a follower is killed, 100 writes are answered without it, it restarts on its log and catches up to the leader's commit with the same history |
| partition | the leader of five is cut off by a drop filter inside its own process: a read it is asked at once is never answered with a value and a write never acknowledged (it answers both `NotLeader` once its check of the quorum steps it down); the others elect; a key written after the cut never reads stale from it; once lifted it follows and every member applies the same history |
| all-killed | every member is killed at once and restarted on its log: every answered write reads back |

```
tick 35 ms (twice the slowest of 16 flushes)
ok commits-3: 3 members; 200 writes answered and read back (31.12 ms per write and read); all applied index 201 alike [7.4 s]
ok commits-5: 5 members; 200 writes answered and read back (34.06 ms per write and read); all applied index 201 alike [7.7 s]
ok leader-killed: leader 3 (term 1) killed with 5 writes in its log unanswered; 1 elected in term 2; 100 answered writes read back; 2 of those 5 were committed by the new leader; member 3 restarted on its log and applied index 104 alike [4.1 s]
ok follower-restarts: follower 1 killed after 30 writes, 100 written without it, restarted on its log, caught up to index 131 alike; 130 writes read back [5.4 s]
ok partition: leader 3 of 5 cut off; at once it answered a read with Some(NotLeader(0)) and a write with Some(NotLeader(0)), and later the read of a replaced key with Some(NotLeader(0)); 1 elected in term 2; after the filter lifted, 1 leads and all applied index 64 alike; 61 writes read back [3.6 s]
ok all-killed: every member killed after 50 answered writes and restarted on its log; 3 leads in term 2; 50 writes read back; all applied index 52 alike [2.8 s]
```

The workspace's tests, these scenarios included, also pass on Linux (aarch64, in Docker on this
machine, `rust:1.98.0`, `fdatasync` on the VM's file system; the tick came out at 36 ms). They
have not been run on Windows here; CI runs the gates on all six targets.

`tests/wal.rs` covers the log's torn-tail cut, its refusal of a damaged record that is not the
last, and its bound; `tests/wire.rs` covers damaged and cut datagrams. One group at a time runs,
at most five member processes of one thread each, and the test itself is one thread
(`harness = false`).

## Commands

```sh
# From the repository root. The comparison is a workspace of its own and fetches the other
# projects' repositories at the revisions in crates/hyper-raft-compare/Cargo.toml.
cd crates/hyper-raft-compare
cargo build --release
B=target/release/hyper-raft-compare

# The batch sweep (5 runs per cell).
$B sweep 5 64

# The tables: every core but slates' publication, a first pass of 10 runs, then each group size
# at 20 runs (the tables above), then slates with its publication at 64 B entries, 4 runs.
CORES=hyper,hyper-copy,control,mantle,slates-core,raftrs $B table 10
VOTERS=3 CORES=hyper,hyper-copy,control,mantle,slates-core,raftrs $B table 20
VOTERS=5 CORES=hyper,hyper-copy,control,mantle,slates-core,raftrs $B table 20
CORES=slates MAX_BYTES=64 $B table 4

# One measurement, as the tables run it: a timed process and a counting process of one seed.
$B one hyper steady 3 1 64 20000 1000 time
$B one hyper steady 3 1 64 20000 1000 count

# The recorded-seed equivalence (from the repository root).
HYPER_RAFT_SEEDS=1000 HYPER_RAFT_SEED=1000 cargo test -p hyper-raft --release -- --test-threads=4 --nocapture

# The end-to-end scenarios, and every gate.
cargo test -p hyper-raft-e2e --test cluster
CARGO_BUILD_JOBS=8 bash scripts/gates.sh
```

# hyper-log against mantle-log and focal-log

The law (`CLAUDE.md` §1a) for the log, moved from mantle by note 32's L-1 and L-2
(`crates/hyper-log/ORIGIN.md`): its allocations, reallocations and page faults per append and per
fetch, before and after L-2; its throughput and latency against mantle's own log and focal's; and
real processes killed mid-append.

## The machine

The same Apple M5 Max (`Mac17,6`, 18 cores, 128 GiB), macOS 26.4.1, rustc 1.98.0, its internal SSD
(APFS, `F_FULLFSYNC`), 2026-10-01 in the morning PDT. The machine was shared with other sessions:
the load average was 20 to 29 during the runs, which the spreads below show.

## Equivalence

`crates/hyper-log/tests/equivalence.rs`: 24 seeds of four cycles of 40 rounds, groups appending,
conflicting, compacting, voting, proposing, leaving and sending invalid updates, each round's
batch fixed by holding the device in a flush, with power cuts, random-sector crashes and bit flips
at rest between cycles, so that opens restore, mark and fence (over the 24 seeds: 9,919 updates
written, 1,802 fenced, 203 invalid, 105 refused for damage, 37 for backlog; ten opens restored
groups, five fenced one). The same harness, renamed, was run against mantle-log at mantle `147f035`
(in a scratch clone; mantle untouched) and wrote 48 files: the transcripts of every answer, view,
fetch and recovery, and each seed's device image with each segment's random nonce replaced by its
incarnation and the checksums over it recomputed. hyper-log writes the same 48 files byte for byte
at L-1 (`5bd0699`), at L-2 (`4e58930`), after the allocation work (`4b48295`) and after the wall
(`af2679b`); the test asserts mantle's hashes of them.

## Allocations

`cargo bench -p hyper-log --bench allocs`: a log on a real file, closed-loop appends from 1 and 16
replicas under `Waits::Measured`, 200 rounds after 20; every thread of the process counted
(`hyper_measure::alloc::begin_process`), since the log runs threads of its own. The updates are built
before the count begins, so an append's count is the log's own. "Before" is L-1 (`50582fa`), which is
mantle-log's code; "after" is this branch.

| Per operation | replicas | before: allocations, reallocations, bytes | after: allocations, reallocations, bytes |
|---|---|---|---|
| append, 128 B | 1 | 23.0, 5.0, 22,793 | **0.00**, 0.01, 63 |
| append, 1 KiB | 1 | 23.0, 5.0, 23,688 | **0.00**, 0.01, 63 |
| append, 16 KiB | 1 | 23.0, 5.0, 39,048 | **0.00**, 0.01, 63 |
| append, 128 B | 16 | 8.38, 0.77, 4,377 | **0.00**, 0.01, 63 |
| append, 16 KiB | 16 | 8.38, 0.77, 20,718 | **0.00**, 0.02, 63 |
| fetch of an entry in memory, `entries` | 16 | 2.00, 0, 248 | 2.00, 0, 256 (128 B) to 16,512 (16 KiB) |
| the same, into a reservation (`fetch`) | 16 | – | **0.00**, 0, 0 |
| fetch of an entry from the file, `entries` | 16 | 5.44, 0, 1,296 (128 B) | 2.56, 0, 882 |
| the same, into a reservation (`fetch`) | 16 | – | **0.00**, 0, 0 |
| term, view | 1, 16 | 0, 0, 0 | 0, 0, 0 |

- An append allocates nothing once warm. The 0.01 to 0.02 reallocations are each group's list of
  retained entries doubling as it grows; the 63 bytes are those reallocations.
- `entries` hands back `Entry`s with bytes of their own, so it makes the vector and a copy of each
  entry: two allocations for one entry, as mantle's did, and the entry's bytes, where mantle's shared
  an `Arc<[u8]>` with the log (248 bytes whatever the size). `fetch` copies into the caller's
  reservation and allocates nothing.
- The rows of one replica's fetches from the file (15 allocations) are a fresh log's first read: the
  device's buffer pool and the thread's reservation are made then.
- Minor page faults were at most 0.015 an append and 0 to 2 a fetch, before and after.

## Throughput and latency

`crates/hyper-log-compare`, a workspace of its own, runs mantle's own benchmark workload
(`mantle bench log`, mantle `crates/mantle/src/bench_log.rs` at `147f035`) on each log: closed-loop
replicas, each appending one entry to its own group and waiting until it is durable, held as
records by at most a granted core's worth of driver threads that hear of each answer through the
replica's waker; every replica keeps 64 entries behind its last; 1 s a point; a scratch file on the
internal SSD.

- **mantle**: mantle-log through mantle's own binary at `147f035`, built in a scratch clone:
  `mantle bench log <dir> --seconds 1 --sizes <bytes> --replicas <n> --skip-device`.
- **hyper**: hyper-log, this branch, driven as `crates/hyper-log/benches/log.rs` drives it.
- **focal**: focal-log's `SharedWal` at focal `origin/slates-port` `4bf7b64`, a `WalLease` per
  replica appending with `append_async_notified`, whose notification is the waker. focal keeps a
  group's window by a checkpoint of the entries it keeps (`rewrite_checkpoint_async_notified`), where
  mantle and hyper-log write a start: every 64th append is that checkpoint of 64 entries, the cadence
  note 32 §3.9 asks the two to be compared at. focal refuses an append with `Capacity` when its
  writer's budget is spent; a refused replica tries again after the next answer.

Five rounds a point, the three logs in a rotated order each round, each point in a fresh process.
Medians, with the least and the most. "Allocations an append" is a separate counting run, every
thread's, building each update included (an entry's bytes and its list: two of hyper's), and is not
measured for mantle's binary, which has no counting allocator (its counts are the "before" column
above).

| entry | replicas | log | appends/s, median (least–most) | p50 ms | p99 ms | p99.9 ms | appends a flush | reopen ms | threads | allocations an append |
|---|---|---|---|---|---|---|---|---|---|---|
| 128 B | 1 | mantle | 60 (51–80) | 17.30 | 25.60 | 25.60 | 1.0 | 14.60 | 3 | – |
| 128 B | 1 | hyper | 53 (37–62) | 16.97 | 27.63 | 28.93 | 1.0 | 12.33 | 4 | 2.89 |
| 128 B | 1 | focal | 31 (27–44) | 32.70 | 42.44 | 42.44 | 1.0 | 0.43 | 3 | 16.82 |
| 128 B | 4 | mantle | 200 (175–355) | 20.40 | 38.80 | 38.80 | 3.9 | 10.80 | 6 | – |
| 128 B | 4 | hyper | 199 (119–240) | 18.98 | 33.07 | 35.55 | 3.9 | 14.52 | 7 | 2.69 |
| 128 B | 4 | focal | 61 (51–73) | 66.93 | 92.41 | 92.44 | 2.2 | 0.57 | 6 | 13.04 |
| 128 B | 16 | mantle | 760 (685–949) | 20.40 | 39.50 | 39.50 | 15.7 | 12.10 | 18 | – |
| 128 B | 16 | hyper | 654 (592–1020) | 20.18 | 43.24 | 43.26 | 15.6 | 12.27 | 19 | 2.33 |
| 128 B | 16 | focal | 246 (122–387) | 54.78 | 104.59 | 104.60 | 9.2 | 1.25 | 18 | 11.13 |
| 128 B | 64 | mantle | 2470 (2200–3180) | 25.70 | 57.70 | 57.80 | 61.2 | 13.80 | 20 | – |
| 128 B | 64 | hyper | 2466 (1453–3002) | 24.08 | 50.42 | 50.52 | 56.9 | 15.47 | 21 | 2.27 |
| 128 B | 64 | focal | 928 (220–2215) | 67.29 | 117.91 | 123.49 | 39.9 | 3.49 | 20 | 10.40 |
| 128 B | 256 | mantle | 9810 (9190–13200) | 26.20 | 49.20 | 49.20 | 217.1 | 19.30 | 20 | – |
| 128 B | 256 | hyper | 13292 (8820–17067) | 15.50 | 41.47 | 48.05 | 242.5 | 15.92 | 21 | 2.19 |
| 128 B | 256 | focal | 875 (601–1327) | 76.44 | 135.64 | 135.68 | 35.8 | 3.68 | 20 | 14.47 |
| 1024 B | 1 | mantle | 57 (49–86) | 17.30 | 32.00 | 32.00 | 1.0 | 16.00 | 3 | – |
| 1024 B | 1 | hyper | 54 (51–101) | 17.97 | 28.67 | 30.96 | 1.0 | 15.63 | 4 | 2.91 |
| 1024 B | 1 | focal | 29 (24–41) | 35.41 | 63.67 | 63.67 | 1.0 | 0.43 | 3 | 16.39 |
| 1024 B | 4 | mantle | 256 (168–335) | 16.30 | 30.80 | 30.80 | 3.9 | 8.42 | 6 | – |
| 1024 B | 4 | hyper | 211 (202–366) | 17.89 | 29.86 | 30.79 | 3.9 | 12.27 | 7 | 2.34 |
| 1024 B | 4 | focal | 72 (55–133) | 54.99 | 80.79 | 84.73 | 2.0 | 0.59 | 6 | 13.62 |
| 1024 B | 16 | mantle | 662 (605–748) | 23.60 | 46.30 | 46.30 | 15.3 | 11.30 | 18 | – |
| 1024 B | 16 | hyper | 812 (712–839) | 20.35 | 34.04 | 34.05 | 15.7 | 11.66 | 19 | 2.58 |
| 1024 B | 16 | focal | 193 (155–252) | 82.37 | 124.95 | 124.96 | 9.5 | 0.97 | 18 | 11.68 |
| 1024 B | 64 | mantle | 2870 (2560–6270) | 13.90 | 44.70 | 44.70 | 61.2 | 15.50 | 20 | – |
| 1024 B | 64 | hyper | 2847 (2365–3322) | 21.27 | 39.24 | 42.33 | 62.4 | 18.54 | 21 | 2.19 |
| 1024 B | 64 | focal | 986 (743–1188) | 54.97 | 115.09 | 134.54 | 43.8 | 3.64 | 20 | 10.22 |
| 1024 B | 256 | mantle | 10100 (7170–13900) | 25.20 | 47.20 | 47.30 | 225.8 | 19.90 | 20 | – |
| 1024 B | 256 | hyper | 9754 (5763–12477) | 26.50 | 40.25 | 40.44 | 212.3 | 21.26 | 21 | 2.21 |
| 1024 B | 256 | focal | 1013 (921–1354) | 50.03 | 118.94 | 118.96 | 38.6 | 4.21 | 20 | 14.64 |
| 16384 B | 1 | mantle | 55 (38–70) | 17.30 | 30.70 | 30.70 | 1.0 | 11.40 | 3 | – |
| 16384 B | 1 | hyper | 50 (46–79) | 20.02 | 30.25 | 30.25 | 1.0 | 12.49 | 4 | 2.56 |
| 16384 B | 1 | focal | 27 (24–44) | 37.07 | 57.25 | 57.25 | 1.0 | 0.71 | 3 | 16.64 |
| 16384 B | 4 | mantle | 198 (120–225) | 20.40 | 39.10 | 39.10 | 3.9 | 9.85 | 6 | – |
| 16384 B | 4 | hyper | 281 (202–347) | 12.66 | 30.55 | 30.93 | 3.9 | 12.29 | 7 | 2.54 |
| 16384 B | 4 | focal | 65 (55–77) | 62.72 | 78.20 | 78.21 | 2.1 | 1.03 | 6 | 13.61 |
| 16384 B | 16 | mantle | 724 (583–1010) | 21.50 | 41.10 | 41.10 | 15.7 | 18.60 | 18 | – |
| 16384 B | 16 | hyper | 707 (687–1302) | 21.39 | 43.41 | 43.51 | 15.7 | 21.78 | 19 | 2.72 |
| 16384 B | 16 | focal | 257 (126–291) | 62.74 | 110.27 | 110.39 | 10.3 | 2.76 | 18 | 11.91 |
| 16384 B | 64 | mantle | 3740 (2650–5510) | 17.80 | 32.00 | 39.70 | 62.5 | 25.00 | 20 | – |
| 16384 B | 64 | hyper | 3253 (2773–4087) | 21.35 | 44.51 | 44.58 | 62.6 | 30.65 | 21 | 2.28 |
| 16384 B | 64 | focal | 1102 (642–3065) | 56.55 | 92.33 | 98.21 | 40.9 | 10.83 | 20 | 10.62 |
| 16384 B | 256 | mantle | 7380 (5040–10600) | 30.90 | 77.20 | 77.20 | 183.2 | 51.70 | 20 | – |
| 16384 B | 256 | hyper | 8255 (6974–12381) | 31.70 | 48.74 | 49.00 | 245.3 | 67.32 | 21 | 2.21 |
| 16384 B | 256 | focal | 706 (353–2019) | 91.64 | 143.44 | 153.42 | 38.5 | 7.79 | 20 | 16.97 |

What it shows, within what the spreads allow:
- **hyper against mantle.** Within each other's spread at every point: the medians differ by
  -18% (four replicas of 1 KiB) to +42% (four of 16 KiB), with no direction across the grid. p99 is
  lower for hyper at 11 of 15 points, higher at 4. A lone replica's append takes two flushes in both, the frame's
  and its confirmation's (mantle `docs/design/raft-log.md` §6), about 17 ms at the median here.
- **The cost of L-2's design** is in hyper's threads, one more than mantle's (the owner and the
  device thread, where mantle had one writer), and in a submission's admission, a round trip to the
  owner where mantle took room with a lock: microseconds against a flush of milliseconds, invisible
  in these rows. Reopen times are alike; recovery is mantle's code.
- **focal** runs at a half to a tenth of mantle's and hyper's rate, at two to three times their
  median latency: its appends
  carry 1 to 44 records a flush at most here (its writer batches at most 64 requests), its
  checkpoints write 64 entries every 64th append, and it refuses for budget at 64 and 256 replicas.
  It reopens far faster: its replay reads only what checkpoints keep, where the mantle format
  replays every live segment.

## End to end

`crates/hyper-log-e2e`: a writer process (`hyper-log-writer`) appends to a log in a real file
(direct I/O where the file system takes it, `F_FULLFSYNC`), eight groups in rounds, each entry with a
hard state and a compaction every 64th, printing each acknowledged append. `tests/kill.rs` kills it
with `SIGKILL` (`TerminateProcess` on Windows) after a number of acknowledgements drawn for each of 24
cycles, so the kill lands while frames are being written, flushed and confirmed; after each kill it
opens the log in its own process and checks, against every append the writer acknowledged: no group
damaged; every acknowledged entry held, with its bytes, from where the group's log starts; no start
behind the one acknowledged; no commit behind the last acknowledged append. Then the writer restarts
on the file and goes on; the last runs to its end and closes the log.

```
24 writers killed, 1784 appends acknowledged across 8 groups, 3 acknowledgements read after a kill
test a_writer_killed_mid_append_loses_nothing_it_acknowledged ... ok
```

## Commands for the log

```sh
# The equivalence, the suites and the kill test.
cargo test -p hyper-log -p hyper-block -p hyper-log-e2e --all-features

# Allocations per append and per fetch.
cargo bench -p hyper-log --bench allocs

# hyper-log alone, mantle's workload and columns.
cargo bench -p hyper-log --bench log -- DIR 1.0

# The comparison: mantle's binary built at 147f035 in a clone (cargo build --release -p mantle).
cd crates/hyper-log-compare && cargo build --release
target/release/hyper-log-compare table DIR 5 1.0 path/to/mantle
```
