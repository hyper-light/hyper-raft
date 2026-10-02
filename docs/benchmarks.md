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

## Ports from focal, counted

Each port from focal (`crates/hyper-raft/ORIGIN.md`, "Ports from focal") was counted with
`hyper-raft-compare one <core> <workload> ... 1000 count`, in place and copying, at three and five
voters, on every workload of the tables, before and after. Allocation and reallocation counts are
exact for a seed, so a change in them is a change in the code, not noise.

| Port | Commit | Allocations and reallocations | Bytes asked for per op |
|---|---|---|---|
| F43, reads share a round | `1173f20` | identical in every row | identical in every row |
| F41, a window bounded in bytes | `c548f42` | identical in every row | identical on steady, catchup and fast; transfer +2,048 (3 voters) and +4,096 (5); failover +1,024 and +3,072; snapshot +48 and +80 |
| F42, heartbeat answers that say where the member is | `24adbae` | identical in every row | identical but for snapshot, +24 (3 voters) and +40 (5) |
| The fast track's safety fix | the commit after `24adbae` | identical in every row | identical in every row |

F41's bytes: a member's window holds each message's bytes beside its last index, sixteen bytes a
slot against eight, and reserves its 128 slots (`Settings::shell`'s window) the first time it
takes a message after a reset. A transfer or an election resets every follower's window once, so
those rows ask 1,024 bytes more per follower per reset, and no more allocations; the steady and
catch-up rows, whose windows are never reset, ask for nothing more. The byte bound itself costs
no walk: the bytes it charges are counted where the page is chosen (ORIGIN.md, F41).

F42's bytes: each member's progress keeps its stalled ticks, eight bytes more a member, and a
member that installs a snapshot builds its tracker anew, so the snapshot rows ask 8 bytes more per
member per snapshot. The workloads deliver every message, so `HeartbeatAnswers::Position` and the
stall's probe change nothing they count.

The safety fix adds no allocation to any row. The fast workload (one leader, no change of the
configuration, every message delivered) counts exactly as before: its followers' logs are of the
leader's term before their first fast vote is counted, and the voters it was elected under are the
voters in force. The two vectors the leader keeps of its term's voters are filled at an election
only in a fast group, and keep their capacity from one election to the next.

Time, all four changes together against `e1e292c` (in place, three voters, 10 timed runs each,
the two builds interleaved run by run, 2026-10-01 near 09:00 PDT, load average 7 to 10; nanoseconds
per op, median [min–max]):

| Workload | `e1e292c` | after the four | ratio |
|---|---|---|---|
| steady b1 64 B | 1,456.6 [1,442.1–1,465.7] | 1,464.7 [1,445.2–1,480.4] | 1.006 |
| steady b64 64 B | 145.5 [139.7–148.7] | 137.6 [135.1–141.4] | 0.946 |
| catchup b64 64 B | 34.0 [32.2–35.2] | 34.7 [34.3–36.3] | 1.021 |
| fast b1 64 B | 2,854.9 [2,826.0–3,054.9] | 2,939.5 [2,889.3–3,228.9] | 1.030 |
| transfer | 4,385.9 [4,150.5–4,648.0] | 4,478.0 [4,087.9–4,664.7] | 1.021 |
| failover | 6,535.6 [5,869.4–6,868.3] | 6,467.4 [5,991.9–6,864.1] | 0.990 |

Every range overlaps its pair's; no difference is claimed.

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

## The message's layout

R-2 (`docs/raft.md` §3.1) made the message hyper-raft's own type, so its layout is hyper-raft's to
choose. The snapshot a message may carry was held inline, as raft-proto held it: 144 of the
message's 280 bytes, in every heartbeat, vote and append, though only a snapshot message fills it.
It is now boxed (`Message::snapshot: Option<Box<Snapshot>>`): a message is 144 bytes, and a
snapshot message makes one allocation more, for a box of 144 bytes beside the image it already
carries.

`hyper-raft-compare one hyper <workload> 3 1 64 20000 1000 count`, before (R-2, `d472677`) and
after, per operation; the same machine and day, 2026-10-01 around 11:30 PDT, shared:

| Workload | Bytes asked before | after | Allocations before | after |
|---|---|---|---|---|
| steady, per entry | 7,064 | 3,800 | 11 | 11 |
| catch-up, per entry | 143.7 | 139.9 | 1.01 | 1.01 |
| snapshot, per snapshot | 73,856 | 70,192 | 12 | 13 |
| transfer, per transfer | 22,824 | 14,120 | 28 | 28 |

slates' core asks 3,368 bytes and makes 51 allocations per entry on the steady workload at batch 1
(`one slates-core steady 3 1 64 20000 1000 count`): the gap in bytes asked, 7,064 against 3,368
in the tables above, is now 3,800 against 3,368. The remaining 432 bytes are not yet traced: the
four-slot first queue (`Outgoing::SMALLEST`) the earlier trace named now asks 576 bytes a ready,
more than the gap, so it is not the whole account. An entry's bytes for the byte rules are now arithmetic on its lengths
(`proto::encoded_bytes`), where raft-proto's were prost's `encoded_len`: the "counting each
entry's encoded length" item above is closed by R-2.

## End to end

`crates/hyper-raft-e2e` runs each member as a process (`hyper-raft-node`) on a UDP socket on the
loopback interface, with a log file it flushes with the platform's full flush before a `Ready` is
acted on (`F_FULLFSYNC` on macOS), driven in place. `tests/cluster.rs` measures the tick first, from the 95/95 one-sided
tolerance bound (Wilks 1941: the slowest of 59 samples) of three times: a flush of the largest
message a member sends, a loopback round trip of that message, and a socket wait asked for 1 ms.
A tick is at least one broadcast time — two flushes and two datagrams in series, leader then
follower — so the election timeout (ten ticks) is an order of magnitude above it (Ongaro and
Ousterhout 2014, §5.6); and at least the wait, since the leader's heartbeat goes out on a tick the
OS timer must keep (Windows ends a timed wait on its 15.6 ms clock interrupt). A member takes every
tick that elapsed when it wakes, at most `2 · election_tick`, so its Raft clock keeps wall time
however late the OS wakes it; an earlier member took one tick per wake, and on windows-2025 a 1 ms
tick ran its elections about fifteen times slower than their budget. The largest datagram is
measured on each socket (macOS caps a send at 9,216 bytes by default), and an append's entries are
bounded to it less the message's fixed bytes. On this machine the tick came out at
17 to 120 ms across runs, at load averages up to 43.

The election timeout is ten ticks (Raft §5.6; etcd's tuning guide asks at least ten round trips)
and the heartbeat one tick, the round trip (etcd's tuning guide). Every count in a scenario is
derived, not picked:

- A phase writes 59 writes, the 95/95 sample count, and `commits` reports their 95/95 bound.
  `leader-killed` sends a phase's worth in flight as the leader dies.
- A request waits `2 · election_tick + 1` ticks for its answer. A member not leading answers at
  once; a leader answers within a broadcast, or, cut off, finds out within two election
  timeouts by its quorum check and then answers all it held `NotLeader`. The extra tick is the
  answer's own trip.
- One write or read allows for as many elections as make every election the run causes (each
  scenario's first, the leader killed, the leader cut off, every member killed) succeed with 95%
  confidence across the run (Bonferroni). An election elects when the earliest of the voters'
  randomized timers, drawn from the ten ticks of `[10, 20)`, fires two ticks (its pre-vote and
  vote rounds) before the next: 61% for three voters, 44% for five, so 6 and 9 elections of at
  most 22 ticks each.
- Each member holds the keys its scenario writes, the asks the in-flight phase and the client
  leave waiting, and an entry for every try a write may make within its budget plus a leader's
  empty entry for every election.
- A member serves until its standard input closes: the test holds the pipe, so no member
  outlives it however the test ends, with no deadline to choose.

Scenarios and what they assert, with one run's output (2026-10-01):

| Scenario | Asserts |
|---|---|
| commits-3, commits-5 | 59 writes answered, each read back at once by a linearizable read; every member applied the same history to the same index |
| leader-killed | the leader is killed with `SIGKILL` (`TerminateProcess` on Windows) holding 59 writes in its log unanswered; the others elect in a later term; every answered write reads back, every unanswered one reads as written or as never written; the killed member restarts on its log and applies the same history |
| follower-restarts | a follower is killed, 59 writes are answered without it, it restarts on its log and catches up to the leader's commit with the same history |
| partition | the leader of five is cut off by a drop filter inside its own process: a read it is asked at once is never answered with a value and a write never acknowledged (it answers both `NotLeader` once its check of the quorum steps it down); the others elect; a key written after the cut never reads stale from it; once lifted it follows and every member applies the same history |
| all-killed | every member is killed at once and restarted on its log: every answered write reads back |

```
tick 18 ms (the 95/95 bounds of a broadcast and a timed wait, 59 samples each)
ok commits-3: 3 members; 59 writes answered and read back (22.65 ms per write and read, 34.80 ms the 95/95 bound); all applied index 60 alike [1.9 s]
ok commits-5: 5 members; 59 writes answered and read back (23.91 ms per write and read, 34.43 ms the 95/95 bound); all applied index 60 alike [1.7 s]
ok leader-killed: leader 3 (term 1) killed with 59 writes in its log unanswered; 1 elected in term 2; 118 answered writes read back; 1 of those 59 were committed by the new leader; member 3 restarted on its log and applied index 121 alike [3.0 s]
ok follower-restarts: follower 1 killed after 59 writes, 59 written without it, restarted on its log, caught up to index 119 alike; 118 writes read back [3.0 s]
ok partition: leader 3 of 5 cut off; at once it answered a read with Some(NotLeader(0)) and a write with Some(NotLeader(0)), and later the read of a replaced key with Some(NotLeader(0)); 5 elected in term 2; after the filter lifted, 5 leads and all applied index 122 alike; 119 writes read back [4.7 s]
ok all-killed: every member killed after 59 answered writes and restarted on its log; 3 leads in term 2; 59 writes read back; all applied index 61 alike [2.2 s]
```

That run was at load average 43.

The workspace's tests, these scenarios included, also pass on Linux (aarch64, in Docker on this
machine, `rust:1.98.0`, `fdatasync` on the VM's file system; the tick came out at 36 ms). They
have not been run on Windows here; CI runs the gates on all six targets.

A member's loop waits on its socket until its next tick, and when the tick is already due it
still takes what has arrived before it ticks. On a loaded machine a turn of the loop can outlast
a tick, so the tick is due at every turn. A member that skipped its socket then kept ticking
without reading anything: a leader stepped down by its quorum check with its followers' answers
still unread in its socket, and the scenario stalled on the election that followed. Two runs
showed this before the fix: one with a parallel build loading the machine (tick 12 ms, load
average 39), one with the tick forced to 4 ms under the same load. Each leader read nothing for
100 to 180 ms, about ten ticks fired with nothing read in between, and the answers it had missed
were the first datagrams it read after stepping down. `node::tests` holds the directed test.

The client waits on facts, with the bounds derived above. One write or read is retried through its
elections rather than for a fixed number of requests:
during an election, members that name no leader or a stale one answer at once, and a count of
requests ran out long before the election ended. When a member names no other leader, the
client lets a heartbeat interval pass before it asks the next member. `leader-killed` sends its
in-flight writes to whoever leads at that moment, not to the first leader, because earlier
writes may have moved the leadership. After the fix the suite passed 100 runs in a row, 50 of
them under load.

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

It held on most CI runs and failed now and then (windows-11-arm at main `16a1621`, seeds 19 and
23; macos-15-intel at `a2d42b5`, seed 0). Pinned to one core (`taskset -c 0` in a `rust:1.98.0`
container), main's test failed about one run in ten, which made the causes visible in the
transcripts and the device images:
- **The harness.** `Holder::is_held` never consumed its event, so from a cycle's second round on a
  round waited for nothing before submitting its updates. The owner's drain could then take the
  plug and the updates into one frame: seed 19's round 114 wrote 67 frames where mantle wrote 68.
  Each round now waits for the plug's flush or the plug's answer, both events the holder hears of.
- **The fair queue's busy period** [SFQ96 §2] ended when the owner parked, after it had drained what
  answered callers sent once the answers had gone out; a caller quick enough found the last period's
  finish tags still standing, and two updates of one frame swapped places (seed 0's frame 116 began
  with group 5's record, not group 2's). The period now ends when the answers go out with nothing
  queued.
- **"Another frame follows"** reached the device after the caller heard its submission admitted,
  so a caller that released a held flush on hearing it could beat the word, and the frame was
  confirmed on its own: one confirmation record more.

With the three fixed (`d04934e`, `7585e19`) the branch passed 200 of 200 runs pinned to one core
and 50 of 50 unpinned, every hash mantle's.

With the owner on its own thread and blocking callers flushing their own frames (`log-switch`),
the same: built once in `rust:1.98.0` (aarch64 Linux in Docker on this Mac, 18 cores), the test
binary passed 200 of 200 runs pinned to core 0 (`taskset -c 0`) and 50 of 50 unpinned, every hash
mantle's, 18:06–18:16 PDT.

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
| append through the group's handle, `GroupLog::submit` | 1, 16 | – | **0.00**, 0.01–0.03, 63–149 |
| fetch, term and bounds through the handle | 1, 16 | – | **0**, 0, 0; the handle asked the owner for nothing |

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

### The same, after the replica's path work

The table again at this branch's `7585e19` (the group's handle, leader/followers), the same
binaries for mantle and focal, 17:18–17:24 PDT, load 26 to 41. The workload drives hyper-log by
`Log::submit_waking`, as mantle's bench drives its log, not through handles.

| entry | replicas | log | appends/s, median (least–most) | p50 ms | p99 ms | p99.9 ms | appends a flush | reopen ms | threads | allocations an append |
|---|---|---|---|---|---|---|---|---|---|---|
| 128 B | 1 | mantle | 40 (31–58) | 25.20 | 44.50 | 44.50 | 1.0 | 15.70 | 3 | – |
| 128 B | 1 | hyper | 39 (30–99) | 24.61 | 41.41 | 41.41 | 1.0 | 16.66 | 4 | 3.40 |
| 128 B | 1 | focal | 14 (11–38) | 71.70 | 106.39 | 106.39 | 1.0 | 0.25 | 3 | 16.95 |
| 128 B | 4 | mantle | 189 (162–296) | 20.40 | 33.90 | 33.90 | 3.9 | 11.30 | 6 | – |
| 128 B | 4 | hyper | 351 (170–432) | 9.47 | 21.21 | 21.49 | 4.0 | 11.68 | 7 | 2.43 |
| 128 B | 4 | focal | 105 (33–140) | 28.03 | 107.31 | 107.32 | 2.1 | 0.50 | 6 | 13.33 |
| 128 B | 16 | mantle | 1870 (1220–1890) | 8.65 | 11.00 | 12.60 | 15.9 | 6.08 | 18 | – |
| 128 B | 16 | hyper | 1881 (1271–1893) | 8.48 | 10.70 | 13.55 | 15.9 | 6.03 | 19 | 2.23 |
| 128 B | 16 | focal | 765 (735–998) | 24.45 | 26.71 | 27.07 | 9.7 | 1.49 | 18 | 10.89 |
| 128 B | 64 | mantle | 5350 (4700–6290) | 10.50 | 24.60 | 27.80 | 61.8 | 7.17 | 20 | – |
| 128 B | 64 | hyper | 4486 (4400–7330) | 13.53 | 28.56 | 28.64 | 63.1 | 9.73 | 21 | 2.10 |
| 128 B | 64 | focal | 3420 (2007–3590) | 14.83 | 51.46 | 58.15 | 51.1 | 5.02 | 20 | 10.17 |
| 128 B | 256 | mantle | 22500 (21600–27600) | 9.70 | 25.60 | 30.10 | 168.7 | 11.90 | 20 | – |
| 128 B | 256 | hyper | 20794 (17920–27543) | 10.39 | 26.71 | 26.83 | 252.6 | 11.93 | 21 | 2.09 |
| 128 B | 256 | focal | 2851 (472–3040) | 25.20 | 42.36 | 51.89 | 40.9 | 4.30 | 20 | 14.03 |
| 1024 B | 1 | mantle | 95 (78–117) | 8.65 | 18.10 | 18.10 | 1.0 | 5.86 | 3 | – |
| 1024 B | 1 | hyper | 109 (62–119) | 8.50 | 19.08 | 21.96 | 1.0 | 5.16 | 4 | 2.50 |
| 1024 B | 1 | focal | 66 (53–75) | 12.79 | 24.75 | 26.73 | 1.0 | 0.37 | 3 | 16.27 |
| 1024 B | 4 | mantle | 202 (172–286) | 20.40 | 28.80 | 34.20 | 3.9 | 9.48 | 6 | – |
| 1024 B | 4 | hyper | 220 (208–259) | 17.05 | 27.72 | 27.76 | 3.9 | 12.30 | 7 | 2.57 |
| 1024 B | 4 | focal | 72 (66–91) | 55.76 | 69.73 | 71.14 | 2.0 | 0.36 | 6 | 13.25 |
| 1024 B | 16 | mantle | 745 (662–881) | 21.50 | 47.90 | 47.90 | 15.6 | 11.80 | 18 | – |
| 1024 B | 16 | hyper | 787 (519–986) | 21.05 | 41.36 | 41.40 | 15.7 | 11.24 | 19 | 2.42 |
| 1024 B | 16 | focal | 278 (218–449) | 56.20 | 97.93 | 102.16 | 10.3 | 0.78 | 18 | 10.97 |
| 1024 B | 64 | mantle | 3660 (3540–6430) | 17.30 | 32.30 | 32.30 | 61.9 | 11.70 | 20 | – |
| 1024 B | 64 | hyper | 4218 (3509–6819) | 16.03 | 24.06 | 24.08 | 62.9 | 11.46 | 21 | 2.13 |
| 1024 B | 64 | focal | 2090 (1832–3486) | 28.87 | 57.20 | 62.52 | 52.7 | 5.09 | 20 | 10.21 |
| 1024 B | 256 | mantle | 16800 (14300–19400) | 16.30 | 30.10 | 30.10 | 238.9 | 17.60 | 20 | – |
| 1024 B | 256 | hyper | 24962 (14206–25168) | 9.63 | 21.20 | 21.38 | 253.3 | 20.71 | 21 | 2.10 |
| 1024 B | 256 | focal | 3530 (2732–4240) | 13.77 | 51.05 | 52.17 | 54.6 | 7.27 | 20 | 14.38 |
| 16384 B | 1 | mantle | 99 (82–114) | 8.91 | 18.00 | 18.90 | 1.0 | 7.33 | 3 | – |
| 16384 B | 1 | hyper | 114 (84–119) | 8.51 | 14.98 | 16.48 | 1.0 | 7.10 | 4 | 2.66 |
| 16384 B | 1 | focal | 63 (52–75) | 12.84 | 27.72 | 28.69 | 1.0 | 0.67 | 3 | 16.34 |
| 16384 B | 4 | mantle | 219 (162–396) | 17.80 | 33.10 | 33.10 | 3.9 | 8.06 | 6 | – |
| 16384 B | 4 | hyper | 234 (178–447) | 16.02 | 28.17 | 32.26 | 3.9 | 11.62 | 7 | 2.38 |
| 16384 B | 4 | focal | 99 (89–149) | 42.50 | 60.96 | 62.68 | 2.0 | 0.97 | 6 | 13.42 |
| 16384 B | 16 | mantle | 1170 (1090–1820) | 13.10 | 25.10 | 25.10 | 15.8 | 13.60 | 18 | – |
| 16384 B | 16 | hyper | 1709 (1034–1869) | 8.53 | 16.16 | 18.97 | 15.9 | 18.03 | 19 | 2.62 |
| 16384 B | 16 | focal | 785 (341–956) | 13.89 | 54.46 | 59.26 | 12.7 | 5.72 | 18 | 10.76 |
| 16384 B | 64 | mantle | 6420 (2710–7320) | 9.70 | 21.50 | 28.80 | 62.4 | 22.90 | 20 | – |
| 16384 B | 64 | hyper | 5118 (2556–7125) | 9.49 | 21.38 | 21.41 | 63.2 | 31.65 | 21 | 2.20 |
| 16384 B | 64 | focal | 3828 (834–4411) | 14.67 | 50.72 | 53.76 | 56.7 | 23.89 | 20 | 10.29 |
| 16384 B | 256 | mantle | 17600 (11700–19400) | 13.10 | 31.90 | 31.90 | 239.4 | 64.00 | 20 | – |
| 16384 B | 256 | hyper | 17655 (13652–20034) | 12.75 | 29.88 | 30.03 | 252.4 | 80.24 | 21 | 2.10 |
| 16384 B | 256 | focal | 3740 (3123–4019) | 13.69 | 40.82 | 50.01 | 57.2 | 24.32 | 20 | 14.59 |

hyper's median is above mantle-log's at 11 of 15 points (1% to 86% higher) and below at 4 (128 B
at 1, 64 and 256 replicas, 16 KiB at 64: 3% to 20% lower); its p99 is lower at 12 of 15. focal-log
runs at 13% to 69% of mantle-log's rate, lowest at 256 replicas. hyper-log runs one thread more
than mantle-log, as before.

## The replica's path

mantle measured its replica path on hyper-log at 828 µs a committed entry against 159 µs on its
`crates/log` (mantle `docs/measurements/2026-10-01-shared-log.md`, load 38–40): every read was a
round trip to the log's owner thread, and every write crossed four threads. `hyper-log-compare
replica` mirrors that run: a group of three members, each a hyper-raft core on its own log on a
simulated device, driven in one process as mantle's `crates/range/tests/group.rs` settles them
(`drive` with waiting, messages delivered in order), 50 entries of 32 bytes to warm, then 300
measured, each proposed by the leader and settled before the next; the store is mantle's
`LogStore` over each log. Per committed entry: the wall time, the storage's calls, the reads
another thread answered, every allocation and reallocation in the process, and its context
switches (`hyper_measure::faults::switches`, getrusage's voluntary and involuntary switches).

- **before**: hyper-log at main `50a711d`, the store as mantle has it at `shared-log` `03bda33`,
  keeping the group's bounds between its writes.
- **after**: this branch, the store on the group's handle (`GroupLog`).
- **mantle-log**: `crates/log` at mantle `a2021df`, the store as it was there.

Nine rounds, the three in a rotated order, each run a fresh process; medians, with the least and the
most. Load average 27 (17:18 PDT, other sessions building).

| | wall µs a committed entry | writes | storage calls: views, terms, fetches | reads another thread answered | allocations | reallocations | context switches |
|---|---|---|---|---|---|---|---|
| before | 697 (622–997) | 6 | 6, 9, 3 (the store's own cache answered the rest) | 18 | 72.2 | 0.17 | – |
| **after** | **166** (156–366) | 6 | 82, 12, 3 | **0** | 72.2 | 0.20 | 19.5 |
| mantle-log | 163 (153–229) | 6 | 82, 12, 3 (under a read lock) | 0 | 213.2 | 21.17 | 12.1 |

- Reads cost nothing: the handle answers all 97 of an entry's reads where the replica is.
- A write is two hand-offs, caller to the thread that flushes and back, as mantle's writer had
  it; it was seven (an admission and two device round trips).
- The 2% left to mantle-log, within the runs' spread, was one context switch a write (19.5 an
  entry against 12.1, six writes). Its cause was the hand-off: the thread that took a submission
  handed the owner to the other before flushing, so the other woke, took the owner and slept on
  the inbox while the frame was flushed, and both log threads slept for every write besides the
  caller, where mantle's writer had one. A protocol that woke the owner only when it waited for
  the device left the count at 19.4, since the wake was the hand-off's. The next section removes
  it.
- Allocations are a third of mantle-log's and reallocations a hundredth: hyper-log's appends
  allocate nothing (below), and the rest are the core's and the store's.

### The owner on its own thread, the caller flushing its own frame

The owner now stays on one thread and never does I/O (`crates/hyper-log/ORIGIN.md`, "The owner on
its own thread"): a frame's I/O goes to a caller of the frame that waits on its answer from the
moment it submitted, which does the write, flush, confirmation and answers on its own thread as
mantle's writer did on its, and any other job to the log's I/O thread; the job comes back through
returns the owner reads before every message, waking the owner only for a completion it must
answer, or when the owner has asked to be woken because work waits on the device. A blocking write
with no one else submitting now wakes two threads, its caller and the owner, as mantle-log's did.

The same run as above, three binaries rotated each round, nine rounds, each run a fresh process:
**before** is main `8e1fd9d` (leader/followers), **after** this branch, **mantle-log** as above.
Medians, with the least and the most.

| | wall µs a committed entry | writes, views, terms, fetches | reads another thread answered | allocations | reallocations | context switches |
|---|---|---|---|---|---|---|
| run 1, 18:05 PDT, load 34 | | | | | | |
| before | 79 (67–127) | 6, 82, 12, 3 | 0 | 72.2 | 0.20 | 19.7 |
| **after** | **69** (58–83) | 6, 82, 12, 3 | 0 | 72.2 | 0.20 | **12.0** |
| mantle-log | 81 (72–138) | 6, 82, 12, 3 | 0 | 213.2 | 21.17 | 12.0 |
| run 2, 18:10 PDT, load 14–16, the Linux equivalence running beside it | | | | | | |
| before | 228 (149–243) | 6, 82, 12, 3 | 0 | 72.2 | 0.20 | 19.5 |
| **after** | **197** (130–216) | 6, 82, 12, 3 | 0 | 72.2 | 0.20 | **12.6** |
| mantle-log | 224 (152–237) | 6, 82, 12, 3 | 0 | 213.2 | 21.17 | 12.6 |

An earlier two-way run (`replicas 9`, 17:56 PDT, load 44–48) gave after 156 µs (147–292) against
mantle-log's 169 (110–189), 12.0 switches each. The wall times move with the machine's load from
run to run; within a run hyper-log is at or below mantle-log each time, with mantle-log's switch
count, a third of its allocations and a hundredth of its reallocations, the allocations and
reallocations unchanged by this work.

mantle's throughput workload (`hyper-log-compare one hyper DIR 128 N 1.0`, before and after
alternated, 18:05–18:06 PDT, load 31–34) drives the log through `submit_waking`, whose callers do
not wait on their answers, so its frames are flushed by the I/O thread. Appends a second: at one
replica 113 before and 98 after over three runs, and over eight more of two seconds each 68 before
and 69 after (p50 14.38 and 14.35 ms: the device's flush); at 16 replicas 1412 and 1850; at 256,
23694 and 23847 (medians of three; the spreads overlap at every point). `cargo bench -p hyper-log
--bench allocs`, before and after alternated twice (load 15–19): allocations per append and per
fetch are the same at every row; a fetch from the file at 16 replicas carries 121 bytes more
(1090 against 969 at 128 B), the carrier box a log allocates at its first job, spread over the
bench's fetches.

## Many small appends

mantle measured its `mantle bench log --skip-device --sizes 128,16384 --replicas 1,256` on this
crate at main `8fe3f23` (mantle `docs/measurements/2026-10-01-group-log.md`, branch `shared-log`):
at 256 replicas appending 128 B, 2–4% fewer appends a second than its `crates/log`, more in 4 of 28
paired rounds, p99 lower in every batch. Its frames carried 249–254 appends against 127–253, about
105 frames a second against 150, and it took the writer's wait for returning submitters to
overshoot.

**The workload here.** `hyper-log-compare` now drives mantle-log at mantle `a2021df` in its own
process, exactly as it drives hyper-log (`src/mantle.rs`: the same configuration, file, driver
threads, wakers and updates), and counts every flush of each log's file and the time in it
(`Flushing`). `pairs` runs the logs, each point in a fresh process, in an order rotated every round,
printing each run with the load average read just before it. This machine as it was: the Apple M5
Max of the sections above, APFS on the internal SSD, `F_FULLFSYNC`, other sessions building.

**What the frames hide.** A frame's appends are answered once a later durable record confirms its
flush: the next frame's persist record, or a confirmation of its own (mantle
`docs/design/raft-log.md` §6). Counting the file's flushes, both logs flush about as often and carry
the same appends a flush (main `7d77169`, load 31–36: 209 and 218 flushes a second, 127.3 and 127.5
appends each). hyper-log writes one frame of nearly every replica and a confirmation for it;
mantle-log writes frames of two shifting cohorts, each confirming the one before. Either way a
replica's append takes two flushes and appears in at most every other frame, so 128 a flush is the
protocol's bound at 256 replicas. What differed was the time the file sat idle between flushes:
20% of the run for hyper-log, 16% for mantle-log.

**The cause.** The idle time is the writer's gathering of the replicas its last frame answered,
1.05–1.27 ms a frame at load 30–34 (temporary probes, not kept). The wait itself was not long: of
those 1.2 ms the owner was blocked waiting for a message for 55–65 µs. It was the owner's own work,
and a sample of the owner thread (`sample`, two seconds of the point) put 145 of its 191 busy
samples in one place: `Ticket::admit`, the reply that tells a caller its submission is admitted,
which wakes the caller (`semaphore_signal`). `Log::submit_waking` waited for that admission, a round
trip to the owner on every submission, so a driver thread holding 14 replicas could send the next
replica's append only once the owner had admitted the last. The replicas a frame answered came back
one owner wake apart, about 4 µs each, while the file sat idle; the wait for them, `p·S/(n + p)`
with `p` learned near one and `S` about 4 ms, kept going because each next one did come within it.
mantle-log took its room under a lock in the caller and sent without hearing back. The wait's rule
and inputs are unchanged.

**The change.** `Log::submit_waking` returns once the submission is on its way: the caller hears
everything through its waker, its admission included, as a group's handle already did
(`GroupLog::submit_waking`). A submission without room still waits in the log, not refused; one
the log refuses (`Busy` past the waiters it holds, `Fenced`, `Claimed`) is answered with the
refusal. `Log::submit` and `Log::submit_waiting`, whose callers block until admitted, are unchanged,
and so is the equivalence harness, whose rounds wait on the plug's flush or its answer
(`tests/equivalence.rs`). Two other changes the frame counts suggested were built and measured and
are not kept: letting the device wait for a frame to follow before confirming one on its own, and
weighing the unconfirmed frame's appends in the wait. At load 26–32 they ran 28,790 appends a
second against 29,985 for this change alone (six paired rounds).

**Measured.** `hyper-log-compare pairs DIR <rounds> 2.0 <sizes> <replicas> hyper
base=<main build>:hyper mantle-log`: **hyper** this change, **base** main `7d77169` with the same
harness, **mantle-log** mantle `a2021df`. Medians, the least and the most; the last column is the
rounds in which that run made more appends a second than hyper, paired by round.

Eight rounds of 2 s at 128 B and 256 replicas, 19:55–19:57 PDT, load 26.3–33.8:

| run | load | appends/s | p50 ms | p99 ms | appends a frame | file flushes/s | flushing | more than hyper |
|---|---|---|---|---|---|---|---|---|
| hyper | 26.3–33.8 | **29,772** (27,620–30,440) | 8.52 | 10.72 | 253.7 | 234 | 90% | – |
| base | 26.7–33.7 | 26,703 (26,255–27,833) | 9.50 | 12.89 | 250.0 | 210 | 82% | 0/8 |
| mantle-log | 26.3–33.8 | 27,726 (19,300–28,298) | 9.37 | 12.67 | 158.9 | 218 | 84% | 0/8 |

mantle's grid, six rounds of 2 s, 19:57–20:01 PDT:

| entry | replicas | run | load | appends/s | p50 ms | p99 ms | appends a frame | file flushes/s | flushing | more than hyper |
|---|---|---|---|---|---|---|---|---|---|---|
| 128 B | 1 | hyper | 33.0–34.4 | 122 (92–125) | 8.44 | 10.25 | 1.0 | 245 | 95% | – |
| 128 B | 1 | base | 32.3–34.4 | 121 (99–124) | 8.42 | 10.16 | 1.0 | 242 | 95% | 3/6 |
| 128 B | 1 | mantle-log | 32.3–34.1 | 121 (99–124) | 8.45 | 10.51 | 1.0 | 242 | 95% | 3/6 |
| 128 B | 256 | hyper | 29.5–34.4 | **29,891** (28,918–30,239) | 8.54 | 11.67 | 253.3 | 235 | 89% | – |
| 128 B | 256 | base | 29.3–35.9 | 27,060 (25,751–27,274) | 9.48 | 12.76 | 251.2 | 214 | 82% | 0/6 |
| 128 B | 256 | mantle-log | 30.8–35.9 | 27,912 (14,323–28,356) | 8.86 | 12.80 | 157.1 | 219 | 84% | 0/6 |
| 16 KiB | 1 | hyper | 29.1–29.9 | 121 (64–125) | 8.47 | 9.96 | 1.0 | 242 | 95% | – |
| 16 KiB | 1 | base | 28.9–30.3 | 123 (121–124) | 8.45 | 9.53 | 1.0 | 245 | 95% | 4/6 |
| 16 KiB | 1 | mantle-log | 28.9–30.3 | 121 (101–126) | 8.47 | 9.57 | 1.0 | 242 | 95% | 3/6 |
| 16 KiB | 256 | hyper | 28.4–31.4 | **23,079** (22,386–23,569) | 10.85 | 15.72 | 253.2 | 182 | 83% | – |
| 16 KiB | 256 | base | 29.2–31.7 | 21,470 (18,395–21,878) | 11.82 | 15.81 | 252.0 | 169 | 77% | 0/6 |
| 16 KiB | 256 | mantle-log | 27.4–31.0 | 20,555 (20,310–21,143) | 12.27 | 16.13 | 207.1 | 162 | 73% | 0/6 |

The same at light load, 19:50–19:55 PDT: at 128 B and 256 replicas, eight rounds at load 3.8–7.7,
hyper 26,713 (21,547–29,669), base 25,228, mantle-log 25,669, hyper ahead of each in 7 of 8; six
more at load 2.7–3.9, hyper 26,840 (26,622–27,119), base 24,875, mantle-log 25,416, hyper ahead in
every round. At 16 KiB and 256 replicas, load 2.7–23.8: hyper 20,367, base 20,114, mantle-log
20,482, hyper ahead of each in 5 of 6. At one replica every run is the device's, two flushes an
append and 95% of the time flushing, and the three are within a flush a second of each other.

- **At 256 replicas** hyper-log now runs 7% (128 B) and 12% (16 KiB) more appends a second than
  mantle-log in the loaded runs, in every paired round, with lower p50 and p99, and keeps 90% of
  the run flushing where mantle-log keeps 84% (main: 82%). It still writes one frame of nearly
  every replica and its confirmation; the file waits less between them.
- **The replica's path** (`hyper-log-compare replicas 9`) does not use `submit_waking` and is
  unchanged: this build 140 µs a committed entry (135–368) against mantle-log's 148 (145–275), main's
  153 (133–215) against 154 (151–453), load 26–27, 72.2 allocations, 0.2 reallocations and 12.1
  context switches an entry in each.
- **Allocations** (`hyper-log-compare one hyper DIR <bytes> <replicas> 1.0 count`, main and this
  change alternated twice, load 29–33): per append, the driver's two included, at or below main at
  every point: 2.42–2.47 against 2.46–2.50 at one replica, 2.20–2.21 against 2.22–2.36 at 16, 2.06–2.07
  against 2.07–2.09 at 256. `cargo bench -p hyper-log --bench allocs`, which submits with
  `Log::submit` and through handles, prints the same table on both.
- **Equivalence**: the 48 files are byte-identical to main's (`HYPER_LOG_EQUIVALENCE_OUT`, `diff -r`),
  `EXPECTED` unchanged. On Linux (aarch64, `rust:1.98.0` in Docker on this Mac), the equivalence
  test binary of this change passed pinned to one core (`taskset -c 0`) 200 times of 200 and
  unpinned 50 of 50.

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

On Linux (aarch64, `rust:1.98.0` in Docker on this Mac, the container's overlay file system), the
same test, with the suites of hyper-block, hyper-log (the equivalence included) and hyper-measure:

```
24 writers killed, 1784 appends acknowledged across 8 groups, 0 acknowledgements read after a kill
test result: ok. 1 passed; 0 failed
```

Windows is linted (`x86_64-pc-windows-msvc`, `aarch64-pc-windows-msvc`), not run here.

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

# The replica's path: hyper-log and mantle-log rotated, nine rounds, each run a fresh process; one
# run of either log, and one point of mantle's workload on hyper-log.
target/release/hyper-log-compare replicas 9
target/release/hyper-log-compare replica <hyper|mantle>
target/release/hyper-log-compare one hyper DIR 128 <replicas> 1.0

# mantle's bench grid on hyper-log, another build of it, and mantle-log a2021df in this process:
# rounds rotated, each run a fresh process, the load average beside each, file flushes counted.
target/release/hyper-log-compare pairs DIR 8 2.0 128,16384 1,256 hyper base=path/to/other/hyper-log-compare:hyper mantle-log
target/release/hyper-log-compare one mantle-log DIR 128 256 2.0

# The equivalence on Linux, pinned to one core 200 times and unpinned 50 times, in rust:1.98.0
# with the repository at /src: build the test binary once, then from crates/hyper-log run
# `taskset -c 0 <binary>` 200 times and `<binary>` 50 times.
cargo test -p hyper-log --test equivalence --all-features --locked --no-run
```

# hyper-datagram against slates' seal

The law (`CLAUDE.md` §1a) for the sealed datagram plane: its allocations, reallocations and page
faults per datagram, and its cost against the plane it replaces, slates' control-datagram seal
(`slates-transport` `src/seal.rs` at `5cce86a`, unchanged since in slates `main` `bfa7298`).

## The machine

The same Apple M5 Max (`Mac17,6`, 18 cores, 128 GiB), macOS 26.4.1, rustc 1.98.0, 2026-10-01 at
10:26 PDT. The machine was shared with two other sessions building and testing: the load average
was 40 to 46 during the runs. The allocation counts are exact whatever the load; the times are
medians of seven fresh processes, with the least and the most beside them.

## Allocations

`cargo bench -p hyper-datagram --bench allocs`: two planes, one round queuing as many messages as
a 1,232-byte path packs for each peer, flushing and opening every datagram, 2,000 rounds after 100,
counted on the calling thread (the plane has no threads).

| Peers | Message | Messages a datagram | Allocations | Reallocations | Bytes asked | Minor faults |
|---|---|---|---|---|---|---|
| 1 and 16 | 16 B | 66 | 0 | 0 | 0 | 0 |
| 1 and 16 | 128 B | 9 | 0 | 0 | 0 | 0 |
| 1 and 16 | 1,024 B | 1 | 0 | 0 | 0 | 0 |
| 1 | a datagram 40 counters late, widening the window | 1 | 0 | 0 (was 1, 24 B) | 0 | 0 |

The one cost the first count found was the replay window reallocating its bitmap when a verified
late datagram widened it, on the receive path. The bitmap is now reserved to the window's limit
when the epoch is installed (`window_limit / 8` bytes, 128 B at a limit of 1,024), so widening
never reallocates.

## Against slates

`crates/hyper-datagram-compare`, a workspace of its own. One round is a consensus round's control
traffic to one peer: as many messages of one size as a 1,232-byte path carries, sealed at one node
and opened at the other in one thread, with no socket between them, so the cost is the plane's.
hyper-datagram packs the round into one datagram; slates' seal carries one message a datagram under
its own envelope (`ControlDatagram::encode_sealed`, `decode_sealed`). The messages are built before
the clock starts.

slates seals with RustCrypto's `aes-gcm` 0.10.3 (`aes` 0.8.4, `polyval` 0.6.2). On aarch64 both
use their portable software code unless built with `--cfg aes_armv8 --cfg polyval_armv8`
(`aes` `src/lib.rs` lines 30 to 36, `polyval` `src/lib.rs` line 37), and slates sets neither
(`.cargo/config.toml` has only musl flags). The first table is slates as it ships on aarch64; the
second builds it with the ARMv8 flags, so the difference left is the plane's design.

slates as it builds (7 runs of 20,000 rounds each):

| Message | Plane | Messages a round | Datagrams a round | ns a message | Allocations a message | Wire bytes a message |
|---|---|---|---|---|---|---|
| 16 B | hyper | 66 | 1 | 22.0 (21.0–24.7) | 0 | 18.6 |
| 16 B | slates | 66 | 66 | 3,313.5 (2,643.0–5,852.6) | 5 | 84.0 |
| 128 B | hyper | 9 | 1 | 86.0 (82.3–90.7) | 0 | 134.6 |
| 128 B | slates | 9 | 9 | 6,932.7 (3,492.4–8,890.3) | 5 | 196.0 |
| 1,024 B | hyper | 1 | 1 | 577.3 (552.3–656.2) | 0 | 1,067.0 |
| 1,024 B | slates | 1 | 1 | 13,638.9 (13,372.7–14,499.6) | 5 | 1,092.0 |

slates with its ARMv8 AES and PMULL code (7 runs of 20,000 rounds each):

| Message | Plane | ns a message | Allocations a message | Wire bytes a message |
|---|---|---|---|---|
| 16 B | hyper | 21.6 (21.3–22.4) | 0 | 18.6 |
| 16 B | slates | 223.0 (210.1–247.8) | 5 | 84.0 |
| 128 B | hyper | 88.3 (83.8–91.4) | 0 | 134.6 |
| 128 B | slates | 322.1 (306.7–345.4) | 5 | 196.0 |
| 1,024 B | hyper | 586.0 (561.6–636.5) | 0 | 1,067.0 |
| 1,024 B | slates | 1,220.5 (1,151.1–1,380.4) | 5 | 1,092.0 |

With the same hardware crypto, hyper-datagram costs a tenth of slates' per 16-byte message, a
quarter per 128-byte message and half per 1,024-byte message. Where it comes from:
- **Packing.** One seal, one tag, one prologue and one checksum serve the whole round: at 16 B,
  66 messages share 41 bytes of overhead, where slates spends 68 a message on its prologue, counter,
  envelope and tag.
- **No allocation.** slates allocates five times a message: the plaintext, the ciphertext, the
  datagram, and on opening the plaintext and the decoded body. hyper-datagram seals into one
  reused buffer and opens in place, handing out the messages as slices of the datagram.
- **One message a datagram**, at 1,024 B, still leaves hyper-datagram at half slates' cost:
  AWS-LC's AES-GCM in place, against `aes-gcm`'s encrypt and decrypt into new vectors.

The comparison's other differences are recorded, not measured away: slates' opener keeps a
high-water mark and refuses any datagram that arrives out of order, where hyper-datagram's RFC 4303
window accepts a verified late datagram once; and slates' keys are not yet tied to a session
(mantle audit §11.8), where hyper-datagram's come from the QUIC connection's exporter.

## Commands for the plane

```sh
# The suites and the two-process test.
cargo test -p hyper-datagram

# Allocations per datagram.
cargo bench -p hyper-datagram --bench allocs

# The comparison, slates as it builds, then slates with its ARMv8 crypto.
cd crates/hyper-datagram-compare && cargo build --release
target/release/hyper-datagram-compare table 7 20000
RUSTFLAGS="--cfg aes_armv8 --cfg polyval_armv8" cargo build --release --target-dir target/armv8
target/armv8/release/hyper-datagram-compare table 7 20000
```

# hyper-swim against slates' detector

The law (`CLAUDE.md` §1a) for the SWIM detector, ported from slates' (`crates/hyper-swim/ORIGIN.md`):
its allocations, reallocations and page faults a member a period, before and after, and its cost
against slates' own detector and wire at `5cce86a` (unchanged since in slates `main` `bfa7298`).

## The machine

The same Apple M5 Max, macOS 26.4.1, rustc 1.98.0, 2026-10-01 at 10:39 to 10:41 PDT, shared with
two other sessions: the load average was 35 to 37. The allocation counts are exact; the times are
medians of seven fresh processes, with the least and the most beside them.

## The workload

`N` detectors in one process run whole periods as a member's driver does (`tests/cluster.rs`):
each ticks, sends its probe target a ping carrying up to eight gossip entries, the target applies
them and answers with an acknowledgement carrying its own gossip and coordinate, and the prober
applies that, credits the probe and folds the round trip into its Vivaldi coordinate. Every
message goes through the wire codec. The transmit budget is λ·ln(n+1) for λ = 3 (SWIM §4.4); the
count starts after twice the periods the joins' gossip takes to drain. Quiet has no membership
changes; churning has one member refute a suspicion every period, so its new incarnation spreads.

## Allocations

`cargo bench -p hyper-swim --bench allocs`, 400 periods, the calling thread's count (the detector
has no threads), per member per period:

| Members | Workload | Allocations before | Reallocations before | Allocations after | Reallocations after |
|---|---|---|---|---|---|
| 4 | quiet | 6.67 | 8.00 | 0 | 0 |
| 4 | churning | 12.56 | 8.76 | 0.00 (0.8 B a period) | 0 |
| 16 | quiet | 6.13 | 8.26 | 0 | 0 |
| 16 | churning | 12.14 | 10.46 | 0 | 0 |
| 64 | quiet | 6.03 | 8.12 | 0 | 0 |
| 64 | churning | 13.37 | 11.91 | 0 | 0 |
| 256 | quiet | 7.52 | 8.95 | 0 | 0 |
| 256 | churning | 14.74 | 12.91 | 0.00 (4.5 B a period) | 0 |

The "before" counts were taken on the detector as ported, with a warm-up of 200 periods, which at
256 members had not yet drained the joins; the "after" counts use the derived warm-up. Minor page
faults were below 0.003 a member a period throughout. The two churning rows that are not exactly
zero are a queue growing once, within the first counted periods, to the most reports a member has
held. What each change removed is in `crates/hyper-swim/ORIGIN.md`, change 4.

## Against slates

`crates/hyper-swim-compare`, a workspace of its own, runs the workload above on both detectors,
each through its own API (7 runs of 400 periods each):

| Members | Workload | Detector | ns a member a period | Allocations a member a period |
|---|---|---|---|---|
| 16 | quiet | hyper | 121 (109–145) | 0 |
| 16 | quiet | slates | 598 (567–693) | 6.13 |
| 16 | churning | hyper | 547 (492–618) | 0 |
| 16 | churning | slates | 1,556 (1,411–1,670) | 12.17 |
| 64 | quiet | hyper | 179 (161–188) | 0 |
| 64 | quiet | slates | 920 (787–972) | 6.04 |
| 64 | churning | hyper | 821 (718–1,521) | 0 |
| 64 | churning | slates | 3,010 (2,564–6,898) | 13.45 |
| 256 | quiet | hyper | 448 (330–1,596) | 0 |
| 256 | quiet | slates | 2,662 (1,606–5,681) | 6.01 |
| 256 | churning | hyper | 1,639 (1,510–2,009) | 0 |
| 256 | churning | slates | 5,078 (4,592–6,192) | 14.48 |

hyper-swim's period costs 2.8 to 5.9 times less. Besides the allocations, the quiet period no
longer scans the membership: slates' tick collected every suspect by walking all members, each
period, which is what made its quiet cost grow with the membership; hyper-swim's membership
indexes its suspects. The 256-member quiet period was 768 ns before that index and 374 to 448 ns
after.

## Commands for the detector

```sh
# The suites and the four-process kill test.
cargo test -p hyper-swim

# Allocations a member a period.
cargo bench -p hyper-swim --bench allocs

# The comparison.
cd crates/hyper-swim-compare && cargo build --release
target/release/hyper-swim-compare table 7 400
```

# hyper-timing against focal-timing and slates' timing

The law (`CLAUDE.md` §1a) for the timing crate (`crates/hyper-timing/ORIGIN.md`): the operations a
member runs each period of a group, measured against hyper-timing's own law before L-1's election
law (`35d35d8`: `ELECTION_MARGIN`, `PATH_WINDOW`, `GRANULARITY_NS`), focal-timing at `a8e95f7`
(the source of the paths, the tick pace and the rounds) and slates' `crates/cluster/src/timing.rs`
at `5cce86a` (the source of the election timing, the priority and the timer).

## The machine and the workload

The Apple M5 Max, macOS 26.4.1, rustc 1.98.0, 2026-10-01 at 21:30 PDT, shared with other sessions
(load average 12.6 to 20.6 over the run, beside each row below). `crates/hyper-timing-compare`, a
workspace of its own, runs each operation two million times in a fresh process of its own and
counts allocations over a further two million. The law before L-1 is the same harness built at
`35d35d8` in a detached worktree, where it is named `hyper`; a driver ran every implementation of
an operation in turn, seven runs, the order rotated each run (the commands are below). A
five-voter group, so four paths from each member, fed the same eight WAN round trips of 40 to
58 ms; a 10 ms heartbeat. The new law's paths take the window derived from macOS's measured
correlation time, 50 ms, at the 10 ms probe (eleven; the law before kept sixteen), its tails
macOS's measured granularity, 45 µs (before: 1 ms), and its detector is configured once for a link
of the paths' one-way latency, a 2 ms deviation and a loss of 10⁻⁵.

## Results

Medians (least–most), nanoseconds a call. No implementation allocates in any operation but slates'
priority, once a call.

| Operation | hyper | before L-1's law | focal | slates | load |
|---|---|---|---|---|---|
| a sample folded into a path | 28.0 (27.5–29.5) | 32.7 (32.0–33.2) | 2.3 (2.2–2.5) | 3.4 (3.3–3.6) | 13 |
| a path's tail read | 1.1 (1.0–1.2) | 0.7 (0.6–1.0) | 74.6 (73.1–75.6) | 0.7 (0.6–0.8) | 13 |
| the election timing from four paths | 341.3 (334.8–344.9) | 8.4 (8.2–8.7) | — | 8.7 (8.4–9.2) | 13 |
| — of it, the ballot | 28.2 (27.6–28.3) | | | | 13 |
| — of it, the span's search | 305.8 (301.0–308.4) | | | | 13 |
| — of it, the timing from a held ballot and span | 3.8 (3.7–4.0) | | | | 13 |
| the quorum priority over four paths | 21.3 (20.3–22.2) | 12.3 (11.2–35.8) | — | 22.8 (22.1–39.7), 1 allocation | 13 |
| the tick pace | 2.0 (1.6–2.6) | 5.0 (4.1–6.6) | 315.7 (282.4–718.7) | — | 13–21 |
| a round's budget | 0.8 (0.7–1.1) | 1.0 (0.9–1.1) | 1.0 (0.9–1.2) | 1.0 (0.9–1.0) | 21 |
| a follower's period of the election timer | 1.0 (1.0–1.7) | 1.0 (1.0–1.7) | — | 1.0 (1.0–1.7) | 21 |

What the two laws derive on this workload (`hyper-timing`'s own derivations, printed once):

| | before L-1's law | L-1's law |
|---|---|---|
| base | `10 × tail` = 58 periods, 580 ms (tail 57.1 ms) | the detector's `η + α`: at the 10 ms heartbeat, `η` 10 ms and `α` 9.96 ms, 2 periods, 20 ms |
| span | `10 × spread` = 14 periods, 140 ms | `W` = 151 ms, 16 periods, `T_E` 100 ms, a split in 11 % of attempts |
| detection of a crash | not stated | `E(D) + α + η` = 42.8 ms |
| the detector's best interval | — | `η` 50 ms, `α` 123 ms (the correlation time binds) |

The law before L-1 waited ten tails to decide a leader was gone whatever the link did; the detector
decides at its freshness point, from the link's own variance and loss, and the span is the one
that minimizes the time to a leader on these paths rather than ten spreads.

## Where hyper-timing does not win, and why

- **The election timing from the paths** is a search where the law before was a multiply: 341 ns
  against 8.4, of which the span's golden-section search over Ongaro's split-vote expectation is
  306. The span is the election cost the detector is configured for (`Costs::election`), so it is
  computed when the detector is (`LinkEstimator::reconfigure_due`, every window's worth of
  heartbeats), beside a configuration that itself costs 715–2,418 ns ("The detector's estimator").
  What a member runs every period is the timing from a held ballot and span: 3.8 ns against the
  8.4 and 8.7 of the laws it replaces.
- **A path's tail read** is 1.1 ns against 0.7. Both compile to the same inlined arithmetic; the
  difference survived a `u64` sum in place of the `u128`, the granularity as a literal, and every
  loop aligned to 64 bytes in both builds. It is under two cycles and not explained.
- **The quorum priority** is 21.3 ns against 12.3, a difference in the data, not the code: with
  windows of sixteen the four paths held the same samples and the counted rank found its answer at
  the second path; with the derived window of eleven they differ and the count runs further. Fed
  the eleven samples the derived window holds and the same 1 ms floor, the law before costs
  20.0–21.0 ns and this one 20.7–21.2 (six runs each, the order alternated, load 20–28). Two changes
  bought that: the count stops once a rank passes the position, and paths are ranked on the
  spread without the granularity floor, which is applied once to the path chosen (the floored
  spread never falls as the unfloored one rises, so both choose paths of the same round trip and
  floored spread; the property test against a sort holds over 2,000 groups).
- **A sample** costs 28 ns against focal's 2.3 and slates' 3.4, the cost of the median window: focal
  stores the sample and leaves the work to every read, slates' path is RFC 9002's smoothed
  estimator. The median stays where one stall must not reorder voters (`docs/timing.md` §2.6,
  item 7), and the derived window of eleven makes it cheaper than the sixteen before (32.7 ns).

Over a period of the four paths, hyper spends about 140 ns (four samples, the priority, the timing,
the pace and a round), the law before about 160, focal about 330 (four samples, the pace and a
round) and slates about 45 for an estimate that one late answer moves.

Before change 7 of `crates/hyper-timing/ORIGIN.md` hyper-timing's tail read was focal's (71.7 ns),
the election timing 940 ns and the priority 1,384 ns. The first measurement also timed a sample
that the optimizer had removed, since nothing read the path afterwards; the harness observes the
path after every sample.

## Commands for the timing

```sh
cargo test -p hyper-timing
cargo test -p hyper-timing --test alloc                 # the allocation law, the election law included
cargo bench -p hyper-timing --bench estimator           # the detector's estimator, per heartbeat
cargo test -p hyper-timing --test replay -- --nocapture # the replay against Theorem 7
cd crates/hyper-timing-compare && cargo build --release
target/release/hyper-timing-compare table 7 2000000
# The law before L-1: the same harness at 35d35d8, built in a detached worktree.
git worktree add --detach <before> 35d35d8
(cd <before>/crates/hyper-timing-compare && cargo build --release)
# Then, for each operation, seven runs of every implementation in a fresh process, the order
# rotated each run, the load average read before each process:
#   <after>/target/release/hyper-timing-compare one <operation> <hyper|focal|slates> 2000000
#   <before>/target/release/hyper-timing-compare one <operation> hyper 2000000
```

# The detector's estimator

`hyper_timing::LinkEstimator` (`crates/hyper-timing/src/link.rs`, `docs/timing.md` §2.6), the
receiving side of a node pair's heartbeats: NFD-E's expected arrival over the window
`min(n_G, n_A)` under the drift bound, the history's prediction-error variance, the Jeffreys loss,
freshness and suspicion. Nothing in focal, slates or mantle estimates NFD-E, so there is no
implementation to replace and compare with; the nearest per-heartbeat work each project already
does, a path estimator's sample, is timed in the same run for reference.

## Per heartbeat, under the allocation law

`cargo bench -p hyper-timing --bench estimator`, the Apple M5 Max, macOS 26.4.1, rustc 1.98.0,
2026-10-01 at 20:50 PDT, shared with other sessions (load average 27.8 to 29.6); the reference
rows again at 21:32 PDT (load 22.8 to 28.3) after L-1's election law. A link at a 50 ms
interval, fed a millisecond of jitter and one heartbeat in a thousand stalled 40 ms, warmed past its
window and configured; each heartbeat is the driver's work (poll the deadline if it passed, feed the
heartbeat, check whether a configuration is due). Two million heartbeats a run, seven runs, the two
rings rotated; configurations timed apart. Medians (least–most); allocations, reallocations and
minor faults per heartbeat, configurations included.

| ring | ns a heartbeat | allocations | reallocations | minor faults | ns a configuration |
|---|---|---|---|---|---|
| 1 ms granularity (window bound 1,332) | 77.2 (75.7–81.7) | 0 | 0 | 0 | 2,407 (2,347–2,549) |
| granularity at the interval (window bound 66,665) | 129.2 (122.0–135.8) | 0 | 0 | 0 | 711 (677–753) |
| `PathRtt::on_sample`, for reference (window of sixteen; at 21:32, the derived three) | 65.5 (65.3–84.2); 18.3 (17.9–19.2) | | | | |
| `ExchangeRtt::on_sample`, for reference | 6.1 (5.7–7.5); 5.7 (5.7–7.3) | | | | |

At 21:32 the two rings measured 80.1 (78.4–82.7) and 125.5 (124.2–144.0) ns a heartbeat, 2,418 and
715 ns a configuration, with no allocation, reallocation or fault: the estimator did not change.

A heartbeat is a step on each Allan level it finishes (one on average, seventeen at most), a ring
write, and the window read from the levels; the larger bound costs more because more levels
qualify for `n_A`. `crates/hyper-timing/tests/alloc.rs` asserts the zero: 100,000 heartbeats with
their polls, estimate reads and every configuration due, and the three folds, with no allocation or
reallocation.

## The replay against Theorem 7

`cargo test -p hyper-timing --test replay -- --nocapture`. No raw trace is kept, so each recorded
shape is a model: a body, hiccups, and stalls that block the sender and delay every heartbeat
inside them to their end, fitted to the run's table and checked on a run as long at the recorded
interval. The estimator runs as the detector at the model's correlation time, configured from its
own estimates whenever they renew, over four simulated hours; every suspicion is a mistake, and
Theorem 7's allowance is `β` summed over the freshness points at the configuration then in force.

| shape (checked at the recorded interval) | η | MTBF | mistakes / points | Theorem 7 allows | holds |
|---|---|---|---|---|---|
| macOS 100 µs: sd/(1.4826·MAD) 21.9 (run 24), mean/median 1.94 (1.85) | 50 ms | 1 h | 52 / 287,944 | 138.0 | yes |
| | 50 ms | 30 d | 0 / 287,944 | 0.1 | yes |
| macOS `F_FULLFSYNC` 10 ms: 10.8 (13), 1.20 (1.19) | 500 ms | 1 h | 23 / 28,744 | 75.3 | yes |
| | 500 ms | 30 d | 0 / 28,744 | 0.1 | yes |

The flush model's stalls block the sender for up to 250 ms, so its correlation time is 500 ms, not
the recorded run's 200 ms (whose stalls were a flushing sender's backlog, draining while it kept
sending). Run at 200 ms, below that, the detector broke the bound, 19 mistakes against 11.5 allowed
at an hour's MTBF, every one a stall that held two heartbeats of a two-heartbeat margin late: the
independence rule of §2.6 at work. A stopped sender was suspected within the detection bound the
configurator gave.

## A trace with the estimator in place

`hyper-timing-trace run <dir> 100 120` and `analyse <dir>`: macOS at 100 µs for 120 s, 2026-10-01
at 20:45 PDT, load 37.1 → 32.4. The analyser now takes NFD-E's window (`n_G`, `n_A` and the drift
bound), the loss and Theorem 7's `β` from hyper-timing, and replays the estimator itself as the
detector (the last table). The raw trace was deleted once summarized.

| series | n | mean ± 95 % | sd (95 %) | median (95 %) | MAD (95 %) | p99 | p99.9 | p99.99 | max | τ_int |
|---|---|---|---|---|---|---|---|---|---|---|
| D (kernel stamp − σ) | 1200000 | 114.3 ± 34.9 | 811.6 (0.0–1290.3) | 63.5 (61.1–66.1) | 28.1 (26.8–29.5) | 310.6 | 11486.3 | 35489.9 | 44373.6 | 577.5 |
| D (process read − σ) | 1200000 | 192.9 ± 69.7 | 1450.9 (0.0–2177.7) | 86.5 (82.9–90.3) | 36.0 (34.3–37.8) | 1105.1 | 25425.8 | 52175.3 | 61971.2 | 721.8 |

The window at 50 ms is the estimator's: `n_A` 64 under `n_G` 73, which is the drift bound for the
receiver's `G` of 56 µs measured at 100 µs waits. The correlation time is 50 ms, as on the
five-minute macOS runs.

| estimate | MTBF | η | α | window | detection bound | T_MR bound | U | replayed mistakes / points | replayed T_MR | bound holds | suspected share |
|---|---|---|---|---|---|---|---|---|---|---|---|
| mean/sd | 3600.000 s | 50000.0 µs | 52359.8 µs | 64 (n_G 73, n_Allan 64, drift 73) | 102474.1 µs | 1964.480 s | 2.901e-5 | 0 / 1199500 | > 60000.000 s | yes | 0.00e0 |
| mean/sd | 1.0 d | 50000.0 µs | 57026.3 µs | 64 (n_G 73, n_Allan 64, drift 73) | 107140.6 µs | 18713.330 s | 1.285e-6 | 0 / 1199500 | > 60000.000 s | yes | 0.00e0 |
| mean/sd | 30.0 d | 50000.0 µs | 70081.0 µs | 64 (n_G 73, n_Allan 64, drift 73) | 120195.3 µs | 2.6 d | 4.965e-8 | 0 / 1199500 | > 60000.000 s | yes | 0.00e0 |
| mean/sd | 365.0 d | 50000.0 µs | 101895.4 µs | 64 (n_G 73, n_Allan 64, drift 73) | 152009.7 µs | 238.8 d | 4.875e-9 | 0 / 1199500 | > 60000.000 s | yes | 0.00e0 |
| median/1.4826·MAD | 3600.000 s | 9007.5 µs | 8951.6 µs | 256 (n_G 413, n_Allan 256, drift 413) | 18022.6 µs | 407.307 s | 6.884e-6 | 690 / 1199910 | 15.652 s | **no** | 8.79e-4 |
| median/1.4826·MAD | 1.0 d | 20028.6 µs | 19972.6 µs | 128 (n_G 185, n_Allan 128, drift 185) | 40064.7 µs | 4194.132 s | 6.355e-7 | 599 / 1199800 | 40.067 s | **no** | 2.37e-4 |
| median/1.4826·MAD | 30.0 d | 50000.0 µs | 50515.2 µs | 64 (n_G 73, n_Allan 64, drift 73) | 100578.7 µs | 81.0 d | 3.917e-8 | 0 / 1199500 | > 60000.000 s | yes | 0.00e0 |
| median/1.4826·MAD | 365.0 d | 50000.0 µs | 51178.1 µs | 64 (n_G 73, n_Allan 64, drift 73) | 101241.6 µs | 427.9 d | 3.251e-9 | 0 / 1199500 | > 60000.000 s | yes | 0.00e0 |

| online, MTBF | η | configurations | mistakes / points | Theorem 7 allows (Σβ) | bound holds |
|---|---|---|---|---|---|
| 3600.000 s | 50000.0 µs | 79674 | 0 / 1154359 | 73.45 | yes |
| 1.0 d | 50000.0 µs | 79674 | 0 / 1154359 | 11.94 | yes |
| 30.0 d | 50000.0 µs | 79674 | 0 / 1154359 | 0.15 | yes |
| 365.0 d | 50000.0 µs | 79674 | 0 / 1154359 | 0.02 | yes |

# Heartbeat traces: hyper-timing's measured inputs

The traces that settle `docs/timing.md` §3 items 2, 6 and 7 and part of 3; the derivations are in
`docs/timing.md` §2.6. `crates/hyper-timing-trace`, a workspace of its own, records two processes
exchanging sequence-numbered heartbeats over UDP on loopback. The sender sleeps to each scheduled
time `σ_i = start + iη` (absolute, so lateness never accumulates into the schedule), optionally
writes one block of a log file and fully flushes it (`F_FULLFSYNC` on macOS, `fdatasync` on Linux),
and sends `(i, σ_i, began waiting, woke, sent, realtime at send)`. The receiver, with the kernel's
receive timestamps on (`SO_TIMESTAMP_MONOTONIC`, `SO_TIMESTAMPNS`), waits in `select(2)` until the
next scheduled heartbeat, as a detector waits for its next freshness point, records each wait
that ended on its timeout, and records each heartbeat with the kernel's stamp and the time it read
it. Both processes read the same monotonic clock, so every difference is exact. Records go to a
fixed buffer that a writer thread drains a 4.5 MiB chunk at a time; memory is three chunks
whatever the run's length, and the load average is logged with each chunk. Raw traces (up to
430 MB a run) stayed in the session's scratch directory and were deleted once summarized.

## The machines and the runs

- **macOS**: the Apple M5 Max (18 cores, 128 GiB), macOS 26.4.1, rustc 1.98.0, 2026-10-01 between
  17:40 and 18:35 PDT, shared with other sessions building and testing (load averages per run
  below). The loaded runs add `cargo build --workspace --all-targets` of this repository with
  four jobs, rebuilt from clean in a loop for the length of the run. The log file is on the
  internal SSD (APFS).
- **Linux**: Docker Desktop's VM on the same Mac, linuxkit 6.12.76 aarch64, 18 vCPUs, image
  `rust:1.98.0`, 2026-10-01 between 18:36 and 18:52 PDT. The kernel has `CONFIG_HZ=1000` and no
  high-resolution timers (`/proc/config.gz`; `/proc/timer_list` resolution 1 ms). The log file is
  on a Docker volume: ext4 in the VM's disk image, which is a file on the Mac's APFS, so
  `fdatasync` there is the hypervisor's, not a disk's.
- **The interval** of each run is the finest on the 1-2-5 grid whose mean sender service (timer
  lateness, plus the write and flush) the sweep below measured under it (Lindley's stability
  condition, `docs/timing.md` §2.6); the 50 µs macOS run was recorded first, at load 41, and
  failed that condition during the run.
- **Uncertainty**: a mean's 95 % interval is `1.96 · sd · √(τ_int / n)`, `τ_int` summed over
  Madras and Sokal's window; a quantile's is the order statistics `nq ± 1.96√(nq(1−q)τ_int)`; the
  deviation's from the fourth moment. A replayed mistake count's is the Poisson score interval.
- **Reading the replay columns**: a detector at `k` times the trace's interval is replayed over
  every phase of the trace (the heartbeats `r` modulo `k`, for each `r`), and the points, the
  mistakes and the replayed time are summed over the phases; "replayed T_MR" is that time over
  the mistakes. "Bound holds" compares the 95 % lower limit of the replayed mistake rate per
  freshness point with Theorem 7's `β`.

## Timer sweeps

Lateness of a wait past what was asked, 2,000 waits a row, and one 4 KiB `pwrite` and full flush
of the log file.

timer sweep, macos aarch64, load 22.44 26.13 29.54:

| wait | asked µs | n | late p50 µs | p90 | p99 | p99.9 | max | mean |
|---|---|---|---|---|---|---|---|---|
| sleep | 1 | 2000 | 9.3 | 14.9 | 43.9 | 77.4 | 79.0 | 10.1 |
| sleep | 2 | 2000 | 9.1 | 13.5 | 27.8 | 55.5 | 60.7 | 9.8 |
| sleep | 5 | 2000 | 10.7 | 15.0 | 34.8 | 69.8 | 76.6 | 11.6 |
| sleep | 10 | 2000 | 13.8 | 35.2 | 89.4 | 124.3 | 185.5 | 19.3 |
| sleep | 20 | 2000 | 25.4 | 86.8 | 131.5 | 166.3 | 213.2 | 40.9 |
| sleep | 50 | 2000 | 34.3 | 40.4 | 65.1 | 99.5 | 100.6 | 35.8 |
| sleep | 100 | 2000 | 60.5 | 122.8 | 175.5 | 264.5 | 373.9 | 73.4 |
| sleep | 200 | 2000 | 113.8 | 165.9 | 203.5 | 248.2 | 296.5 | 126.4 |
| sleep | 500 | 2000 | 261.7 | 300.6 | 344.4 | 372.8 | 553.0 | 262.7 |
| sleep | 1000 | 2000 | 512.7 | 551.9 | 581.5 | 613.9 | 620.8 | 495.4 |
| sleep | 2000 | 2000 | 1012.7 | 1049.4 | 1078.2 | 1155.1 | 1700.2 | 917.9 |
| sleep | 5000 | 2000 | 2511.6 | 2539.2 | 2575.0 | 2594.6 | 2597.2 | 1987.8 |
| sleep | 10000 | 2000 | 3353.3 | 5020.8 | 5072.5 | 5081.9 | 5094.8 | 3160.3 |
| select | 1 | 2000 | 12.8 | 34.5 | 68.8 | 81.4 | 101.4 | 17.3 |
| select | 2 | 2000 | 11.4 | 40.6 | 72.8 | 79.2 | 89.9 | 18.0 |
| select | 5 | 2000 | 13.2 | 55.4 | 99.4 | 170.3 | 193.5 | 23.5 |
| select | 10 | 2000 | 18.6 | 55.5 | 75.7 | 81.2 | 99.4 | 26.6 |
| select | 20 | 2000 | 27.2 | 67.8 | 83.6 | 98.7 | 112.0 | 34.8 |
| select | 50 | 2000 | 46.4 | 85.8 | 103.2 | 118.0 | 132.4 | 52.8 |
| select | 100 | 2000 | 64.1 | 101.7 | 127.8 | 148.5 | 159.4 | 72.4 |
| select | 200 | 2000 | 113.3 | 145.9 | 172.0 | 192.3 | 197.3 | 119.9 |
| select | 500 | 2000 | 262.6 | 301.4 | 324.4 | 354.1 | 361.4 | 267.2 |
| select | 1000 | 2000 | 509.5 | 550.2 | 586.0 | 692.7 | 1604.8 | 492.5 |
| select | 2000 | 2000 | 1008.7 | 1044.6 | 1073.5 | 1091.0 | 1101.5 | 910.4 |
| select | 5000 | 2000 | 2507.1 | 2537.5 | 2570.6 | 2599.8 | 2712.0 | 1940.4 |
| select | 10000 | 2000 | 3892.5 | 5022.0 | 5073.1 | 5104.5 | 5112.1 | 3378.1 |
| write+flush 4096 B | — | 2000 | 4493.4 | 8642.2 | 11655.8 | 56930.1 | 60231.9 | 5850.0 |

timer sweep, linux aarch64, load 4.01 4.45 4.65 (the container's `/proc/loadavg` at the end: 6.50 5.20 4.90):

| wait | asked µs | n | late p50 µs | p90 | p99 | p99.9 | max | mean |
|---|---|---|---|---|---|---|---|---|
| sleep | 1 | 2000 | 997.1 | 1114.9 | 1430.2 | 1490.1 | 1508.0 | 998.7 |
| sleep | 2 | 2000 | 997.2 | 1090.1 | 1422.5 | 1498.1 | 1571.2 | 998.1 |
| sleep | 5 | 2000 | 993.7 | 1086.1 | 1430.2 | 1474.0 | 1498.0 | 994.6 |
| sleep | 10 | 2000 | 989.2 | 1096.2 | 1435.0 | 1485.5 | 1520.4 | 989.5 |
| sleep | 20 | 2000 | 978.1 | 1091.6 | 1424.0 | 1500.1 | 3147.7 | 981.0 |
| sleep | 50 | 2000 | 947.6 | 1084.0 | 1377.1 | 1423.5 | 1454.7 | 949.4 |
| sleep | 100 | 2000 | 898.4 | 1003.9 | 1319.2 | 1384.7 | 1385.6 | 899.6 |
| sleep | 200 | 2000 | 798.7 | 887.6 | 1215.2 | 1278.1 | 1293.9 | 799.6 |
| sleep | 500 | 2000 | 502.8 | 933.7 | 1808.6 | 1855.1 | 1868.0 | 593.0 |
| sleep | 1000 | 2000 | 993.7 | 1510.3 | 2035.8 | 2096.5 | 2432.5 | 1003.0 |
| sleep | 2000 | 2000 | 1095.2 | 1932.5 | 2408.3 | 2572.1 | 2579.1 | 1150.6 |
| sleep | 5000 | 2000 | 1112.7 | 2511.2 | 3707.1 | 4016.9 | 4072.0 | 1332.9 |
| sleep | 10000 | 2000 | 1109.8 | 2632.5 | 5176.7 | 5758.4 | 6595.8 | 1424.5 |
| select | 1 | 2000 | 998.4 | 1075.9 | 1406.8 | 1464.0 | 2959.9 | 999.9 |
| select | 2 | 2000 | 997.0 | 1095.7 | 1423.3 | 1488.2 | 1491.7 | 997.6 |
| select | 5 | 2000 | 994.5 | 1073.4 | 1420.8 | 1466.5 | 1495.2 | 994.7 |
| select | 10 | 2000 | 989.2 | 1083.8 | 1430.6 | 1502.1 | 5160.3 | 992.1 |
| select | 20 | 2000 | 979.3 | 1055.6 | 1395.9 | 1448.3 | 1468.9 | 979.6 |
| select | 50 | 2000 | 949.1 | 1053.0 | 1390.2 | 1441.9 | 1946.8 | 950.1 |
| select | 100 | 2000 | 898.4 | 1027.2 | 1320.1 | 1372.3 | 1390.4 | 899.5 |
| select | 200 | 2000 | 799.2 | 906.4 | 1211.8 | 1294.8 | 1313.2 | 799.6 |
| select | 500 | 2000 | 502.3 | 917.8 | 1799.3 | 1859.3 | 1864.9 | 588.1 |
| select | 1000 | 2000 | 984.6 | 1493.4 | 1967.4 | 2098.0 | 2116.5 | 995.2 |
| select | 2000 | 2000 | 1134.2 | 1970.5 | 2443.2 | 2595.2 | 2608.1 | 1198.0 |
| select | 5000 | 2000 | 1141.6 | 2648.0 | 3646.1 | 4022.2 | 4078.8 | 1360.2 |
| select | 10000 | 2000 | 1120.4 | 2638.1 | 4993.4 | 6152.8 | 6436.5 | 1406.1 |
| write+flush 4096 B | — | 2000 | 76.8 | 106.3 | 279.1 | 331.8 | 2464.5 | 86.9 |

## Each run

### macOS, 100 µs

macos aarch64, η = 100 µs, flush false, 300.003 s, load 17.50 32.46 37.76 → 30.32 27.31 33.33

sent 3000000, received 3000000, lost 0, reordered 0; p_L (Jeffreys mean) 1.667e-7

| series | n | mean ± 95 % | sd (95 %) | median (95 %) | MAD (95 %) | p99 | p99.9 | p99.99 | max | τ_int |
|---|---|---|---|---|---|---|---|---|---|---|
| D (kernel stamp − σ) | 3000000 | 95.7 ± 52.7 | 521.5 (0.0–1201.1) | 51.7 (48.8–54.9) | 14.8 (13.2–17.1) | 517.5 | 7119.8 | 23135.8 | 42104.3 | 7982.4 |
| D (process read − σ) | 3000000 | 134.7 ± 57.0 | 708.9 (0.0–1418.6) | 67.7 (64.2–71.7) | 23.0 (21.3–24.7) | 1307.7 | 8871.5 | 32766.8 | 49499.8 | 5047.9 |
| send → kernel | 3000000 | 11.2 ± 1.2 | 5.0 (0.0–26.8) | 10.8 (9.2–12.3) | 3.1 (2.3–4.0) | 22.7 | 36.9 | 64.6 | 1847.4 | 48833.0 |
| kernel → read (gap) | 3000000 | 39.0 ± 10.0 | 445.7 (0.0–721.6) | 10.2 (10.0–10.5) | 4.5 (4.3–4.6) | 314.2 | 3835.9 | 20376.4 | 49439.1 | 395.4 |
| sender timer lateness | 2570518 | 55.0 ± 1.4 | 73.8 (0.0–126.1) | 40.8 (40.4–41.3) | 11.5 (11.2–11.9) | 189.2 | 492.2 | 2362.7 | 16755.4 | 236.8 |
| sender write+flush | 3000000 | 0.0 ± 0.0 | 0.6 (0.0–0.9) | 0.0 (0.0–0.0) | 0.0 (0.0–0.0) | 0.1 | 0.2 | 0.9 | 811.8 | 1.0 |
| receiver wait lateness | 2352423 | 45.4 ± 0.1 | 70.2 (65.5–74.6) | 34.0 (34.0–34.0) | 11.7 (11.7–11.8) | 163.8 | 541.4 | 2416.1 | 16976.8 | 1.0 |

sender behind its schedule (no wait) 429482; receiver waits asked median 41.7 µs, lateness/asked median 0.903

| lag (heartbeats) | 1 | 2 | 5 | 10 | 20 | 50 | 100 | 200 | 500 | 1000 | 2000 | 5000 | 10000 | 20000 | 50000 | 100000 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| ρ of D | 0.985 | 0.977 | 0.950 | 0.907 | 0.830 | 0.665 | 0.545 | 0.440 | 0.213 | 0.128 | 0.145 | 0.079 | 0.159 | 0.076 | 0.023 | -0.000 |
| ρ of ranks | -0.331 | 0.381 | 0.182 | 0.158 | 0.153 | 0.150 | 0.148 | 0.145 | 0.141 | 0.133 | 0.132 | 0.129 | 0.126 | 0.120 | 0.097 | 0.068 |
| P(both > p99) / P(>p99)² | 90.740 | 83.703 | 70.047 | 63.937 | 57.330 | 47.057 | 42.848 | 38.496 | 31.609 | 24.355 | 22.382 | 21.239 | 22.736 | 19.591 | 10.193 | 1.100 |

exceedances of p99 (517.5 µs): 30000, extremal index θ 0.0013 (mean cluster 757.9 heartbeats), clusters separated by ≥ 10009 heartbeats = 1000900.0 µs
| P(both > p99.9) / P(>p99.9)² | 974.667 | 949.667 | 877.001 | 769.003 | 598.337 | 320.339 | 178.006 | 177.345 | 73.679 | 62.021 | 63.376 | 16.027 | 70.569 | 12.416 | 4.746 | 0.000 |

exceedances of p99.9 (7119.8 µs): 3000, extremal index θ 0.0009 (mean cluster 1089.7 heartbeats), clusters separated by ≥ 812830 heartbeats = 81283000.0 µs
τ_int 7982.4 heartbeats = 798241.0 µs; Bartlett band ±0.0011, values inside from lag None, ranks from None

| window m | windows | Allan dev of mean, µs | white-noise σ/√m, µs | Allan dev of median, µs |
|---|---|---|---|---|
| 1 (0.1 ms) | 3000000 | 64.75 | 521.48 | 64.75 |
| 2 (0.2 ms) | 1500000 | 63.92 | 368.74 | 76.55 |
| 4 (0.4 ms) | 750000 | 82.62 | 260.74 | 101.34 |
| 8 (0.8 ms) | 375000 | 112.68 | 184.37 | 136.13 |
| 16 (1.6 ms) | 187500 | 154.97 | 130.37 | 182.09 |
| 32 (3.2 ms) | 93750 | 201.63 | 92.19 | 239.67 |
| 64 (6.4 ms) | 46875 | 233.71 | 65.19 | 274.55 |
| 128 (12.8 ms) | 23437 | 227.64 | 46.09 | 252.87 |
| 256 (25.6 ms) | 11718 | 263.33 | 32.59 | 250.33 |
| 512 (51.2 ms) | 5859 | 299.63 | 23.05 | 318.07 |
| 1024 (102.4 ms) | 2929 | 257.83 | 16.30 | 195.82 |
| 2048 (204.8 ms) | 1464 | 202.02 | 11.52 | 49.58 |
| 4096 (409.6 ms) | 732 | 122.58 | 8.15 | 10.39 |
| 8192 (819.2 ms) | 366 | 110.76 | 5.76 | 8.92 |
| 16384 (1638.4 ms) | 183 | 109.27 | 4.07 | 12.22 |
| 32768 (3276.8 ms) | 91 | 153.28 | 2.88 | 16.73 |
| 65536 (6553.6 ms) | 45 | 170.47 | 2.04 | 18.01 |
| 131072 (13107.2 ms) | 22 | 140.68 | 1.44 | 11.25 |

Allan minimum (mean): window 2 = 0.2 ms, deviation 63.92 µs; (median): window 8192
G (mean lateness of the receiver's own waits) 45.4 µs; n_G = τ_int·V/G² = 1053844

| m | blocks | median sd_m/sd | p05 sd_m/sd |
|---|---|---|---|
| 2 | 1500000 | 0.041 | 0.002 |
| 8 | 375000 | 0.054 | 0.006 |
| 32 | 93750 | 0.052 | 0.009 |
| 128 | 23437 | 0.051 | 0.011 |
| 512 | 5859 | 0.051 | 0.012 |
| 2048 | 1464 | 0.053 | 0.013 |

| spacing η | α (quantile) | heartbeats in margin | window | Theorem 7 β (mean/sd) | replayed mistakes / points | verdict |
|---|---|---|---|---|---|---|
| 100.0 µs | p99 421.8 µs | 5 | 2 | 3.51e-1 | 3064 / 2999999 | holds |
| 100.0 µs | p99.9 7024.1 µs | 71 | 2 | 1.90e-106 | 73 / 2999999 | **refuted** |
| 100.0 µs | p99.99 23040.2 µs | 231 | 2 | 0.00e0 | 0 / 2999999 | holds |
| 100.0 µs | 2 beats 150.0 µs | 2 | 2 | 9.15e-1 | 18488 / 2999999 | holds |
| 100.0 µs | 3 beats 250.0 µs | 3 | 2 | 7.44e-1 | 5970 / 2999999 | holds |
| 100.0 µs | 5 beats 450.0 µs | 5 | 2 | 2.94e-1 | 2801 / 2999999 | holds |
| 100.0 µs | 10 beats 950.0 µs | 10 | 2 | 1.12e-3 | 951 / 2999999 | holds |
| 100.0 µs | 20 beats 1950.0 µs | 20 | 2 | 3.10e-13 | 417 / 2999999 | **refuted** |
| 200.0 µs | p99 421.8 µs | 3 | 1 | 5.11e-1 | 5330 / 2999998 | holds |
| 200.0 µs | p99.9 7024.1 µs | 36 | 1 | 3.74e-54 | 140 / 2999998 | **refuted** |
| 200.0 µs | p99.99 23040.2 µs | 116 | 1 | 6.66e-285 | 1 / 2999998 | **refuted** |
| 200.0 µs | 2 beats 300.0 µs | 2 | 1 | 7.25e-1 | 7945 / 2999998 | holds |
| 200.0 µs | 3 beats 500.0 µs | 3 | 1 | 3.78e-1 | 4300 / 2999998 | holds |
| 200.0 µs | 5 beats 900.0 µs | 5 | 1 | 3.39e-2 | 1883 / 2999998 | holds |
| 200.0 µs | 10 beats 1900.0 µs | 10 | 1 | 5.60e-7 | 821 / 2999998 | **refuted** |
| 200.0 µs | 20 beats 3900.0 µs | 20 | 1 | 3.72e-22 | 380 / 2999998 | **refuted** |
| 500.0 µs | p99.9 7024.1 µs | 15 | 2048 | 8.91e-23 | 295 / 2999995 | **refuted** |
| 500.0 µs | p99.99 23040.2 µs | 47 | 2048 | 2.22e-115 | 10 / 2999995 | **refuted** |
| 500.0 µs | 2 beats 750.0 µs | 2 | 2048 | 2.65e-1 | 3205 / 2999995 | holds |
| 500.0 µs | 3 beats 1250.0 µs | 3 | 2048 | 3.93e-2 | 2278 / 2999995 | holds |
| 500.0 µs | 5 beats 2250.0 µs | 5 | 2048 | 1.63e-4 | 1465 / 2999995 | **refuted** |
| 500.0 µs | 10 beats 4750.0 µs | 10 | 2048 | 4.77e-13 | 550 / 2999995 | **refuted** |
| 500.0 µs | 20 beats 9750.0 µs | 20 | 2048 | 4.61e-36 | 128 / 2999995 | **refuted** |
| 1000.0 µs | p99.9 7024.1 µs | 8 | 1024 | 2.65e-12 | 543 / 2999990 | **refuted** |
| 1000.0 µs | p99.99 23040.2 µs | 24 | 1024 | 7.45e-59 | 20 / 2999990 | **refuted** |
| 1000.0 µs | 2 beats 1500.0 µs | 2 | 1024 | 5.62e-2 | 3600 / 2999990 | holds |
| 1000.0 µs | 3 beats 2500.0 µs | 3 | 1024 | 2.34e-3 | 2413 / 2999990 | holds |
| 1000.0 µs | 5 beats 4500.0 µs | 5 | 1024 | 6.74e-7 | 1121 / 2999990 | **refuted** |
| 1000.0 µs | 10 beats 9500.0 µs | 10 | 1024 | 2.08e-18 | 244 / 2999990 | **refuted** |
| 1000.0 µs | 20 beats 19500.0 µs | 20 | 1024 | 2.00e-47 | 21 / 2999990 | **refuted** |
| 2000.0 µs | p99.9 7024.1 µs | 4 | 512 | 3.47e-7 | 907 / 2999980 | **refuted** |
| 2000.0 µs | p99.99 23040.2 µs | 12 | 512 | 1.04e-30 | 40 / 2999980 | **refuted** |
| 2000.0 µs | 2 beats 3000.0 µs | 2 | 512 | 6.27e-3 | 3358 / 2999980 | holds |
| 2000.0 µs | 3 beats 5000.0 µs | 3 | 512 | 6.75e-5 | 1648 / 2999980 | **refuted** |
| 2000.0 µs | 5 beats 9000.0 µs | 5 | 512 | 1.25e-9 | 478 / 2999980 | **refuted** |
| 2000.0 µs | 10 beats 19000.0 µs | 10 | 512 | 3.84e-24 | 42 / 2999980 | **refuted** |
| 2000.0 µs | 20 beats 39000.0 µs | 20 | 512 | 3.57e-59 | 11 / 2999980 | **refuted** |
| 5000.0 µs | p99.9 7024.1 µs | 2 | 2048 | 3.41e-4 | 1916 / 2999950 | **refuted** |
| 5000.0 µs | p99.99 23040.2 µs | 5 | 2048 | 8.18e-14 | 104 / 2999950 | **refuted** |
| 5000.0 µs | 2 beats 7500.0 µs | 2 | 2048 | 2.01e-4 | 1640 / 2999950 | **refuted** |
| 5000.0 µs | 3 beats 12500.0 µs | 3 | 2048 | 3.49e-7 | 350 / 2999950 | **refuted** |
| 5000.0 µs | 5 beats 22500.0 µs | 5 | 2048 | 1.66e-13 | 105 / 2999950 | **refuted** |
| 10000.0 µs | p99.99 23040.2 µs | 3 | 1024 | 2.34e-8 | 184 / 2999900 | **refuted** |
| 10000.0 µs | 2 beats 15000.0 µs | 2 | 1024 | 1.30e-5 | 282 / 2999900 | **refuted** |
| 10000.0 µs | 3 beats 25000.0 µs | 3 | 1024 | 5.65e-9 | 167 / 2999900 | **refuted** |
| 20000.0 µs | p99.99 23040.2 µs | 2 | 32 | 1.46e-5 | 262 / 2999800 | **refuted** |
| 20000.0 µs | 2 beats 30000.0 µs | 2 | 32 | 8.20e-7 | 123 / 2999800 | **refuted** |

correlation time (least spacing past every refutation) 50000.0 µs

floors: G 45.4 µs, correlation time 50000.0 µs, flush stability E[flush]+G 0.0 µs → floor 50000.0 µs; election inputs: l = 50.3 µs, vote round = 100.6 µs

| estimate | η | α (at quantile) | window | Theorem 7 β | replayed mistakes / points | replayed rate (95 %) | bound holds |
|---|---|---|---|---|---|---|---|
| mean/sd | 100.0 µs | 24.0 µs (p90) | 2 | 9.98e-1 | 578339 / 2999999 | 1.93e-1 (1.92e-1–1.93e-1) | yes |
| mean/sd | 100.0 µs | 421.8 µs (p99) | 2 | 3.51e-1 | 3064 / 2999999 | 1.02e-3 (9.86e-4–1.06e-3) | yes |
| mean/sd | 100.0 µs | 7024.1 µs (p99.9) | 2 | 1.90e-106 | 73 / 2999999 | 2.43e-5 (1.94e-5–3.06e-5) | **no** |
| mean/sd | 100.0 µs | 23040.2 µs (p99.99) | 2 | 0.00e0 | 0 / 2999999 | 0.00e0 (0.00e0–1.28e-6) | yes |
| mean/sd | 50000.0 µs | 24.0 µs (p90) | 128 | 9.98e-1 | 493719 / 2999500 | 1.65e-1 (1.64e-1–1.65e-1) | yes |
| mean/sd | 50000.0 µs | 421.8 µs (p99) | 128 | 6.04e-1 | 21576 / 2999500 | 7.19e-3 (7.10e-3–7.29e-3) | yes |
| mean/sd | 50000.0 µs | 7024.1 µs (p99.9) | 128 | 5.48e-3 | 2620 / 2999500 | 8.73e-4 (8.41e-4–9.08e-4) | yes |
| mean/sd | 50000.0 µs | 23040.2 µs (p99.99) | 128 | 5.12e-4 | 294 / 2999500 | 9.80e-5 (8.74e-5–1.10e-4) | yes |
| median/1.4826·MAD | 100.0 µs | 68.0 µs (p90) | 2 | 9.46e-2 | 97898 / 2999999 | 3.26e-2 (3.24e-2–3.28e-2) | yes |
| median/1.4826·MAD | 100.0 µs | 465.8 µs (p99) | 2 | 9.44e-11 | 2419 / 2999999 | 8.06e-4 (7.75e-4–8.39e-4) | **no** |
| median/1.4826·MAD | 100.0 µs | 7068.1 µs (p99.9) | 2 | 1.94e-296 | 69 / 2999999 | 2.30e-5 (1.82e-5–2.91e-5) | **no** |
| median/1.4826·MAD | 100.0 µs | 23084.2 µs (p99.99) | 2 | 0.00e0 | 0 / 2999999 | 0.00e0 (0.00e0–1.28e-6) | yes |
| median/1.4826·MAD | 50000.0 µs | 68.0 µs (p90) | 128 | 9.46e-2 | 232407 / 2999500 | 7.75e-2 (7.72e-2–7.78e-2) | yes |
| median/1.4826·MAD | 50000.0 µs | 465.8 µs (p99) | 128 | 2.22e-3 | 28642 / 2999500 | 9.55e-3 (9.44e-3–9.66e-3) | **no** |
| median/1.4826·MAD | 50000.0 µs | 7068.1 µs (p99.9) | 128 | 9.85e-6 | 2990 / 2999500 | 9.97e-4 (9.62e-4–1.03e-3) | **no** |
| median/1.4826·MAD | 50000.0 µs | 23084.2 µs (p99.99) | 128 | 1.07e-6 | 302 / 2999500 | 1.01e-4 (9.00e-5–1.13e-4) | **no** |

| voters/up | span W | T_E | split |
|---|---|---|---|
| 3/3 | 244.4 µs | 204.1 µs | 0.110 |
| 3/2 | 298.9 µs | 378.2 µs | 0.308 |
| 5/5 | 260.8 µs | 164.1 µs | 0.053 |
| 5/4 | 335.2 µs | 221.2 µs | 0.110 |

| estimate | MTBF | η | α | window | detection bound | T_MR bound | U | replayed mistakes / points | replayed T_MR | bound holds | suspected share |
|---|---|---|---|---|---|---|---|---|---|---|---|
| mean/sd | 3600.000 s | 50000.0 µs | 24418.8 µs | 128 (n_G 3219, n_Allan 128) | 74514.5 µs | 109.643 s | 2.425e-5 | 265 / 2999500 | 566.038 s | yes | 1.33e-5 |
| mean/sd | 1.0 d | 50000.0 µs | 56001.2 µs | 128 (n_G 3219, n_Allan 128) | 106096.9 µs | 76798.224 s | 1.237e-6 | 0 / 2999500 | > 1.7 d | yes | 0.00e0 |
| mean/sd | 30.0 d | 50000.0 µs | 65775.8 µs | 128 (n_G 3219, n_Allan 128) | 115871.4 µs | 8.4 d | 4.537e-8 | 0 / 2999500 | > 1.7 d | yes | 0.00e0 |
| mean/sd | 365.0 d | 50000.0 µs | 69184.4 µs | 128 (n_G 3219, n_Allan 128) | 119280.1 µs | 13.8 d | 4.113e-9 | 0 / 2999500 | > 1.7 d | yes | 0.00e0 |
| median/1.4826·MAD | 3600.000 s | 50000.0 µs | 14653.9 µs | 128 (n_G 3219, n_Allan 128) | 64705.6 µs | 20672.043 s | 1.810e-5 | 576 / 2999500 | 260.417 s | **no** | 3.93e-5 |
| median/1.4826·MAD | 1.0 d | 50000.0 µs | 14686.8 µs | 128 (n_G 3219, n_Allan 128) | 64738.5 µs | 20758.545 s | 7.719e-7 | 572 / 2999500 | 262.238 s | **no** | 3.92e-5 |
| median/1.4826·MAD | 30.0 d | 50000.0 µs | 25374.8 µs | 128 (n_G 3219, n_Allan 128) | 75426.5 µs | 54481.497 s | 3.619e-8 | 253 / 2999500 | 592.885 s | **no** | 1.23e-5 |
| median/1.4826·MAD | 365.0 d | 50000.0 µs | 60785.1 µs | 128 (n_G 3219, n_Allan 128) | 110836.8 µs | 449763.7 d | 3.527e-9 | 0 / 2999500 | > 1.7 d | yes | 0.00e0 |

### macOS, 100 µs, under a parallel cargo build

macos aarch64, η = 100 µs, flush false, 300.003 s, load 22.46 19.08 26.14 → 32.05 27.60 28.06

sent 3000000, received 3000000, lost 0, reordered 0; p_L (Jeffreys mean) 1.667e-7

| series | n | mean ± 95 % | sd (95 %) | median (95 %) | MAD (95 %) | p99 | p99.9 | p99.99 | max | τ_int |
|---|---|---|---|---|---|---|---|---|---|---|
| D (kernel stamp − σ) | 3000000 | 103.9 ± 23.3 | 821.2 (0.0–1226.8) | 60.6 (59.4–61.8) | 22.7 (21.9–23.5) | 273.2 | 9097.9 | 39675.5 | 57853.5 | 627.9 |
| D (process read − σ) | 3000000 | 164.7 ± 51.1 | 1547.4 (0.0–2376.4) | 82.4 (80.4–84.4) | 29.8 (28.8–30.8) | 594.2 | 18700.8 | 74526.7 | 101100.5 | 850.8 |
| send → kernel | 3000000 | 13.3 ± 0.0 | 5.7 (4.2–6.9) | 13.0 (13.0–13.0) | 2.9 (2.9–2.9) | 23.5 | 29.8 | 58.8 | 3926.8 | 1.5 |
| kernel → read (gap) | 3000000 | 60.8 ± 44.9 | 1299.4 (0.0–2221.9) | 13.3 (12.9–13.7) | 5.6 (5.4–5.8) | 237.2 | 4735.3 | 74448.8 | 100925.1 | 932.3 |
| sender timer lateness | 2422412 | 63.1 ± 0.5 | 70.0 (0.0–135.5) | 49.5 (49.2–49.9) | 20.6 (20.5–20.8) | 222.5 | 345.0 | 1225.0 | 49383.0 | 32.5 |
| sender write+flush | 3000000 | 0.0 ± 0.0 | 0.1 (0.1–0.1) | 0.0 (0.0–0.0) | 0.0 (0.0–0.0) | 0.1 | 0.2 | 0.6 | 82.1 | 1.0 |
| receiver wait lateness | 2123568 | 49.6 ± 0.1 | 83.1 (31.9–113.1) | 39.2 (39.1–39.2) | 17.8 (17.7–17.8) | 173.5 | 384.2 | 1773.5 | 77569.5 | 1.0 |

sender behind its schedule (no wait) 577588; receiver waits asked median 35.6 µs, lateness/asked median 1.211

| lag (heartbeats) | 1 | 2 | 5 | 10 | 20 | 50 | 100 | 200 | 500 | 1000 | 2000 | 5000 | 10000 | 20000 | 50000 | 100000 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| ρ of D | 0.993 | 0.991 | 0.980 | 0.963 | 0.931 | 0.841 | 0.710 | 0.505 | 0.244 | 0.039 | 0.001 | 0.000 | 0.001 | 0.001 | -0.001 | -0.001 |
| ρ of ranks | -0.516 | 0.382 | -0.020 | 0.070 | 0.065 | 0.064 | 0.061 | 0.054 | 0.050 | 0.038 | 0.031 | 0.026 | 0.017 | 0.011 | 0.005 | 0.003 |
| P(both > p99) / P(>p99)² | 60.215 | 51.568 | 48.923 | 46.175 | 44.120 | 40.125 | 36.221 | 31.769 | 21.412 | 9.355 | 3.134 | 2.485 | 2.573 | 2.706 | 1.350 | 1.239 |

exceedances of p99 (273.2 µs): 29992, extremal index θ 0.0062 (mean cluster 160.8 heartbeats), clusters separated by ≥ 1532 heartbeats = 153200.0 µs
| P(both > p99.9) / P(>p99.9)² | 990.000 | 980.001 | 951.668 | 906.670 | 835.339 | 682.011 | 561.685 | 478.032 | 235.039 | 40.013 | 0.000 | 0.000 | 0.000 | 0.000 | 0.000 | 0.000 |

exceedances of p99.9 (9097.9 µs): 3000, extremal index θ 0.0027 (mean cluster 372.8 heartbeats), clusters separated by ≥ 137252 heartbeats = 13725200.0 µs
τ_int 627.9 heartbeats = 62787.1 µs; Bartlett band ±0.0011, values inside from lag Some(3535), ranks from None

| window m | windows | Allan dev of mean, µs | white-noise σ/√m, µs | Allan dev of median, µs |
|---|---|---|---|---|
| 1 (0.1 ms) | 3000000 | 67.44 | 821.24 | 67.44 |
| 2 (0.2 ms) | 1500000 | 66.56 | 580.71 | 76.02 |
| 4 (0.4 ms) | 750000 | 82.22 | 410.62 | 100.14 |
| 8 (0.8 ms) | 375000 | 111.86 | 290.35 | 136.08 |
| 16 (1.6 ms) | 187500 | 162.56 | 205.31 | 188.28 |
| 32 (3.2 ms) | 93750 | 212.91 | 145.18 | 252.49 |
| 64 (6.4 ms) | 46875 | 302.74 | 102.66 | 340.66 |
| 128 (12.8 ms) | 23437 | 384.37 | 72.59 | 436.95 |
| 256 (25.6 ms) | 11718 | 440.29 | 51.33 | 494.45 |
| 512 (51.2 ms) | 5859 | 423.16 | 36.29 | 448.27 |
| 1024 (102.4 ms) | 2929 | 397.67 | 25.66 | 279.36 |
| 2048 (204.8 ms) | 1464 | 383.40 | 18.15 | 139.13 |
| 4096 (409.6 ms) | 732 | 280.89 | 12.83 | 6.26 |
| 8192 (819.2 ms) | 366 | 206.15 | 9.07 | 5.42 |
| 16384 (1638.4 ms) | 183 | 148.13 | 6.42 | 5.10 |
| 32768 (3276.8 ms) | 91 | 115.84 | 4.54 | 4.30 |
| 65536 (6553.6 ms) | 45 | 77.89 | 3.21 | 3.54 |
| 131072 (13107.2 ms) | 22 | 48.61 | 2.27 | 2.62 |

Allan minimum (mean): window 131072 = 13107.2 ms, deviation 48.61 µs; (median): window 131072
G (mean lateness of the receiver's own waits) 49.6 µs; n_G = τ_int·V/G² = 171920

| m | blocks | median sd_m/sd | p05 sd_m/sd |
|---|---|---|---|
| 2 | 1500000 | 0.041 | 0.003 |
| 8 | 375000 | 0.044 | 0.014 |
| 32 | 93750 | 0.043 | 0.021 |
| 128 | 23437 | 0.042 | 0.024 |
| 512 | 5859 | 0.042 | 0.026 |
| 2048 | 1464 | 0.043 | 0.029 |

| spacing η | α (quantile) | heartbeats in margin | window | Theorem 7 β (mean/sd) | replayed mistakes / points | verdict |
|---|---|---|---|---|---|---|
| 100.0 µs | p99 169.4 µs | 2 | 131072 | 9.52e-1 | 14652 / 2999999 | holds |
| 100.0 µs | p99.9 8994.0 µs | 90 | 131072 | 1.65e-121 | 32 / 2999999 | **refuted** |
| 100.0 µs | p99.99 39571.7 µs | 396 | 131072 | 0.00e0 | 3 / 2999999 | **refuted** |
| 100.0 µs | 2 beats 150.0 µs | 2 | 131072 | 9.64e-1 | 21868 / 2999999 | holds |
| 100.0 µs | 3 beats 250.0 µs | 3 | 131072 | 8.82e-1 | 3481 / 2999999 | holds |
| 100.0 µs | 5 beats 450.0 µs | 5 | 131072 | 5.74e-1 | 1002 / 2999999 | holds |
| 100.0 µs | 10 beats 950.0 µs | 10 | 131072 | 2.74e-2 | 414 / 2999999 | holds |
| 100.0 µs | 20 beats 1950.0 µs | 20 | 131072 | 1.36e-8 | 179 / 2999999 | **refuted** |
| 200.0 µs | p99.9 8994.0 µs | 45 | 65536 | 1.22e-61 | 62 / 2999998 | **refuted** |
| 200.0 µs | p99.99 39571.7 µs | 198 | 65536 | 0.00e0 | 6 / 2999998 | **refuted** |
| 200.0 µs | 2 beats 300.0 µs | 2 | 65536 | 8.69e-1 | 3220 / 2999998 | holds |
| 200.0 µs | 3 beats 500.0 µs | 3 | 65536 | 6.34e-1 | 1550 / 2999998 | holds |
| 200.0 µs | 5 beats 900.0 µs | 5 | 65536 | 1.67e-1 | 830 / 2999998 | holds |
| 200.0 µs | 10 beats 1900.0 µs | 10 | 65536 | 1.17e-4 | 360 / 2999998 | holds |
| 200.0 µs | 20 beats 3900.0 µs | 20 | 65536 | 4.28e-16 | 166 / 2999998 | **refuted** |
| 500.0 µs | p99.9 8994.0 µs | 18 | 32768 | 1.01e-25 | 154 / 2999995 | **refuted** |
| 500.0 µs | p99.99 39571.7 µs | 80 | 32768 | 2.87e-202 | 11 / 2999995 | **refuted** |
| 500.0 µs | 2 beats 750.0 µs | 2 | 32768 | 4.99e-1 | 2132 / 2999995 | holds |
| 500.0 µs | 3 beats 1250.0 µs | 3 | 32768 | 1.50e-1 | 1389 / 2999995 | holds |
| 500.0 µs | 5 beats 2250.0 µs | 5 | 32768 | 3.19e-3 | 700 / 2999995 | holds |
| 500.0 µs | 10 beats 4750.0 µs | 10 | 32768 | 7.50e-10 | 282 / 2999995 | **refuted** |
| 500.0 µs | 20 beats 9750.0 µs | 20 | 32768 | 5.89e-29 | 129 / 2999995 | **refuted** |
| 1000.0 µs | p99.9 8994.0 µs | 9 | 16384 | 9.52e-14 | 289 / 2999990 | **refuted** |
| 1000.0 µs | p99.99 39571.7 µs | 40 | 16384 | 2.39e-102 | 22 / 2999990 | **refuted** |
| 1000.0 µs | 2 beats 1500.0 µs | 2 | 16384 | 1.68e-1 | 1904 / 2999990 | holds |
| 1000.0 µs | 3 beats 2500.0 µs | 3 | 16384 | 1.64e-2 | 1147 / 2999990 | holds |
| 1000.0 µs | 5 beats 4500.0 µs | 5 | 16384 | 2.76e-5 | 562 / 2999990 | **refuted** |
| 1000.0 µs | 10 beats 9500.0 µs | 10 | 16384 | 7.68e-15 | 249 / 2999990 | **refuted** |
| 1000.0 µs | 20 beats 19500.0 µs | 20 | 16384 | 6.36e-40 | 74 / 2999990 | **refuted** |
| 2000.0 µs | p99.9 8994.0 µs | 5 | 8192 | 8.41e-8 | 516 / 2999980 | **refuted** |
| 2000.0 µs | p99.99 39571.7 µs | 20 | 8192 | 2.07e-52 | 42 / 2999980 | **refuted** |
| 2000.0 µs | 2 beats 3000.0 µs | 2 | 8192 | 2.81e-2 | 1682 / 2999980 | holds |
| 2000.0 µs | 3 beats 5000.0 µs | 3 | 8192 | 7.38e-4 | 904 / 2999980 | holds |
| 2000.0 µs | 5 beats 9000.0 µs | 5 | 8192 | 8.27e-8 | 513 / 2999980 | **refuted** |
| 2000.0 µs | 10 beats 19000.0 µs | 10 | 8192 | 2.37e-20 | 143 / 2999980 | **refuted** |
| 2000.0 µs | 20 beats 39000.0 µs | 20 | 8192 | 1.92e-51 | 53 / 2999980 | **refuted** |
| 5000.0 µs | p99.9 8994.0 µs | 2 | 2048 | 3.35e-4 | 1056 / 2999950 | holds |
| 5000.0 µs | p99.99 39571.7 µs | 8 | 2048 | 2.66e-22 | 111 / 2999950 | **refuted** |
| 5000.0 µs | 2 beats 7500.0 µs | 2 | 2048 | 1.15e-3 | 1355 / 2999950 | holds |
| 5000.0 µs | 3 beats 12500.0 µs | 3 | 2048 | 4.96e-6 | 609 / 2999950 | **refuted** |
| 5000.0 µs | 5 beats 22500.0 µs | 5 | 2048 | 1.45e-11 | 305 / 2999950 | **refuted** |
| 5000.0 µs | 10 beats 47500.0 µs | 10 | 2048 | 4.42e-28 | 73 / 2999950 | **refuted** |
| 10000.0 µs | p99.99 39571.7 µs | 4 | 1024 | 4.26e-12 | 215 / 2999900 | **refuted** |
| 10000.0 µs | 2 beats 15000.0 µs | 2 | 1024 | 7.85e-5 | 755 / 2999900 | **refuted** |
| 10000.0 µs | 3 beats 25000.0 µs | 3 | 1024 | 8.46e-8 | 534 / 2999900 | **refuted** |
| 10000.0 µs | 5 beats 45000.0 µs | 5 | 1024 | 1.55e-14 | 150 / 2999900 | **refuted** |
| 20000.0 µs | p99.99 39571.7 µs | 2 | 512 | 7.57e-7 | 301 / 2999800 | **refuted** |
| 20000.0 µs | 2 beats 30000.0 µs | 2 | 512 | 5.02e-6 | 677 / 2999800 | **refuted** |
| 20000.0 µs | 3 beats 50000.0 µs | 3 | 512 | 1.35e-9 | 81 / 2999800 | **refuted** |

correlation time (least spacing past every refutation) 50000.0 µs

floors: G 49.6 µs, correlation time 50000.0 µs, flush stability E[flush]+G 0.0 µs → floor 50000.0 µs; election inputs: l = 74.1 µs, vote round = 148.2 µs

| estimate | η | α (at quantile) | window | Theorem 7 β | replayed mistakes / points | replayed rate (95 %) | bound holds |
|---|---|---|---|---|---|---|---|
| mean/sd | 100.0 µs | 26.4 µs (p90) | 131072 | 9.99e-1 | 432730 / 2999999 | 1.44e-1 (1.44e-1–1.45e-1) | yes |
| mean/sd | 100.0 µs | 169.4 µs (p99) | 131072 | 9.52e-1 | 14652 / 2999999 | 4.88e-3 (4.81e-3–4.96e-3) | yes |
| mean/sd | 100.0 µs | 8994.0 µs (p99.9) | 131072 | 1.65e-121 | 32 / 2999999 | 1.07e-5 (7.56e-6–1.51e-5) | **no** |
| mean/sd | 100.0 µs | 39571.7 µs (p99.99) | 131072 | 0.00e0 | 3 / 2999999 | 1.00e-6 (3.40e-7–2.94e-6) | **no** |
| mean/sd | 50000.0 µs | 26.4 µs (p90) | 256 | 9.99e-1 | 510314 / 2999500 | 1.70e-1 (1.70e-1–1.71e-1) | yes |
| mean/sd | 50000.0 µs | 169.4 µs (p99) | 256 | 9.59e-1 | 33217 / 2999500 | 1.11e-2 (1.10e-2–1.12e-2) | yes |
| mean/sd | 50000.0 µs | 8994.0 µs (p99.9) | 256 | 8.27e-3 | 2995 / 2999500 | 9.98e-4 (9.63e-4–1.03e-3) | yes |
| mean/sd | 50000.0 µs | 39571.7 µs (p99.99) | 256 | 4.31e-4 | 301 / 2999500 | 1.00e-4 (8.96e-5–1.12e-4) | yes |
| median/1.4826·MAD | 100.0 µs | 69.7 µs (p90) | 131072 | 1.89e-1 | 248006 / 2999999 | 8.27e-2 (8.23e-2–8.30e-2) | yes |
| median/1.4826·MAD | 100.0 µs | 212.7 µs (p99) | 131072 | 1.74e-3 | 11693 / 2999999 | 3.90e-3 (3.83e-3–3.97e-3) | **no** |
| median/1.4826·MAD | 100.0 µs | 9037.3 µs (p99.9) | 131072 | 0.00e0 | 31 / 2999999 | 1.03e-5 (7.28e-6–1.47e-5) | **no** |
| median/1.4826·MAD | 100.0 µs | 39615.0 µs (p99.99) | 131072 | 0.00e0 | 4 / 2999999 | 1.33e-6 (5.19e-7–3.43e-6) | **no** |
| median/1.4826·MAD | 50000.0 µs | 69.7 µs (p90) | 256 | 1.89e-1 | 295040 / 2999500 | 9.84e-2 (9.80e-2–9.87e-2) | yes |
| median/1.4826·MAD | 50000.0 µs | 212.7 µs (p99) | 256 | 2.44e-2 | 29107 / 2999500 | 9.70e-3 (9.59e-3–9.82e-3) | yes |
| median/1.4826·MAD | 50000.0 µs | 9037.3 µs (p99.9) | 256 | 1.40e-5 | 3000 / 2999500 | 1.00e-3 (9.65e-4–1.04e-3) | **no** |
| median/1.4826·MAD | 50000.0 µs | 39615.0 µs (p99.99) | 256 | 8.86e-7 | 301 / 2999500 | 1.00e-4 (8.96e-5–1.12e-4) | **no** |

| voters/up | span W | T_E | split |
|---|---|---|---|
| 3/3 | 351.0 µs | 300.6 µs | 0.115 |
| 3/2 | 440.3 µs | 557.0 µs | 0.308 |
| 5/5 | 376.0 µs | 241.7 µs | 0.056 |
| 5/4 | 493.7 µs | 325.9 µs | 0.110 |

| estimate | MTBF | η | α | window | detection bound | T_MR bound | U | replayed mistakes / points | replayed T_MR | bound holds | suspected share |
|---|---|---|---|---|---|---|---|---|---|---|---|
| mean/sd | 3600.000 s | 50000.0 µs | 52635.6 µs | 256 (n_G 646, n_Allan 256) | 102739.5 µs | 2319.816 s | 2.893e-5 | 54 / 2999500 | 2777.778 s | yes | 9.49e-7 |
| mean/sd | 1.0 d | 50000.0 µs | 64997.3 µs | 256 (n_G 646, n_Allan 256) | 115101.2 µs | 1.2 d | 1.344e-6 | 0 / 2999500 | > 1.7 d | yes | 0.00e0 |
| mean/sd | 30.0 d | 50000.0 µs | 64177.4 µs | 256 (n_G 646, n_Allan 256) | 114281.3 µs | 1.1 d | 5.041e-8 | 0 / 2999500 | > 1.7 d | yes | 0.00e0 |
| mean/sd | 365.0 d | 50000.0 µs | 86009.2 µs | 256 (n_G 646, n_Allan 256) | 136113.1 µs | 12.2 d | 4.863e-9 | 0 / 2999500 | > 1.7 d | yes | 0.00e0 |
| median/1.4826·MAD | 3600.000 s | 50000.0 µs | 14683.5 µs | 256 (n_G 646, n_Allan 256) | 64744.1 µs | 9251.162 s | 1.820e-5 | 1850 / 2999500 | 81.081 s | **no** | 1.74e-4 |
| median/1.4826·MAD | 1.0 d | 50000.0 µs | 14766.8 µs | 256 (n_G 646, n_Allan 256) | 64827.3 µs | 9353.049 s | 8.163e-7 | 1840 / 2999500 | 81.522 s | **no** | 1.73e-4 |
| median/1.4826·MAD | 30.0 d | 50000.0 µs | 55936.1 µs | 256 (n_G 646, n_Allan 256) | 105996.7 µs | 34046.1 d | 4.111e-8 | 21 / 2999500 | 7142.857 s | **no** | 1.30e-7 |
| median/1.4826·MAD | 365.0 d | 50000.0 µs | 64092.4 µs | 256 (n_G 646, n_Allan 256) | 114153.0 µs | 223885.3 d | 3.637e-9 | 0 / 2999500 | > 1.7 d | yes | 0.00e0 |

### macOS, 50 µs at load 41 (the sender unstable)

macos aarch64, η = 50 µs, flush false, 300.003 s, load 41.44 32.29 30.62 → 43.46 38.89 34.27

sent 6000000, received 6000000, lost 0, reordered 0; p_L (Jeffreys mean) 8.333e-8

| series | n | mean ± 95 % | sd (95 %) | median (95 %) | MAD (95 %) | p99 | p99.9 | p99.99 | max | τ_int |
|---|---|---|---|---|---|---|---|---|---|---|
| D (kernel stamp − σ) | 6000000 | 386.4 ± 152.4 | 4365.5 (2252.7–5748.1) | 96.4 (91.5–101.4) | 60.6 (59.3–61.9) | 739.9 | 86459.7 | 120900.5 | 125174.2 | 1904.6 |
| D (process read − σ) | 6000000 | 621.5 ± 194.6 | 5717.5 (3868.9–7100.1) | 138.5 (133.3–143.6) | 73.3 (70.9–75.8) | 6256.2 | 101689.0 | 122499.0 | 125246.5 | 1810.2 |
| send → kernel | 6000000 | 10.6 ± 0.0 | 6.3 (4.2–7.9) | 9.9 (9.9–9.9) | 3.1 (3.1–3.1) | 21.7 | 28.8 | 60.9 | 6771.3 | 1.5 |
| kernel → read (gap) | 6000000 | 235.1 ± 126.0 | 3701.4 (1494.6–5016.7) | 13.2 (13.0–13.5) | 4.4 (4.2–4.6) | 332.4 | 73446.5 | 117775.2 | 125138.2 | 1809.2 |
| sender timer lateness | 2016355 | 110.8 ± 0.4 | 221.0 (114.4–290.9) | 54.7 (54.3–55.1) | 40.0 (40.0–40.1) | 340.9 | 534.4 | 1979.7 | 125139.2 | 1.6 |
| sender write+flush | 6000000 | 0.0 ± 0.0 | 0.2 (0.2–0.3) | 0.0 (0.0–0.0) | 0.0 (0.0–0.0) | 0.1 | 0.2 | 0.3 | 213.9 | 1.0 |
| receiver wait lateness | 1729282 | 87.8 ± 0.3 | 213.1 (129.6–272.1) | 34.5 (34.4–34.6) | 22.5 (22.5–22.5) | 311.7 | 485.6 | 2176.4 | 125182.9 | 1.0 |

sender behind its schedule (no wait) 3983645; receiver waits asked median 15.5 µs, lateness/asked median 3.380

| lag (heartbeats) | 1 | 2 | 5 | 10 | 20 | 50 | 100 | 200 | 500 | 1000 | 2000 | 5000 | 10000 | 20000 | 50000 | 100000 | 200000 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| ρ of D | 0.999 | 0.999 | 0.997 | 0.994 | 0.988 | 0.969 | 0.938 | 0.879 | 0.713 | 0.451 | 0.076 | -0.003 | 0.001 | -0.003 | -0.003 | 0.001 | 0.019 |
| ρ of ranks | 0.379 | -0.003 | 0.067 | -0.028 | 0.124 | 0.079 | 0.078 | 0.071 | 0.058 | 0.042 | 0.020 | 0.007 | 0.006 | 0.003 | 0.004 | 0.004 | 0.000 |
| P(both > p99) / P(>p99)² | 97.935 | 96.132 | 91.718 | 86.313 | 80.305 | 74.716 | 69.552 | 63.413 | 51.166 | 37.873 | 15.572 | 2.544 | 4.596 | 3.186 | 1.395 | 3.998 | 2.926 |

exceedances of p99 (739.9 µs): 59999, extremal index θ 0.0015 (mean cluster 656.2 heartbeats), clusters separated by ≥ 8632 heartbeats = 431600.0 µs
| P(both > p99.9) / P(>p99.9)² | 998.500 | 997.000 | 992.501 | 985.002 | 970.837 | 930.841 | 864.181 | 730.858 | 372.531 | 0.000 | 0.000 | 0.000 | 0.000 | 0.000 | 0.000 | 0.000 | 0.000 |

exceedances of p99.9 (86459.7 µs): 6000, extremal index θ 0.0014 (mean cluster 737.5 heartbeats), clusters separated by ≥ 109180 heartbeats = 5459000.0 µs
τ_int 1904.6 heartbeats = 95229.4 µs; Bartlett band ±0.0008, values inside from lag None, ranks from None

| window m | windows | Allan dev of mean, µs | white-noise σ/√m, µs | Allan dev of median, µs |
|---|---|---|---|---|
| 1 (0.05 ms) | 6000000 | 114.89 | 4365.50 | 114.89 |
| 2 (0.1 ms) | 3000000 | 141.54 | 3086.87 | 162.59 |
| 4 (0.2 ms) | 1500000 | 181.69 | 2182.75 | 219.16 |
| 8 (0.4 ms) | 750000 | 249.14 | 1543.44 | 297.75 |
| 16 (0.8 ms) | 375000 | 337.34 | 1091.37 | 429.36 |
| 32 (1.6 ms) | 187500 | 507.10 | 771.72 | 603.12 |
| 64 (3.2 ms) | 93750 | 703.30 | 545.69 | 867.69 |
| 128 (6.4 ms) | 46875 | 928.43 | 385.86 | 1205.96 |
| 256 (12.8 ms) | 23437 | 1387.89 | 272.84 | 1670.63 |
| 512 (25.6 ms) | 11718 | 2014.60 | 192.93 | 2265.26 |
| 1024 (51.2 ms) | 5859 | 2794.01 | 136.42 | 3126.93 |
| 2048 (102.4 ms) | 2929 | 2833.32 | 96.46 | 3302.41 |
| 4096 (204.8 ms) | 1464 | 2799.71 | 68.21 | 1861.37 |
| 8192 (409.6 ms) | 732 | 2090.52 | 48.23 | 16.90 |
| 16384 (819.2 ms) | 366 | 1511.79 | 34.11 | 12.49 |
| 32768 (1638.4 ms) | 183 | 1075.38 | 24.12 | 8.16 |
| 65536 (3276.8 ms) | 91 | 702.59 | 17.05 | 6.16 |
| 131072 (6553.6 ms) | 45 | 394.93 | 12.06 | 6.51 |
| 262144 (13107.2 ms) | 22 | 354.49 | 8.53 | 4.58 |

Allan minimum (mean): window 1 = 0.05 ms, deviation 114.89 µs; (median): window 262144
G (mean lateness of the receiver's own waits) 87.8 µs; n_G = τ_int·V/G² = 4704973

| m | blocks | median sd_m/sd | p05 sd_m/sd |
|---|---|---|---|
| 2 | 3000000 | 0.008 | 0.001 |
| 8 | 750000 | 0.018 | 0.008 |
| 32 | 187500 | 0.019 | 0.012 |
| 128 | 46875 | 0.019 | 0.012 |
| 512 | 11718 | 0.019 | 0.013 |
| 2048 | 2929 | 0.019 | 0.014 |

| spacing η | α (quantile) | heartbeats in margin | window | Theorem 7 β (mean/sd) | replayed mistakes / points | verdict |
|---|---|---|---|---|---|---|
| 50.0 µs | p99 353.5 µs | 8 | 1 | 9.81e-1 | 13411 / 5999999 | holds |
| 50.0 µs | p99.9 86073.3 µs | 1722 | 1 | 0.00e0 | 4 / 5999999 | **refuted** |
| 50.0 µs | p99.99 120514.1 µs | 2411 | 1 | 0.00e0 | 3 / 5999999 | **refuted** |
| 50.0 µs | 2 beats 75.0 µs | 2 | 1 | 1.00e0 | 900870 / 5999999 | holds |
| 50.0 µs | 3 beats 125.0 µs | 3 | 1 | 9.99e-1 | 751906 / 5999999 | holds |
| 50.0 µs | 5 beats 225.0 µs | 5 | 1 | 9.95e-1 | 290780 / 5999999 | holds |
| 50.0 µs | 10 beats 475.0 µs | 10 | 1 | 9.57e-1 | 2981 / 5999999 | holds |
| 50.0 µs | 20 beats 975.0 µs | 20 | 1 | 7.09e-1 | 714 / 5999999 | holds |
| 100.0 µs | p99 353.5 µs | 4 | 1 | 9.89e-1 | 13070 / 5999998 | holds |
| 100.0 µs | p99.9 86073.3 µs | 861 | 1 | 0.00e0 | 10 / 5999998 | **refuted** |
| 100.0 µs | p99.99 120514.1 µs | 1206 | 1 | 0.00e0 | 8 / 5999998 | **refuted** |
| 100.0 µs | 2 beats 150.0 µs | 2 | 1 | 9.99e-1 | 1002266 / 5999998 | holds |
| 100.0 µs | 3 beats 250.0 µs | 3 | 1 | 9.95e-1 | 176307 / 5999998 | holds |
| 100.0 µs | 5 beats 450.0 µs | 5 | 1 | 9.79e-1 | 5665 / 5999998 | holds |
| 100.0 µs | 10 beats 950.0 µs | 10 | 1 | 8.42e-1 | 1418 / 5999998 | holds |
| 100.0 µs | 20 beats 1950.0 µs | 20 | 1 | 2.68e-1 | 549 / 5999998 | holds |
| 250.0 µs | p99 353.5 µs | 2 | 1 | 9.93e-1 | 14558 / 5999995 | holds |
| 250.0 µs | p99.9 86073.3 µs | 345 | 1 | 0.00e0 | 31 / 5999995 | **refuted** |
| 250.0 µs | p99.99 120514.1 µs | 483 | 1 | 0.00e0 | 26 / 5999995 | **refuted** |
| 250.0 µs | 2 beats 375.0 µs | 2 | 1 | 9.92e-1 | 12542 / 5999995 | holds |
| 250.0 µs | 3 beats 625.0 µs | 3 | 1 | 9.72e-1 | 5265 / 5999995 | holds |
| 250.0 µs | 5 beats 1125.0 µs | 5 | 1 | 8.76e-1 | 2468 / 5999995 | holds |
| 250.0 µs | 10 beats 2375.0 µs | 10 | 1 | 3.69e-1 | 956 / 5999995 | holds |
| 250.0 µs | 20 beats 4875.0 µs | 20 | 1 | 1.43e-3 | 428 / 5999995 | holds |
| 500.0 µs | p99.9 86073.3 µs | 173 | 1 | 5.09e-310 | 66 / 5999990 | **refuted** |
| 500.0 µs | p99.99 120514.1 µs | 242 | 1 | 0.00e0 | 56 / 5999990 | **refuted** |
| 500.0 µs | 2 beats 750.0 µs | 2 | 1 | 9.68e-1 | 6450 / 5999990 | holds |
| 500.0 µs | 3 beats 1250.0 µs | 3 | 1 | 8.95e-1 | 3732 / 5999990 | holds |
| 500.0 µs | 5 beats 2250.0 µs | 5 | 1 | 6.09e-1 | 1854 / 5999990 | holds |
| 500.0 µs | 10 beats 4750.0 µs | 10 | 1 | 3.80e-2 | 849 / 5999990 | holds |
| 500.0 µs | 20 beats 9750.0 µs | 20 | 1 | 4.66e-8 | 392 / 5999990 | **refuted** |
| 1000.0 µs | p99.9 86073.3 µs | 87 | 1 | 5.07e-156 | 136 / 5999980 | **refuted** |
| 1000.0 µs | p99.99 120514.1 µs | 121 | 1 | 1.06e-250 | 116 / 5999980 | **refuted** |
| 1000.0 µs | 2 beats 1500.0 µs | 2 | 1 | 8.83e-1 | 5078 / 5999980 | holds |
| 1000.0 µs | 3 beats 2500.0 µs | 3 | 1 | 6.65e-1 | 2936 / 5999980 | holds |
| 1000.0 µs | 5 beats 4500.0 µs | 5 | 1 | 1.96e-1 | 1691 / 5999980 | holds |
| 1000.0 µs | 10 beats 9500.0 µs | 10 | 1 | 2.17e-4 | 778 / 5999980 | holds |
| 1000.0 µs | 20 beats 19500.0 µs | 20 | 1 | 2.44e-15 | 427 / 5999980 | **refuted** |
| 2500.0 µs | p99.9 86073.3 µs | 35 | 4096 | 1.27e-63 | 449 / 5999950 | **refuted** |
| 2500.0 µs | p99.99 120514.1 µs | 49 | 4096 | 1.40e-101 | 372 / 5999950 | **refuted** |
| 2500.0 µs | 2 beats 3750.0 µs | 2 | 4096 | 5.32e-1 | 4566 / 5999950 | holds |
| 2500.0 µs | 3 beats 6250.0 µs | 3 | 4096 | 1.74e-1 | 3141 / 5999950 | holds |
| 2500.0 µs | 5 beats 11250.0 µs | 5 | 4096 | 4.55e-3 | 1706 / 5999950 | holds |
| 2500.0 µs | 10 beats 23750.0 µs | 10 | 4096 | 1.91e-9 | 802 / 5999950 | **refuted** |
| 2500.0 µs | 20 beats 48750.0 µs | 20 | 4096 | 5.01e-28 | 638 / 5999950 | **refuted** |
| 5000.0 µs | p99.9 86073.3 µs | 18 | 2048 | 8.17e-33 | 872 / 5999900 | **refuted** |
| 5000.0 µs | p99.99 120514.1 µs | 25 | 2048 | 7.23e-52 | 584 / 5999900 | **refuted** |
| 5000.0 µs | 2 beats 7500.0 µs | 2 | 2048 | 1.91e-1 | 4551 / 5999900 | holds |
| 5000.0 µs | 3 beats 12500.0 µs | 3 | 2048 | 2.07e-2 | 2696 / 5999900 | holds |
| 5000.0 µs | 5 beats 22500.0 µs | 5 | 2048 | 4.40e-5 | 1572 / 5999900 | **refuted** |
| 5000.0 µs | 10 beats 47500.0 µs | 10 | 2048 | 2.24e-14 | 1259 / 5999900 | **refuted** |
| 5000.0 µs | 20 beats 97500.0 µs | 20 | 2048 | 6.29e-39 | 753 / 5999900 | **refuted** |
| 10000.0 µs | p99.9 86073.3 µs | 9 | 1024 | 1.81e-17 | 1725 / 5999800 | **refuted** |
| 10000.0 µs | p99.99 120514.1 µs | 13 | 1024 | 5.54e-27 | 585 / 5999800 | **refuted** |
| 10000.0 µs | 2 beats 15000.0 µs | 2 | 1024 | 3.38e-2 | 3938 / 5999800 | holds |
| 10000.0 µs | 3 beats 25000.0 µs | 3 | 1024 | 9.99e-4 | 2873 / 5999800 | holds |
| 10000.0 µs | 5 beats 45000.0 µs | 5 | 1024 | 1.43e-7 | 2498 / 5999800 | **refuted** |
| 10000.0 µs | 10 beats 95000.0 µs | 10 | 1024 | 7.51e-20 | 1544 / 5999800 | **refuted** |
| 25000.0 µs | p99.9 86073.3 µs | 4 | 512 | 2.53e-8 | 4003 / 5999500 | **refuted** |
| 25000.0 µs | p99.99 120514.1 µs | 5 | 512 | 4.12e-12 | 588 / 5999500 | **refuted** |
| 25000.0 µs | 2 beats 37500.0 µs | 2 | 512 | 1.45e-3 | 6385 / 5999500 | holds |
| 25000.0 µs | 3 beats 62500.0 µs | 3 | 512 | 7.06e-6 | 4843 / 5999500 | **refuted** |
| 25000.0 µs | 5 beats 112500.0 µs | 5 | 512 | 2.63e-11 | 1789 / 5999500 | **refuted** |
| 50000.0 µs | p99.9 86073.3 µs | 2 | 256 | 3.70e-5 | 6003 / 5999000 | **refuted** |
| 50000.0 µs | p99.99 120514.1 µs | 3 | 256 | 2.17e-7 | 594 / 5999000 | **refuted** |
| 50000.0 µs | 2 beats 75000.0 µs | 2 | 256 | 9.99e-5 | 8056 / 5999000 | **refuted** |
| 50000.0 µs | 3 beats 125000.0 µs | 3 | 256 | 1.22e-7 | 0 / 5999000 | holds |
| 100000.0 µs | p99.99 120514.1 µs | 2 | 128 | 5.68e-5 | 594 / 5998000 | **refuted** |

correlation time (least spacing past every refutation) 250000.0 µs

floors: G 87.8 µs, correlation time 250000.0 µs, flush stability E[flush]+G 0.0 µs → floor 250000.0 µs; election inputs: l = 245.7 µs, vote round = 491.4 µs

| estimate | η | α (at quantile) | window | Theorem 7 β | replayed mistakes / points | replayed rate (95 %) | bound holds |
|---|---|---|---|---|---|---|---|
| mean/sd | 50.0 µs | 353.5 µs (p99) | 1 | 9.81e-1 | 13411 / 5999999 | 2.24e-3 (2.20e-3–2.27e-3) | yes |
| mean/sd | 50.0 µs | 86073.3 µs (p99.9) | 1 | 0.00e0 | 4 / 5999999 | 6.67e-7 (2.59e-7–1.71e-6) | **no** |
| mean/sd | 50.0 µs | 120514.1 µs (p99.99) | 1 | 0.00e0 | 3 / 5999999 | 5.00e-7 (1.70e-7–1.47e-6) | **no** |
| mean/sd | 250000.0 µs | 353.5 µs (p99) | 64 | 9.93e-1 | 66035 / 5995000 | 1.10e-2 (1.09e-2–1.11e-2) | yes |
| mean/sd | 250000.0 µs | 86073.3 µs (p99.9) | 64 | 2.57e-3 | 5991 / 5995000 | 9.99e-4 (9.74e-4–1.02e-3) | yes |
| mean/sd | 250000.0 µs | 120514.1 µs (p99.99) | 64 | 1.31e-3 | 559 / 5995000 | 9.32e-5 (8.58e-5–1.01e-4) | yes |
| median/1.4826·MAD | 50.0 µs | 149.6 µs (p90) | 1 | 9.10e-2 | 655847 / 5999999 | 1.09e-1 (1.09e-1–1.10e-1) | **no** |
| median/1.4826·MAD | 50.0 µs | 643.5 µs (p99) | 1 | 9.10e-15 | 1494 / 5999999 | 2.49e-4 (2.37e-4–2.62e-4) | **no** |
| median/1.4826·MAD | 50.0 µs | 86363.2 µs (p99.9) | 1 | 0.00e0 | 4 / 5999999 | 6.67e-7 (2.59e-7–1.71e-6) | **no** |
| median/1.4826·MAD | 50.0 µs | 120804.0 µs (p99.99) | 1 | 0.00e0 | 3 / 5999999 | 5.00e-7 (1.70e-7–1.47e-6) | **no** |
| median/1.4826·MAD | 250000.0 µs | 149.6 µs (p90) | 64 | 2.65e-1 | 587821 / 5995000 | 9.81e-2 (9.78e-2–9.83e-2) | yes |
| median/1.4826·MAD | 250000.0 µs | 643.5 µs (p99) | 64 | 1.91e-2 | 59927 / 5995000 | 1.00e-2 (9.92e-3–1.01e-2) | yes |
| median/1.4826·MAD | 250000.0 µs | 86363.2 µs (p99.9) | 64 | 1.17e-6 | 6000 / 5995000 | 1.00e-3 (9.76e-4–1.03e-3) | **no** |
| median/1.4826·MAD | 250000.0 µs | 120804.0 µs (p99.99) | 64 | 6.36e-7 | 600 / 5995000 | 1.00e-4 (9.24e-5–1.08e-4) | **no** |

| voters/up | span W | T_E | split |
|---|---|---|---|
| 3/3 | 1164.2 µs | 997.2 µs | 0.115 |
| 3/2 | 1460.5 µs | 1847.6 µs | 0.308 |
| 5/5 | 1247.3 µs | 801.7 µs | 0.056 |
| 5/4 | 1637.7 µs | 1080.9 µs | 0.110 |

| estimate | MTBF | η | α | window | detection bound | T_MR bound | U | replayed mistakes / points | replayed T_MR | bound holds | suspected share |
|---|---|---|---|---|---|---|---|---|---|---|---|
| mean/sd | 3600.000 s | 250000.0 µs | 120476.4 µs | 64 (n_G 2205, n_Allan 64) | 370862.8 µs | 190.642 s | 1.132e-4 | 562 / 5995000 | 2669.039 s | yes | 7.38e-7 |
| mean/sd | 1.0 d | 250000.0 µs | 266204.4 µs | 64 (n_G 2205, n_Allan 64) | 516590.7 µs | 13737.536 s | 6.135e-6 | 0 / 5995000 | > 17.4 d | yes | 0.00e0 |
| mean/sd | 30.0 d | 250000.0 µs | 301919.1 µs | 64 (n_G 2205, n_Allan 64) | 552305.5 µs | 2.0 d | 2.246e-7 | 0 / 5995000 | > 17.4 d | yes | 0.00e0 |
| mean/sd | 365.0 d | 250000.0 µs | 359624.2 µs | 64 (n_G 2205, n_Allan 64) | 610010.6 µs | 12.4 d | 2.113e-8 | 0 / 5995000 | > 17.4 d | yes | 0.00e0 |
| median/1.4826·MAD | 3600.000 s | 250000.0 µs | 73233.5 µs | 64 (n_G 2205, n_Allan 64) | 323329.9 µs | 1.8 d | 9.034e-5 | 8527 / 5995000 | 175.912 s | **no** | 1.38e-4 |
| median/1.4826·MAD | 1.0 d | 250000.0 µs | 73252.4 µs | 64 (n_G 2205, n_Allan 64) | 323348.9 µs | 1.8 d | 3.776e-6 | 8526 / 5995000 | 175.932 s | **no** | 1.38e-4 |
| median/1.4826·MAD | 30.0 d | 250000.0 µs | 73826.3 µs | 64 (n_G 2205, n_Allan 64) | 323922.8 µs | 1.9 d | 1.372e-7 | 8414 / 5995000 | 178.274 s | **no** | 1.35e-4 |
| median/1.4826·MAD | 365.0 d | 250000.0 µs | 130180.2 µs | 64 (n_G 2205, n_Allan 64) | 380276.7 µs | 5.2 d | 1.625e-8 | 0 / 5995000 | > 17.4 d | yes | 0.00e0 |

### macOS, write and F_FULLFSYNC, 10 ms

macos aarch64, η = 10000 µs, flush true, 600.014 s, load 30.32 27.31 33.33 → 15.43 17.64 25.95

sent 60000, received 60000, lost 0, reordered 0; p_L (Jeffreys mean) 8.333e-6

| series | n | mean ± 95 % | sd (95 %) | median (95 %) | MAD (95 %) | p99 | p99.9 | p99.99 | max | τ_int |
|---|---|---|---|---|---|---|---|---|---|---|
| D (kernel stamp − σ) | 60000 | 7002.5 ± 470.2 | 10823.0 (6311.1–13944.3) | 5889.7 (5811.9–5994.2) | 576.7 (547.9–616.2) | 15678.2 | 189154.6 | 233140.3 | 245560.1 | 29.5 |
| D (process read − σ) | 60000 | 7030.1 ± 470.1 | 10824.2 (6314.6–13944.6) | 5929.2 (5842.0–6016.2) | 567.1 (538.4–606.5) | 15756.4 | 189170.8 | 233160.8 | 245577.0 | 29.5 |
| send → kernel | 60000 | 30.6 ± 0.8 | 7.9 (0.0–17.8) | 30.3 (29.6–31.0) | 3.5 (3.1–4.0) | 55.7 | 84.2 | 92.0 | 795.4 | 174.6 |
| kernel → read (gap) | 60000 | 27.5 ± 2.3 | 99.1 (0.0–152.1) | 19.2 (19.1–19.3) | 2.7 (2.6–2.8) | 114.2 | 823.7 | 5160.5 | 9785.8 | 8.4 |
| sender timer lateness | 57123 | 1637.5 ± 11.9 | 748.7 (732.9–764.1) | 1794.3 (1785.7–1802.1) | 460.0 (454.9–465.1) | 3151.8 | 3282.7 | 6488.4 | 19023.4 | 3.8 |
| sender write+flush | 60000 | 4535.8 ± 328.5 | 2186.2 (0.0–4514.1) | 4042.3 (3933.2–4255.3) | 393.8 (343.0–472.7) | 10981.8 | 17318.0 | 60825.5 | 117557.4 | 352.6 |
| receiver wait lateness | 58308 | 1627.4 ± 6.2 | 763.3 (755.8–770.7) | 1773.7 (1769.7–1777.8) | 465.2 (462.5–467.8) | 3192.9 | 4466.1 | 6356.2 | 17836.0 | 1.0 |

sender behind its schedule (no wait) 2877; receiver waits asked median 4174.6 µs, lateness/asked median 0.502

| lag (heartbeats) | 1 | 2 | 5 | 10 | 20 | 50 | 100 | 200 | 500 | 1000 | 2000 |
|---|---|---|---|---|---|---|---|---|---|---|---|
| ρ of D | 0.977 | 0.954 | 0.840 | 0.644 | 0.302 | -0.002 | -0.002 | -0.000 | -0.005 | -0.004 | -0.008 |
| ρ of ranks | 0.022 | 0.241 | 0.168 | 0.142 | 0.222 | 0.075 | 0.147 | 0.124 | 0.084 | 0.060 | 0.049 |
| P(both > p99) / P(>p99)² | 84.835 | 77.336 | 67.839 | 55.176 | 34.178 | 1.668 | 1.169 | 2.007 | 1.176 | 1.186 | 0.172 |

exceedances of p99 (15678.2 µs): 600, extremal index θ 0.0456 (mean cluster 21.9 heartbeats), clusters separated by ≥ 108 heartbeats = 1080000.0 µs
| P(both > p99.9) / P(>p99.9)² | 883.348 | 766.692 | 416.701 | 50.008 | 0.000 | 0.000 | 0.000 | 0.000 | 0.000 | 0.000 | 0.000 |

exceedances of p99.9 (189154.6 µs): 60, extremal index θ 0.1517 (mean cluster 6.6 heartbeats), clusters separated by ≥ 1 heartbeats = 10000.0 µs
τ_int 29.5 heartbeats = 294789.0 µs; Bartlett band ±0.0080, values inside from lag Some(38), ranks from None

| window m | windows | Allan dev of mean, µs | white-noise σ/√m, µs | Allan dev of median, µs |
|---|---|---|---|---|
| 1 (10 ms) | 60000 | 1647.37 | 10822.97 | 1647.37 |
| 2 (20 ms) | 30000 | 2099.32 | 7652.99 | 2224.70 |
| 4 (40 ms) | 15000 | 3333.62 | 5411.48 | 3745.97 |
| 8 (80 ms) | 7500 | 4735.10 | 3826.50 | 5371.06 |
| 16 (160 ms) | 3750 | 6459.28 | 2705.74 | 7705.58 |
| 32 (320 ms) | 1875 | 7220.81 | 1913.25 | 8777.59 |
| 64 (640 ms) | 937 | 5375.62 | 1352.87 | 2319.17 |
| 128 (1280 ms) | 468 | 4522.87 | 956.62 | 354.70 |
| 256 (2560 ms) | 234 | 3522.68 | 676.44 | 406.47 |
| 512 (5120 ms) | 117 | 2654.43 | 478.31 | 491.92 |
| 1024 (10240 ms) | 58 | 1886.25 | 338.22 | 637.88 |
| 2048 (20480 ms) | 29 | 1043.35 | 239.16 | 434.12 |

Allan minimum (mean): window 2048 = 20480 ms, deviation 1043.35 µs; (median): window 128
G (mean lateness of the receiver's own waits) 1627.4 µs; n_G = τ_int·V/G² = 1304

| m | blocks | median sd_m/sd | p05 sd_m/sd |
|---|---|---|---|
| 2 | 30000 | 0.054 | 0.011 |
| 8 | 7500 | 0.086 | 0.032 |
| 32 | 1875 | 0.087 | 0.057 |
| 128 | 468 | 0.098 | 0.067 |
| 512 | 117 | 0.109 | 0.072 |
| 2048 | 29 | 0.176 | 0.076 |

| spacing η | α (quantile) | heartbeats in margin | window | Theorem 7 β (mean/sd) | replayed mistakes / points | verdict |
|---|---|---|---|---|---|---|
| 10000.0 µs | p99.9 182152.1 µs | 19 | 1304 | 3.08e-32 | 13 / 59999 | **refuted** |
| 10000.0 µs | p99.99 226137.8 µs | 23 | 1304 | 1.48e-43 | 5 / 59999 | **refuted** |
| 10000.0 µs | 2 beats 15000.0 µs | 2 | 1304 | 2.82e-1 | 45 / 59999 | holds |
| 10000.0 µs | 3 beats 25000.0 µs | 3 | 1304 | 4.45e-2 | 18 / 59999 | holds |
| 10000.0 µs | 5 beats 45000.0 µs | 5 | 1304 | 2.13e-4 | 34 / 59999 | **refuted** |
| 10000.0 µs | 10 beats 95000.0 µs | 10 | 1304 | 8.93e-13 | 30 / 59999 | **refuted** |
| 10000.0 µs | 20 beats 195000.0 µs | 20 | 1304 | 1.83e-35 | 19 / 59999 | **refuted** |
| 20000.0 µs | p99.9 182152.1 µs | 10 | 653 | 4.51e-17 | 30 / 59998 | **refuted** |
| 20000.0 µs | p99.99 226137.8 µs | 12 | 653 | 8.73e-23 | 8 / 59998 | **refuted** |
| 20000.0 µs | 2 beats 30000.0 µs | 2 | 653 | 6.21e-2 | 37 / 59998 | holds |
| 20000.0 µs | 3 beats 50000.0 µs | 3 | 653 | 2.78e-3 | 50 / 59998 | holds |
| 20000.0 µs | 5 beats 90000.0 µs | 5 | 653 | 9.27e-7 | 50 / 59998 | **refuted** |
| 20000.0 µs | 10 beats 190000.0 µs | 10 | 653 | 4.17e-18 | 31 / 59998 | **refuted** |
| 50000.0 µs | p99.9 182152.1 µs | 4 | 257 | 4.09e-8 | 55 / 59995 | **refuted** |
| 50000.0 µs | p99.99 226137.8 µs | 5 | 257 | 1.83e-10 | 8 / 59995 | **refuted** |
| 50000.0 µs | 2 beats 75000.0 µs | 2 | 257 | 3.22e-3 | 117 / 59995 | holds |
| 50000.0 µs | 3 beats 125000.0 µs | 3 | 257 | 2.40e-5 | 106 / 59995 | **refuted** |
| 50000.0 µs | 5 beats 225000.0 µs | 5 | 257 | 2.12e-10 | 9 / 59995 | **refuted** |
| 100000.0 µs | p99.9 182152.1 µs | 2 | 124 | 6.02e-5 | 60 / 59990 | **refuted** |
| 100000.0 µs | p99.99 226137.8 µs | 3 | 124 | 2.46e-6 | 8 / 59990 | **refuted** |
| 100000.0 µs | 2 beats 150000.0 µs | 2 | 124 | 2.32e-4 | 121 / 59990 | **refuted** |
| 200000.0 µs | p99.99 226137.8 µs | 2 | 79 | 3.36e-4 | 9 / 59980 | holds |

correlation time (least spacing past every refutation) 200000.0 µs

floors: G 1627.4 µs, correlation time 200000.0 µs, flush stability E[flush]+G 6163.2 µs → floor 200000.0 µs; election inputs: l = 58.1 µs, vote round = 4652.1 µs

| estimate | η | α (at quantile) | window | Theorem 7 β | replayed mistakes / points | replayed rate (95 %) | bound holds |
|---|---|---|---|---|---|---|---|
| mean/sd | 10000.0 µs | 662.2 µs (p90) | 1304 | 9.96e-1 | 10670 / 59999 | 1.78e-1 (1.74e-1–1.81e-1) | yes |
| mean/sd | 10000.0 µs | 8675.6 µs (p99) | 1304 | 6.09e-1 | 106 / 59999 | 1.77e-3 (1.46e-3–2.14e-3) | yes |
| mean/sd | 10000.0 µs | 182152.1 µs (p99.9) | 1304 | 3.08e-32 | 13 / 59999 | 2.17e-4 (1.27e-4–3.71e-4) | **no** |
| mean/sd | 10000.0 µs | 226137.8 µs (p99.99) | 1304 | 1.48e-43 | 5 / 59999 | 8.33e-5 (3.56e-5–1.95e-4) | **no** |
| mean/sd | 200000.0 µs | 662.2 µs (p90) | 79 | 9.96e-1 | 10634 / 59980 | 1.77e-1 (1.74e-1–1.81e-1) | yes |
| mean/sd | 200000.0 µs | 8675.6 µs (p99) | 79 | 6.09e-1 | 526 / 59980 | 8.77e-3 (8.05e-3–9.55e-3) | yes |
| mean/sd | 200000.0 µs | 182152.1 µs (p99.9) | 79 | 3.53e-3 | 61 / 59980 | 1.02e-3 (7.92e-4–1.31e-3) | yes |
| mean/sd | 200000.0 µs | 226137.8 µs (p99.99) | 79 | 3.36e-4 | 9 / 59980 | 1.50e-4 (7.89e-5–2.85e-4) | yes |
| median/1.4826·MAD | 10000.0 µs | 1775.0 µs (p90) | 1304 | 1.88e-1 | 4857 / 59999 | 8.10e-2 (7.87e-2–8.33e-2) | yes |
| median/1.4826·MAD | 10000.0 µs | 9788.5 µs (p99) | 1304 | 7.58e-3 | 87 / 59999 | 1.45e-3 (1.18e-3–1.79e-3) | yes |
| median/1.4826·MAD | 10000.0 µs | 183264.9 µs (p99.9) | 1304 | 6.36e-72 | 14 / 59999 | 2.33e-4 (1.39e-4–3.92e-4) | **no** |
| median/1.4826·MAD | 10000.0 µs | 227250.6 µs (p99.99) | 1304 | 5.75e-92 | 4 / 59999 | 6.67e-5 (2.59e-5–1.71e-4) | **no** |
| median/1.4826·MAD | 200000.0 µs | 1775.0 µs (p90) | 79 | 1.88e-1 | 5526 / 59980 | 9.21e-2 (8.97e-2–9.46e-2) | yes |
| median/1.4826·MAD | 200000.0 µs | 9788.5 µs (p99) | 79 | 7.58e-3 | 510 / 59980 | 8.50e-3 (7.80e-3–9.27e-3) | **no** |
| median/1.4826·MAD | 200000.0 µs | 183264.9 µs (p99.9) | 79 | 3.01e-5 | 60 / 59980 | 1.00e-3 (7.77e-4–1.29e-3) | **no** |
| median/1.4826·MAD | 200000.0 µs | 227250.6 µs (p99.99) | 79 | 2.23e-8 | 6 / 59980 | 1.00e-4 (4.58e-5–2.18e-4) | **no** |

| voters/up | span W | T_E | split |
|---|---|---|---|
| 3/3 | 1627.4 µs | 5082.5 µs | 0.004 |
| 3/2 | 1627.4 µs | 5668.4 µs | 0.070 |
| 5/5 | 1627.4 µs | 4926.0 µs | 0.000 |
| 5/4 | 1627.4 µs | 5023.7 µs | 0.007 |

| estimate | MTBF | η | α | window | detection bound | T_MR bound | U | replayed mistakes / points | replayed T_MR | bound holds | suspected share |
|---|---|---|---|---|---|---|---|---|---|---|---|
| mean/sd | 3600.000 s | 200000.0 µs | 232097.4 µs | 79 (n_G 79, n_Allan 128) | 439099.9 µs | 899.379 s | 1.298e-4 | 5 / 59980 | 2400.000 s | yes | 1.96e-6 |
| mean/sd | 1.0 d | 200000.0 µs | 322828.4 µs | 79 (n_G 79, n_Allan 128) | 529831.0 µs | 22927.623 s | 6.445e-6 | 0 / 59980 | > 12000.000 s | yes | 0.00e0 |
| mean/sd | 30.0 d | 200000.0 µs | 450972.0 µs | 79 (n_G 79, n_Allan 128) | 657974.5 µs | 49.3 d | 2.574e-7 | 0 / 59980 | > 12000.000 s | yes | 0.00e0 |
| mean/sd | 365.0 d | 200000.0 µs | 453027.8 µs | 79 (n_G 79, n_Allan 128) | 660030.4 µs | 54.5 d | 2.231e-8 | 0 / 59980 | > 12000.000 s | yes | 0.00e0 |
| median/1.4826·MAD | 3600.000 s | 200000.0 µs | 60441.6 µs | 79 (n_G 79, n_Allan 128) | 266331.3 µs | 959.643 s | 8.146e-5 | 325 / 59980 | 36.923 s | **no** | 2.06e-3 |
| median/1.4826·MAD | 1.0 d | 200000.0 µs | 225075.8 µs | 79 (n_G 79, n_Allan 128) | 430965.5 µs | 86.9 d | 5.054e-6 | 9 / 59980 | 1333.333 s | **no** | 6.07e-6 |
| median/1.4826·MAD | 30.0 d | 200000.0 µs | 213374.8 µs | 79 (n_G 79, n_Allan 128) | 419264.5 µs | 23.3 d | 1.668e-7 | 16 / 59980 | 750.000 s | **no** | 1.83e-5 |
| median/1.4826·MAD | 365.0 d | 200000.0 µs | 271861.0 µs | 79 (n_G 79, n_Allan 128) | 477750.7 µs | 847.4 d | 1.541e-8 | 0 / 59980 | > 12000.000 s | yes | 0.00e0 |

### macOS, write and F_FULLFSYNC, 10 ms, under a parallel cargo build

macos aarch64, η = 10000 µs, flush true, 300.015 s, load 32.05 27.60 28.06 → 32.57 31.56 29.79

sent 30000, received 30000, lost 0, reordered 0; p_L (Jeffreys mean) 1.667e-5

| series | n | mean ± 95 % | sd (95 %) | median (95 %) | MAD (95 %) | p99 | p99.9 | p99.99 | max | τ_int |
|---|---|---|---|---|---|---|---|---|---|---|
| D (kernel stamp − σ) | 30000 | 7505.9 ± 1096.9 | 15889.2 (7154.7–21301.3) | 5693.2 (5645.0–5865.9) | 613.7 (580.3–649.3) | 69672.3 | 224418.9 | 275502.5 | 276650.9 | 37.2 |
| D (process read − σ) | 30000 | 7530.7 ± 1097.0 | 15889.1 (7154.0–21301.3) | 5722.9 (5668.1–5885.5) | 605.5 (571.2–645.1) | 69687.8 | 224441.1 | 275523.9 | 276667.8 | 37.2 |
| send → kernel | 30000 | 30.4 ± 1.0 | 5.8 (3.1–7.7) | 30.1 (29.1–31.2) | 3.1 (2.5–3.8) | 50.0 | 77.2 | 91.2 | 96.3 | 248.1 |
| kernel → read (gap) | 30000 | 24.8 ± 0.9 | 46.2 (0.0–74.6) | 17.7 (17.6–17.8) | 2.2 (2.2–2.2) | 105.5 | 198.0 | 952.6 | 5109.9 | 2.7 |
| sender timer lateness | 28830 | 1713.6 ± 10.0 | 746.6 (740.4–752.8) | 1861.8 (1856.5–1866.0) | 451.2 (446.9–455.7) | 3210.3 | 3332.6 | 4676.8 | 4942.5 | 1.3 |
| sender write+flush | 30000 | 4209.4 ± 391.4 | 2455.5 (0.0–5278.5) | 3858.8 (3748.1–4039.8) | 363.2 (321.4–415.2) | 11683.0 | 56865.8 | 60987.5 | 113078.2 | 198.5 |
| receiver wait lateness | 29498 | 1678.3 ± 8.7 | 761.4 (755.7–767.0) | 1831.3 (1826.1–1835.9) | 462.6 (459.3–465.6) | 3239.2 | 4352.4 | 4920.2 | 4962.4 | 1.0 |

sender behind its schedule (no wait) 1170; receiver waits asked median 4320.6 µs, lateness/asked median 0.502

| lag (heartbeats) | 1 | 2 | 5 | 10 | 20 | 50 | 100 | 200 | 500 | 1000 |
|---|---|---|---|---|---|---|---|---|---|---|
| ρ of D | 0.986 | 0.967 | 0.870 | 0.700 | 0.396 | 0.023 | 0.002 | -0.001 | -0.007 | -0.002 |
| ρ of ranks | -0.095 | 0.136 | 0.083 | 0.065 | 0.251 | 0.042 | 0.155 | 0.135 | 0.077 | 0.044 |
| P(both > p99) / P(>p99)² | 96.670 | 93.340 | 83.347 | 66.689 | 33.356 | 1.669 | 0.000 | 0.000 | 0.000 | 0.000 |

exceedances of p99 (69672.3 µs): 300, extremal index θ 0.0582 (mean cluster 17.2 heartbeats), clusters separated by ≥ 1 heartbeats = 10000.0 µs
| P(both > p99.9) / P(>p99.9)² | 900.030 | 800.053 | 633.439 | 466.822 | 133.422 | 0.000 | 0.000 | 0.000 | 0.000 | 0.000 |

exceedances of p99.9 (224418.9 µs): 30, extremal index θ 0.0881 (mean cluster 11.4 heartbeats), clusters separated by ≥ 21712 heartbeats = 217120000.0 µs
τ_int 37.2 heartbeats = 372190.3 µs; Bartlett band ±0.0113, values inside from lag Some(55), ranks from None

| window m | windows | Allan dev of mean, µs | white-noise σ/√m, µs | Allan dev of median, µs |
|---|---|---|---|---|
| 1 (10 ms) | 30000 | 1853.93 | 15889.23 | 1853.93 |
| 2 (20 ms) | 15000 | 2704.52 | 11235.38 | 2853.83 |
| 4 (40 ms) | 7500 | 4297.90 | 7944.61 | 4598.56 |
| 8 (80 ms) | 3750 | 6770.78 | 5617.69 | 7870.21 |
| 16 (160 ms) | 1875 | 9509.90 | 3972.31 | 10588.35 |
| 32 (320 ms) | 937 | 11800.18 | 2808.84 | 13402.56 |
| 64 (640 ms) | 468 | 10523.38 | 1986.15 | 9804.31 |
| 128 (1280 ms) | 234 | 8387.34 | 1404.42 | 1047.03 |
| 256 (2560 ms) | 117 | 5951.37 | 993.08 | 427.79 |
| 512 (5120 ms) | 58 | 4336.81 | 702.21 | 587.84 |
| 1024 (10240 ms) | 29 | 3035.45 | 496.54 | 639.54 |

Allan minimum (mean): window 1 = 10 ms, deviation 1853.93 µs; (median): window 256
G (mean lateness of the receiver's own waits) 1678.3 µs; n_G = τ_int·V/G² = 3337

| m | blocks | median sd_m/sd | p05 sd_m/sd |
|---|---|---|---|
| 2 | 15000 | 0.035 | 0.008 |
| 8 | 3750 | 0.057 | 0.022 |
| 32 | 937 | 0.057 | 0.037 |
| 128 | 234 | 0.057 | 0.044 |
| 512 | 58 | 0.060 | 0.046 |

| spacing η | α (quantile) | heartbeats in margin | window | Theorem 7 β (mean/sd) | replayed mistakes / points | verdict |
|---|---|---|---|---|---|---|
| 10000.0 µs | p99 62166.5 µs | 7 | 1 | 2.67e-5 | 2 / 29999 | holds |
| 10000.0 µs | p99.9 216913.1 µs | 22 | 1 | 2.21e-34 | 0 / 29999 | holds |
| 10000.0 µs | p99.99 267996.7 µs | 27 | 1 | 1.48e-46 | 0 / 29999 | holds |
| 10000.0 µs | 2 beats 15000.0 µs | 2 | 1 | 4.81e-1 | 43 / 29999 | holds |
| 10000.0 µs | 3 beats 25000.0 µs | 3 | 1 | 1.38e-1 | 39 / 29999 | holds |
| 10000.0 µs | 5 beats 45000.0 µs | 5 | 1 | 2.62e-3 | 36 / 29999 | holds |
| 10000.0 µs | 10 beats 95000.0 µs | 10 | 1 | 4.51e-10 | 2 / 29999 | **refuted** |
| 10000.0 µs | 20 beats 195000.0 µs | 20 | 1 | 1.87e-29 | 0 / 29999 | holds |
| 20000.0 µs | p99 62166.5 µs | 4 | 1 | 2.54e-3 | 35 / 29998 | holds |
| 20000.0 µs | p99.9 216913.1 µs | 11 | 1 | 3.89e-18 | 0 / 29998 | holds |
| 20000.0 µs | p99.99 267996.7 µs | 14 | 1 | 2.97e-24 | 0 / 29998 | holds |
| 20000.0 µs | 2 beats 30000.0 µs | 2 | 1 | 1.57e-1 | 49 / 29998 | holds |
| 20000.0 µs | 3 beats 50000.0 µs | 3 | 1 | 1.44e-2 | 45 / 29998 | holds |
| 20000.0 µs | 5 beats 90000.0 µs | 5 | 1 | 2.13e-5 | 27 / 29998 | **refuted** |
| 20000.0 µs | 10 beats 190000.0 µs | 10 | 1 | 4.32e-15 | 1 / 29998 | **refuted** |
| 50000.0 µs | p99 62166.5 µs | 2 | 256 | 3.87e-2 | 91 / 29995 | holds |
| 50000.0 µs | p99.9 216913.1 µs | 5 | 256 | 2.19e-8 | 30 / 29995 | **refuted** |
| 50000.0 µs | p99.99 267996.7 µs | 6 | 256 | 6.70e-11 | 0 / 29995 | holds |
| 50000.0 µs | 2 beats 75000.0 µs | 2 | 256 | 1.24e-2 | 92 / 29995 | holds |
| 50000.0 µs | 3 beats 125000.0 µs | 3 | 256 | 1.97e-4 | 96 / 29995 | **refuted** |
| 50000.0 µs | 5 beats 225000.0 µs | 5 | 256 | 8.03e-9 | 22 / 29995 | **refuted** |
| 100000.0 µs | p99.9 216913.1 µs | 3 | 128 | 4.56e-5 | 30 / 29990 | **refuted** |
| 100000.0 µs | p99.99 267996.7 µs | 3 | 128 | 1.62e-6 | 0 / 29990 | holds |
| 100000.0 µs | 2 beats 150000.0 µs | 2 | 128 | 1.02e-3 | 127 / 29990 | **refuted** |
| 100000.0 µs | 3 beats 250000.0 µs | 3 | 128 | 4.12e-6 | 10 / 29990 | **refuted** |
| 200000.0 µs | p99.9 216913.1 µs | 2 | 64 | 2.51e-3 | 30 / 29980 | holds |
| 200000.0 µs | p99.99 267996.7 µs | 2 | 64 | 1.82e-4 | 0 / 29980 | holds |

correlation time (least spacing past every refutation) 200000.0 µs

floors: G 1678.3 µs, correlation time 200000.0 µs, flush stability E[flush]+G 5887.6 µs → floor 200000.0 µs; election inputs: l = 55.3 µs, vote round = 4319.9 µs

| estimate | η | α (at quantile) | window | Theorem 7 β | replayed mistakes / points | replayed rate (95 %) | bound holds |
|---|---|---|---|---|---|---|---|
| mean/sd | 10000.0 µs | 62166.5 µs (p99) | 1 | 2.67e-5 | 2 / 29999 | 6.67e-5 (1.83e-5–2.43e-4) | yes |
| mean/sd | 10000.0 µs | 216913.1 µs (p99.9) | 1 | 2.21e-34 | 0 / 29999 | 0.00e0 (0.00e0–1.28e-4) | yes |
| mean/sd | 10000.0 µs | 267996.7 µs (p99.99) | 1 | 1.48e-46 | 0 / 29999 | 0.00e0 (0.00e0–1.28e-4) | yes |
| mean/sd | 200000.0 µs | 62166.5 µs (p99) | 64 | 6.13e-2 | 294 / 29980 | 9.81e-3 (8.75e-3–1.10e-2) | yes |
| mean/sd | 200000.0 µs | 216913.1 µs (p99.9) | 64 | 2.51e-3 | 30 / 29980 | 1.00e-3 (7.01e-4–1.43e-3) | yes |
| mean/sd | 200000.0 µs | 267996.7 µs (p99.99) | 64 | 1.82e-4 | 0 / 29980 | 0.00e0 (0.00e0–1.28e-4) | yes |
| median/1.4826·MAD | 10000.0 µs | 1545.8 µs (p90) | 1 | 2.57e-1 | 4747 / 29999 | 1.58e-1 (1.54e-1–1.63e-1) | yes |
| median/1.4826·MAD | 10000.0 µs | 63979.1 µs (p99) | 1 | 6.56e-21 | 2 / 29999 | 6.67e-5 (1.83e-5–2.43e-4) | **no** |
| median/1.4826·MAD | 10000.0 µs | 218725.7 µs (p99.9) | 1 | 1.22e-85 | 0 / 29999 | 0.00e0 (0.00e0–1.28e-4) | yes |
| median/1.4826·MAD | 10000.0 µs | 269809.3 µs (p99.99) | 1 | 1.41e-108 | 0 / 29999 | 0.00e0 (0.00e0–1.28e-4) | yes |
| median/1.4826·MAD | 200000.0 µs | 1545.8 µs (p90) | 64 | 2.57e-1 | 2119 / 29980 | 7.07e-2 (6.77e-2–7.38e-2) | yes |
| median/1.4826·MAD | 200000.0 µs | 63979.1 µs (p99) | 64 | 2.19e-4 | 295 / 29980 | 9.84e-3 (8.78e-3–1.10e-2) | **no** |
| median/1.4826·MAD | 200000.0 µs | 218725.7 µs (p99.9) | 64 | 8.06e-8 | 30 / 29980 | 1.00e-3 (7.01e-4–1.43e-3) | **no** |
| median/1.4826·MAD | 200000.0 µs | 269809.3 µs (p99.99) | 64 | 5.23e-9 | 0 / 29980 | 0.00e0 (0.00e0–1.28e-4) | yes |

| voters/up | span W | T_E | split |
|---|---|---|---|
| 3/3 | 1678.3 µs | 4758.6 µs | 0.003 |
| 3/2 | 1678.3 µs | 5294.6 µs | 0.065 |
| 5/5 | 1678.3 µs | 4601.6 µs | 0.000 |
| 5/4 | 1678.3 µs | 4693.1 µs | 0.006 |

| estimate | MTBF | η | α | window | detection bound | T_MR bound | U | replayed mistakes / points | replayed T_MR | bound holds | suspected share |
|---|---|---|---|---|---|---|---|---|---|---|---|
| mean/sd | 3600.000 s | 200000.0 µs | 238931.7 µs | 64 (n_G 155, n_Allan 64) | 446437.6 µs | 316.901 s | 1.422e-4 | 13 / 29980 | 461.538 s | yes | 4.31e-5 |
| mean/sd | 1.0 d | 200000.0 µs | 389320.3 µs | 64 (n_G 155, n_Allan 64) | 596826.1 µs | 16984.329 s | 7.281e-6 | 0 / 29980 | > 6000.000 s | yes | 0.00e0 |
| mean/sd | 30.0 d | 200000.0 µs | 437619.7 µs | 64 (n_G 155, n_Allan 64) | 645125.5 µs | 2.6 d | 2.748e-7 | 0 / 29980 | > 6000.000 s | yes | 0.00e0 |
| mean/sd | 365.0 d | 200000.0 µs | 530318.2 µs | 64 (n_G 155, n_Allan 64) | 737824.0 µs | 74.3 d | 2.439e-8 | 0 / 29980 | > 6000.000 s | yes | 0.00e0 |
| median/1.4826·MAD | 3600.000 s | 200000.0 µs | 60481.8 µs | 64 (n_G 155, n_Allan 64) | 266175.0 µs | 823.380 s | 8.184e-5 | 298 / 29980 | 20.134 s | **no** | 4.37e-3 |
| median/1.4826·MAD | 1.0 d | 200000.0 µs | 234387.3 µs | 64 (n_G 155, n_Allan 64) | 440080.5 µs | 101.9 d | 5.155e-6 | 14 / 29980 | 428.571 s | **no** | 5.37e-5 |
| median/1.4826·MAD | 30.0 d | 200000.0 µs | 229083.7 µs | 64 (n_G 155, n_Allan 64) | 434776.9 µs | 71.8 d | 1.706e-7 | 20 / 29980 | 300.000 s | **no** | 6.90e-5 |
| median/1.4826·MAD | 365.0 d | 200000.0 µs | 256936.6 µs | 64 (n_G 155, n_Allan 64) | 462629.8 µs | 291.5 d | 1.505e-8 | 8 / 29980 | 750.000 s | **no** | 1.09e-5 |

### Linux (Docker Desktop VM), 2 ms

linux aarch64, η = 2000 µs, flush false, 300.004 s, load 6.41 5.26 4.93 → 6.21 5.81 5.25

sent 150000, received 150000, lost 0, reordered 0; p_L (Jeffreys mean) 3.333e-6

| series | n | mean ± 95 % | sd (95 %) | median (95 %) | MAD (95 %) | p99 | p99.9 | p99.99 | max | τ_int |
|---|---|---|---|---|---|---|---|---|---|---|
| D (kernel stamp − σ) | 150000 | 991.7 ± 2.7 | 254.9 (207.4–294.9) | 1066.8 (1065.5–1068.0) | 97.9 (94.8–101.1) | 1380.6 | 1430.6 | 7060.7 | 21673.3 | 4.3 |
| D (process read − σ) | 150000 | 1025.7 ± 2.8 | 256.2 (200.3–302.0) | 1090.4 (1089.1–1091.6) | 98.7 (95.8–101.7) | 1418.4 | 1492.4 | 7218.2 | 22939.9 | 4.7 |
| send → kernel | 150000 | 4.8 ± 0.3 | 2.2 (1.0–2.9) | 4.6 (4.4–4.6) | 1.0 (0.8–1.3) | 11.8 | 23.6 | 43.0 | 58.4 | 561.6 |
| kernel → read (gap) | 150000 | 34.0 ± 0.4 | 26.2 (0.0–45.1) | 33.0 (32.8–33.3) | 9.5 (9.5–9.7) | 77.8 | 154.1 | 881.4 | 5987.8 | 8.3 |
| sender timer lateness | 149942 | 985.6 ± 1.6 | 226.9 (205.3–246.7) | 1062.6 (1061.8–1063.4) | 97.6 (95.5–99.8) | 1374.9 | 1418.5 | 2866.4 | 21665.8 | 1.9 |
| sender write+flush | 150000 | 0.0 ± 0.0 | 0.1 (0.0–0.1) | 0.0 (0.0–0.0) | 0.0 (0.0–0.0) | 0.1 | 0.2 | 2.2 | 16.0 | 1.3 |
| receiver wait lateness | 110437 | 965.4 ± 1.3 | 225.6 (207.7–242.1) | 1050.7 (1049.8–1051.7) | 95.3 (93.3–97.1) | 1352.6 | 1396.4 | 2461.6 | 21614.7 | 1.0 |

sender behind its schedule (no wait) 58; receiver waits asked median 903.6 µs, lateness/asked median 1.132

| lag (heartbeats) | 1 | 2 | 5 | 10 | 20 | 50 | 100 | 200 | 500 | 1000 | 2000 | 5000 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| ρ of D | 0.183 | 0.326 | 0.130 | 0.031 | 0.020 | 0.028 | 0.014 | 0.009 | 0.040 | 0.037 | 0.038 | 0.045 |
| ρ of ranks | -0.282 | 0.285 | 0.012 | 0.029 | 0.023 | 0.021 | 0.020 | 0.009 | 0.043 | 0.042 | 0.037 | 0.043 |
| P(both > p99) / P(>p99)² | 2.667 | 2.933 | 2.000 | 0.933 | 0.933 | 1.134 | 0.667 | 1.001 | 1.271 | 1.208 | 0.405 | 0.897 |

exceedances of p99 (1380.6 µs): 1500, extremal index θ 0.9867 (mean cluster 1.0 heartbeats), clusters separated by ≥ 1 heartbeats = 2000.0 µs
| P(both > p99.9) / P(>p99.9)² | 253.335 | 193.336 | 120.004 | 26.668 | 0.000 | 13.338 | 6.671 | 6.676 | 0.000 | 6.711 | 0.000 | 0.000 |

exceedances of p99.9 (1430.6 µs): 150, extremal index θ 0.5390 (mean cluster 1.9 heartbeats), clusters separated by ≥ 235 heartbeats = 470000.0 µs
τ_int 4.3 heartbeats = 8595.5 µs; Bartlett band ±0.0051, values inside from lag Some(9372), ranks from Some(9372)

| window m | windows | Allan dev of mean, µs | white-noise σ/√m, µs | Allan dev of median, µs |
|---|---|---|---|---|
| 1 (2 ms) | 150000 | 230.37 | 254.95 | 230.37 |
| 2 (4 ms) | 75000 | 152.61 | 180.27 | 183.44 |
| 4 (8 ms) | 37500 | 132.63 | 127.47 | 158.05 |
| 8 (16 ms) | 18750 | 127.87 | 90.14 | 144.82 |
| 16 (32 ms) | 9375 | 107.28 | 63.74 | 101.12 |
| 32 (64 ms) | 4687 | 77.64 | 45.07 | 55.92 |
| 64 (128 ms) | 2343 | 58.54 | 31.87 | 47.71 |
| 128 (256 ms) | 1171 | 42.82 | 22.53 | 42.47 |
| 256 (512 ms) | 585 | 32.40 | 15.93 | 35.27 |
| 512 (1024 ms) | 292 | 26.63 | 11.27 | 32.74 |
| 1024 (2048 ms) | 146 | 21.57 | 7.97 | 17.78 |
| 2048 (4096 ms) | 73 | 17.45 | 5.63 | 11.72 |
| 4096 (8192 ms) | 36 | 15.21 | 3.98 | 8.85 |
| 8192 (16384 ms) | 18 | 10.29 | 2.82 | 8.21 |

Allan minimum (mean): window 8192 = 16384 ms, deviation 10.29 µs; (median): window 4096
G (mean lateness of the receiver's own waits) 965.4 µs; n_G = τ_int·V/G² = 1

| m | blocks | median sd_m/sd | p05 sd_m/sd |
|---|---|---|---|
| 2 | 75000 | 0.459 | 0.014 |
| 8 | 18750 | 0.779 | 0.079 |
| 32 | 4687 | 0.813 | 0.502 |
| 128 | 1171 | 0.814 | 0.663 |
| 512 | 292 | 0.822 | 0.739 |
| 2048 | 73 | 0.831 | 0.779 |

| spacing η | α (quantile) | heartbeats in margin | window | Theorem 7 β (mean/sd) | replayed mistakes / points | verdict |
|---|---|---|---|---|---|---|
| 2000.0 µs | p99.99 6069.0 µs | 4 | 1 | 9.63e-8 | 3 / 149999 | **refuted** |
| 2000.0 µs | 2 beats 3000.0 µs | 2 | 1 | 4.38e-4 | 11 / 149999 | holds |
| 2000.0 µs | 3 beats 5000.0 µs | 3 | 1 | 1.14e-6 | 5 / 149999 | **refuted** |
| 2000.0 µs | 5 beats 9000.0 µs | 5 | 1 | 1.22e-12 | 2 / 149999 | **refuted** |
| 2000.0 µs | 10 beats 19000.0 µs | 10 | 1 | 3.11e-30 | 1 / 149999 | **refuted** |
| 4000.0 µs | p99.99 6069.0 µs | 2 | 1 | 2.64e-5 | 5 / 149998 | holds |
| 4000.0 µs | 2 beats 6000.0 µs | 2 | 1 | 2.89e-5 | 5 / 149998 | holds |
| 4000.0 µs | 3 beats 10000.0 µs | 3 | 1 | 1.89e-8 | 4 / 149998 | **refuted** |
| 4000.0 µs | 5 beats 18000.0 µs | 5 | 1 | 1.29e-15 | 2 / 149998 | **refuted** |
| 10000.0 µs | 2 beats 15000.0 µs | 2 | 1 | 7.58e-7 | 2 / 149995 | **refuted** |

correlation time (least spacing past every refutation) 20000.0 µs

floors: G 965.4 µs, correlation time 20000.0 µs, flush stability E[flush]+G 0.0 µs → floor 20000.0 µs; election inputs: l = 38.8 µs, vote round = 77.6 µs

| estimate | η | α (at quantile) | window | Theorem 7 β | replayed mistakes / points | replayed rate (95 %) | bound holds |
|---|---|---|---|---|---|---|---|
| mean/sd | 2000.0 µs | 227.8 µs (p90) | 1 | 5.56e-1 | 28443 / 149999 | 1.90e-1 (1.87e-1–1.92e-1) | yes |
| mean/sd | 2000.0 µs | 388.9 µs (p99) | 1 | 3.01e-1 | 17429 / 149999 | 1.16e-1 (1.14e-1–1.18e-1) | yes |
| mean/sd | 2000.0 µs | 438.9 µs (p99.9) | 1 | 2.52e-1 | 14708 / 149999 | 9.81e-2 (9.65e-2–9.97e-2) | yes |
| mean/sd | 2000.0 µs | 6069.0 µs (p99.99) | 1 | 9.63e-8 | 3 / 149999 | 2.00e-5 (6.80e-6–5.88e-5) | **no** |
| mean/sd | 20000.0 µs | 227.8 µs (p90) | 1 | 5.56e-1 | 31296 / 149990 | 2.09e-1 (2.06e-1–2.11e-1) | yes |
| mean/sd | 20000.0 µs | 388.9 µs (p99) | 1 | 3.01e-1 | 14563 / 149990 | 9.71e-2 (9.55e-2–9.87e-2) | yes |
| mean/sd | 20000.0 µs | 438.9 µs (p99.9) | 1 | 2.52e-1 | 11453 / 149990 | 7.64e-2 (7.50e-2–7.78e-2) | yes |
| mean/sd | 20000.0 µs | 6069.0 µs (p99.99) | 1 | 1.76e-3 | 14 / 149990 | 9.33e-5 (5.56e-5–1.57e-4) | yes |
| median/1.4826·MAD | 2000.0 µs | 152.7 µs (p90) | 1 | 4.75e-1 | 34195 / 149999 | 2.28e-1 (2.26e-1–2.30e-1) | yes |
| median/1.4826·MAD | 2000.0 µs | 313.8 µs (p99) | 1 | 1.76e-1 | 23090 / 149999 | 1.54e-1 (1.52e-1–1.56e-1) | yes |
| median/1.4826·MAD | 2000.0 µs | 363.8 µs (p99.9) | 1 | 1.37e-1 | 19185 / 149999 | 1.28e-1 (1.26e-1–1.30e-1) | yes |
| median/1.4826·MAD | 2000.0 µs | 5993.9 µs (p99.99) | 1 | 4.12e-9 | 3 / 149999 | 2.00e-5 (6.80e-6–5.88e-5) | **no** |
| median/1.4826·MAD | 20000.0 µs | 152.7 µs (p90) | 1 | 4.75e-1 | 41530 / 149990 | 2.77e-1 (2.74e-1–2.80e-1) | yes |
| median/1.4826·MAD | 20000.0 µs | 313.8 µs (p99) | 1 | 1.76e-1 | 21053 / 149990 | 1.40e-1 (1.38e-1–1.42e-1) | yes |
| median/1.4826·MAD | 20000.0 µs | 363.8 µs (p99.9) | 1 | 1.37e-1 | 16461 / 149990 | 1.10e-1 (1.08e-1–1.11e-1) | yes |
| median/1.4826·MAD | 20000.0 µs | 5993.9 µs (p99.99) | 1 | 5.90e-4 | 14 / 149990 | 9.33e-5 (5.56e-5–1.57e-4) | yes |

| voters/up | span W | T_E | split |
|---|---|---|---|
| 3/3 | 965.4 µs | 323.8 µs | 0.005 |
| 3/2 | 965.4 µs | 488.4 µs | 0.079 |
| 5/5 | 965.4 µs | 239.1 µs | 0.001 |
| 5/4 | 965.4 µs | 280.3 µs | 0.009 |

| estimate | MTBF | η | α | window | detection bound | T_MR bound | U | replayed mistakes / points | replayed T_MR | bound holds | suspected share |
|---|---|---|---|---|---|---|---|---|---|---|---|
| mean/sd | 3600.000 s | 20000.0 µs | 21411.9 µs | 1 (n_G 1, n_Allan 512) | 42403.6 µs | 4365.362 s | 1.203e-5 | 0 / 149990 | > 3000.000 s | yes | 0.00e0 |
| mean/sd | 1.0 d | 20000.0 µs | 26956.1 µs | 1 (n_G 1, n_Allan 512) | 47947.8 µs | 1.9 d | 5.637e-7 | 0 / 149990 | > 3000.000 s | yes | 0.00e0 |
| mean/sd | 30.0 d | 20000.0 µs | 27949.4 µs | 1 (n_G 1, n_Allan 512) | 48941.0 µs | 2.6 d | 2.125e-8 | 0 / 149990 | > 3000.000 s | yes | 0.00e0 |
| mean/sd | 365.0 d | 20000.0 µs | 40989.2 µs | 1 (n_G 1, n_Allan 512) | 61980.9 µs | 586.3 d | 1.991e-9 | 0 / 149990 | > 3000.000 s | yes | 0.00e0 |
| median/1.4826·MAD | 3600.000 s | 20000.0 µs | 21945.4 µs | 1 (n_G 1, n_Allan 512) | 43012.2 µs | 76619.760 s | 1.209e-5 | 0 / 149990 | > 3000.000 s | yes | 0.00e0 |
| median/1.4826·MAD | 1.0 d | 20000.0 µs | 23311.0 µs | 1 (n_G 1, n_Allan 512) | 44377.8 µs | 2.9 d | 5.213e-7 | 0 / 149990 | > 3000.000 s | yes | 0.00e0 |
| median/1.4826·MAD | 30.0 d | 20000.0 µs | 24917.6 µs | 1 (n_G 1, n_Allan 512) | 45984.4 µs | 7.1 d | 1.873e-8 | 0 / 149990 | > 3000.000 s | yes | 0.00e0 |
| median/1.4826·MAD | 365.0 d | 20000.0 µs | 31665.7 µs | 1 (n_G 1, n_Allan 512) | 52732.5 µs | 60.1 d | 1.782e-9 | 0 / 149990 | > 3000.000 s | yes | 0.00e0 |

### Linux (Docker Desktop VM), write and fdatasync, 2 ms

linux aarch64, η = 2000 µs, flush true, 600.004 s, load 6.21 5.81 5.25 → 4.39 5.14 5.21

sent 300000, received 300000, lost 0, reordered 0; p_L (Jeffreys mean) 1.667e-6

| series | n | mean ± 95 % | sd (95 %) | median (95 %) | MAD (95 %) | p99 | p99.9 | p99.99 | max | τ_int |
|---|---|---|---|---|---|---|---|---|---|---|
| D (kernel stamp − σ) | 300000 | 1360.5 ± 281.4 | 3791.8 (0.0–6227.2) | 1142.7 (1125.0–1155.8) | 116.6 (103.7–130.9) | 5738.8 | 70079.4 | 118960.4 | 139750.8 | 430.1 |
| D (process read − σ) | 300000 | 1387.9 ± 281.9 | 3792.5 (0.0–6231.0) | 1166.5 (1148.8–1179.4) | 118.5 (105.9–132.3) | 5767.9 | 70100.7 | 118980.9 | 139803.6 | 431.5 |
| send → kernel | 300000 | 5.3 ± 0.4 | 1.6 (0.0–3.3) | 5.4 (4.9–5.6) | 0.8 (0.5–1.0) | 9.5 | 20.0 | 36.1 | 100.4 | 5942.4 |
| kernel → read (gap) | 300000 | 27.3 ± 0.2 | 54.4 (0.0–86.4) | 24.1 (23.8–24.1) | 3.3 (3.3–3.6) | 63.5 | 90.4 | 294.7 | 22467.8 | 1.3 |
| sender timer lateness | 293574 | 881.1 ± 1.4 | 218.9 (184.2–248.8) | 944.2 (942.8–945.5) | 107.5 (106.3–108.5) | 1249.2 | 1318.0 | 2754.1 | 28930.8 | 3.0 |
| sender write+flush | 300000 | 230.0 ± 19.1 | 559.9 (0.0–1362.1) | 186.0 (185.0–187.0) | 17.4 (16.5–18.2) | 563.2 | 5874.7 | 7421.6 | 138727.1 | 91.3 |
| receiver wait lateness | 251834 | 899.0 ± 1.4 | 364.0 (0.0–529.1) | 952.4 (951.9–952.9) | 88.8 (88.1–89.4) | 1253.0 | 1436.2 | 3320.2 | 137136.2 | 1.0 |

sender behind its schedule (no wait) 6426; receiver waits asked median 822.5 µs, lateness/asked median 1.147

| lag (heartbeats) | 1 | 2 | 5 | 10 | 20 | 50 | 100 | 200 | 500 | 1000 | 2000 | 5000 | 10000 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| ρ of D | 0.986 | 0.978 | 0.955 | 0.913 | 0.830 | 0.642 | 0.519 | 0.220 | 0.057 | 0.078 | 0.028 | -0.004 | 0.000 |
| ρ of ranks | 0.002 | 0.294 | 0.125 | 0.113 | 0.095 | 0.091 | 0.068 | 0.051 | 0.069 | 0.059 | 0.054 | 0.058 | 0.048 |
| P(both > p99) / P(>p99)² | 75.300 | 73.967 | 70.001 | 65.102 | 59.737 | 51.642 | 47.716 | 37.358 | 23.372 | 14.181 | 3.658 | 0.475 | 1.552 |

exceedances of p99 (5738.8 µs): 3000, extremal index θ 0.0435 (mean cluster 23.0 heartbeats), clusters separated by ≥ 619 heartbeats = 1238000.0 µs
| P(both > p99.9) / P(>p99.9)² | 980.003 | 966.673 | 916.682 | 833.361 | 666.711 | 376.729 | 303.434 | 0.000 | 0.000 | 0.000 | 0.000 | 0.000 | 0.000 |

exceedances of p99.9 (70079.4 µs): 300, extremal index θ 0.0156 (mean cluster 64.1 heartbeats), clusters separated by ≥ 9553 heartbeats = 19106000.0 µs
τ_int 430.1 heartbeats = 860163.2 µs; Bartlett band ±0.0036, values inside from lag None, ranks from Some(1)

| window m | windows | Allan dev of mean, µs | white-noise σ/√m, µs | Allan dev of median, µs |
|---|---|---|---|---|
| 1 (2 ms) | 300000 | 452.93 | 3791.81 | 452.93 |
| 2 (4 ms) | 150000 | 459.52 | 2681.21 | 540.50 |
| 4 (8 ms) | 75000 | 545.55 | 1895.90 | 675.43 |
| 8 (16 ms) | 37500 | 778.68 | 1340.61 | 910.78 |
| 16 (32 ms) | 18750 | 1085.86 | 947.95 | 1278.97 |
| 32 (64 ms) | 9375 | 1423.89 | 670.30 | 1594.15 |
| 64 (128 ms) | 4687 | 1825.27 | 473.98 | 2022.94 |
| 128 (256 ms) | 2343 | 2023.59 | 335.15 | 1952.84 |
| 256 (512 ms) | 1171 | 1938.94 | 236.99 | 1991.97 |
| 512 (1024 ms) | 585 | 1636.51 | 167.58 | 962.08 |
| 1024 (2048 ms) | 292 | 1569.53 | 118.49 | 768.97 |
| 2048 (4096 ms) | 146 | 1361.19 | 83.79 | 282.35 |
| 4096 (8192 ms) | 73 | 1144.64 | 59.25 | 21.94 |
| 8192 (16384 ms) | 36 | 762.44 | 41.89 | 18.25 |
| 16384 (32768 ms) | 18 | 543.10 | 29.62 | 18.23 |

Allan minimum (mean): window 1 = 2 ms, deviation 452.93 µs; (median): window 8192
G (mean lateness of the receiver's own waits) 899.0 µs; n_G = τ_int·V/G² = 7651

| m | blocks | median sd_m/sd | p05 sd_m/sd |
|---|---|---|---|
| 2 | 150000 | 0.028 | 0.002 |
| 8 | 37500 | 0.048 | 0.009 |
| 32 | 9375 | 0.051 | 0.030 |
| 128 | 2343 | 0.053 | 0.039 |
| 512 | 585 | 0.067 | 0.045 |
| 2048 | 146 | 0.088 | 0.048 |

| spacing η | α (quantile) | heartbeats in margin | window | Theorem 7 β (mean/sd) | replayed mistakes / points | verdict |
|---|---|---|---|---|---|---|
| 2000.0 µs | p99 4378.3 µs | 3 | 1 | 3.05e-1 | 576 / 299999 | holds |
| 2000.0 µs | p99.9 68718.9 µs | 35 | 1 | 3.82e-61 | 2 / 299999 | **refuted** |
| 2000.0 µs | p99.99 117599.9 µs | 59 | 1 | 4.21e-129 | 2 / 299999 | **refuted** |
| 2000.0 µs | 2 beats 3000.0 µs | 2 | 1 | 5.75e-1 | 1595 / 299999 | holds |
| 2000.0 µs | 3 beats 5000.0 µs | 3 | 1 | 2.10e-1 | 344 / 299999 | holds |
| 2000.0 µs | 5 beats 9000.0 µs | 5 | 1 | 7.18e-3 | 29 / 299999 | holds |
| 2000.0 µs | 10 beats 19000.0 µs | 10 | 1 | 6.52e-9 | 10 / 299999 | **refuted** |
| 2000.0 µs | 20 beats 39000.0 µs | 20 | 1 | 8.65e-27 | 4 / 299999 | **refuted** |
| 4000.0 µs | p99 4378.3 µs | 2 | 1 | 4.24e-1 | 580 / 299998 | holds |
| 4000.0 µs | p99.9 68718.9 µs | 18 | 1 | 1.47e-31 | 4 / 299998 | **refuted** |
| 4000.0 µs | p99.99 117599.9 µs | 30 | 1 | 1.17e-65 | 4 / 299998 | **refuted** |
| 4000.0 µs | 2 beats 6000.0 µs | 2 | 1 | 2.23e-1 | 153 / 299998 | holds |
| 4000.0 µs | 3 beats 10000.0 µs | 3 | 1 | 2.81e-2 | 46 / 299998 | holds |
| 4000.0 µs | 5 beats 18000.0 µs | 5 | 1 | 8.15e-5 | 21 / 299998 | holds |
| 4000.0 µs | 10 beats 38000.0 µs | 10 | 1 | 9.33e-14 | 8 / 299998 | **refuted** |
| 4000.0 µs | 20 beats 78000.0 µs | 20 | 1 | 1.35e-37 | 4 / 299998 | **refuted** |
| 10000.0 µs | p99.9 68718.9 µs | 7 | 1534 | 7.76e-14 | 53 / 299995 | **refuted** |
| 10000.0 µs | p99.99 117599.9 µs | 12 | 1534 | 1.23e-27 | 13 / 299995 | **refuted** |
| 10000.0 µs | 2 beats 15000.0 µs | 2 | 1534 | 2.19e-2 | 425 / 299995 | holds |
| 10000.0 µs | 3 beats 25000.0 µs | 3 | 1534 | 4.93e-4 | 255 / 299995 | **refuted** |
| 10000.0 µs | 5 beats 45000.0 µs | 5 | 1534 | 4.04e-8 | 81 / 299995 | **refuted** |
| 10000.0 µs | 10 beats 95000.0 µs | 10 | 1534 | 5.23e-21 | 76 / 299995 | **refuted** |
| 20000.0 µs | p99.9 68718.9 µs | 4 | 777 | 4.99e-8 | 130 / 299990 | **refuted** |
| 20000.0 µs | p99.99 117599.9 µs | 6 | 777 | 7.21e-15 | 29 / 299990 | **refuted** |
| 20000.0 µs | 2 beats 30000.0 µs | 2 | 777 | 1.98e-3 | 252 / 299990 | holds |
| 20000.0 µs | 3 beats 50000.0 µs | 3 | 777 | 1.13e-5 | 194 / 299990 | **refuted** |
| 20000.0 µs | 5 beats 90000.0 µs | 5 | 777 | 5.87e-11 | 114 / 299990 | **refuted** |
| 40000.0 µs | p99.9 68718.9 µs | 2 | 386 | 5.20e-5 | 251 / 299980 | **refuted** |
| 40000.0 µs | p99.99 117599.9 µs | 3 | 386 | 2.50e-8 | 30 / 299980 | **refuted** |
| 40000.0 µs | 2 beats 60000.0 µs | 2 | 386 | 1.38e-4 | 268 / 299980 | **refuted** |
| 40000.0 µs | 3 beats 100000.0 µs | 3 | 386 | 1.98e-7 | 101 / 299980 | **refuted** |
| 100000.0 µs | p99.99 117599.9 µs | 2 | 167 | 4.61e-5 | 30 / 299950 | **refuted** |

correlation time (least spacing past every refutation) 200000.0 µs

floors: G 899.0 µs, correlation time 200000.0 µs, flush stability E[flush]+G 1129.1 µs → floor 200000.0 µs; election inputs: l = 32.6 µs, vote round = 295.1 µs

| estimate | η | α (at quantile) | window | Theorem 7 β | replayed mistakes / points | replayed rate (95 %) | bound holds |
|---|---|---|---|---|---|---|---|
| mean/sd | 2000.0 µs | 4378.3 µs (p99) | 1 | 3.05e-1 | 576 / 299999 | 1.92e-3 (1.77e-3–2.08e-3) | yes |
| mean/sd | 2000.0 µs | 68718.9 µs (p99.9) | 1 | 3.82e-61 | 2 / 299999 | 6.67e-6 (1.83e-6–2.43e-5) | **no** |
| mean/sd | 2000.0 µs | 117599.9 µs (p99.99) | 1 | 4.21e-129 | 2 / 299999 | 6.67e-6 (1.83e-6–2.43e-5) | **no** |
| mean/sd | 200000.0 µs | 4378.3 µs (p99) | 77 | 4.29e-1 | 2761 / 299900 | 9.21e-3 (8.87e-3–9.56e-3) | yes |
| mean/sd | 200000.0 µs | 68718.9 µs (p99.9) | 77 | 3.04e-3 | 298 / 299900 | 9.94e-4 (8.87e-4–1.11e-3) | yes |
| mean/sd | 200000.0 µs | 117599.9 µs (p99.99) | 77 | 1.04e-3 | 30 / 299900 | 1.00e-4 (7.01e-5–1.43e-4) | yes |
| median/1.4826·MAD | 2000.0 µs | 167.4 µs (p90) | 1 | 5.16e-1 | 66121 / 299999 | 2.20e-1 (2.19e-1–2.22e-1) | yes |
| median/1.4826·MAD | 2000.0 µs | 4596.1 µs (p99) | 1 | 4.86e-7 | 497 / 299999 | 1.66e-3 (1.52e-3–1.81e-3) | **no** |
| median/1.4826·MAD | 2000.0 µs | 68936.7 µs (p99.9) | 1 | 1.02e-151 | 2 / 299999 | 6.67e-6 (1.83e-6–2.43e-5) | **no** |
| median/1.4826·MAD | 2000.0 µs | 117817.7 µs (p99.99) | 1 | 1.64e-280 | 2 / 299999 | 6.67e-6 (1.83e-6–2.43e-5) | **no** |
| median/1.4826·MAD | 200000.0 µs | 167.4 µs (p90) | 77 | 5.16e-1 | 36432 / 299900 | 1.21e-1 (1.20e-1–1.23e-1) | yes |
| median/1.4826·MAD | 200000.0 µs | 4596.1 µs (p99) | 77 | 1.42e-3 | 2990 / 299900 | 9.97e-3 (9.62e-3–1.03e-2) | **no** |
| median/1.4826·MAD | 200000.0 µs | 68936.7 µs (p99.9) | 77 | 7.96e-6 | 300 / 299900 | 1.00e-3 (8.93e-4–1.12e-3) | **no** |
| median/1.4826·MAD | 200000.0 µs | 117817.7 µs (p99.99) | 77 | 3.82e-6 | 30 / 299900 | 1.00e-4 (7.01e-5–1.43e-4) | **no** |

| voters/up | span W | T_E | split |
|---|---|---|---|
| 3/3 | 899.0 µs | 524.5 µs | 0.004 |
| 3/2 | 899.0 µs | 686.2 µs | 0.071 |
| 5/5 | 899.0 µs | 445.5 µs | 0.000 |
| 5/4 | 899.0 µs | 484.0 µs | 0.007 |

| estimate | MTBF | η | α | window | detection bound | T_MR bound | U | replayed mistakes / points | replayed T_MR | bound holds | suspected share |
|---|---|---|---|---|---|---|---|---|---|---|---|
| mean/sd | 3600.000 s | 200000.0 µs | 59308.4 µs | 77 (n_G 77, n_Allan 128) | 260668.9 µs | 49.109 s | 8.657e-5 | 383 / 299900 | 156.658 s | yes | 1.77e-4 |
| mean/sd | 1.0 d | 200000.0 µs | 240345.9 µs | 77 (n_G 77, n_Allan 128) | 441706.5 µs | 1.1 d | 5.128e-6 | 0 / 299900 | > 60000.000 s | yes | 0.00e0 |
| mean/sd | 30.0 d | 200000.0 µs | 248840.3 µs | 77 (n_G 77, n_Allan 128) | 450200.9 µs | 1.7 d | 1.788e-7 | 0 / 299900 | > 60000.000 s | yes | 0.00e0 |
| mean/sd | 365.0 d | 200000.0 µs | 274040.4 µs | 77 (n_G 77, n_Allan 128) | 475400.9 µs | 4.6 d | 1.683e-8 | 0 / 299900 | > 60000.000 s | yes | 0.00e0 |
| median/1.4826·MAD | 3600.000 s | 200000.0 µs | 58630.4 µs | 77 (n_G 77, n_Allan 128) | 259773.2 µs | 19294.629 s | 7.239e-5 | 404 / 299900 | 148.515 s | **no** | 1.92e-4 |
| median/1.4826·MAD | 1.0 d | 200000.0 µs | 58730.5 µs | 77 (n_G 77, n_Allan 128) | 259873.2 µs | 19349.919 s | 3.051e-6 | 403 / 299900 | 148.883 s | **no** | 1.91e-4 |
| median/1.4826·MAD | 30.0 d | 200000.0 µs | 99927.6 µs | 77 (n_G 77, n_Allan 128) | 301070.4 µs | 42906.611 s | 1.324e-7 | 120 / 299900 | 500.000 s | **no** | 2.35e-5 |
| median/1.4826·MAD | 365.0 d | 200000.0 µs | 216315.9 µs | 77 (n_G 77, n_Allan 128) | 417458.7 µs | 8807.9 d | 1.326e-8 | 0 / 299900 | > 60000.000 s | yes | 0.00e0 |

## Commands for the traces

```sh
cd crates/hyper-timing-trace && cargo build --release
B=target/release/hyper-timing-trace
$B timer <dir>/flush.log                        # the sweeps
$B run <dir>/mac-plain 100 300                  # interval µs, seconds
$B run <dir>/mac-flush 10000 600 <dir>/flush.log
$B analyse <dir>/mac-plain <dir>/mac-flush      # the tables above
# The loaded runs: the same, while a loop rebuilds the workspace from clean in another target:
#   while :; do CARGO_TARGET_DIR=<dir>/load CARGO_BUILD_JOBS=4 cargo build --workspace --all-targets; rm -rf <dir>/load; done
# Linux, from the repository root:
docker run --rm -v "$PWD":/work -w /work/crates/hyper-timing-trace \
  -v trace-target:/work/crates/hyper-timing-trace/target -v trace-disk:/disk rust:1.98.0 bash -c \
  'cargo build --release && B=target/release/hyper-timing-trace &&
   $B timer /disk/flush.log && $B run /disk/linux-plain 2000 300 &&
   $B run /disk/linux-flush 2000 600 /disk/flush.log && $B analyse /disk/linux-plain /disk/linux-flush'
```

# hyper-transport against focal-wire's core

The law (`CLAUDE.md` §1a) for the application layer over hyper-quic: its allocations and
reallocations per exchange and per lane frame, measured against bare hyper-quic streams to isolate
what the layer adds, and its cost against the core it replaces, focal-wire's, on one workload.

## The machine

The same Apple M5 Max (18 cores, 128 GiB), macOS 26.4.1, rustc 1.98.0, 2026-10-01 between 13:16
and 13:40 PDT, shared with other sessions building and testing: the load average was 22 to 27
during every run of the allocation bench and the end-to-end scenarios of hyper-transport. Allocation
counts are exact whatever the load. The comparison against focal-wire was run later, at the load
its section states.

## Allocations

`cargo bench -p hyper-transport --bench allocs`: two endpoints over the in-memory network of
`tests/common`, which reuses its datagram buffers, so the counts are the endpoints' own, both sides,
hyper-quic and hyper-tls included. 2,000 exchanges after 200 (500 for 64 KiB). The bare rows are the
same exchanges written directly on hyper-quic streams, with the same heads and bodies in one write
each way.

| Workload | Allocations | Reallocations | Bytes | Time (in memory) |
|---|---|---|---|---|
| hyper-transport exchange, 16 B heads, no body | 8.35 | 0 | 1,110 | 3.2 µs |
| bare hyper-quic stream, 16 B each way | 8.08 | 0 | 1,009 | 2.1 µs |
| hyper-transport exchange, 16 B heads, 4 KiB bodies | 21.0 (12.4 since) | 2.33 (0 since) | 17,760 (9,313 since) | 11.5 µs |
| bare hyper-quic stream, 4 KiB + 16 B each way | 16.2 | 2.08 | 17,493 | 8.6 µs |
| hyper-transport exchange, 16 B heads, 64 KiB bodies | 211.1 (26.4 since) | 2.33 (0 since) | 286,124 (148,633 since) | 147.2 µs |
| bare hyper-quic stream, 64 KiB + 16 B each way | 208.2 | 2.08 | 285,881 | 116.3 µs |
| hyper-transport lane frame, 512 B | 1.07 | 0.09 | 620 | 0.8 µs |

What the layer adds:
- **An exchange without a body: 0.27 allocations.** Every buffer it holds is a reservation the
  budget recycles: the prefix and head written in one QUIC write, the head read into a reservation,
  the event queue, the exchange table and the per-connection lists reused.
- **An exchange with a body: about 5.** Each body is three QUIC writes a side where the bare stream
  makes one: the prefix and head, the body, and the body's CRC-32C trailer. hyper-quic copies each
  write into its own buffer (`SendStream::write`), one allocation each. The trailer is mantle's
  checksum rule; merging it into the owner's last write would copy the owner's bytes instead.
- **Reallocations equal to the bare stream's** once the budget's spare buffers are chosen by size:
  before, a reservation took whichever spare buffer was on top and grew it: 10.33 reallocations an
  exchange at 64 KiB before, 2.33 now, against the bare stream's 2.08.
- **A lane frame: 1.07**, the frame's one QUIC write; the frame is queued in a recycled reservation.
- Time at 64 KiB is 27 % over the bare stream: the CRC-32C of 128 KiB and the copy of each body into
  the reader's reservation, which the bare stream drops without reading.

## Against focal-wire

`crates/hyper-transport-compare` (a workspace of its own; focal-wire from focal at `99191da`): two
endpoints in one process over two UDP sockets on loopback, on one current-thread tokio runtime,
the answering side in a spawned task and the asking side in the runtime's own. One round is a
request of the size and a reply of the size, the next round once the reply is whole. Both sides
are driven the same way: parked on tokio's reactor (kqueue here) and timer between datagrams.
- focal-wire runs its own transport configuration (`server_tls`, `client_tls`,
  `quic_transport`), its frame codec (`write_frame`, `read_frame_header`,
  `read_payload_arriving`, `require_end`) and its exchange shape (a bidirectional stream per
  request, a task per request on the answering side) over quinn 0.11; its domain envelope and
  registry are left out.
- hyper-transport runs each endpoint under hyper-tokio's `Driver`, with a batch of 10 (one of
  macOS's datagrams a system call either way, as quinn's here).

An earlier version of this table drove hyper-transport from one loop that busy-polled both
sockets while focal-wire parked, which favoured hyper-transport's round times; those rows are
replaced by these.

The machine is the one above, 2026-10-01 14:15–14:18 PDT, load average 35.7 to 49.5 (median 43.5)
across the rows. Each row is a fresh process; nine runs visit the eight rows in an order rotated by
one from the run before (`compare.sh`). 2,000 rounds (250 at 512 KiB) after a tenth as many. The
round is the median of the nine runs' medians, the range of those medians beside it. Allocations,
reallocations and bytes are the whole process's per round, both sides and tokio included;
datagrams are those the asking side sent and received per round.

| Size each way | | Round | Allocations | Reallocations | Bytes | Datagrams |
|---|---|---|---|---|---|---|
| 64 B | focal-wire | 199 µs (94–262) | 24.7 | 0 | 2,714 | 2.7 |
| | hyper-transport | 70 µs (64–124) | 12.4 | 0 | 1,266 | 2.8 |
| 4 KiB | focal-wire | 115 µs (101–152) | 36.0 | 0 | 43,439 | 8.3 |
| | hyper-transport | 82 µs (74–94) | 20.1 | 1.88 | 17,186 | 8.2 |
| 64 KiB | focal-wire | 706 µs (700–726) | 233.5 | 0.01 | 680,969 | 100.1 |
| | hyper-transport | 648 µs (639–675) | 212.3 | 2.00 | 286,430 | 97.0 |
| 512 KiB | focal-wire | 5,798 µs (5,570–6,066) | 1,743.7 | 0.13 | 5,437,834 | 800.9 |
| | hyper-transport | 4,806 µs (4,640–5,020) | 1,618.3 | 4.00 | 2,294,453 | 749.3 |

A second pass of five runs at load 32.8 to 35.4 (14:18–14:19) agreed: 86 against 123 µs at 64 B,
83 against 138 at 4 KiB, 651 against 725 at 64 KiB, 4,860 against 6,032 at 512 KiB, every count
within 1 % of the table's.

**Where hyper-transport wins.** Its rounds are shorter at every size on the same driving model:
by 65 % at 64 B, 29 % at 4 KiB, 8 % at 64 KiB and 17 % at 512 KiB; the 64 B rows of both spread
widely under this load. It makes half focal-wire's allocations at 64 B, 44 % fewer at 4 KiB and 7
to 9 % fewer at 64 and 512 KiB, and two fifths to a half of its bytes: focal-wire allocates each payload whole before reading it and
serialises each into a fresh buffer, where hyper-transport reads into the budget's recycled
reservations.

**Where hyper-transport loses.**
- **Reallocations** (since closed, below): 1.9 to 4 a round with a body, against focal-wire's 0 to
  0.13.
- **Datagrams at 64 B**: 2.8 a round against 2.7, an acknowledgement more in one round of ten.
- **Allocations against its own earlier driver**: draining every datagram that has arrived before
  surfacing an event (below) cut the datagrams of a 64 KiB round from 108.6 to 97.0 and its time
  from 762 to 648 µs, but holds more datagrams at once, so hyper-quic's reused receive buffer is
  reclaimed less often: 195 allocations a 64 KiB round became 212, 1,483 at 512 KiB became 1,618.

**The adapter's turn order, measured.** hyper-tokio's first version took one batch of datagrams a
turn and surfaced the endpoint's event before reading more. On this workload it lost at 64 KiB:
762 µs (751–1,461) against focal-wire's 731 (710–1,193), nine runs at load 33.5 to 45.8, with 108.6
datagrams a round against 100.4. A profile of each process over 5 s at 64 KiB (`sample`) put
hyper-transport's thread in `sendto` for 2,170 of its samples and parked in `kevent` for 15, where
focal-wire's spent 935 in `sendmsg` and 2,292 parked: hyper-transport's endpoint answered each
batch as it came, in more and smaller acknowledgements and window updates. The driver now drains
the socket, up to 128 batches, before it fires timers, sends and surfaces an event, and hands out an
event already queued without any system call; the table above is that version.

### Reallocations, traced and closed (2026-10-01, 14:30–15:20 PDT)

Each reallocation was traced to its site by a counting allocator that recorded the call stack of
every reallocation in the measured rounds (both the in-memory bench and this comparison; a
throwaway harness, not kept). Every one with a body was the same site: `Endpoint::handle_datagram`
copied each datagram into one reused `BytesMut` with `reserve(len)`. hyper-quic keeps views of a
datagram while the owner has not read its stream data, so the buffer was rarely reclaimable, and
`reserve` grew it in place a datagram at a time (311 → 1,515 bytes, 975 → 1,950, ...). The budget's
reservations were not a site: its buffers, chosen by size, never grew once warm.

Datagrams are now cut from a bounded pool of chunks (`crates/hyper-transport/src/receive.rs`), each
65,527 bytes (RFC 9000 §18.2's largest datagram) and never grown: the active chunk takes the next
datagram, or is reclaimed whole once every view of it is gone (`try_reclaim`), or a held chunk
that is free takes over, or a new chunk is added, reserved from the budget first. Past
`Limits::receive_chunks` chunks, or when the budget refuses one, a datagram is copied into a buffer
of its own size.

**The memory a peer can pin, stated.** A view pins its whole chunk, so a peer whose data sits unread
could leave chunks each pinned by one small datagram, up to 54 times the bytes those datagrams
carry at QUIC's 1,200-byte floor. Bounded now:
- **chunks, per endpoint**: at most `receive_chunks × 65,527` bytes, whatever any peer sends, every
  byte reserved from the budget on `Lane::Window` and held for the endpoint's life; one connection
  can pin at most the same. The tests set `receive_chunks` to what a full 16 MiB window of unread
  data packed into chunks occupies, 257 (16.8 MB, charged);
- **copies, per connection**: a copied datagram holds its own bytes. What hyper-quic keeps of a
  stream is bounded by its assembler: over-allocation, the datagram bytes kept beyond the stream
  bytes buffered, at most `max(32,768, 1.5 × buffered)` before it compacts (`assembler.rs`
  `insert`), and buffered bytes by the receive window. So a connection's copies hold at most
  `2.5 × W + 32 KiB × s` for a receive window `W` (charged, its grant) and `s` open streams. The
  over-allocation part is hyper-quic's own allowance, as before the pool; it is not charged to the
  budget, and is recorded here as owed.

`unread_datagrams_pin_no_more_receive_chunks_than_the_bound` (`tests/exchange.rs`) is that peer:
sixteen bulk requests whose bodies the receiver holds unread, each followed by 48 answered
requests of 1,300-byte bodies so that each chunk is pinned by one small datagram, against a pool of
three chunks. The pool never passes three, datagrams past it are copied, and everything completes
once the bodies are read; without the bound the pool grows past three and the test fails.
`receive::tests::single_unread_datagrams_pin_no_more_than_the_pool` does the same on the pool
alone, with the budget's charge checked to the byte.

`bash compare.sh 5` with the bounded pool, load average 52.9 to 67.7 (median 58.9), against the
table's hyper-transport rows (load 35.7 to 49.5); focal-wire's rows of the same five runs beside
them:

| Size each way | Reallocations before → after | focal-wire | Allocations before → after | focal-wire | Bytes before → after | focal-wire | Round after | focal-wire round |
|---|---|---|---|---|---|---|---|---|
| 64 B | 0 → 0 | 0 | 12.4 → 12.4 | 24.7 | 1,266 → 1,299 | 2,714 | 279 µs (218–302) | 303 µs (103–360) |
| 4 KiB | 1.88 → 0 | 0 | 20.1 → 12.7 | 36.0 | 17,186 → 9,386 | 43,443 | 83 µs (74–108) | 132 µs (125–150) |
| 64 KiB | 2.00 → 0 | 0.01 | 212.3 → 28.3 | 235.2 | 286,430 → 149,066 | 680,859 | 642 µs (640–656) | 737 µs (728–743) |
| 512 KiB | 4.00 → 0 | 0.12 | 1,618.3 → 141.5 | 1,746.0 | 2,294,453 → 1,192,383 | 5,435,061 | 4,620 µs (4,598–4,627) | 6,099 µs (5,688–6,643) |

Again on the final code (with the `Writable` fix below), five runs at load average 8.0 to 9.1: the
same counts to within 0.2 allocations and 0 reallocations at every size (focal-wire 0 to 0.19);
rounds 24 against 32 µs at 64 B, 39 against 46 at 4 KiB, 389 against 410 at 64 KiB, 2,940 against
3,678 at 512 KiB.

**A livelock found by the gate run of this change.** One run of hyper-tokio's end-to-end exchanges
never finished: the 8 MiB bulk exchange heard `Writable` after `Writable`. The endpoint judged a
body writable by the connection's credit alone; when the stream's own window refused a write the
credit allowed, the refused write queued a `Writable` at once, the owner wrote nothing and was told
again, and hyper-tokio's driver, which hands out a queued event without a system call, never read
the peer's window update. A body refused by its stream's window now hears `Writable` only after
QUIC's own `Writable` for that stream
(`a_body_blocked_by_its_stream_window_hears_no_writable_until_quic_says_so`, failing before);
hyper-tokio's end-to-end ran five times clean after it, at load 13 to 18.

The same change took allocations and bytes too: a datagram that could not reuse the old buffer had
been an allocation, and the pool's chunks are reused instead of reallocated. A 64 KiB round now makes
28 allocations where focal-wire makes 235, and asks for a fifth of its bytes. The 64 B row's 33 bytes
more are the first chunk of each side, amortised over 2,000 rounds. In the in-memory bench
(`cargo bench -p hyper-transport --bench allocs`, load 41 to 57): 12.4 allocations, 0 reallocations
and 9,313 bytes a 4 KiB exchange (21.0, 2.33, 17,760 before); 26.4, 0 and 148,633 at 64 KiB (211.1,
2.33, 286,124). A lane frame keeps 0.09 reallocations: hyper-quic's send buffer (`SendBuffer::ack`,
upstream's `shrink_to_fit` once a stream's queue is under a quarter of its capacity) gives its
segment queue back after each burst and grows it again with the next; that is upstream's bound on
what an idle stream holds, kept.

## hyper-tokio end to end

`cargo test -p hyper-tokio --test e2e`: the asking process and one peer process, each a
current-thread tokio runtime whose one task owns a `Driver` and a `PlaneSocket`; the asking task
races every wait against its own 1 ms tick, so the driver's future is dropped mid-wait whenever the
tick wins. macOS on this machine at load 33 to 45 (debug build), and Linux (`rust:1.98.0`
container on the same machine, aarch64, kernel with `UDP_SEGMENT` and `UDP_GRO`; five runs).

| Scenario | macOS | Linux |
|---|---|---|
| exchanges: 19 exchanges in every class, 8 MiB and 16 × 64 KiB bodies each way, every byte checked | 9.0 MiB each way in 1.01–1.46 s (the busy-polled driver of `hyper-transport`'s E2E: 1.33 s at the same load); a vote answered in 12–25 ms; the driver's future dropped mid-wait 29–64 times | 0.97–1.29 s; a vote in 13–16 ms; 7,100–7,400 datagrams each way |
| lanes: 8,000 frames of 512 B on two lanes, echoed in order | 144 ms | 132–184 ms; 7.5–8.1 datagrams a `sendmmsg` call, 13.6–18.7 a `recvmmsg` call (segmented and coalesced) |
| plane: 2,000 messages of 48 B sealed under keys from the connection's TLS exporter, on a socket of their own, echoed | 87 datagrams, none sent again, 19 ms | 87 datagrams, none sent again, 15–17 ms |
| killed: a peer killed with SIGKILL mid-upload | refused as stalled 3.93–3.98 s after the kill (two 2 s periods); the next peer process answered; no receive error | 3.95–3.96 s; the next peer answered |

Release builds on macOS, minutes apart: the exchanges scenario in 104 ms against the busy-polled
driver's 99 ms.

**A finding in the harness, not the adapter.** The first harness moved every exchange (a pass that
fills a 64 KiB piece for each blocked body) after every single event. On Linux that owner fell so
far behind that the peer refused seven of the sixteen 64 KiB requests as stalled at its 2 s period,
and on macOS the scenario took 3.4 s. An owner takes the event it woke for and every one queued
behind it (`Endpoint::poll_event`) before it moves its exchanges, as the busy-polled E2E did; then
every run passed. The adapter hands out an event already queued without a system call, so that
pattern costs nothing.

## Against slates' session plane

Not run. slates' session plane has no exchange API of its own outside its runtime: its benches
(`class_latency_bench`, `congestion_bench`, `path_mtu_bench`) drive whole sessions through slates'
scheduler and simulated paths, and hyper-transport replaces the plane's application layer only
above a different wire (standard QUIC, not slates' dialect). The comparison owed is slates' class
latency grid run on hyper-transport once slates' adapter drives it (note 32 §3.4, "slates").

## End to end

`cargo test -p hyper-transport --test e2e`: this machine, load 22 to 27, one peer process at a time.

| Scenario | Result |
|---|---|
| exchanges | handshake 43–50 ms; 19 exchanges (control, request, bulk; 8 MiB and 16 × 64 KiB bodies) moved 9.0 MiB each way in 1.4–2.0 s; a vote answered in 1.3–13 ms |
| reserve | four held bulk bodies stopped at 11,904 bytes, the peer's window less the bulk class's reserve; a vote crossed in 4.4–6.4 ms; the 16 MiB of bulk finished in 1.3–1.8 s once released |
| refusals | kind, frame bound and budget refused by the peer, each typed; the exchange table's bound and the role refused locally; an identity past its bound replaced; one past the identity bound and an unknown certificate refused |
| killed | an upload to a peer killed with SIGKILL refused as stalled 3.95–3.96 s after the kill in six runs at load 28–36 (two 2 s periods: the judgement after the kill still hears the peer's last acknowledgements, the next hears silence); the next peer process answered after the route was retired. Before the fix below, one loaded gate run took 7.96 s, four periods |
| lanes | 8,000 frames of 512 B on two lanes, echoed by the peer, in order, in 152 ms |

**What sent into silence is not progress.** The first version counted a period's sent bytes alone,
as focal's `carried` did. After a peer died the sender kept sending: the flight in the air, then the
probe timeout's probes (RFC 9002 §6.2.4), a datagram or two each at a doubling backoff. Every period
that held a probe counted as progress, so the end varied with how the backoff fell against the
periods: two periods on one run, four on a loaded gate run. A period now also has to hear the peer;
a live peer acknowledges within its `max_ack_delay` (RFC 9000 §13.2.1). In the in-memory test the
dead peer's exchange now ends at the first judgement (it took two, the first having counted 13,776
bytes sent into silence), and `progress::tests::what_is_sent_into_silence_is_not_progress` fails
without the change.

**An answer is charged with what arrives (2026-10-01).** The `exchanges` scenario's asker refused
its 8 MiB bulk reply as stalled at 5.6 and 5.5 MB read on two CI runs (ubuntu-24.04 at `ac10f6f`,
windows-11-arm at `35d35d8`). Nothing was lost and the server had not stopped: the answering wait
gave a body its residency at two datagrams a round trip, 1.38 s for 8 MiB at the 395 µs QUIC measured,
so one period, and the body was moving at 2.7 MB/s. The law is now charged with the bytes delivered
against what the peer declared (`src/progress.rs`). Runs of the scenario (debug builds; macOS on
this machine at load 33–39; Linux in `rust:1.98.0` containers on it):

| Where | Before | After |
|---|---|---|
| macOS, three at once under background QoS (`taskpolicy -c background`) | 21 of 24 passed (a 64 KiB reply refused with none of it read while its period brought 464 KB) | 90 of 90, the longest 5.3 s |
| Linux, `--cpus=0.5`, three at once | 8 of 15 (the server refusing requests queued in the asker's upload) | 45 of 45, the longest 11.0 s |
| Linux, `--cpu-period=100000 --cpu-quota=20000`, two at once | 5 of 30 | 30 of 30, the longest 22.9 s |
| Linux, `--cpus=4`, one at a time | | 20 of 20; every scenario 10 of 10 |
| Linux, `--cpus=1`, every scenario | | 5 of 5 |

The receive window is tuned under the owner's measured timer granularity `G` since the same date
(`Endpoint::set_granularity`; RFC 9002's 1 ms until the first report). In these scenarios neither
driver's timed wait ran out while bodies moved (each turn ended on a datagram), so `G` stayed at
1 ms and the windows grew as before: hyper-tokio's exchanges on Linux at `--cpus=4`, 16 runs of each
build alternated at load 30–37, took 0.77–1.20 s before and 0.82–1.28 s after, the same law
throughout; the allocation bench is unchanged at every size.

## Commands for the transport

```sh
# The suites, the in-memory scenarios and the end-to-end scenarios.
cargo test -p hyper-transport
# Allocations per exchange and per frame.
cargo bench -p hyper-transport --bench allocs
# The comparison against focal-wire (fetches focal at 99191da): nine rotated runs of fresh
# processes, then the table.
bash crates/hyper-transport-compare/compare.sh 9
# One row: implementation (focal or hyper), size, rounds.
cd crates/hyper-transport-compare && cargo run --release -- hyper 65536 2000
# hyper-tokio: the end-to-end scenarios, and the refusals.
cargo test -p hyper-tokio
# The same on Linux, in a container on this machine (sendmmsg, recvmmsg, UDP_SEGMENT, UDP_GRO).
docker run --rm -v "$PWD":/work -w /work rust:1.98.0 cargo test -p hyper-tokio --locked
```
