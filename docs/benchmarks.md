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

A member's loop waits on its socket until its next tick, and when the tick is already due it
still takes what has arrived before it ticks. On a loaded machine a turn of the loop can outlast
a tick, so the tick is due at every turn. A member that skipped its socket then kept ticking
without reading anything: a leader stepped down by its quorum check with its followers' answers
still unread in its socket, and the scenario stalled on the election that followed. Two runs
showed this before the fix: one with a parallel build loading the machine (tick 12 ms, load
average 39), one with the tick forced to 4 ms under the same load. Each leader read nothing for
100 to 180 ms, about ten ticks fired with nothing read in between, and the answers it had missed
were the first datagrams it read after stepping down. `node::tests` holds the directed test.

The client waits on facts, with bounds taken from the protocol. A request waits for its answer
through twice the longest election timeout. One write or read is retried for `WAIT_ELECTIONS`
elections, each within twice the election timeout, rather than for a fixed number of requests:
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
member runs each period of a group, measured against focal-timing at `a8e95f7` (the source of the
paths, the tick pace and the rounds) and slates' `crates/cluster/src/timing.rs` at `5cce86a` (the
source of the election timing, the priority and the timer).

## The machine and the workload

The same Apple M5 Max, macOS 26.4.1, rustc 1.98.0, 2026-10-01 at 10:52 PDT, shared with two other
sessions (load average 17 to 28). `crates/hyper-timing-compare`, a workspace of its own, runs each
operation two million times in a fresh process of its own, seven runs, the implementations rotated
each run, and counts allocations over a further two million. A five-voter group, so four paths from
each member, fed the same eight WAN round trips of 40 to 58 ms; a 10 ms heartbeat.

## Results

Medians (least–most), nanoseconds a call; no implementation allocates in any operation but slates'
priority, once a call.

| Operation | hyper | focal | slates |
|---|---|---|---|
| a sample folded into a path | 29.2 (28.3–30.6) | 0.9 (0.8–1.1) | 2.1 (1.7–2.4) |
| a path's tail read | 1.2 (0.8–1.2) | 87.5 (82.2–93.0) | 1.2 (0.8–1.3) |
| the election timing from four paths | 10.9 (10.7–12.0) | — | 10.9 (10.9–11.4) |
| the quorum priority over four paths | 12.5 (11.8–13.5) | — | 24.8 (24.2–26.3), 1 allocation |
| the tick pace from four paths | 5.5 (4.1–6.4) | 356.2 (284.5–386.0) | — |
| a round's budget | 1.0 (0.9–1.2) | 1.1 (0.9–1.2) | 1.0 (0.9–1.1) |
| a follower's period of the election timer | 1.6 (1.0–1.6) | — | 1.7 (1.0–1.7) |

## Where hyper-timing does not win, and why

A sample costs 29 ns, against focal's 0.9 and slates' 2.1. focal stores the sample and leaves the
work to every read; slates' path is RFC 9002's smoothed estimator, two multiplies a sample. hyper's
path is focal's median window, kept because one late answer from a starting or stalled peer must not
move a group's election timeout (`PathRtt`'s documentation, from focal), and it now does the work of
the median once a sample instead of three sorts a read. A member samples a path about once a period
and reads it several times (the election timing, the priority, the tick pace, the round budget), so
over a period of the four paths hyper spends about 150 ns (four samples and every derivation), focal about 360 ns (four samples, the tick pace and a round) and slates about 45 ns
for an estimate that one late answer moves.

Before these changes hyper-timing's tail read was focal's (71.7 ns), the election timing 940 ns and
the priority 1,384 ns: the first measurement of this comparison found them, and the changes are in
`crates/hyper-timing/ORIGIN.md`, change 7. The first measurement also timed a sample that the
optimizer had removed, since nothing read the path afterwards; the harness now observes the path
after every sample.

## Commands for the timing

```sh
cargo test -p hyper-timing
cd crates/hyper-timing-compare && cargo build --release
target/release/hyper-timing-compare table 7 2000000
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
| hyper-transport exchange, 16 B heads, 4 KiB bodies | 21.0 (12.5 since) | 2.33 (0 since) | 17,760 | 11.5 µs |
| bare hyper-quic stream, 4 KiB + 16 B each way | 16.2 | 2.08 | 17,493 | 8.6 µs |
| hyper-transport exchange, 16 B heads, 64 KiB bodies | 211.1 (30.5 since) | 2.33 (0 since) | 286,124 | 147.2 µs |
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

### Reallocations, traced and closed (2026-10-01, 14:30–15:05 PDT)

Each reallocation was traced to its site by a counting allocator that recorded the call stack of
every reallocation in the measured rounds (both the in-memory bench and this comparison; a
throwaway harness, not kept). Every one with a body was the same site: `Endpoint::handle_datagram`
copied each datagram into one reused `BytesMut` with `reserve(len)`. hyper-quic keeps views of a
datagram while the owner has not read its stream data, so the buffer was rarely reclaimable, and
`reserve` grew it in place a datagram at a time (311 → 1,515 bytes, 975 → 1,950, ...). Datagrams are
now cut from 65,527-byte chunks (RFC 9000 §18.2's largest datagram): a chunk with room takes the
next, a chunk whose views are gone is reclaimed whole (`try_reclaim`), and otherwise a fresh one is
allocated, never grown, as quinn cuts each receive batch from one `BytesMut`. The reservations were
not a site: the budget's buffers chosen by size never grew once warm.

`bash compare.sh 5` after the change, load average 16.5 to 21.2 (median 18.5), against the table's
hyper-transport rows (load 35.7 to 49.5); focal-wire's rows of the same five runs beside them:

| Size each way | Reallocations before | after | focal-wire | Allocations before | after | focal-wire | Bytes before | after | Round after | focal-wire round |
|---|---|---|---|---|---|---|---|---|---|---|
| 64 B | 0 | 0 | 0 | 12.4 | 12.4 | 24.7 | 1,266 | 1,299 | 40 µs (37–42) | 59 µs (56–62) |
| 4 KiB | 1.88 | 0 | 0 | 20.1 | 12.8 | 36.0 | 17,186 | 14,660 | 48 µs (45–51) | 65 µs (61–68) |
| 64 KiB | 2.00 | 0 | 0.01 | 212.3 | 32.4 | 230.4 | 286,430 | 283,086 | 428 µs (424–434) | 449 µs (446–458) |
| 512 KiB | 4.00 | 0 | 0.22 | 1,618.3 | 175.1 | 1,704.8 | 2,294,453 | 2,266,370 | 3,987 µs (3,963–4,087) | 4,112 µs (3,845–4,219) |

The same change took the allocations too: a datagram that could not reuse the buffer had been an
allocation, so a 64 KiB round now makes 32 allocations where focal-wire makes 230. The 64 B row's 33
bytes more are the first chunk of each side, amortised over 2,000 rounds. In the in-memory bench
(`cargo bench -p hyper-transport --bench allocs`, load 31 to 37): 12.5 allocations and 0
reallocations a 4 KiB exchange (21.0 and 2.33 before), 30.5 and 0 at 64 KiB (211.1 and 2.33).
A lane frame keeps 0.09 reallocations: hyper-quic's send buffer (`SendBuffer::ack`, upstream's
`shrink_to_fit` once a stream's queue is under a quarter of its capacity) gives its segment queue
back after each burst and grows it again with the next; that is upstream's bound on what an idle
stream holds, kept.

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
