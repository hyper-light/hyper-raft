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

## Readies in flight (R-4)

Core step R-4 (`docs/durable.md` §2.1) lets a member take `Ready`s while earlier writes are out.
Measured on 2026-10-02 on the machine above, against `main` at `9aafcd9` built the same way.

**The synchronous path did not move.** An owner that finishes each write before it takes the next
(`advance_append`, every row of the tables above) runs a path of its own that passes through no
queue.

- Allocations, reallocations and bytes asked for, by the core and by the whole loop: identical on
  every workload of the table at three and five voters, in place and copying (36 counting runs,
  `hyper-raft-compare one <core> <workload> ... 1000 count`, before and after; 11.0 allocations per
  entry at batch 1 in place, 23.0 copying, as before).
- Time per op, `main` and R-4 interleaved run by run in fresh processes, the order alternated, at
  load 7.3–11 (01:18–01:19 PDT); median [least–most], nanoseconds:

  | Workload | Core | `main` | R-4 | ratio |
  |---|---|---|---|---|
  | steady batch 1, 64 B (30 runs) | in place | 1,041 [946–1,771] | 1,059 [983–1,712] | 1.02 |
  | | copying | 1,261 [1,203–2,363] | 1,288 [1,201–1,422] | 1.02 |
  | steady batch 1, 4 KiB (30 runs, twice) | in place | 1,369 [1,324–1,461]; 1,385 [1,329–1,599] | 1,392 [1,357–1,596]; 1,438 [1,355–1,662] | 1.02; 1.04 |
  | | copying | 2,539 [2,269–3,298] | 2,537 [2,314–2,912] | 1.00 |
  | transfer (30 runs) | in place | 2,848 [2,622–3,362] | 2,866 [2,641–3,396] | 1.01 |
  | | copying | 3,221 [3,007–3,544] | 3,276 [3,023–3,975] | 1.02 |
  | steady batch 64, 64 B (10 runs) | in place | 88 [85–93] | 90 [86–98] | 1.02 |
  | catchup, per entry (10 runs) | in place | 22 [19–25] | 22 [20–26] | 1.00 |
  | snapshot (10 runs) | in place | 3,444 [2,913–5,907] | 3,103 [2,856–4,012] | 0.90 |
  | fast (10 runs) | in place | 2,230 [2,004–2,515] | 2,224 [2,047–2,816] | 1.00 |

  Every ratio is inside the two cores' ranges. The first build of R-4 was 15 % slower on steady
  batch 1 (1,168 ns against 1,017) with the same allocations. A loop of one voter's proposal,
  `Ready` in place and `advance_append_keeping`, built with LTO against both trees (the best of five
  passes of 400,000 proposals), put it at 115 ns a proposal against 89, and `sample` attributed the
  difference to the record of each write being built, moved into the queue and out again. The
  synchronous path now passes through no queue and records nothing it does not need: 93.6 ns against
  92.8 at load 8–10.

**What readies in flight buy** (`cargo bench -p hyper-raft --bench pipeline`, `benches/pipeline.rs`).
Three voters; each device flushes one batch at a time and takes into it every write submitted before
the flush began (group commit); the network delivers each message a fixed one-way time later;
closed-loop clients propose 64-byte entries at the leader and again once theirs is applied there.
Time is simulated in microseconds from this machine's measurements: a flush is 4,330 µs (mantle's
p50 for an append that waits one `F_FULLFSYNC` on APFS, `docs/measurements/2026-09-29-log-confirmation.md`
there) and a one-way trip 198 µs (half the 395 µs loopback round trip of hyper-transport, "An
answer is charged with what arrives"). A write that asks for no flush (a commit alone) is answered
with the writes before it. Two devices: **flush**, where a flush answers what it holds; and
**confirmed**, hyper-log's, where a frame is answered once a later flush has written its persist
record and a frame nothing follows is confirmed by a flush of its own. 20,000 entries a cell; wall
time the better of three, at load 7.3–8.4 (01:19 PDT):

| device | clients | depth 1: p50 µs, entries/s, flushes/entry | depth 2 | depth 3 |
|---|---|---|---|---|
| flush | 1 | 4,726, 212, 3.00 | 4,726, 212, 3.00 | 4,726, 212, 3.00 |
| flush | 4 | 12,990, 308, 2.25 | 12,990, 308, 2.25 | 12,990, 346, 2.00 |
| flush | 16 | 12,990, 1,231, 0.563 | 12,990, 1,231, 0.563 | 12,990, 1,259, 0.550 |
| flush | 64 | 12,990, 4,902, 0.141 | 12,990, 4,907, 0.141 | 12,990, 4,939, 0.140 |
| confirmed | 1 | 9,056, 110, 6.00 | 9,056, 110, 6.00 | 9,056, 110, 6.00 |
| confirmed | 4 | 25,980, 154, 4.50 | 21,650, 188, 3.69 | 17,320, 231, 3.00 |
| confirmed | 16 | 25,980, 615, 1.125 | 21,650, 707, 0.980 | 17,320, 923, 0.750 |
| confirmed | 64 | 25,980, 2,451, 0.282 | 21,650, 2,780, 0.249 | 17,320, 3,671, 0.188 |

- On a device whose flush answers its own writes, depth buys almost nothing: the device is serial
  and group commit already batches what arrives during a flush, depth one or three (at most +12 %
  entries a second, at four clients).
- On hyper-log's confirmed device it is what the design said (`docs/durable.md` §2.1): at depth one a
  write is answered only after a second flush confirms it, so the next write waits two flushes; at
  depth three the next write's flush confirms the last. Under load p50 falls by a third (25,980 to
  17,320 µs), throughput rises by half (2,451 to 3,671 entries a second at 64 clients) and flushes
  per entry fall by a third. One client sees no change: nothing follows its write, so a flush of its
  own confirms it (6.0 flushes per entry, 9,056 µs, mantle's two-flush 8.65 ms), the idle
  confirmation of `docs/durable.md` §14.6.
- The core's CPU per committed entry, with the harness, is the same at every depth: 1,190 to
  2,110 ns, unordered by depth.

## The durable commit and the apply pause (R-6)

Core step R-6 (`docs/durable.md` §4.4). Measured on 2026-10-02, 01:59–02:26 PDT, on the machine
above, against `main` at `b73e18b` built the same way; another session's work held the load at 21
to 27 for most of it, and each figure says its load.

**Allocations did not move.** `hyper-raft-compare one <core> <workload> 3|5 ... 1000 count`, `main`
and R-6, every workload (steady at batch 1 with 64 B and 4 KiB, steady at batch 64, transfer,
failover, catch-up, snapshot, fast), in place and copying, three and five voters: allocations,
reallocations, bytes asked for, peak and the whole loop's totals identical in all 32 cells, before
and after the last change. Minor faults are single digits a run, either way by cell; no major fault.

**`benches/pipeline.rs`**: every simulated figure identical at every cell (p50, p99, entries a
second, flushes an entry, both devices, depths one to three); the CPU an entry is noisy at load 24 and
not ordered by tree.

**Time per op.** What R-6 adds on the synchronous path is a few loads and branches a `Ready`: the
pause's check in `has_ready` and `light`, the durable commit compared with the commit (the walk that
holds answers runs only where the durable commit is behind it, never for a leader, and out of line),
the `max` at a notice, the apply bound's match. Two instruments:

- The one-voter loop R-4 timed (a proposal, `ready_in_place`, `advance_append_keeping`), built with
  LTO against each tree, the best of five passes of 400,000 proposals, 12 interleaved runs at load
  23–25: `main` median 112.6 ns, minimum 109.5; R-6 114.1 and 109.3; R-6 with only its new fields
  111.1 and 105.5. At 4 KiB, six runs at load 25–27: minimum 181.5 against 189.2.
- The comparison, `main` and R-6 interleaved in fresh processes, the order alternated, minimum, lower
  quartile and median, ratio R-6 to `main`, at load 21–24 (02:25 PDT):

  | Workload | Core | runs | min | q1 | median |
  |---|---|---|---|---|---|
  | steady batch 1, 64 B | in place | 30 | 1.00 | 1.03 | 1.02 |
  | | copying | 30 | 1.08 | 1.04 | 1.05 |
  | steady batch 1, 4 KiB | in place | 30 | 1.01 | 1.00 | 0.98 |
  | | copying | 30 | 1.01 | 1.03 | 1.04 |
  | transfer | in place | 30 | 1.02 | 1.03 | 1.05 |
  | | copying | 30 | 1.02 | 1.01 | 1.03 |
  | steady batch 64, 64 B | in place | 10 | 1.01 | 1.01 | 0.97 |
  | catch-up | in place | 10 | 0.99 | 1.02 | 1.02 |
  | snapshot | in place | 10 | 0.94 | 1.02 | 1.08 |
  | fast | in place | 10 | 1.05 | 1.02 | 1.01 |
  | failover | in place | 10 | 1.03 | 1.02 | 1.02 |

  `main` against itself the same way, at load 2 (02:01 PDT): 1.00 to 1.02 on the steady cells and
  fast. At load 2–3, an earlier build of R-6 (the walk guarded, not yet out of line) ranged 0.98 to
  1.03 with every median inside `main`'s range.

So: no core function grew in `sample`'s profile of the steady 4 KiB cell (`ready_given` 82 samples
against 90, `light` 63 against 52, `slice_page` 89 against 57, of four seconds each), the harness's
own unchanged functions moved more than that (`flush` 156 against 189, `quiet` 113 against 151), and
`RawNode::operate` had fallen out of line on the proposal path, which `#[inline]` restored. Under the
load of these runs R-6's medians lean 1–3 % above `main`'s on the comparison and within 1 % on the
core alone; the noise band measured at load 2 is 2 %.
That reading was wrong on both counts; the next section settles it.

### The lean, settled

Measured on 2026-10-02, 02:40–04:55 PDT, on the machine above. Trees: `main` at `b73e18b`, R-6 at
`94a3a6a`, and R-6 with the fix below, each `hyper-raft-compare` built with LTO the same way, and the
one-voter loop R-4 timed (`micro`, LTO, the best of five passes). The runs:

- **Interleaved fresh processes.** Each round runs every tree once, the order rotated and reversed
  round to round; a cell's statistic is over the rounds.
- **Per round, the ratio of a tree to `main`** (adjacent runs, so a slow stretch of the machine falls
  on both). Stated: the geometric mean of the ratios with its Student-t 95 % interval on their
  logarithms, and the median with its sign-test (distribution-free, binomial order statistics) 95 %
  interval. A cell leans only where the interval excludes 1.
- **Three measures.** The loop's own time (`ns=`, the harness's clock). Instructions retired and
  cycles for the whole process, from `proc_pid_rusage(RUSAGE_INFO_V4)` (`ri_instructions`,
  `ri_cycles`), which are counted by the core's PMU and do not depend on the load. The whole-process
  counts include the group's set-up and so dilute a ratio towards 1. Where per-op instructions are
  quoted below they are net of the set-up: `(I(R) − I(R/5)) / (ops(R) − ops(R/5))`, median of 3 to 5.
- **Two schedulings.** The default, where the scheduler places a run on the 6 super cores or the 12
  performance cores as the load allows, and `taskpolicy -c background`, which holds it to the
  performance cluster at a fixed lower clock; cycles are the measure there.
- **What could not be had.** Linux `perf stat` was not available: Docker Desktop's VM exposes no PMU
  (`/sys/bus/event_source/devices` has only software, tracepoint, kprobe and uprobe). Instruments'
  CPU Counters template (CPU Bottlenecks mode) gave the split of R-6's extra cycles: on the in-place
  steady 4 KiB cell, six runs a tree, useful work 51–54 % of cycles against `main`'s 56–62 %, with
  the processing (back-end) share 31–35 % against 26–29 %; instruction delivery and discarded work
  were the same.

**The lean was real, and on the in-place path.** At load 11–13 the first full pass (30 rounds) put
R-6 above `main` on the in-place steady cells: 64 B time 1.063 [1.054, 1.071], cycles 1.055; 4 KiB
time 1.098 [1.084, 1.113], cycles 1.067 [1.060, 1.074], with instructions only 1.004–1.007. The
copying cells were 0.99–1.00 in time and cycles. At load 2–4, R-6's 4 KiB in-place cell stays above
(time 1.031 [1.020, 1.041], 1.042 [1.033, 1.050] in two passes), and on the performance cores its
cycles are 1.051–1.056 at load 2–22. The 64 B in-place cell is the one whose sign moved with the
load (1.055 at load 12, 0.97–0.98 at load 3–9).

**Cause 1: storage's walk fell out of line.** R-6's `Log::next_range_since` gave its page closure to
storage from two places, `Storage::any_entry` directly and through `Log::any_entry`. With a third call
site, LLVM no longer inlined the in-memory store's `any_entry` (it appears as a 252-byte function in
R-6's binary and not in `main`'s), so the closure, which storage takes as `&mut dyn FnMut`, was
called through its vtable once an entry (a 152-byte `FnMut` shim, also absent in `main`). The fix
asks storage once, from one place, for the part it holds, and walks the leader's not-yet-durable tail
(`Config::apply_unpersisted`) from the log's own slice after it: both symbols vanish, and the in-place
steady cells go from R-6's 1.03–1.10 to 0.96–0.97 of `main` (below).

**Cause 2: `Log` grew by 16 bytes.** With cause 1 fixed, leadership transfer and failover still ran
3.7–5.3 % slower than `main` at load 3.5–4 with instructions within 0.2 %, and R-6's hold on answers
never ran in any workload (a counter in `hold_answers`: zero calls in steady, transfer, failover,
catch-up, fast and snapshot). `main` with R-6's four new fields added and no other change (`durable_commit`
in `Raft`, `unpersisted_after: Option<u64>` in `Log`, `apply_paused` in `RawNode`, `commit` in `Mark`,
none read) reproduced most of it: transfer 1.017–1.018, failover 1.023–1.028, both intervals above 1;
`main` with 32 inert bytes in `RawNode` instead moved transfer by 0.4 % and failover by 1.0–1.7 %, and
the steady in-place cells by −3 to −4 %: these loops are that sensitive to where the member's fields
fall. `unpersisted_after` is now a plain index with `u64::MAX` for none (8 bytes, not 16):
transfer 1.005 [1.001, 1.009], failover 1.002 [0.994, 1.010] in the same pass where the `Option` gave
1.045 and 1.049. `Mark::commit` is likewise a plain `u64`, 0 for none (the durable commit only rises),
which drops a branch a notice and 8 bytes a write out.

**After, at load 1.8–3.5** (04:00–04:45 PDT, 30 rounds; time ratio to `main` with its 95 % interval,
then the performance cores' cycles ratio at load 2.0–4.0, 16 rounds):

| Cell | R-6 time | fixed time | R-6 cycles (perf. cores) | fixed cycles (perf. cores) |
|---|---|---|---|---|
| loop, 64 B | 1.007 [0.999, 1.015] | 1.005 [0.998, 1.012] | 0.992 [0.989, 0.994] | 0.997 [0.993, 1.000] |
| loop, 4 KiB | 1.022 [1.017, 1.028] | 1.034 [1.027, 1.041] | 0.993 [0.979, 1.007] | 1.003 [0.991, 1.015] |
| steady 64 B, in place | 0.974 [0.969, 0.980] | 0.960 [0.954, 0.965] | 1.019 [1.007, 1.031] | 1.004 [0.992, 1.016] |
| steady 64 B, copying | 0.985 [0.979, 0.991] | 0.974 [0.969, 0.980] | 1.007 [0.998, 1.016] | 1.006 [1.000, 1.013] |
| steady 4 KiB, in place | 1.031 [1.020, 1.041] | 1.005 [0.987, 1.022] | 1.056 [1.036, 1.076] | 1.011 [0.993, 1.030] |
| steady 4 KiB, copying | 0.989 [0.983, 0.994] | 0.995 [0.989, 1.001] | 0.995 [0.984, 1.007] | 0.997 [0.987, 1.007] |
| transfer, in place | 1.024 [1.015, 1.033] | 1.005 [0.995, 1.015] | 1.029 [1.016, 1.042] | 1.015 [1.006, 1.025] |
| transfer, copying | 1.023 [1.013, 1.034] | 1.022 [1.009, 1.035] | 1.007 [0.999, 1.015] | 1.009 [1.003, 1.016] |
| steady batch 64 | 0.996 [0.983, 1.009] | 0.998 [0.983, 1.013] | 1.000 [0.996, 1.003] | 1.004 [1.001, 1.007] |
| catch-up | 0.980 [0.973, 0.986] | 1.003 [0.986, 1.021] | 1.006 [1.001, 1.012] | 0.984 [0.980, 0.988] |
| snapshot | 1.000 [0.996, 1.004] | 1.008 [1.004, 1.013] | 1.011 [1.008, 1.015] | 0.988 [0.984, 0.991] |
| fast | 1.010 [0.999, 1.021] | 1.005 [0.993, 1.017] | 1.015 [1.006, 1.023] | 1.011 [1.001, 1.021] |
| failover | 1.034 [1.027, 1.042] | 1.004 [0.998, 1.010] | 1.019 [1.009, 1.030] | 1.013 [1.002, 1.024] |

Instructions an op, net of set-up, `main` / R-6 / fixed: steady 64 B in place 24,007 / 24,141 /
23,973; copying 29,988 / 30,293 / 30,223; steady 4 KiB in place 26,738 / 26,867 / 26,736; transfer in
place 89,714 / 90,374 / 89,885; copying 99,891 / 100,656 / 100,544; failover 113,793 / 114,828 /
114,434; the one-voter loop 2,820.4 / 2,830.2 / 2,822.2 (64 B) and 3,899.5 / 3,909.5 / 3,901.5
(4 KiB). Allocations, reallocations and faults are R-6's, which were `main`'s.

**What is left, and why it stays.**
- *What correctness needs.* On the copying path, where nothing offsets it, R-6 costs 230 instructions
  an op at steady 64 B (0.8 %) and 650 on transfer (0.7 %): the loads and compares that state the
  durable commit (the follower's `Ready` comparing its durable commit with its commit, the notice's
  `max`, the release's check), the pause's two flag tests, the apply bound's compare. Removing every
  one of them at once (not shippable) brought the copying cell to +45 ± 65; removing any one alone
  moved it within the run-to-run spread (±50–190), so the cost is spread, a few instructions each
  across the three members' `Ready`s and notices. In place, the cheaper storage walk more than pays
  for it (−0.1 %); the one-voter loop is +2 instructions an op (0.06 %).
- *Where the intervals still exclude 1 above.* Time on the 4 KiB one-voter loop (1.034), transfer
  copying (1.022) and snapshot (1.008) with default scheduling; cycles on the performance cores for transfer (1.015,
  1.009), failover (1.013), fast (1.011), steady 64 B copying and batch 64 (1.006, 1.004). Each sits
  with instructions within 0.06–0.7 % and, in the other scheduling, an interval that includes 1 or is
  below it (the loop's cycles 1.003 [0.991, 1.015], snapshot's 0.988; failover's time 1.004; fast's 1.005), against
  layout moves of 1–4 % that inert bytes alone produce in these same loops. Not closed: placing
  `Raft`'s and `Log`'s fields would need `#[repr(C)]` and a hand order across the member, which no
  measurement here yet justifies over the compiler's.
- The lean seen at load 21–27 in the first runs above was mostly cause 1, whose cost grew with the
  load (the 64 B in-place cell: 1.055 at load 12, 0.98 at load 3–9).

Commands (scratch harnesses outside the workspace; the tree's own binaries):
`hyper-raft-compare one hyper[-copy] <workload> 3 <batch> <bytes> <rounds> 1000 time` for
steady (rounds 40,000), transfer (20,000), steady batch 64 (20,000), catch-up (4,000), snapshot
(4,000), fast (20,000), failover (5,000); `micro <bytes> 400000|200000`; each under `taskpolicy -c
background` for the performance-core pass.

## Elections by suspicion (L-2)

Timing step L-2 (`docs/timing.md` §2.9). Measured on 2026-10-02 on the machine above (Apple M5 Max,
macOS 26.4.1), with other sessions' work holding the load at 19 to 21 throughout; each figure says
its load.

**The core did not regress.** On ticks the member carries one more field, `watch: Option<Box<Watch>>`
(eight bytes, none on ticks), and a test of it where a tick, a reset, a vote, a lease and a
heartbeat's answer branch on the mode.
- **Allocations**: `hyper-raft-compare one <core> <workload> 3|5 ... 1000 1000 count`, `main` at
  `edb920b` (the core is unchanged to `634d45c`) and L-2, every workload in place and copying where
  the tables run both, three and five voters: allocations, reallocations, bytes asked for, peak
  and the whole loop's totals identical in all 22 cells.
- **Time, instructions and cycles**, `main` and L-2 interleaved in fresh processes, the order
  alternated round to round; per round the ratio L-2 to `main`; the geometric mean with its
  Student-t 95 % interval on the logarithms (the median and its sign-test interval agree); time is
  the loop's own (`ns=`), instructions retired and cycles the whole process's (`/usr/bin/time -l`,
  the PMU's counts), set-up included; load 21.1 at the start and 20.2 at the end:

  | Cell | rounds | time | instructions | cycles |
  |---|---|---|---|---|
  | steady 64 B, in place | 30 | 1.002 [0.982, 1.024] | 1.001 [1.001, 1.001] | 0.990 [0.973, 1.007] |
  | steady 64 B, copying | 30 | 0.977 [0.960, 0.994] | 0.999 [0.998, 1.000] | 0.983 [0.964, 1.002] |
  | steady 4 KiB, in place | 30 | 0.957 [0.946, 0.968] | 1.001 [1.001, 1.001] | 0.968 [0.947, 0.989] |
  | steady 4 KiB, copying | 30 | 0.967 [0.957, 0.977] | 1.004 [1.002, 1.006] | 0.983 [0.969, 0.998] |
  | transfer, in place | 30 | 0.989 [0.977, 1.001] | 1.001 [1.001, 1.002] | 1.002 [0.988, 1.017] |
  | transfer, copying | 30 | 0.983 [0.972, 0.994] | 1.000 [1.000, 1.001] | 1.000 [0.983, 1.018] |
  | steady batch 64 | 10 | 0.970 [0.948, 0.992] | 1.001 [1.000, 1.001] | 0.970 [0.933, 1.008] |
  | catch-up | 10 | 1.002 [0.994, 1.010] | 1.001 [1.001, 1.001] | 1.004 [0.992, 1.016] |
  | snapshot | 10 | 1.003 [0.990, 1.017] | 1.001 [1.000, 1.001] | 0.994 [0.977, 1.011] |
  | fast | 10 | 0.989 [0.957, 1.021] | 0.999 [0.997, 1.001] | 0.982 [0.951, 1.013] |
  | failover | 10 | 1.008 [0.986, 1.031] | 1.002 [1.001, 1.003] | 0.999 [0.965, 1.035] |

  No interval of time or cycles lies above 1; four of time lie below it, which is the layout
  sensitivity the R-6 section measured (inert bytes move these loops by 1–4 %), not a gain claimed.
  Instructions rise by 0.1 % in most cells and 0.4 % on the copying 4 KiB cell, the whole process's
  count with the set-up in it: the mode's tests.
- **`benches/pipeline.rs`**: every simulated figure (p50, p99, entries a second, flushes an entry,
  both devices, depths one to three) identical to `main`'s.

**What an idle group costs** (`cargo bench -p hyper-raft --bench idle`, `benches/idle.rs`): 10,000
groups of three voters in one process, each elected and holding one committed entry, every member
caught up; then 400 of the owner's periods with nothing proposed. On ticks (`Settings::focal`, a
heartbeat every two ticks) the owner ticks every member each period, takes what each gives and
delivers every message within the group; by suspicion it asks each member's `deadline()`, the most
an owner without a timer queue does, and finds none due. Each process run with 0 and with 400
periods under `/usr/bin/time -l`, the difference taken, five interleaved rounds, load 19.0–19.3:

| | CPU a group a period | instructions | cycles | messages a group a period |
|---|---|---|---|---|
| ticks | 542.5 ns (535–545) | 7,327 | 1,574 | 2.0 |
| by suspicion, an owner that scans | 25.0 ns (22.5–25.0) | 234 | 69 | 0 |
| by suspicion, an owner with a timer queue | none | none | none | 0 |

At a tick of 15 ms, hyper-durable-e2e's measured period on this machine, a group on ticks costs its
owner 36 µs of CPU each second (10,000 groups, 36 % of a core) and 133 messages; at etcd's 100 ms
heartbeat, 5.4 µs and 20 messages. By suspicion an idle group costs nothing it does not ask for: no
message, no wake (its deadline is none); a scanning owner pays 25 ns a group a scan, and one that
keeps deadlines in a queue nothing. Liveness is then the node pairs' (below, "hyper-liveness: node-pair
heartbeats against per-group heartbeats and slates' detector"), whose cost does not grow with the
groups.

Commands: `crates/hyper-raft-compare` built `--release` in both trees; the interleaved runs and the
counting runs are a scratch harness over `hyper-raft-compare one hyper[-copy] <workload> 3 <batch>
<bytes> <rounds> 1000 time|count`; `cargo bench -p hyper-raft --bench idle -- <ticks|suspicion>
10000 <0|400>` under `/usr/bin/time -l`; `cargo bench -p hyper-raft --bench pipeline` in both trees.

## Repair by entries (R-5)

Core step R-5 (`docs/durable.md` §5.1). Measured on 2026-10-02 on the machine above (Apple M5 Max,
macOS 26.4.1), with other sessions' work holding the load at 24 to 52; each figure says its load.

**What a repair costs** (`cargo bench -p hyper-raft --bench repair`, `benches/repair.rs`; load 24.0
at the start, 23.4 at the end). Protocol-Aware Recovery's workload (Alagappan et al., FAST 2018,
§5.2: 30,000 entries, a snapshot of 32 MB, so a kibibyte an entry): three voters hold 30,000
entries of 1 KiB; the third loses its last `lost` entries at rest and reopens marked. By entries,
the leader sends them again (R-5); by a snapshot, the leader has compacted its log into an image of
the state (every entry's bytes) and sends that, mantle's repair before R-5. Every message is written
in the wire format and read back as a transport carries it; the time is the repair's on one thread
from the member's reopening until its log holds the leader's, the median of five; no device, no
network: the bytes are what they would carry.

| Lost | By entries | | By a snapshot | |
|---|---|---|---|---|
| | time | bytes | time | bytes |
| 1 | 32.4 µs | 1,817 | 8,886 µs | 30,720,830 |
| 10 | 89.4 µs | 11,258 | 9,059 µs | 30,720,830 |
| 100 | 444.7 µs | 105,668 | 8,380 µs | 30,720,830 |
| 1,000 | 3,885 µs | 1,049,768 | 8,318 µs | 30,720,830 |
| 10,000 | 38,797 µs | 10,490,960 | 8,238 µs | 30,720,830 |

- One lost entry: 1.8 KB and 32 µs against 30.7 MB and 8.9 ms, a seventeen-thousandth of the bytes
  and a two-hundred-seventy-fifth of the time. PAR's stock LogCabin moved 32 MB in 1.24 s over its
  network and disk, CTRL 7 KB in 1.2 ms; the bytes here are the same order as theirs, the time is
  this machine's in-process cost alone.
- Bytes by entries grow with what was lost, about 1,050 a kibibyte entry (its record and its share
  of the append's), so they stay below the snapshot's until the member lost nearly the whole log.
  In-process time crosses near 2,000 lost entries (3.9 µs an entry carried, written and read
  against a snapshot's copies at 0.27 ns a byte); on a network or a disk the bytes decide, and the
  snapshot's time here leaves out what making and installing a real state machine's image costs.
  The leader sends a snapshot only where it compacted the entries (§5.1); whether it should choose
  one past a measured crossover on a real path is open (§14.5).

**The core did not regress.** Unmarked, R-5 adds to the core a test of an `Option` at each notice
and each heartbeat, a flag read only on a refused append's answer, and one byte in the message's
padding (it stays 144 bytes).
- **Allocations**: `hyper-raft-compare one <core> <workload> 3|5 ... 1000 1000 count`, `main` at
  `5aea1a3` and R-5, every workload in place and copying, three and five voters: allocations,
  reallocations, bytes asked for, peak and the whole loop's totals identical in all 32 cells; minor
  faults single digits a run either way, no major fault.
- **A cost found and removed.** The first build of R-5 retired 1.0–1.2 % more instructions in every
  cell, 300 an op net of set-up on the steady 64 B cell in place (24,451 against 24,149, the mean
  of three runs each). Built variant by variant: the out-of-line mark check at every write made durable cost
  about 50 (a call where a test does), the flag tested on every acknowledgement about 30, and the
  marked member's priority folded into `settle_priority`, which runs at every operation's edges,
  about 210. The check is now a test in line with the rest out of it, called once a notice; the
  flag is read inside the refusal's branch; and priority is judged in `step_vote`, where votes are.
  Then 24,204 against 24,160 in place and 30,333 against 30,277 copying (five runs each, load
  42–50): 44 and 56 instructions an op, 0.2 %.
- **Time, instructions and cycles**, `main` and R-5 interleaved in fresh processes as the L-2
  section runs them (per round the ratio R-5 to `main`; the geometric mean and its Student-t 95 %
  interval; time the loop's own, instructions and cycles the whole process's), load 49.8 at the
  start and 42.7 at the end:

  | Cell | rounds | time | instructions | cycles |
  |---|---|---|---|---|
  | steady 64 B, in place | 30 | 1.008 [0.992, 1.024] | 1.002 [1.002, 1.002] | 0.992 [0.975, 1.010] |
  | steady 64 B, copying | 30 | 1.008 [0.992, 1.023] | 1.003 [1.002, 1.004] | 1.001 [0.982, 1.021] |
  | steady 4 KiB, in place | 30 | 1.014 [0.997, 1.032] | 1.002 [1.001, 1.002] | 1.022 [1.005, 1.040] |
  | steady 4 KiB, copying | 30 | 1.009 [1.000, 1.018] | 1.001 [0.999, 1.003] | 1.002 [0.982, 1.022] |
  | transfer, in place | 30 | 1.005 [0.993, 1.017] | 1.000 [1.000, 1.001] | 1.007 [0.989, 1.026] |
  | transfer, copying | 30 | 0.993 [0.983, 1.002] | 1.000 [1.000, 1.001] | 1.005 [0.986, 1.025] |
  | steady batch 64 | 10 | 0.989 [0.938, 1.043] | 1.000 [1.000, 1.000] | 0.980 [0.935, 1.027] |
  | catch-up | 10 | 0.982 [0.961, 1.003] | 1.002 [1.001, 1.002] | 1.013 [0.985, 1.042] |
  | snapshot | 10 | 0.990 [0.933, 1.051] | 1.001 [1.001, 1.002] | 1.022 [0.999, 1.046] |
  | fast | 10 | 0.980 [0.939, 1.023] | 1.001 [1.000, 1.002] | 1.026 [0.981, 1.074] |
  | failover | 10 | 1.020 [0.971, 1.071] | 1.000 [0.999, 1.001] | 0.947 [0.898, 0.998] |

  No interval of time lies above 1. One of cycles does, the steady 4 KiB in-place cell (1.022
  [1.005, 1.040]) at a load of 42–50, with instructions 1.002 and time's interval including 1: the
  cell the R-6 section found moving 1–4 % with inert bytes alone, and the member is 48 bytes larger
  (the mark and its copy in the configuration). One of cycles lies below (failover). Not a cost
  claimed, nor a gain; the R-7 section measures it again.
- **`benches/pipeline.rs`**: every simulated figure (p50, p99, entries a second, flushes an entry,
  both devices, depths one to three) identical to `main`'s.

Commands: `cargo bench -p hyper-raft --bench repair`; `crates/hyper-raft-compare` built `--release`
in both trees; the counting and interleaved runs are a scratch harness over `hyper-raft-compare one
hyper[-copy] <workload> 3|5 <batch> <bytes> <rounds> 1000 time|count` under `/usr/bin/time -l`,
instructions an op `(I(R) − I(R/5)) / (ops(R) − ops(R/5))` at R = 40,000.

## A marked member's election (R-7)

Core step R-7 (`docs/durable.md` §5.2). Measured on 2026-10-02 on the machine above, with other
sessions' work holding the load at 42 to 57; each figure says its load. R-7 adds to the core a test
of the mark where a campaign starts and where a candidate counts itself, and changes two rules of
L-2's hand-over and restart (`docs/timing.md` §2.9); nothing on a path that replicates.

**Allocations**: identical to `main`'s on all 32 cells (three and five voters, in place and copying,
every workload).

**Time, instructions and cycles**, `main`, R-5 and R-7 interleaved in fresh processes, each round
every tree once, the order rotated and reversed; per round the ratio to `main`; the geometric mean
and its Student-t 95 % interval (load 43.5 at the start, 42.5 at the end):

| Cell | rounds | R-5 time | R-5 instr. | R-5 cycles | R-7 time | R-7 instr. | R-7 cycles |
|---|---|---|---|---|---|---|---|
| steady 64 B, in place | 30 | 1.039 [0.901, 1.199] | 1.002 [1.002, 1.003] | 0.995 [0.978, 1.012] | 0.996 [0.879, 1.127] | 1.002 [1.002, 1.002] | 1.001 [0.989, 1.012] |
| steady 64 B, copying | 30 | 0.983 [0.946, 1.022] | 1.002 [1.002, 1.003] | 0.999 [0.982, 1.016] | 0.998 [0.959, 1.040] | 1.002 [1.001, 1.003] | 0.995 [0.981, 1.008] |
| steady 4 KiB, in place | 30 | 1.017 [0.993, 1.042] | 1.001 [1.001, 1.002] | 1.029 [0.994, 1.066] | 1.028 [0.998, 1.058] | 1.001 [1.000, 1.001] | 1.000 [0.966, 1.035] |
| steady 4 KiB, copying | 30 | 1.019 [0.997, 1.041] | 1.001 [0.999, 1.003] | 1.014 [0.991, 1.036] | 1.021 [1.001, 1.041] | 0.994 [0.993, 0.996] | 1.013 [0.999, 1.028] |
| transfer, in place | 30 | 0.994 [0.962, 1.027] | 1.000 [1.000, 1.001] | 0.985 [0.961, 1.009] | 1.033 [1.000, 1.067] | 1.000 [0.999, 1.000] | 0.987 [0.967, 1.007] |
| transfer, copying | 30 | 0.993 [0.972, 1.015] | 1.001 [1.000, 1.001] | 1.006 [0.973, 1.040] | 0.996 [0.977, 1.015] | 1.000 [0.999, 1.000] | 0.988 [0.959, 1.017] |
| steady batch 64 | 10 | 0.986 [0.951, 1.023] | 1.000 [1.000, 1.000] | 0.958 [0.914, 1.005] | 0.977 [0.903, 1.057] | 1.000 [1.000, 1.001] | 1.002 [0.979, 1.025] |
| catch-up | 10 | 1.041 [0.691, 1.567] | 1.002 [1.002, 1.002] | 1.022 [1.007, 1.037] | 0.933 [0.691, 1.260] | 1.002 [1.001, 1.002] | 1.021 [1.005, 1.039] |
| snapshot | 10 | 1.274 [0.769, 2.110] | 1.002 [1.002, 1.002] | 1.019 [1.007, 1.031] | 1.244 [0.630, 2.453] | 1.001 [1.001, 1.002] | 1.026 [1.013, 1.040] |
| fast | 10 | 0.994 [0.949, 1.040] | 1.001 [1.000, 1.002] | 0.987 [0.952, 1.023] | 1.000 [0.970, 1.031] | 0.999 [0.998, 1.000] | 1.004 [0.958, 1.051] |
| failover | 10 | 0.995 [0.970, 1.021] | 1.000 [1.000, 1.001] | 1.021 [0.976, 1.069] | 1.024 [0.989, 1.061] | 1.000 [0.999, 1.001] | 0.995 [0.965, 1.025] |

Instructions an op, net of set-up, five runs each, load 42–44: steady 64 B in place `main` 24,136,
R-7 24,199 (0.26 %); copying 30,261 and 30,251. At this load the time intervals are wide (catch-up's
and snapshot's span a factor of two), and four lie above 1 or touch it: R-7's steady 4 KiB copying
(1.021 [1.001, 1.041]) and transfer in place (1.033 [1.000, 1.067]), and the cycles of catch-up and
snapshot for R-5 and R-7 alike (1.02). A second pass on those cells, 16 rounds at load 42–57, put
`main` with 48 inert bytes in the member beside them (what R-5 and R-7 add: the mark and its copy in
the configuration, the R-6 section's test of layout):

| Cell | main + 48 inert bytes: time | instr. | cycles | R-7: time | instr. | cycles |
|---|---|---|---|---|---|---|
| steady 4 KiB, in place | 0.966 [0.940, 0.994] | 1.000 [1.000, 1.001] | 1.000 [0.957, 1.044] | 1.030 [0.988, 1.075] | 1.001 [1.001, 1.002] | 1.029 [0.975, 1.085] |
| steady 4 KiB, copying | 0.986 [0.939, 1.035] | 1.004 [1.002, 1.007] | 1.011 [0.961, 1.063] | 0.995 [0.925, 1.071] | 0.995 [0.993, 0.998] | 0.989 [0.952, 1.028] |
| transfer, in place | 1.040 [0.925, 1.170] | 1.001 [1.000, 1.001] | 0.990 [0.942, 1.041] | 1.000 [0.928, 1.078] | 1.000 [0.999, 1.000] | 1.000 [0.949, 1.053] |
| catch-up | 0.885 [0.746, 1.050] | 1.000 [1.000, 1.000] | 1.001 [0.985, 1.017] | 0.897 [0.720, 1.118] | 1.001 [1.001, 1.002] | 0.996 [0.984, 1.009] |
| snapshot | 1.016 [0.757, 1.362] | 1.000 [1.000, 1.000] | 1.002 [0.986, 1.017] | 1.147 [0.798, 1.649] | 1.001 [1.001, 1.001] | 1.020 [0.998, 1.043] |

In the second pass no interval of R-7's time or cycles lies above 1, and inert bytes alone move the
4 KiB in-place cell's time by −3.4 %: the leans of the first pass did not hold, at a load where a
cell's interval spans several per cent. Instructions stay within 0.3 % of `main`'s in every cell.
Not closed: a pass at a load low enough to state these cells to 1 %, which this machine did not have
today.

**`benches/pipeline.rs`**: every simulated figure identical to `main`'s.

Commands: as R-5's; the second pass `CELLS=... RUNS=16` over the same harness, `main` built with
`inert: [u64; 6]` in `Raft`.

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

# Readies in flight (R-4): the schedules, the crash at every persistence step, and the bench.
HYPER_RAFT_SEEDS=1000 HYPER_RAFT_STEPS=4000 HYPER_RAFT_CRASH_SEEDS=40 cargo test -p hyper-raft --release --test pipeline -- --nocapture
cargo bench -p hyper-raft --bench pipeline

# The durable commit and the apply pause (R-6): the same schedules, crash enumeration and bench,
# and the comparison's counts and times against main built the same way.
HYPER_RAFT_SEEDS=1000 HYPER_RAFT_STEPS=4000 HYPER_RAFT_CRASH_SEEDS=40 cargo test -p hyper-raft --release --test pipeline -- --nocapture
$B one hyper steady 3 1 64 20000 1000 count

# Repair by entries (R-5): the schedules with faults at rest, the repair's cost, and the
# comparison's counts and times against main built the same way.
HYPER_RAFT_SEEDS=1000 HYPER_RAFT_STEPS=4000 HYPER_RAFT_CRASH_SEEDS=40 cargo test -p hyper-raft --release --test pipeline --test repair -- --nocapture
cargo bench -p hyper-raft --bench repair

# A marked member's election (R-7): the same schedules, faults at rest on two members at once
# among them, and the comparison of main, R-5 and R-7 interleaved.
HYPER_RAFT_SEEDS=1000 HYPER_RAFT_STEPS=4000 HYPER_RAFT_CRASH_SEEDS=40 cargo test -p hyper-raft --release --test pipeline --test repair --test suspicion -- --nocapture

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
Since its timing is measured (`docs/timing.md` §2.7, ORIGIN change 5), a hyper-swim period does
more than slates': it is polled at its own deadline, takes the round trip it is answered in into
the pair's NFD-E estimator (and the pool's, while the pool judges), and reconfigures the pair when
its estimates renew; on a LAN-like link whose variance is under `G²` that is every probe.

## The machine

The same Apple M5 Max, macOS 26.4.1, rustc 1.98.0, shared with other sessions. The allocation
counts are exact; the times are medians of seven fresh processes, with the least and the most
beside them.

## The workload

`N` detectors in one process run whole periods as a member's driver does (`tests/cluster.rs`),
each on its own simulated clock: it is polled at the wake its detector asks, 50 µs late (Linux's
default timer slack), which ends its period and starts the next; its ping goes through the wire
codec to its target, which applies the gossip and answers with an acknowledgement carrying its
own gossip and coordinate; the acknowledgement lands a round trip of 200 to 300 µs later and the
prober applies it and measures it. A message carries the gossip that fits a 1,200-byte datagram
beside an acknowledgement, 60 entries, and each update is sent SWIM §4.1's budget, as
`tests/cluster.rs` derives both; slates' detector is given the same. Counting starts once every
pair is configured by its own estimator. Quiet has no membership changes; churning has one member
refute a suspicion every period, so its new incarnation spreads.

## Allocations

`cargo bench -p hyper-swim --bench allocs`, 400 periods, the calling thread's count (the detector
has no threads), per member per period, 2026-10-01 at 23:18 PDT (load 36.5):

| Members | Workload | Allocations, slates as ported | Reallocations, as ported | Allocations now | Reallocations now |
|---|---|---|---|---|---|
| 4 | quiet | 6.67 | 8.00 | 0 | 0 |
| 4 | churning | 12.56 | 8.76 | 0.00 (0.9 B a period) | 0 |
| 16 | quiet | 6.13 | 8.26 | 0 | 0 |
| 16 | churning | 12.14 | 10.46 | 0.00 (0.1 B a period) | 0 |
| 64 | quiet | 6.03 | 8.12 | 0 | 0 |
| 64 | churning | 13.37 | 11.91 | 0.00 (0.2 B a period) | 0 |
| 256 | quiet | 7.52 | 8.95 | 0 | 0 |
| 256 | churning | 14.74 | 12.91 | 0 | 0 |

Minor page faults were below 0.001 a member a period throughout. The churning rows that are not
exactly zero are a queue growing once, within the first counted periods, to the most reports a
member has held. A pair's estimator is built, boxed with its ring, when the peer first answers,
and the pool's when the member first measures a round trip: allocations at a join, not in a
period. What each earlier change removed is in `crates/hyper-swim/ORIGIN.md`, change 4.

## Against slates

`crates/hyper-swim-compare`, a workspace of its own, runs the workload above on both detectors,
each through its own API (7 runs of 400 periods each), 2026-10-01 at 23:18–23:24 PDT, load 36.6
before and 32.1 after (other sessions):

| Members | Workload | Detector | ns a member a period | Allocations a member a period |
|---|---|---|---|---|
| 16 | quiet | hyper | 413 (358–422) | 0 |
| 16 | quiet | slates | 511 (467–604) | 6.13 |
| 16 | churning | hyper | 627 (563–651) | 0 |
| 16 | churning | slates | 920 (859–1,027) | 11.24 |
| 64 | quiet | hyper | 648 (596–902) | 0 |
| 64 | quiet | slates | 700 (591–748) | 6.03 |
| 64 | churning | hyper | 934 (910–973) | 0 |
| 64 | churning | slates | 1,199 (1,147–1,283) | 11.43 |
| 256 | quiet | hyper | 1,384 (1,172–3,983) | 0 |
| 256 | quiet | slates | 1,489 (1,328–3,464) | 6.01 |
| 256 | churning | hyper | 2,030 (1,855–2,362) | 0 |
| 256 | churning | slates | 2,141 (2,094–2,247) | 11.63 |

hyper-swim's median is below slates' at every point, 5 to 32 % less: ahead at 16 members and at 64
churning, even at 64 quiet and both 256-member rows, where the ranges overlap. It allocates nothing
where slates allocates 6 to 11.6 times a period. Two earlier runs of the same table during the work
(load 20–45) had the same order. Before the timing was measured hyper-swim was 2.8 to 5.9 times faster; the difference is the estimation
and the configuration, which slates does not do. Two changes kept it ahead, measured on the 16- and
64-member quiet points (load 30–39): configuring the pool only when a probe needs it and feeding it
only while it judges took 16 members from 575 to 430 ns; boxing each pair's estimator, whose Allan
levels are a kilobyte, so that a peer's other fields stay small and together, took 64 members from
815 to 678 ns (a profile had shown the period's time in cache misses on the peers' map, not in
arithmetic). A configuration is about 175 ns of the 16-member period: a golden-section search of
`detector_at`, made each time a pair's window renews.

## The cluster test

`tests/cluster.rs`, four member processes over hyper-datagram on loopback UDP, each run a fresh
supervisor; the detector as the library configures it, the supervisor waiting on every member
judging every peer by a configured verdict (its own or the pool's), then on every survivor holding
the killed member dead. "Suspicions" and
"condemnations" are of live members, summed over the cluster and the runs, against Theorem 7's
allowance `Σβ` the configured detectors stated; "detection" is from the victim's last answer to
each survivor holding it dead, on the survivor's clock, against the bound each stated.

Each run is counted on the final code, 2026-10-01 at 23:15–23:18 PDT, the macOS and Linux series
running at the same time (Docker Desktop's VM shares the Mac's cores, so each loaded the other).
A run passes when every survivor holds the victim dead within the bound it stated, and neither the
suspicions nor the condemnations of live members refute their allowance (the 95 % rule).

| Platform | Limits and competing load | Runs | Passed | Load (1 min) | Suspicions (allowed) | Condemnations (allowed) | Detection median / p95 / max | Stated bound median / max |
|---|---|---|---|---|---|---|---|---|
| macOS 26.4.1, M5 Max | none added | 400 | 400 | 26.6–28.2 | 0 (407) | 0 (277) | 3.8 / 9.2 / 18.3 ms | 13.3 / 53.0 ms |
| macOS 26.4.1, M5 Max | 24 busy loops on 18 cores | 300 | 300 | 33.9–36.4 | 0 (349) | 0 (247) | 3.7 / 5.2 / 7.3 ms | 12.7 / 47.8 ms |
| Linux 6.12.76 (Docker Desktop), rust:1.98.0 | `--cpus 1`, 2 busy loops | 500 | 500 | 4.2–4.6 | 444 (1,887) | 41 (1,224) | 17.0 / 93.9 / 196.4 ms | 54.5 / 2,074.6 ms |
| Linux 6.12.76 (Docker Desktop), rust:1.98.0 | `--cpus 2`, 4 busy loops | 500 | 500 | 3.7–4.4 | 168 (4,129) | 21 (2,892) | 18.2 / 86.7 / 200.0 ms | 57.1 / 1,914.2 ms |
| Linux 6.12.76 (Docker Desktop), rust:1.98.0 | `--cpus 4`, beside macOS's 24 busy loops | 300 | 300 | 3.4–3.8 | 91 (781) | 71 (594) | 16.0 / 22.4 / 44.7 ms | 47.6 / 261.6 ms |

The kill comes as soon as every member judges every peer, which on loopback is within a few hundred
milliseconds, mostly by the pool: at the kill a mean of 0.0 to 0.3 of the twelve pairs had their
own verdict. The allowance is loose: it is dominated by the loss term, Jeffreys' `(k + ½)/(m + 1)`
and the chance `1/(m + 1)` of a delay past everything a history of `m` has seen, which a run that
young keeps near a percent a probe; it tightens as `1/m`. The suspicions and condemnations of live
members are the throttled series', where every member stalls at once; each condemned member was
told and refuted, and came back.

**What the runs found.** Each earlier version of the code and of the test was run the same way, and
every failure was traced to its cause and fixed, with a test that fails without the fix where the
cause is in the library (`docs/timing.md` §2.7 records each):
- members holding one another dead, with nobody left to probe, never sent again (`--cpus 1`):
  the isolation rule, `members_that_hold_each_other_dead_heal`;
- every member's measurement probe lost to a throttled burst, all waiting for one another for ever
  (`--cpus 2`): the measurement period ends at its expected arrival,
  `a_lost_measurement_probe_ends_at_its_expected_arrival`;
- the kill waited on every pair's own estimator, which under a throttle can refuse for as long as
  the round trips stay correlated: the kill now waits on every peer judged;
- one run's count against its expected-count bound is not a promise: the 95 % rule, after twelve
  runs in a hundred at `--cpus 1` failed so while the series kept far inside its bound;
- the condemnation allowance took two consecutive probes as independent where `τ_int` was one:
  refuted twice in three hundred at `--cpus 1`, now Fréchet's bound;
- the detection bound, five times: the longest period run so far (one macOS run in a hundred), one
  period for the answer from another member (four Linux runs in four hundred), the periods ended
  only (seven in three hundred), a verdict dropped when a renewal was refused
  (`a_refused_reconfiguration_leaves_the_verdict_in_force`), a suspicion re-adopted resetting its told
  probes (`a_suspicion_adopted_again_keeps_its_told_probes`), and the current round's size for `m`
  (`the_detection_bound_does_not_shrink_with_the_round`);
- in the test itself: a death adopted by gossip was noted only at the next period's report, and a
  member's last answer was recorded only for a probe it still held outstanding.

## Commands for the detector

```sh
# The suites and the four-process kill test.
cargo test -p hyper-swim

# Allocations a member a period.
cargo bench -p hyper-swim --bench allocs

# The comparison.
cd crates/hyper-swim-compare && cargo build --release
target/release/hyper-swim-compare table 7 400

# The cluster test, repeated (the binary from `cargo test -p hyper-swim --test cluster --no-run`):
# each run a fresh supervisor, the load average read before it. In Linux, the same in rust:1.98.0
# with the repository at /src, `docker update --cpus N` between series, and `2N` busy loops
# (`while :; do :; done`) running beside it.
for i in $(seq 1 100); do
  sysctl -n vm.loadavg
  target/debug/deps/cluster-<hash> --exact \
    a_killed_member_is_declared_dead_by_every_survivor_and_no_live_one_is --nocapture
done
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
`docs/timing.md` §2.6. `crates/hyper-timing-trace`, a workspace member the gates lint and test on all
six targets (the recorder runs on Linux and macOS and refuses elsewhere), records two processes
exchanging sequence-numbered heartbeats over UDP on loopback. The sender sleeps to each scheduled
time `σ_i = start + iη` (absolute, so lateness never accumulates into the schedule), optionally
writes one block of a log file and fully flushes it (`F_FULLFSYNC` on macOS, `fdatasync` on Linux),
and sends `(i, σ_i, began waiting, woke, sent, realtime at send)`. The receiver, with the kernel's
receive timestamps on (`SO_TIMESTAMP_MONOTONIC`, `SO_TIMESTAMPNS`), waits in `select(2)` on macOS
and `ppoll(2)` on Linux (one timer path in the kernel's `fs/select.c`; `src/sys.rs`) until the
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
cargo build --release -p hyper-timing-trace
B=target/release/hyper-timing-trace
$B timer <dir>/flush.log                        # the sweeps
$B run <dir>/mac-plain 100 300                  # interval µs, seconds
$B run <dir>/mac-flush 10000 600 <dir>/flush.log
$B analyse <dir>/mac-plain <dir>/mac-flush      # the tables above
# The loaded runs: the same, while a loop rebuilds the workspace from clean in another target:
#   while :; do CARGO_TARGET_DIR=<dir>/load CARGO_BUILD_JOBS=4 cargo build --workspace --all-targets; rm -rf <dir>/load; done
# Linux, from the repository root:
docker run --rm -v "$PWD":/work -w /work \
  -v trace-target:/work/target -v trace-disk:/disk rust:1.98.0 bash -c \
  'cargo build --release -p hyper-timing-trace && B=target/release/hyper-timing-trace &&
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

# The durable shell against mantle's

`crates/hyper-durable-compare` (a workspace of its own, like the other comparisons) runs mantle's
replica workload through both shells in one process: a range group of 1, 3 or 5 members, each on a
log of its own, member 1 leading, committing one entry at a time through the leader (closed loop),
each entry a mantle `wire::Entry` applied by mantle's engine (`Model`) and its Name layer
(`apply_entry`) on both sides, so the shells and what they drive the core and the log with are what
differs.

- **mantle 1c179e8**: mantle's range `Replica` at origin/dev `1c179e8` (its commit fence), over the
  hyper-raft, hyper-log and hyper-block mantle vendors there, driven as its node drives it: every
  member `begin`s its ready, the messages are delivered, every member `wait_persisted`s.
- **hyper-durable**: this branch's `Replica` over `GroupStore` on this repository's hyper-log, with
  mantle's engine and layer as its `StateMachine`, held by one `Owner` and driven by its turns,
  woken by the logs' answers; readies taken ahead to the log's depth (three).

Both logs run mantle's group-test configuration with the writer's measured waits. `file` is a real
file on this machine's SSD, direct I/O and `F_FULLFSYNC`; `sim` is hyper-block's simulated device,
whose flush costs nothing. Each point runs five rounds, the two shells alternating with the order
rotated each round, each run a fresh group on fresh logs: 50 entries to warm, 300 measured.
Latencies are pooled over the rounds; the other columns are per committed entry, medians over the
rounds: frames flushed by every member's log, the process's allocations and reallocations (every
thread), minor and major page faults, context switches, and the threads alive. Apple M5 Max (18
cores, 128 GiB), macOS 26.4.1, 2026-10-02 02:55–03:15 PDT, other sessions building; the load
average is beside each point (1, 5 and 15 minutes, before → after).


**File, register entries, 1 member(s); load 12.70 8.67 12.74 → 10.99 8.65 12.59**

| shell | p50 µs | p99 µs | p99.9 µs | entries/s | flushes | allocations | reallocations | faults | switches | threads |
|---|---|---|---|---|---|---|---|---|---|---|
| mantle 1c179e8 | 8573 | 18957 | 21283 | 96 | 1.00 | 38.4 | 9.05 | 0.00 | 17.7 | 3 |
| hyper-durable | 8507 | 14731 | 95632 | 115 | 1.00 | 30.4 | 9.05 | 0.00 | 15.1 | 3 |

**File, register entries, 3 member(s); load 10.99 8.65 12.59 → 23.09 12.15 13.19**

| shell | p50 µs | p99 µs | p99.9 µs | entries/s | flushes | allocations | reallocations | faults | switches | threads |
|---|---|---|---|---|---|---|---|---|---|---|
| mantle 1c179e8 | 50079 | 98295 | 271506 | 20 | 6.00 | 135.1 | 21.16 | 0.00 | 117.4 | 7 |
| hyper-durable | 36384 | 56620 | 198911 | 26 | 5.73 | 100.7 | 21.22 | 0.00 | 135.3 | 7 |

**File, register entries, 5 member(s); load 23.09 12.15 13.19 → 39.32 25.69 18.81**

| shell | p50 µs | p99 µs | p99.9 µs | entries/s | flushes | allocations | reallocations | faults | switches | threads |
|---|---|---|---|---|---|---|---|---|---|---|
| mantle 1c179e8 | 60909 | 132943 | 375740 | 15 | 10.00 | 225.8 | 33.27 | 0.00 | 228.1 | 11 |
| hyper-durable | 46389 | 78367 | 290526 | 21 | 9.83 | 168.5 | 33.61 | 0.00 | 228.8 | 11 |

**File, put-1KiB entries, 1 member(s); load 39.32 25.69 18.81 → 35.95 26.19 19.25**

| shell | p50 µs | p99 µs | p99.9 µs | entries/s | flushes | allocations | reallocations | faults | switches | threads |
|---|---|---|---|---|---|---|---|---|---|---|
| mantle 1c179e8 | 8559 | 23281 | 34940 | 106 | 1.00 | 26.0 | 10.04 | 0.00 | 15.6 | 3 |
| hyper-durable | 8520 | 26734 | 154405 | 99 | 1.00 | 18.0 | 10.04 | 0.00 | 15.1 | 3 |

**File, put-1KiB entries, 3 member(s); load 35.95 26.19 19.25 → 31.34 28.17 21.16**

| shell | p50 µs | p99 µs | p99.9 µs | entries/s | flushes | allocations | reallocations | faults | switches | threads |
|---|---|---|---|---|---|---|---|---|---|---|
| mantle 1c179e8 | 45014 | 111936 | 309714 | 22 | 6.00 | 96.1 | 12.13 | 0.00 | 105.9 | 7 |
| hyper-durable | 34736 | 47489 | 283334 | 28 | 5.78 | 61.6 | 12.19 | 0.00 | 107.4 | 7 |

**File, put-1KiB entries, 5 member(s); load 31.34 28.17 21.16 → 10.03 22.24 20.30**

| shell | p50 µs | p99 µs | p99.9 µs | entries/s | flushes | allocations | reallocations | faults | switches | threads |
|---|---|---|---|---|---|---|---|---|---|---|
| mantle 1c179e8 | 58496 | 107002 | 354744 | 17 | 10.00 | 160.2 | 14.22 | 0.00 | 215.5 | 11 |
| hyper-durable | 43442 | 69734 | 288825 | 22 | 9.82 | 102.8 | 14.47 | 0.00 | 230.9 | 11 |

**Sim, register entries, 1 member(s); load 10.03 22.24 20.30 → 10.03 22.24 20.30**

| shell | p50 µs | p99 µs | p99.9 µs | entries/s | flushes | allocations | reallocations | faults | switches | threads |
|---|---|---|---|---|---|---|---|---|---|---|
| mantle 1c179e8 | 7 | 21 | 92 | 121751 | 1.00 | 44.4 | 9.07 | 0.00 | 3.0 | 3 |
| hyper-durable | 7 | 21 | 33 | 131459 | 1.00 | 36.4 | 9.07 | 0.01 | 3.0 | 3 |

**Sim, register entries, 3 member(s); load 10.03 22.24 20.30 → 10.03 22.24 20.30**

| shell | p50 µs | p99 µs | p99.9 µs | entries/s | flushes | allocations | reallocations | faults | switches | threads |
|---|---|---|---|---|---|---|---|---|---|---|
| mantle 1c179e8 | 54 | 155 | 403 | 17461 | 6.00 | 171.2 | 21.20 | 2.65 | 16.5 | 7 |
| hyper-durable | 34 | 166 | 237 | 26157 | 5.55 | 130.3 | 21.36 | 2.66 | 14.0 | 7 |

**Sim, register entries, 5 member(s); load 10.03 22.24 20.30 → 10.03 22.24 20.30**

| shell | p50 µs | p99 µs | p99.9 µs | entries/s | flushes | allocations | reallocations | faults | switches | threads |
|---|---|---|---|---|---|---|---|---|---|---|
| mantle 1c179e8 | 74 | 220 | 516 | 12801 | 10.00 | 286.0 | 33.33 | 4.65 | 26.0 | 11 |
| hyper-durable | 49 | 102 | 252 | 18936 | 9.12 | 219.5 | 33.61 | 4.54 | 22.9 | 11 |

**Sim, put-1KiB entries, 1 member(s); load 10.03 22.24 20.30 → 10.03 22.24 20.30**

| shell | p50 µs | p99 µs | p99.9 µs | entries/s | flushes | allocations | reallocations | faults | switches | threads |
|---|---|---|---|---|---|---|---|---|---|---|
| mantle 1c179e8 | 7 | 20 | 173 | 117948 | 1.00 | 32.0 | 10.06 | 0.00 | 3.0 | 3 |
| hyper-durable | 7 | 18 | 30 | 118505 | 1.00 | 24.0 | 10.06 | 0.00 | 3.0 | 3 |

**Sim, put-1KiB entries, 3 member(s); load 10.03 22.24 20.30 → 10.03 22.24 20.30**

| shell | p50 µs | p99 µs | p99.9 µs | entries/s | flushes | allocations | reallocations | faults | switches | threads |
|---|---|---|---|---|---|---|---|---|---|---|
| mantle 1c179e8 | 53 | 149 | 420 | 17507 | 6.00 | 132.2 | 12.17 | 2.66 | 16.3 | 7 |
| hyper-durable | 32 | 146 | 230 | 28119 | 5.52 | 90.3 | 12.31 | 2.66 | 13.6 | 7 |

**Sim, put-1KiB entries, 5 member(s); load 10.03 22.24 20.30 → 10.03 22.24 20.30**

| shell | p50 µs | p99 µs | p99.9 µs | entries/s | flushes | allocations | reallocations | faults | switches | threads |
|---|---|---|---|---|---|---|---|---|---|---|
| mantle 1c179e8 | 73 | 214 | 510 | 12929 | 10.00 | 220.3 | 14.28 | 4.65 | 25.7 | 11 |
| hyper-durable | 49 | 100 | 244 | 19066 | 9.13 | 153.2 | 14.60 | 4.60 | 23.5 | 11 |

- **Faster with more than one member**: on the device p50 23–27% lower (36.4 against 50.1 ms at
  three members, 46.4 against 60.9 at five), p99 35–58% lower, entries a second 27–40% higher; on
  the simulated device p50 33–40% lower and entries a second 47–61% higher, p99 even at three
  members (166 against 155 µs with register entries, 146 against 149 with 1 KiB) and half at
  five. A follower's next append no longer waits for its last write's answer, and a leader's own
  write overlaps its followers'. With one member p50 is even (one flush an entry for both: the
  commit rides the entry's own write, `commit = last`).
- **Fewer flushes**: 5.5–5.8 frames an entry at three members against 6.0, 9.1–9.8 at five against
  10.0: frames carry more than one write of a member.
- **Fewer allocations**: 18–36% fewer at every point (100.7 against 135.1 at three members on the
  device, register entries; 61.6 against 96.1 with 1 KiB entries): the store encodes each entry
  once into its frame from where the core holds it, a write is not cloned per part, and the state
  machine applies from the log's own buffers.
- **Losses, open before mantle switches (`CLAUDE.md` §1a):**
  - *Reallocations*, 0.06–0.34 an entry more (21.22 against 21.16 at three members; 33.61 against
    33.27 at five). They are the driving thread's (the shell, the core, the store, the state
    machine), not the logs': split by thread on the simulated device at three members they are
    21.09 against 21.03 at depth one and 21.14 at depths two and three. So about half comes with
    taking readies ahead (the core's unstable log keeps entries until their notice, R-4) and half
    is there at depth one; neither is traced to its site yet.
  - *Context switches on the device* at three and five members (135 against 117 an entry with
    register entries at three; even at five). On the simulated device hyper-durable switches less
    (14.0 against 16.5). The device runs rose from load 11 to 39 across them; to be measured again
    with the runs interleaved by point, and traced.
  - *One member on the device*: p99.9 96 ms against 21 with register entries and 154 against 35
    with 1 KiB, and with 1 KiB entries p99 26.7 against 23.3 ms and 99 entries a second against
    106: a few flushes of 300 a round under load 10–40, with the p50 even; not traced yet.

```
# From the repository root: the comparison is a workspace of its own and fetches mantle at the
# revision in crates/hyper-durable-compare/Cargo.toml.
cd crates/hyper-durable-compare && CARGO_BUILD_JOBS=4 cargo build --release
./target/release/hyper-durable-compare --rounds 5 --entries 300 --warm 50
# The driving thread's counts at depths one to three (the split above).
for d in 1 2 3; do HYPER_DURABLE_DEPTH=$d ./target/release/hyper-durable-compare \
  --devices sim --members 3 --shapes register --rounds 3 --entries 300; done
```

focal's and slates' comparisons are the next steps: focal's needs `DurableNode`'s workloads
(`cargo bench -p focal-consensus --bench commits`) after F-1 converts its WAL to hyper-log, and a
`StateMachine` over focal's control and data groups with `acts_at_start` true for the control
groups; slates' needs a `LogStore` over its anchor publication (`RamStore` is its shape, depth one)
with `SavedRaft` as what the core's `Storage` reads, measured on `publication_cost`'s workload
against its own path at `ec5e0df`.


## The three losses against mantle's shell, traced (2026-10-02, 03:30–04:30 PDT)

Each loss of the section above, root-caused in the same runs against mantle `1c179e8`. Same machine.
The load average fell from 28 to 2 over the hour, and each table says the load it ran at.

**Reallocations.** Every reallocation's call site was recorded with a tracing allocator: a
`GlobalAlloc` over `System` that captures a backtrace for each reallocation of an 8-aligned
block, on every thread, during the measured entries. It was a scratch build, not committed: its
`unsafe` is outside the contract script's list. Byte buffers were sampled one in 300, because
capturing every one slowed the run until the difference vanished; the difference depends on
timing. All of the byte-buffer sites are mantle's engine and layer, and they are identical for
both shells. Simulated device, three members, 4 × 3,000 entries; reallocations a committed entry
at the sites only hyper-durable has:

| site | per entry | whose |
|---|---|---|
| `hyper_raft::log::Unstable::truncate_and_append` | 0.09–0.16 | the core |
| `hyper_raft::raft::Outgoing::push_counted` | 0.01–0.02 | the core |
| `hyper_durable::hyperlog::update_of` | 0.002–0.007 | the shell |

- **The shell's site is fixed.** The store collected a write's entries through a `Result`, so the
  iterator gave no lower bound and the list grew by reallocation. Both lists are now reserved at
  their exact length. Traced after the fix, the site is gone.
- **The core's two sites are a consequence of taking readies ahead (R-4).**
  - When nothing is unstable, `truncate_and_append` adopts the incoming exact-sized `Vec` and
    drops the grown one, so the next proposal appended while a write is out reallocates.
  - When the notice of a write makes everything durable, `take_stable_to` hands the whole `Vec`
    away (`mem::take`), so the capacity is lost again.
  - `Outgoing::take` gives the queue's `Vec` to the owner and starts again from four slots, so a
    `Ready` that carries more messages, which pipelining makes more likely, grows it.
- **The proof.** Driving the members as mantle's node does (`HYPER_DURABLE_ROUNDS=1`: every write
  answered before the next turn, so nothing is taken ahead) gives 21.16 an entry, mantle's
  number. The same holds at depth one.
- **An experiment on the core, reverted.** In `truncate_and_append`, reuse the retained capacity
  when it holds the incoming entries; in `take_stable_to`, drain what became durable in place
  rather than hand the `Vec` away. With both, the driving thread's reallocations were 21.02
  against mantle's 21.02 at three members, and 33.03 against 33.02 at five, with the pipeline on.
  Those runs counted 5.6 more allocations an entry, which is the experiment's environment-variable
  switch (an `OsString` a call). It has not been measured with the switch removed.
- **The change asked of the core** (`crates/hyper-raft/src` is another agent's; not made here):
  1. `Unstable::truncate_and_append`, with `kept == 0`: when `self.entries.capacity() >=
     entries.len()`, `clear` and `append` instead of replacing the `Vec`.
  2. `Log::take_stable_to`, the full-take branch: for `RawNode::on_persist` (an owner that does
     not keep the entries), drop what became durable in place with `drain(..count)` instead of
     `mem::take`. `on_persist_keeping` keeps its zero-copy hand-off.
  3. `Outgoing::take` (0.01–0.02): an alternative that swaps the queue with an owner's emptied
     buffer, so neither side's capacity is lost.
- **Made in the core** (2026-10-02, 05:20–05:40 PDT, load 3.7–4.2), each as asked:
  1. `truncate_and_append` with nothing kept appends into the log's own emptied vector when it has
     the room, and takes the incoming one only when it does not.
  2. `take_stable_to` takes `Option<&mut Vec<Entry>>`: `on_persist` and `advance_append` (no `keep`)
     drop what became durable in place, `clear` or `drain`, so the vector keeps its room;
     `on_persist_keeping` and `advance_append_keeping` hand it off as before.
  3. `RawNode::recycle_messages(emptied)`: an owner gives back the vector a `Ready` or `LightReady`
     gave its messages in; it is the member's queue at once if nothing waits, else the next
     `take`'s, kept only where it has more room than what it replaces and no more than
     `Limits::pending_messages` slots (`Outgoing::resident_bytes` counts it). hyper-durable does
     not call it yet, so the runs below do not show it.

  Simulated device, register, 5 rounds of 3,000 entries, the core before and after in alternate
  runs, two each (`--devices sim --members 3,5 --shapes register --rounds 5 --entries 3000`);
  mantle `1c179e8` in the same runs:

  | members | shell | driver's reallocs/entry | driver's allocs/entry | process reallocs/entry |
  |---|---|---|---|---|
  | 3 | mantle 1c179e8 | 21.00; 21.00; 21.00; 21.00 | 135.0 | 21.29 |
  | | hyper-durable, core before | 21.11; 21.12 | 99.6; 99.7 | 21.37; 21.38 |
  | | hyper-durable, core after | 21.01; 21.01 | 99.4; 99.5 | 21.27; 21.27 |
  | 5 | mantle 1c179e8 | 33.01 (all four) | 225.7 | 33.48 |
  | | hyper-durable, core before | 33.21; 33.22 | 166.0; 166.0 | 33.62; 33.63 |
  | | hyper-durable, core after | 33.02; 33.02 | 165.7; 165.7 | 33.43; 33.43 |

  The driving thread's reallocations are mantle's to 0.01, the 0.01 that the third change leaves
  for the shell to take up; the process's are below mantle's. Allocations fell by 0.2–0.3 an
  entry: the 5.6 more of the earlier experiment were its environment-variable switch, not the
  change. `hyper-raft-compare ... count`, every workload, in place and copying, three voters:
  allocations, reallocations and bytes unchanged (no reallocation in either), since that harness
  finishes each write before the next.

**Context switches.** `/usr/bin/time -l` on one shell a process (file device, three members,
three rounds of 300, two runs each, load 2–7):

| shell | voluntary | involuntary | user + sys s | wall s |
|---|---|---|---|---|
| mantle 1c179e8 | 100,106; 99,449 | 25,810; 25,359 | 0.99; 0.90 | 55.9; 55.7 |
| hyper-durable | 98,564; 113,543 | 32,737; 34,342 | 0.92; 1.29 | 32.9; 37.1 |

- **Writes and wakes are exact.** `HYPER_DURABLE_DIAGNOSIS=1` counts 5.70–5.81 writes and the
  same number of log wakes an entry at three members. There are no writes of the commit alone,
  no quiet writes and no parts.
- **The extra switches are involuntary:** +7,000 to +8,500 a run, about +30%. Voluntary switches
  are even within the runs' spread, and CPU time is even.
- **They are the overlap's.** Driven as mantle's node drives (`HYPER_DURABLE_ROUNDS=1`),
  hyper-durable switches within 2% of mantle (129.1 against 126.6 an entry). It also loses its
  latency then: 53.4 ms p50 against 53.2, 6.00 flushes an entry. Taking readies ahead keeps a
  leader's log and its followers' logs (seven threads) runnable at once, and the OS preempts more
  among them.
- **Not fixed in the shell.** No shell change removes these switches without giving up that
  overlap. Trading the 35% latency for an even count of switches is not a fix, and per entry the
  CPU time is no higher. A scheduler-level cause (wakeup preemption on Apple silicon) is not
  measured further here.

**The one-member tail** does not reproduce. Nine rotated rounds of 500 entries, load 34 → 2,
real disk:

| point | shell | p50 µs | p99 µs | p99.9 µs | entries/s |
|---|---|---|---|---|---|
| register, 1 member | mantle 1c179e8 | 19,088 | 30,910 | 269,923 | 50 |
| | hyper-durable | 19,279 | 32,071 | 260,213 | 49 |
| put 1 KiB, 1 member | mantle 1c179e8 | 8,542 | 21,446 | 165,243 | 102 |
| | hyper-durable | 8,520 | 21,083 | 78,537 | 102 |

- Per entry both shells make one write, one flush and one wake. hyper-durable writes no commit of
  its own, because a sole voter's `commit = last` rides its append. `shell.rs`
  (`a_sole_voter_logs_its_commit_in_the_write_of_its_entries`) now also asserts that 32 entries
  make 32 writes and no write of the commit alone.
- The earlier tail was a few flushes of 1,500 at loads 10–40.
- None of the candidates is in play: no commit-only write, a lone frame's confirmation that both
  logs pay alike, and no third write, since one member's closed loop has one write out.

```
cd crates/hyper-durable-compare && cargo build --release
HYPER_DURABLE_DIAGNOSIS=1 ./target/release/hyper-durable-compare --devices file --members 1,3 \
  --shapes register,put --rounds 9 --entries 500
HYPER_DURABLE_ROUNDS=1 ./target/release/hyper-durable-compare --devices file --members 3 \
  --shapes register --rounds 5 --entries 300
for o in mantle hyper; do HYPER_DURABLE_ONLY=$o /usr/bin/time -l ./target/release/hyper-durable-compare \
  --devices file --members 3 --shapes register --rounds 3 --entries 300; done
```

# hyper-liveness: node-pair heartbeats against per-group heartbeats and slates' detector

`crates/hyper-liveness` (`docs/timing.md` §2.8), timing step L-3. Measured on 2026-10-02: an Apple
M5 Max, macOS 26.4.1, rustc 1.98.0, release builds, shared with other sessions (load average 22–28
throughout, recorded beside each run); Linux in Docker Desktop's VM (linuxkit 6.12.76, aarch64,
`rust:1.98.0`), its load recorded by each soak.

## What a node pays a second

`cargo bench -p hyper-liveness --bench cost`. Per node per second, messages sent and received and
the CPU time of the calls, for `G` groups a node over `P` peers, each design put at the same
interval between heartbeats on a pair, 50 ms (the macOS trace's configured `η`, `docs/timing.md`
§2.6), so each detects as fast:
- **pair**: this crate, `P + 1` nodes on one simulated clock (`benches/support/world.rs`: seeded LAN
  delays, `fdatasync`-sized flushes, wakes 50–100 µs late), each pair sharing the node's groups;
  its measured cost per heartbeat times `P` sent and `P` taken each interval; `flushes` the liveness
  writes an interval the idle node asked for, shared by its pairs.
- **group**: the Raft core mantle and focal run (`hyper-raft`, which mantle vendors and into which
  focal's core changes are ported), `election_tick` 10 and `heartbeat_tick` 2 (focal-consensus's
  defaults), `G` groups a node of three voters placed round robin, every group with its leader and
  nothing to replicate; ticks, readies and deliveries timed, a tick every 25 ms.
- **swim**: slates' detector (`hyper-swim`), `P + 1` members, a member's round of `P` periods every
  50 ms; the detector's calls only, each member judging every peer by a configured verdict.

| P | G | pair msgs | pair µs | liveness flushes / η | group msgs | group µs | swim msgs | swim µs |
|---|---|---|---|---|---|---|---|---|
| 2 | 1 | 80 | 12.0 | 1.11 | 53 | 3.6 | 160 | 12.5 |
| 2 | 64 | 80 | 11.4 | 1.11 | 3,413 | 291.4 | 160 | 12.5 |
| 2 | 1,024 | 80 | 11.1 | 1.11 | 54,613 | 5,209.4 | 160 | 12.5 |
| 8 | 1 | 320 | 60.6 | 1.53 | 53 | 3.4 | 640 | 50.3 |
| 8 | 64 | 320 | 59.6 | 1.53 | 3,413 | 304.7 | 640 | 50.3 |
| 8 | 1,024 | 320 | 60.4 | 1.53 | 54,613 | 5,900.3 | 640 | 50.3 |

- The pair stream's cost does not move with `G`: its messages are `2P/η` and its work per
  heartbeat is the same whatever the groups (the counts below are identical at one group a pair and
  a thousand).
- Per-group heartbeats grow linearly with `G`, about 4.6 µs and 53 messages a second per group. The
  pair stream is cheaper from about 3 groups a node at two peers and 13 at eight, and 18 to 470
  times cheaper at 1,024. At one group a node per-group heartbeats cost less (3.5 µs against 12 and
  60): the stream's estimator and configurator are what a group's tick does not do.
- Against slates' detector the stream is even: a SWIM member probes and answers, two messages a
  peer a round as the stream's two, at 12.5 against 12.0 µs (two peers) and 50.3 against 60.6
  (eight). The stream's row includes its codec; the detector's does not include its gossip, codec or
  coordinates.
- What the stream adds that neither does: an idle node's liveness write and flush, 1.1 to 1.5 an
  interval, shared by all its pairs; a node whose groups write makes none.

## Allocations and time a heartbeat

`cargo bench -p hyper-liveness --bench allocs`, the same world, counted over 10 s of simulated time
after every pair is configured and 10 s more; per heartbeat sent or taken, with the time of the
crate's calls (which includes the configurations the doubling schedule makes in that span; the
bench then also counted the warm-up's calls against the window's heartbeats, a defect found and
fixed below, "The cost of a heartbeat, settled"):

| nodes | groups a pair | sent | taken | allocs | reallocs | bytes | minor faults | ns |
|---|---|---|---|---|---|---|---|---|
| 2 | 1 | 282 | 282 | 0 | 0 | 0 | 0 | 727 |
| 4 | 1 | 960 | 960 | 0 | 0 | 0 | 0 | 814 |
| 8 | 1 | 4,109 | 4,108 | 0 | 0 | 0 | 0 | 944 |
| 8 | 1,000 | 4,109 | 4,108 | 0 | 0 | 0 | 0 | 980 |

`tests/alloc.rs` holds the law in the gates. A heartbeat without a configuration costs about 0.3 µs
(the configurator held off, 2–8 nodes, load 24); the rest is the configurations.

## Two costs found and removed

- **The configurator's margin search** (`hyper-timing`, `qos::best_margin`). Its bracket
  `[0, MTBF·U(η, η)]` held, on the lossy young links the stream configures at bootstrap, margins of
  thousands of heartbeats for `β`'s product to multiply: a configuration took 150–360 µs. Closing the
  bracket by probing doublings while `α/MTBF` is below the least `U` seen keeps every margin that
  could do better (a property test against the whole bracket): 6–15 µs (`examples` timing at load
  20–26, four link shapes, MTBF 100 s to 30 days).
- **The renewal cadence.** Configuring once a window of heartbeats, `LinkEstimator`'s cadence,
  configured every few heartbeats at short intervals (the drift bound held the window to nine at
  0.47 ms): 24 µs a heartbeat with the slow search, 1.5–2.1 µs with the fast one. On the doubling
  schedule of §2.8, 0.29–0.32 µs at those intervals; at the longer intervals one-heartbeat margins
  choose, fewer heartbeats share each configuration, 0.7–1.0 µs (the table above).

## The real-process test

`crates/hyper-liveness/tests/processes.rs`: four member processes on the sealed plane through
hyper-tokio's kernel-stamped socket, each liveness write a 4 KiB write and the platform's flush of a
real file on a device thread; one member's disk stalled, then another SIGKILLed. Each run reports,
for every other member, the time from the stalled or killed member's last heartbeat's schedule to the
suspicion and the bound the detector stated; and the suspicions of live members against Theorem 7's
allowance. The soak runs the test binary in a loop, each run a fresh supervisor:

| host | runs | passed | detection, median (most) | stated bound less detection, least (median) |
|---|---|---|---|---|
| macOS, load 20–33 | 50, then 50 | 49 (one wait that could not end, the first defect below), then 50 | 189 ms (311 ms) | 0.30 ms (5.4 ms) |
| Linux, one CPU, two busy loops | 50, then 100 | 50, then 100 (after the clock-order fix) | 102 ms (733 ms) | 0.011 ms (3.0 ms) |
| Linux, two CPUs, four busy loops | 50, then 100 | 49 (the third defect below), then 100 | 43 ms (204 ms) | 0.054 ms (2.1 ms) |
| Linux, four CPUs, eight busy loops | 50 | 50 | 95 ms (371 ms) | 0.053 ms (3.3 ms) |

The stated bound always held: its least slack is the reverse direction's delay, which the echo's sum
adds and the heartbeat's own delay does not have (§2.8); on loopback that is tens of microseconds.

What failed on the way, each a defect fixed at its cause:
- **A wait on a suspicion that could not come** (macOS, one run in fifty): a survivor that falsely
  suspected a member just before it was killed, and never trusted it again, reported no new
  suspicion after the kill. The supervisor now waits for each member to state, after the event, that
  it holds the peer suspected, and checks the suspicion it holds.
- **Many-heartbeat margins on a young link** (Linux at one CPU, two busy loops, six runs in ten
  refuted the allowance): taking the link's own `τ_int·η` for the correlation time let the
  configurator multiply Theorem 7's factors over heartbeats a throttle's 100 ms stall delays
  together, which the young history had not seen. The margin now holds one heartbeat, a single
  Cantelli factor with no independence assumed, as hyper-swim's probes (`docs/timing.md` §2.8): 50
  of 50 after.
- **A stamp before its datagram** (Linux at two CPUs, one run in fifty): the realtime and monotonic
  clocks were read monotonic first, and a preemption between the reads aged the stamp too much, so
  the echo said a heartbeat arrived before the one it echoed was sent and the bound went unstated.
  Read realtime first, a preemption makes a stamp late, never early (`docs/transport.md` §4b).

```sh
cargo bench -p hyper-liveness --bench cost
cargo bench -p hyper-liveness --bench allocs
cargo test -p hyper-liveness --release                    # sim, processes, alloc, codec, bound
cargo test -p hyper-tokio --test stamps                   # kernel stamps (macOS, Linux)
# The soak: the binary from `cargo test --release -p hyper-liveness --test processes --no-run`, run
# in a loop with `--exact a_stalled_disk_and_a_killed_node_are_suspected_and_no_live_one_is`; in
# Linux, the same in rust:1.98.0 with `docker run --cpus N` and busy loops (`while :; do :; done`)
# beside it. HYPER_LIVENESS_TRACE=1 echoes every member's lines.
```

## After the follow-ups: the evidence's interval, the pool, restarts (2026-10-02)

`docs/timing.md` §2.8–§2.9. The same benches, this branch against main built the same way, each
binary run alternately in the same minutes on the same machine, now under heavier load (load average
40–60, other sessions' builds and a Linux VM running the E2E beside them).

Allocations and time a heartbeat (`cargo bench -p hyper-liveness --bench allocs`), three
alternating runs each, ns the range:

| nodes | groups a pair | sent (main / now) | allocs | reallocs | faults | ns, main | ns, now |
|---|---|---|---|---|---|---|---|
| 2 | 1 | 178 / 216 | 0 | 0 | 0 | 837–996 | 931–1,094 |
| 4 | 1 | 628 / 612 | 0 | 0 | 0 | 968–1,152 | 1,298–1,407 |
| 8 | 1 | 3,174 / 3,139 | 0 | 0 | 0 | 1,037–1,154 | 1,575–1,739 |
| 8 | 1,000 | 3,174 / 3,139 | 0 | 0 | 0 | 1,032–1,201 | 1,547–1,747 |

Zero allocations a heartbeat, kept. On the way a ring grew: a link's rings were resized at a move
by the latest `G`, larger than the one they were built with, so a move to a longer interval could
allocate; they are now sized by the granularity they were built with (`LinkEstimator::retime`).
Both builds showed, in one run in a few at two nodes, one allocation in the window (main too, with
a minor fault): not from this branch, not yet found. The time a heartbeat is higher by about 0.1 µs
at two nodes and 0.5 µs at eight: the configure calls are as many (3,070 against 3,116 at eight
nodes, 41 against 48 of them configuring), the pool is fed nothing once every pair is configured
(counted), and the evidence is asked for only on a refusal for want of `τ_int`; what is left is the
links' dynamics, which the moves and the floor rule change (the heartbeats sent differ), and
per-heartbeat bookkeeping (the expected move, the told trust). Measured, not removed. (Settled
below, "The cost of a heartbeat, settled": the ns column counted the warm-up's calls, and the rest
was real per-heartbeat cost, now removed.)

What a node pays a second (`cargo bench -p hyper-liveness --bench cost`, both at load 47, one run
each, the same minute):

| P | G | pair µs, main | pair µs, now | group µs | swim µs |
|---|---|---|---|---|---|
| 2 | 1 | 11.1 | 14.3 | 3.6–4.4 | 12.1 |
| 2 | 1,024 | 10.7 | 13.3 | 9,531–10,201 | 12.1 |
| 8 | 1 | 88.6 | 70.8 | 3.7 | 66.1–106.7 |
| 8 | 1,024 | 66.0 | 96.8 | 13,736–21,891 | 66.1–106.7 |

At this load the per-group and SWIM baselines themselves moved by up to two times between runs a
minute apart; the stream's cost stays flat in the groups, and at two peers is about 3 µs a second
more than main's, some 40 ns a message.

The simulation (`cargo test --release -p hyper-liveness --test sim`): every pair configured within
302 heartbeats over seeds 0–31 of three worlds (a LAN; hosts frozen up to 50 ms about every 250 ms;
groups writing every 2 ms to a device that stalls one flush in fifty for up to 60 ms), every killed
node suspected by every survivor within the bound it stated (169 by the link's own configuration,
117 by the pool's margin, 2 never heard from). The stalling world under the old interval rule (the
floor followed down, no evidence asked): a link took 5,775 heartbeats unconfigured and the run took
453 s against 1 s. The real processes (`tests/processes.rs`, load 45–50): the stalled disk
suspected in 694–719 ms against stated bounds of 698–727 ms, the killed node in 64–580 ms against
65–591 ms, live members 18 suspicions against an allowance of 98.6.

## The cost of a heartbeat, settled (2026-10-02)

The section above left a rise in the time a heartbeat, 0.84–1.0 µs to 0.93–1.09 at two nodes and
1.04–1.20 to 1.55–1.75 at eight, attributed to the links' dynamics and bookkeeping. Settled here,
on the commits `634d45c` (L-3), `798c481` (before the follow-ups) and `e7638c5` (after them), and
this change.

**The method.** A scratch probe (not committed: it reads the PMU through `unsafe` FFI) runs one cell
of `benches/allocs.rs`'s world in a fresh process and reads, around the measured window alone, the
time of the crate's calls (the world's clock, as the bench), and instructions retired and cycles
from `proc_pid_rusage(RUSAGE_INFO_V4)` on itself (`ri_instructions`, `ri_cycles`, the core's PMU;
they include the world's own work, the same code in every variant). The bootstrap, from the first
poll to every pair configured, is measured apart. Two worlds:
- **adaptive**: the bench's own, where each receiver asks the interval its configuration chooses,
  so the heartbeats sent differ between the commits (the follow-ups changed the dynamics);
- **fixed**: the same world with every receiver asking 50 ms, so every variant sends the same
  heartbeats at the same intervals (400 a pair in the window), the cost a heartbeat at fixed
  intervals.

Each round runs every variant once a cell and world, the order rotated by one each round and
reversed every other; 30 rounds. Stated: per round the ratio of a variant to `634d45c` (adjacent
runs), the geometric mean with its Student-t 95 % interval on the logarithms. Apple M5 Max, macOS,
rustc 1.98.0, release with LTO, load average 84–86 throughout (other sessions' builds).

**It was a real per-heartbeat cost, and a measurement defect besides.**
- *The defect.* `benches/allocs.rs` zeroed the heartbeats after the warm-up and not the time, so its
  ns was the warm-up's and the window's calls over the window's heartbeats; the warm-up ran
  0.8 s simulated at eight nodes after the follow-ups against 0.2 s before, with the pool fed, so
  most of the reported rise was the bootstrap. The bench now times the window alone and reports
  the bootstrap apart (`boot ns`, `boot s`).
- *The real cost.* At fixed intervals, the same heartbeats, `e7638c5` against `634d45c`: time
  1.185–1.255, instructions 1.090–1.150, cycles 1.096–1.142 in every cell; `798c481` was 1.00 of
  `634d45c` in instructions. Counted per call (instrumented scratch builds), in the window:
  configure was called on 3,070 of 3,139 heartbeats taken at eight nodes after and 3,116 of 3,174
  before (a link that moved is configured at each heartbeat until its levels measure `τ_int` at
  the new interval), each refusal now also asking `independent_interval`, a walk of all seventeen
  Allan levels; every poll read the MTBF (a float division and a conversion) and the floor twice,
  each a `u128` mean (`__udivti3`, which a profile showed growing); `deadline` built the pair's
  trust; and the pool was fed nothing (0 of 3,139). The links' dynamics are a different workload,
  not a cost a heartbeat: at fixed intervals the commits send identical heartbeats and the rise is
  the same size.

**Removed at the causes**, every change leaving what the crates compute exactly as it was (the
heartbeats sent, taken, configured and the bootstrap's length are identical to `e7638c5`'s in every
cell of both worlds, and the simulation passes its seeds unchanged):
- the Allan levels are walked to the first level without its windows, not all seventeen: a level's
  windows are `⌊taken/2^j⌋`, so those with enough are the first ones (a property test against the
  full walk); three walks a heartbeat (`n_A`, `τ_int`, the evidence's interval);
- a level's sums are `i64` (every offset is within `OFFSET_LIMIT`, so a window of `2^16` fits), so
  its mean is one conversion, not `__floattidf`;
- a fold's mean and the drift bound divide in `u64` where the sum fits, the `u128` division only
  past it (property tests against the wide computation);
- a move of `G`, which comes with nearly every heartbeat (the mean lateness moves at each wake),
  places the window without recomputing `τ_int`, which `G` does not move (a property test against
  the full update); `n_G`'s rounding converts once instead of bisecting seventeen bits;
- `behaviour` builds only what it returns; a heartbeat's prediction reads the window's sum once;
- a pair asks its estimator whether it has its evidence before building costs and floors, so a link
  waiting for `τ_int` pays one refusal and the evidence's interval, as before, and nothing else;
- a poll reads `G` and the floor once and the MTBF only for a pair waiting for a pool margin; a pair
  not due returns before its interval is computed; the wake is gathered in the poll's one walk of
  the pairs;
- `wake()` answers what the last poll found, kept by each heartbeat taken no later than its pair's
  freshness point, instead of walking every pair at every call (an owner asks after every call; the
  bench's world asks every node at every event).

**After** (`fix`), against `634d45c`, ratio [95 % interval], and medians a heartbeat sent or taken
(ns of the crate's calls; instructions and cycles of the window, world included):

| world | cell | time | instructions | cycles | ns, 634 / e76 / fix | instructions, 634 / e76 / fix |
|---|---|---|---|---|---|---|
| fixed | 2 nodes | 0.823 [0.799, 0.848] | 0.750 [0.749, 0.750] | 0.766 [0.742, 0.790] | 131 / 164 / 107 | 3,449 / 3,967 / 2,586 |
| fixed | 4 nodes | 0.877 [0.866, 0.889] | 0.667 [0.667, 0.667] | 0.656 [0.638, 0.674] | 125 / 151 / 110 | 3,641 / 4,094 / 2,429 |
| fixed | 8 nodes | 0.915 [0.894, 0.937] | 0.494 [0.494, 0.494] | 0.576 [0.541, 0.613] | 142 / 171 / 130 | 5,203 / 5,673 / 2,573 |
| fixed | 8 nodes, 1,000 groups | 0.971 [0.871, 1.082] | 0.494 [0.494, 0.495] | 0.584 [0.557, 0.612] | 142 / 171 / 131 | 5,203 / 5,673 / 2,573 |
| adaptive | 2 nodes | 0.775 [0.758, 0.792] | 0.700 [0.699, 0.701] | 0.712 [0.687, 0.738] | 134 / 164 / 103 | 3,504 / 4,022 / 2,452 |
| adaptive | 4 nodes | 0.975 [0.957, 0.992] | 0.648 [0.648, 0.648] | 0.694 [0.667, 0.723] | 123 / 172 / 120 | 3,989 / 4,806 / 2,584 |
| adaptive | 8 nodes | 0.899 [0.877, 0.922] | 0.425 [0.425, 0.425] | 0.482 [0.453, 0.514] | 187 / 222 / 164 | 7,631 / 8,283 / 3,243 |
| adaptive | 8 nodes, 1,000 groups | 0.866 [0.844, 0.890] | 0.425 [0.425, 0.425] | 0.480 [0.456, 0.506] | 187 / 224 / 159 | 7,632 / 8,284 / 3,243 |

Against `e7638c5` the time is 0.63–0.81 in every cell. The time a heartbeat includes the world's two
clock reads around each call, the same in every variant, which narrows its ratios; instructions and
cycles do not depend on the load. The one interval that reaches past 1 (fixed, 1,000 groups) is two
slow rounds of thirty; its median is 131 ns against 142. Allocations, reallocations and faults a
heartbeat: zero in every run of every variant.

The bootstrap a heartbeat (ns; instructions), which the follow-ups' pool adds to: at fixed intervals
`634d45c` 177, 136, 151, 152; `e7638c5` 250, 192, 217, 212; now 181, 141, 172, 171 (instructions
4,244, 3,998, 5,729, 5,729 against now 3,709, 3,089, 3,451, 3,450); in the adaptive world now 224,
171, 207, 205 against `634d45c`'s 252, 195, 202, 204, at a bootstrap four times as long at eight
nodes (0.8 s against 0.2 s simulated: the floor followed up and not down, §2.9).

`cargo bench -p hyper-liveness --bench allocs` after (load 80–84):

| nodes | groups a pair | sent | taken | allocs | reallocs | bytes | minor faults | ns | boot ns | boot s |
|---|---|---|---|---|---|---|---|---|---|---|
| 2 | 1 | 216 | 216 | 0 | 0 | 0 | 0 | 115 | 253 | 0.2 |
| 4 | 1 | 612 | 612 | 0 | 0 | 0 | 0 | 127 | 179 | 0.2 |
| 8 | 1 | 3,139 | 3,139 | 0 | 0 | 0 | 0 | 168 | 206 | 0.8 |
| 8 | 1,000 | 3,139 | 3,139 | 0 | 0 | 0 | 0 | 164 | 200 | 0.8 |

`cargo bench -p hyper-liveness --bench cost`, `e7638c5` and now alternately, two runs each, load
56–60, pair µs a node a second: two peers 13.4–15.1 against 9.4–14.3; eight peers 67.6–75.1
against 54.7–66.9. SWIM's column moved too (12.4–14.5 to 9.3–10.2 at two peers, 53.2–54.2 to
44.7–46.3 at eight): hyper-swim reads the same folds' means.

**The allocation at two nodes.** The earlier section saw one allocation in the window in one run in
a few at two nodes, on `798c481` too. Hunted with `hyper_measure`'s counting allocator patched in a
scratch build to capture a backtrace at every allocation and reallocation while counting (not
committed): 5,300 sequential and 6,000 concurrent runs (six at once) of `e7638c5`'s `allocs` bench
at load 30–60, 3,000 more of the plain `e7638c5` and `798c481` binaries, and 960 two-node windows of
the probe: no allocation or reallocation in any window. The world and the crates are deterministic
from the seed (the counts above are identical run to run), so an allocation that comes in some runs
and not others is not the crate's logic; it does not reproduce on these builds, and no source in
the crates was found. `tests/alloc.rs` now holds the law at two nodes as well as three and five, so
the gate would catch it.

```sh
cargo bench -p hyper-liveness --bench allocs
cargo bench -p hyper-liveness --bench cost
cargo test --release -p hyper-liveness -p hyper-timing
# The probe and its runner are scratch: one cell a process, `proc_pid_rusage(getpid(),
# RUSAGE_INFO_V4)` before and after the window; the fixed world asks 50 ms in `Pair::send`.
```

## hyper-durable-e2e on its own detectors

`crates/hyper-durable-e2e` with each member running the stream itself (`docs/timing.md` §2.9, "On
real detectors"), the test telling no member what to believe. Every scenario: the six kills at named
points, the stalled disk (leader and follower), F17's two founder cases, the fence host, the failed
flush, six random kills. A run is the whole binary, `cargo test -p hyper-durable-e2e --test kill`.

| host | runs | passed | a run |
|---|---|---|---|
| Linux (Docker, rust:1.98.0, aarch64), four CPUs | 4 | 4 | 32–108 s |
| Linux, two CPUs | 4 | 4 | 16–42 s |
| macOS, load 39–48 | 3 | 3 | 63–107 s |

On the way to this code, 19 more Linux runs (two and four CPUs) and 2 on macOS, each found defect
fixed at its cause before the runs above: a restart expected of a member that had not heard the
last run, or whose configuration did not name the restarted member (founder-fenced), and the
false suspicion at a moved interval (`docs/timing.md` §2.8), which had made single scenarios take
up to 85 s.

Before (main, the test the members' detector), the first scenario of every Linux run in Docker
failed: no member measured its timer, as described in the commit that fixed it; after that fix the
same eight runs passed in 2–23 s. A run on real detectors is slower: a member's links begin at its
flush floor and move to the interval their evidence needs, and the group is judged only once its
links or their pools have it, seconds under this load.

```sh
cargo bench -p hyper-liveness --bench allocs     # and the same binary built from main, alternately
cargo bench -p hyper-liveness --bench cost
cargo test --release -p hyper-liveness           # sim, processes, alloc
HYPER_DURABLE_LIVENESS_SEEDS=1000 cargo test --release -p hyper-durable --test liveness
cargo test -p hyper-durable-e2e --test kill      # in Linux: docker run --cpus N rust:1.98.0 ...
```

## A link younger than its evidence, judged by what its node measured (2026-10-02)

`docs/timing.md` §2.8, "Judged before its own evidence", and §3 item 10: a link with no
configuration of its own is judged by the widest of its node's pool and its configured links,
widened by its own errors, where it was judged by the pool alone. Before is main at `0eaac7e`
(hyper-liveness as at `4769074`).

**The cost.** `cargo bench -p hyper-liveness --bench allocs` and `--bench cost`, main and this change
built the same way and run alternately, three rounds each, Apple M5 Max, load average 43 (other
sessions' builds). The bench's world configures every pair, so both send the same heartbeats
(216, 612 and 3,139); a heartbeat reads one more `Option` pair, and a configuration made walks the
pairs once for the widest.

| nodes | groups a pair | allocs, reallocs, faults | ns, main | ns, now | boot ns, main | boot ns, now |
|---|---|---|---|---|---|---|
| 2 | 1 | 0, 0, 0 | 104–112 | 109–118 | 235–252 | 222–275 |
| 4 | 1 | 0, 0, 0 | 123–131 | 126–148 | 173–198 | 175–207 |
| 8 | 1 | 0, 0, 0 | 166–172 | 170–173 | 211–253 | 219–234 |
| 8 | 1,000 | 0, 0, 0 | 162–174 | 171–188 | 213–219 | 212–230 |

Pair µs a node a second (`cost`): two peers 9.6–10.1 against 10.0–10.3 now (one main run at 31.1
for 64 groups, an outlier of the load), eight peers 55.4–64.3 against 57.7–63.9; the per-group and
SWIM columns moved as much between rounds. Zero allocations a heartbeat, kept.

**The simulation** (`cargo test --release -p hyper-liveness --test sim`,
`a_peer_dead_before_its_links_have_evidence_is_suspected_once_a_sibling_has_its_own`): three nodes,
the victim killed in its links' first heartbeats, seeds 0–31 of four worlds; the time from the kill
to each survivor's suspicion, median / 90th percentile / most over 64 survivors. Under the pool
alone the test's bound (no later than the first poll once the live link configured) failed for 33
survivors of 256.

| world | the pool alone | the node's evidence |
|---|---|---|
| LAN | 33 / 349 / 576 ms | 23 / 39 / 86 ms |
| hosts frozen up to 50 ms | 6 / 63 / 831 ms | 5 / 32 / 207 ms |
| flushes stalled up to 60 ms | 89 / 1,199 / 13,829 ms | 107 / 1,141 / 3,919 ms |
| Windows' timer and flush | 578 / 4,468 / 33,828 ms | 578 / 2,080 / 5,867 ms |

Without the link's own errors in the widening the stalling world's row was 89 / 997 / 3,919 ms, the
others unchanged; the prototype's rule, the pool preferred to the configured links
(`pool.or(configured)`) and no widening by the link's own, gave the same rows as that on these
seeds: the link's own errors cost the stalling world's slowest survivors 0.1–0.2 s, the price of
never judging a link narrower than it has shown itself to be.

**The shell** (`HYPER_DURABLE_LIVENESS_SEEDS=1000 cargo test --release -p hyper-durable --test
liveness`, `a_leader_killed_before_its_links_have_evidence_is_replaced`): the leader killed as soon as
it is elected, replaced within 855 ms of the kill at the most over 1,000 seeds (707 ms over the 64 of
the gates), against 243.8 s (47.7 s) under the pool alone.

**The real processes** (`tests/processes.rs`,
`a_node_killed_in_its_first_heartbeats_is_suspected_once_a_sibling_has_its_evidence`, three
members, SIGKILL once the victim has heard every peer and sent to each). Main failed its bound in 2
runs of 3 on this machine (a survivor noticing 200 ms after its live link configured). Now, the time
from the kill to each survivor's suspicion:

| host | runs | passed | noticed after the kill |
|---|---|---|---|
| macOS, load 31–61 | 7 | 7 | 1.4–15.2 s |
| macos-15 (CI) | 5 | 5 | 0.31–2.81 s |
| ubuntu-24.04 (CI) | 5 | 5 | 0.06–21.9 s |
| ubuntu-24.04-arm (CI) | 5 | 5 | 0.04–0.53 s |
| windows-2025 (CI) | 5 | 5 | 3.2–7.8 s |
| windows-11-arm (CI) | 5 | 5 | 2.4–7.6 s |

**hyper-durable-e2e**, `cargo test -p hyper-durable-e2e --test kill -- stall` (the stalled leader
and the stalled follower, each a member whose links may be young when its disk stops) and the whole
binary; main from `diag-young-base`, this change from `diag-young-links`, the same hour, on GitHub's
runners, and on this machine at load 26–61:

| host | stall, main | stall, now | whole, main | whole, now |
|---|---|---|---|---|
| macOS (this machine) | 8.2–112.0 s (8 runs; 3 past 25 s) | 5.9–15.3 s (8) | – | 41.5 s (1) |
| macos-15 | – | 0.68–3.05 s (8) | – | 15.1–21.8 s (3) |
| ubuntu-24.04 | 0.70–56.4 s (6) | 0.95–5.48 s (8) | 15.0–25.7 s (2) | 27.1–59.7 s (3) |
| ubuntu-24.04-arm | 1.1–6.3 s (6) | 0.27–0.46 s (8) | 23.0–25.7 s (2) | 18.4–23.5 s (3) |
| windows-2025 | 6.8–89.3 s (6) | 4.3–8.2 s (8) | 56.9 s, and one failed (2) | 37.4–49.7 s (3) |
| windows-11-arm | 7.8–76.6 s (6) | 9.5–18.8 s (8) | 69.2–120.9 s (2) | 60.0–71.8 s (3) |

Every run of this change passed. Its quiet period now covers an unjudged pair's interval
(`PairReport::interval`), so a wait no longer gives up between the heartbeats of a young link moved
to a long interval.

```sh
cargo bench -p hyper-liveness --bench allocs     # and the same binary built from main, alternately
cargo bench -p hyper-liveness --bench cost
cargo test --release -p hyper-liveness --test sim -- --nocapture a_peer_dead
cargo test -p hyper-liveness --test processes -- --nocapture
HYPER_DURABLE_LIVENESS_SEEDS=1000 cargo test --release -p hyper-durable --test liveness -- --nocapture
cargo test -p hyper-durable-e2e --test kill -- stall
```

## The node's evidence, kept (2026-10-02)

The section above measured a heartbeat at 109–118 ns against 104–112 (two nodes), 126–148 against
123–131 (four), 170–173 against 166–172 (eight) and 171–188 against 162–174 (eight, a thousand
groups), three rounds each. Its causes, each removed:
- `Liveness::on_heartbeat` and `poll` computed the node's evidence, `pair::wider` of the pool's
  measure and the widest configured link's (three maxima), at every heartbeat and poll, and a
  configured pair threw it away. It is now a field, renewed where either part moves (a pool error
  fed, a configuration made, a pair let go) and read by reference, only for a pair with no
  configuration of its own.
- A pair kept a whole `PairReport` for its six counters; `7a8812c` added an interval to it, and a
  pair grew from 632 to 648 bytes, which a poll's walk of the pairs moves through. It keeps the
  counters and builds the report when asked: 608 bytes.

And two found on the way, in the bootstrap's profile (`sample` of a loop of the eight-node
bootstrap, 600 times, `bootloop.rs` below):
- A move of `G`, which comes with nearly every heartbeat and every feed of the node's pool, placed
  the estimator's window by walking the Allan levels again (`LinkEstimator::set_granularity`), and the
  heartbeat that followed placed it again: the placements were 292 of the 2,226 samples, the most
  of any of the crate's functions but the poll's own walk. The levels move only with an offset taken, and `G` reaches `n_A` only through the power
  of two of the drift bound, so a placement now reads the `n_A` last found while neither moved
  (`hyper-timing`, a property test holds it to the walk after every heartbeat, move of `G` and move
  of the interval).
- A heartbeat on a pair with no configuration of its own read the MTBF (a float division) for the
  margin of the node's evidence, which is renewed on the doubling schedule only: it is read only
  at a renewal.

**The method**, as "The cost of a heartbeat, settled" above: `benches/allocs.rs` and
`benches/cost.rs` built at `4769074`, `7a8812c` and this change, and the scratch probe (one cell of
the allocs world in a fresh process, the PMU's instructions and cycles from
`proc_pid_rusage(RUSAGE_INFO_V4)` around the window and around the bootstrap), each round running
every variant once a bench and cell, the order rotated by one each round and reversed every other;
the one-minute load average read before each process. Stated: per round the ratio of a variant to a
reference (adjacent runs), the geometric mean with its Student-t 95 % interval on the logarithms.
Apple M5 Max, macOS 26.4.1, rustc 1.98.0, release with LTO; 60 rounds at load 30–48 (median 40.5).

A heartbeat sent or taken, the adaptive world (the bench's own; every variant sends the same
heartbeats, 216, 612 and 3,139), medians and ratios to `4769074` and to `7a8812c`:

| cell | ns (allocs), 4769 / 7a88 / now | now / 4769 | now / 7a88 | instructions, 4769 / 7a88 / now | now / 4769 | cycles, now / 4769 |
|---|---|---|---|---|---|---|
| 2 nodes | 105.0 / 109.0 / 101.5 | 0.970 [0.955, 0.985] | 0.935 [0.921, 0.949] | 2,453 / 2,518 / 2,397 | 0.977 | 0.977 [0.963, 0.992] |
| 4 nodes | 122.0 / 126.0 / 119.0 | 0.982 [0.971, 0.993] | 0.946 [0.934, 0.958] | 2,585 / 2,624 / 2,516 | 0.973 | 0.980 [0.960, 1.000] |
| 8 nodes | 161.0 / 170.0 / 164.0 | 1.008 [0.991, 1.026] | 0.950 [0.925, 0.976] | 3,245 / 3,252 / 3,159 | 0.974 | 1.003 [0.974, 1.032] |
| 8 nodes, 1,000 groups | 166.0 / 170.0 / 169.0 | 1.011 [0.979, 1.043] | 0.996 [0.976, 1.016] | 3,244 / 3,252 / 3,159 | 0.973 | 0.993 [0.964, 1.022] |

Instructions are the window's, the world's own work included (the same code in every variant), and
their intervals are within ±0.001. Against `7a8812c` the instructions are 0.951, 0.959, 0.971 and
0.971, the cycles 0.954, 0.968, 0.940 and 0.969. In the fixed world (every receiver asking 50 ms,
so every variant sends the same heartbeats at the same intervals; 30 rounds at load 57–61) the
instructions are 0.948, 0.943, 0.943 and 0.944 of `4769074`'s and the time 0.965 [0.939, 0.993],
0.995 [0.935, 1.058], 0.935 [0.897, 0.974] and 1.005 [0.965, 1.046]. Allocations, reallocations
and faults a heartbeat: zero in every run of every variant.

`cost`, pair µs a node a second: at two peers 9.7, 9.5 and 9.5 against `4769074`'s 9.8, 9.6 and 9.7
(0.982–0.988); at eight peers 59.8, 59.2 and 59.5 against 57.2, 56.6 and 56.6 (1.037 [1.026, 1.049]
to 1.045 [1.033, 1.057]). The eight-peer rows run after the per-group and SWIM baselines in the same
process. The same `per_pair` alone in a fresh process (`costpair.rs` below, 40 rounds at load
43–45) is 1.000 [0.972, 1.029], 0.967 [0.928, 1.008] and 0.983 [0.946, 1.021] of `4769074` at eight
peers, and the probe at the cost bench's seed and its nine-node cells (30 rounds, load 38) 1.011
[0.972, 1.052], 1.004 [0.977, 1.032] and 0.997 [0.963, 1.032] in time at 0.974 of its
instructions: the rise in `cost` follows what the process ran before, not the stream's work.

**The bootstrap against L-3** (`634d45c`), which "The cost of a heartbeat, settled" left above it
in some cells. The allocs bench times the crate's calls, and at L-3 an owner's `wake()` walked every
pair at every call, outside them (in the bootstrap's profile, 1,428 of L-3's 3,350 samples); since
`4769074` the poll gathers the wake in its own walk, inside them. Timed as the bench times it, the
bootstrap a heartbeat is now 1.04–1.12 of L-3's in the fixed world and 0.88–0.99 in the adaptive
one. With the owners' asks for the wake timed too (a scratch world, `world-timed.rs` below; 20
rounds at load 65–66), what an owner pays a heartbeat of the bootstrap is now 0.909 [0.846, 0.977],
0.763 [0.745, 0.782] and 0.607 [0.592, 0.622] of L-3's at two, four and eight nodes in the fixed
world, and 0.802 [0.776, 0.829], 0.694 [0.678, 0.710] and 0.595 [0.569, 0.623] in the adaptive one,
at 0.56–0.86 of its instructions. Against `4769074` the bootstrap's instructions are 0.956–0.972
and its time 0.964–0.981. At eight nodes the adaptive bootstrap is four times as long as L-3's
(0.8 s simulated, 21,129 heartbeats, against 0.2 s and 16,863): the moves to the interval the
evidence needs (`docs/timing.md` §2.8), and while a link is young its errors also feed the node's
pool, an estimator update a heartbeat L-3 did not make, which the design needs (§3, item 10).

`hyper-timing`'s own estimator bench, whose heartbeats move no `G`, pays the placement's check and
gains nothing from it: 0.15 % more instructions over the whole process, cycles within its
run-to-run spread (three alternating runs each, `/usr/bin/time -l`).

```sh
cargo bench -p hyper-liveness --bench allocs     # and the same at 4769074 and 7a8812c, alternately
cargo bench -p hyper-liveness --bench cost
cargo test --release -p hyper-liveness -p hyper-timing
# Scratch, not committed: the probe (one cell a process, its PMU counts from
# proc_pid_rusage(getpid(), RUSAGE_INFO_V4) around the window and the bootstrap; args nodes groups
# seconds [seed]), costpair.rs (cost's per_pair alone), bootloop.rs (the bootstrap in a loop, for
# `sample <pid> 4`), world-timed.rs (the bench's world with the owners' wake asks timed), and the
# fixed world (every receiver asking 50 ms in Pair::send).
```

## The simulation's worlds (2026-10-02)

`crates/hyper-liveness/tests/sim.rs` draws every quantity of a world from a host's measured
distribution: its quantiles at 107 probabilities (every hundredth, then 0.995, 0.999, 0.9995,
0.9999, 0.99995, 0.99999 and the most), by the inverse transform, linear between neighbouring
points. The tables are `tests/support/worlds.rs`, which `scripts/liveness-worlds.py` writes from
the trace tool's output and nobody edits. The worlds had been picked: delays of 80–140 µs, one
message in 500 stalled up to 20 ms, one in 1,000 lost, flushes of 0.2–0.6 ms, wakes 20–80 µs late,
hosts frozen up to 50 ms about every 250 ms, a device stalling one flush in fifty for up to 60 ms.

| quantity | macOS (`MACOS`, `BUSY`) | Linux in Docker Desktop's VM (`LINUX`) | Windows (`WINDOWS`) |
|---|---|---|---|
| one-way delay, send to the kernel's stamp | the 1 ms run: median 14.7 µs, p99 44 µs, p99.9 195 µs, most 12.4 ms | the 2 ms run: median 4.9 µs, p99 13 µs, p99.9 86 µs, most 2.7 ms | Linux's: not measured, three orders below its timer and flush |
| a 4 KiB write and full flush | `F_FULLFSYNC` every 10 ms: median 4.7 ms, p99 15.9 ms, most 205 ms; `BUSY`, back to back (the sweep): median 11.7 ms, p99 29.8 ms, most 54 ms | `fdatasync` every 2 ms: median 0.52 ms, p99 16.0 ms, most 3.66 s | uniform over 12–30 ms: the means hyper-durable-e2e's floors give on the runners, no shape within them measured (Jaynes 1957) |
| an owner's timer past its deadline | `select(2)`, 2,000 waits at each asked wait from 1 µs to 10 ms; at 1 ms median 255 µs, most 9.3 ms; at 10 ms median 1.8 ms, most 7.1 ms | `ppoll(2)`, the same sweep; at 1 ms median 0.99 ms, most 10.0 ms; at 10 ms median 1.17 ms, most 54 ms | the next 15.625 ms clock interrupt, its phase uniform (`timeBeginPeriod`) |
| the host's freezes | the 1 ms run: 103 in 300 s, 3.59 s frozen; 26 of 10 ms or more, 7 of 100 ms or more, the longest 575 ms and 1.21 s; 67 of them in 31 s of the run | the 2 ms run: 3 in 300 s, of 27, 48 and 126 ms | not measured; none |
| loss | none: 0 of 300,000 | none: 0 of 150,000 | none |

A wait between two rows of the sweep is late as both are at the same probability, interpolated in
the wait, and one longer than the longest row as that row: extrapolating macOS's coalescing in
proportion to the wait (its leeway grows with it, the cap unmeasured, `docs/timing.md` §2.4) made
the lateness grow with every interval a link moved to, and the links never configured. A link's
delays are drawn one by one: no trace showed a delay correlated past what the freezes make.

**A freeze**, as `hyper-timing-trace freezes` reads it, is a span in which both of a run's processes
were behind past what a wait of theirs is late by: the sender past a heartbeat's schedule without
having sent it, the receiver past a timed wait's deadline without having woken, each by more than
the sweep measured of a wait of that length at the most (a send that did not wait, more than the
shortest sleep's most). One process late alone is its own timer's lateness, which the sweep measured
and the owner's timer draws; the two processes' timers fall together, a heartbeat apart, and macOS
coalesces them, so both are late together by a fraction of a millisecond at nearly every heartbeat,
which the sweep's bound keeps out. Both late past it at once, the host ran neither: in the 1 ms run
the sender's wait and the receiver's, due at the same instant, ended 574.6 ms past it within 23 µs
of each other, two processes. Each node replays its host's freezes from a point of the trace drawn from its own
stream, round the trace again past its end; a frozen node's owner does nothing, and at the thaw it
takes what arrived, each datagram judged at its kernel stamp, and what completed, reported then,
and polls once.

Two readings of the stalls came first and were wrong. Clusters of heartbeats past the run's 99th
percentile of delay, each from its first schedule to its last arrival, held the sender's backlog
after each stall as well, and the run length that declustered them merged distinct stalls (in the
1 ms run, two "stalls" over 300 s, of 28.6 ms and 35.9 s). And either process's waits late past the
sweep's most, alone, counted every scheduling delay of one process as the host's: 646 episodes in
the 1 ms run, two in three of 1–5 ms, against the 103 freezes both processes saw.

**Found on the way, each at its cause.** The owner thawed with a poll after each datagram it had
held, so each poll found the freshness point of a datagram whose successor it had not yet taken:
59 suspicions of live peers against an allowance of 10 in seed 5 of the stalled-disk test. It now
takes everything held, then polls once, as `LinkEstimator::on_heartbeat` asks of its caller. And
with freezes replayed, a peer could suspect the node whose disk was about to stall, its host frozen,
and hold that suspicion through the stall, so the test found no suspicion after the stall to
measure (seed 86 of 800); the disk now stalls while every peer trusts the node, as the killed node
is killed.

**Horizons are facts.** Every test runs until what it asserts on has happened: every pair
configured, every live pair through a renewal of its configuration (`run_until_doubled`: as many
heartbeats again as it holds), every survivor holding the victim suspected, the stalled node having
taken a heartbeat from each peer since, or nothing left in flight but the hosts' freezes. A pair
that takes more heartbeats unconfigured than any window holds (`WINDOW_LIMIT`) fails the test that
waits on it.

**The soak**, every test of `tests/sim.rs` at ten and at a hundred times its seeds
(`HYPER_LIVENESS_SEEDS`), each test a process, three at a time, load average 49–78:

| test | seeds, gate / soak | 10× | 100× |
|---|---|---|---|
| `live_peers_configure_and_keep_their_allowance` | 8 / 80, 800 | passed | passed |
| `no_heartbeat_leaves_without_a_newer_flush` (two worlds) | 1 / 10, 100 | passed | passed |
| `a_killed_peer_is_suspected_within_the_stated_bound` | 16 / 160, 1,600 | passed | passed |
| `a_stalled_disk_is_suspected_as_a_crash_is` | 8 / 80, 800 | passed | seed 86 failed (above), then passed |
| `groups_share_one_stream_and_an_unshared_pair_is_silent` | 1 / 10, 100 | passed | passed |
| `a_restarted_peer_is_trusted_again_and_counted` | 1 / 10, 100 | passed | passed |
| `every_link_configures_or_suspects_a_crash_within_its_bound` (three worlds) | 32 / 320, 3,200 | passed | passed |
| `a_peer_never_heard_from_is_suspected` | 16 / 160, 1,600 | passed | passed |
| `a_peer_dead_before_its_links_have_evidence_is_suspected_once_a_sibling_has_its_own` (four worlds) | 32 / 320, 3,200 | passed | passed |

The 10× soak took 5.7 s, the 100× 70 s. At 100×, the most heartbeats any link took to configure was
1,113 (Linux, seed index 1,940 of the configure test), the longest freeze replayed 1.21 s, and the
killed node was suspected by its own detector's margin 15,989 times, by the node's evidence's 12,497
and as never heard 314 times. From the kill of a node dead in its links' first heartbeats to each
survivor's notice, median, 90th percentile and most:

| world | 10× (640 survivors) | 100× (6,400 survivors) |
|---|---|---|
| macOS | 144 / 549 / 7,038 ms | 128 / 543 / 23,191 ms |
| macOS, its log busy | 299 / 1,321 / 5,843 ms | 362 / 1,369 / 34,530 ms |
| Linux | 61 / 305 / 2,544 ms | 66 / 291 / 18,840 ms |
| Windows | 448 / 2,067 / 8,136 ms | 422 / 2,141 / 13,160 ms |

The most is the time a survivor took to hold evidence of its own to judge the dead link by: its
live link's configuration, or its pool's own measure where that came first (every freshness point
had passed within 70 ms of the kill). It is the live link's evidence, which no rule can lend it
(`docs/timing.md` §2.8, "What the pool alone missed").

**The runs**, on the Apple M5 Max of "Heartbeat traces", macOS 26.4.1, 2026-10-02 (PDT): the macOS
sweep from 18:36:59, load 49.14; the flushed run from 18:40:59 to 18:45:59, load 54.08 to 80.81; the
1 ms run from 19:36:02 to 19:41:02, load 42.46 to 59.76. In Docker Desktop's VM (linuxkit 6.12.76,
aarch64, `rust:1.98.0`, the log on a Docker volume): the sweep at 18:50, the VM's load 20.15; the
flushed run from 18:52 to 18:57, 23.35 to 18.52; the 2 ms run from 20:07 to 20:12, the VM's load 8.50
to 11.97 and the Mac's 56.08 to 71.83. The macOS world is the 1 ms run's, not the 100 µs run of
18:38 (load 44.19 to 54.08, the table these worlds first drew delays from): at 100 µs the sender's
service is near its interval, so its backlog after each lateness, not the host, fills its record.
The runs of "Heartbeat traces" (2026-10-01) are summaries only; a freeze is read from raw records.

```sh
cargo build --release -p hyper-timing-trace
B=target/release/hyper-timing-trace
$B timer <dir>/flush.log > mac-timer.txt
$B run <dir>/mac-plain 1000 300 && $B quantiles <dir>/mac-plain > mac-plain.txt
$B freezes mac-timer.txt <dir>/mac-plain > mac-freezes.txt
$B run <dir>/mac-flush 10000 300 <dir>/flush.log && $B quantiles <dir>/mac-flush > mac-flush.txt
# Linux: the same in rust:1.98.0 as in "Commands for the traces", at 2 ms, the freezes read
# against the Linux sweep; then
python3 scripts/liveness-worlds.py mac-timer.txt mac-plain.txt mac-flush.txt mac-freezes.txt \
    linux-timer.txt linux-plain.txt linux-flush.txt linux-freezes.txt \
    > crates/hyper-liveness/tests/support/worlds.rs && cargo fmt --all
# The soak: the binary from `cargo test --release -p hyper-liveness --test sim --no-run`, each
# test with `--exact <test>` and HYPER_LIVENESS_SEEDS at ten and a hundred times its default;
# HYPER_LIVENESS_SEED=<n> with HYPER_LIVENESS_SEEDS=1 reruns the seed a failure printed (its low
# 32 bits).
```

## Simulation harnesses as they are (2026-10-02)

The baseline `hyper-sim` and `hyper-check` are measured against when the harnesses move onto them
(`docs/sim.md` §10, step S-6). Each figure is the wall time of one test binary at its default scale in
the debug gate run (`bash scripts/gates.sh`, `CARGO_BUILD_JOBS=4`), the binary's tests on the
harness's threads; Apple M5 Max, 18 cores, 128 GiB, macOS, rustc 1.98.0, load average 21–22 from
other work on the machine; hyper-raft `634d45c`. Per-step time, allocations and page faults in release,
on the same schedules, are S-6's measurement.

| Binary | Scale | Wall |
|---|---|---|
| `hyper-raft` `tests/differential.rs` | 7 tests, most of 96 seeds × 4,000 steps | 25.5 s |
| `hyper-raft` `tests/fast.rs` | 96 × 4,000, and directed cases | 5.4 s |
| `hyper-raft` `tests/group.rs` | 96 × 4,000, and directed cases | 4.4 s |
| `hyper-raft` `tests/pipeline.rs` | 24 × 2,000 at four settings; a crash at every persistence step of 3 × 400 at two | 1.2 s |
| `hyper-durable` `tests/sim.rs` | 128 × 5,000 at five shapes; a crash after every event of 4 × 800 at two | 2.0 s |
| `hyper-liveness` `tests/sim.rs` | seven timed scenarios | 0.9 s |
| `hyper-log` `tests/equivalence.rs` | the recorded seeds against mantle-log's hashes | 1.6 s |
| `hyper-log-e2e` `tests/kill.rs` | 24 SIGKILLs of a real writer | 9.0 s |

```sh
CARGO_BUILD_JOBS=4 bash scripts/gates.sh   # each binary's "finished in" line
```

## hyper-sim's world against the harnesses (S-1, 2026-10-02)

What a step of `hyper-sim`'s world costs against the harnesses it will replace (`docs/sim.md` §10,
§12.7). Three workloads, each run on the replaced harness's own machinery and on the world, with
the same process logic, the same draws a step and the same payloads:
- **untimed**: hyper-raft's `tests/support` schedule. The harness's machinery is its multiply-shift
  `Seeded`, a `Vec` network of 144-byte messages taken at a drawn index with `Vec::remove`, and the
  oldest dropped at 2,048. The world runs it under the free discipline with `Random`. Each step
  delivers one message and draws its loss, and the receiver sends one to a peer it draws, so the
  messages in flight stay at the population.
- **timed**: hyper-liveness's `tests/sim.rs` world. The harness's machinery is a `BinaryHeap` of
  `(time, key)` with the events in a `BTreeMap`, a xorshift with `% span`, and each node's wake
  found by a scan with its lateness drawn every turn. The world runs it under the ordered
  discipline. Every node heartbeats every peer each 10 ms with the test's `LAN` delays, stalls and
  loss, 64-byte payloads held inline.
- **hyper-liveness**: the crate itself on its test's `Sim` (copied, its assertion records left out)
  and on the world. Three nodes are configured, then a minute of virtual time is measured. It is
  counted per heartbeat sent: the two schedules come from different generators and differ in steps
  a heartbeat.

Counted after a warm-up: time, allocations and reallocations by the counting allocator, and page
faults by `getrusage`. The variants run in an order rotated by one each round, with the one-minute
load average read before each round. Seven rounds, median and range. Apple M5 Max (Mac17,6), 18
cores, macOS, rustc 1.98.0, release with LTO, hyper-raft `c4aea65` with S-1; load 22.6 from other
work on the machine.

| workload | ns a step | allocations | reallocations | page faults | the machinery's allocations and reallocations |
|---|---|---|---|---|---|
| untimed 16 in flight, hyper-raft's support | 29.2 (22.6-29.2) | 0.000 | 0.000 | 0.0000 | 0.0000 |
| untimed 16 in flight, world (free) | 23.6 (22.1-23.8) | 0.000 | 0.000 | 0.0000 | 0.0000 |
| untimed 256 in flight, hyper-raft's support | 235.6 (222.0-237.2) | 0.000 | 0.000 | 0.0000 | 0.0000 |
| untimed 256 in flight, world (free) | 23.1 (21.1-23.8) | 0.000 | 0.000 | 0.0000 | 0.0000 |
| untimed 2048 in flight, hyper-raft's support | 2677.0 (2656.7-2684.8) | 0.000 | 0.000 | 0.0000 | 0.0000 |
| untimed 2048 in flight, world (free) | 26.0 (25.6-27.1) | 0.000 | 0.000 | 0.0000 | 0.0000 |
| timed 3 nodes, hyper-liveness's sim | 14.8 (12.1-14.8) | 0.000 | 0.000 | 0.0000 | 0.0000 |
| timed 3 nodes, world (ordered) | 22.0 (20.0-22.9) | 0.000 | 0.000 | 0.0000 | 0.0000 |
| timed 8 nodes, hyper-liveness's sim | 34.4 (28.9-34.6) | 0.000 | 0.000 | 0.0000 | 0.0000 |
| timed 8 nodes, world (ordered) | 33.2 (29.0-33.6) | 0.000 | 0.000 | 0.0000 | 0.0000 |
| timed 64 nodes, hyper-liveness's sim | 135.2 (119.0-135.8) | 0.158 | 0.000 | 0.0000 | 0.0000 |
| timed 64 nodes, world (ordered) | 45.5 (45.3-45.8) | 0.000 | 0.000 | 0.0000 | 0.0000 |
| hyper-liveness 3 nodes a heartbeat, its sim | 553.1 (537.3-565.8) | 2.772 | 0.000 | 0.0000 | 0.0000 |
| hyper-liveness 3 nodes a heartbeat, world (ordered) | 538.2 (524.2-546.2) | 2.848 | 0.000 | 0.0000 | 0.0000 |

The last column counts the machinery's own calls apart (`alloc::aside` around every queue and world
call). It is measured for the hyper-liveness pair; in the synthetic workloads every allocation is
the machinery's.

- **Untimed.** The world is flat in the messages in flight: 23 to 26 ns at 16, 256 and 2,048 against
  29, 236 and 2,677. The harness's `Vec::remove` shifts the network behind the drawn message, about
  1.3 ns a message in flight. Neither allocates a step.
- **Timed.** The world is even at 8 nodes and three times faster at 64, where the harness's scan of
  every node's wake is the step's cost and its `BTreeMap` allocates 0.16 times a step. At 3 nodes the
  world costs 7 ns a step more (22.0 against 14.8). Taking each part out in turn and measuring
  (5 rounds each, same load) attributes 3 ns to recording the trace (20.9 without it) and 2.4 ns to
  the exact `below`'s two divisions (19.6 with a multiply-shift instead). The harness has neither a
  trace nor exact draws. Before two changes the world cost 40.8 ns a step there: folding every
  decision into the digest as it was made (now folded from the trace at the end, 25.2) and 128-bit
  division in the node clocks (now `u64`, 22.8). An open item for S-2, when hyper-liveness's
  simulation moves.
- **hyper-liveness.** Even: 538 ns a heartbeat on the world against 553 on its harness, with
  overlapping ranges, since the crate's own work is the step's cost. The machinery allocates nothing
  on either. The totals differ (2.85 against 2.77 allocations a heartbeat) in the harness's own work
  between the two schedules: the election law the harness runs every 100 ms of virtual time
  allocates its peer lists. On the world it ran 533 times over configuration and the minute, and
  the minute sent 3,846 heartbeats; on the harness it ran 513 times, and the minute sent 3,994.

```sh
cargo bench -p hyper-sim --bench step -- 7            # all workloads, seven rotated rounds
cargo bench -p hyper-sim --bench step -- 5 "timed 3"  # one workload by name
```

