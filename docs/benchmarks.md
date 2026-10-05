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

## The fast track's release (design A)

`docs/raft.md` §3.5: a fast quorum counts holdings only, and a member holds what it holds until it
knows the index committed by a classic quorum. Measured 2026-10-05 on macOS 26.4.1, Apple M5 Max,
beside other sessions' work and two of this series' seed searches (load 5.7–6.1), against the core
before it (`dropped`, `b5e372d`), each built from its own tree with the comparison's fast count
(`fast=`, the indexes the members committed by a fast quorum while measured):

```text
cd crates/hyper-raft-compare && cargo build --release --locked
hyper-raft-compare one hyper fast <3|5> 1 64 10000 1 count      # once each
hyper-raft-compare one hyper fast <3|5> 1 64 10000 <1..15> time # 15 runs each, interleaved
```

| Voters | Core | Committed by a fast quorum | Allocations a proposal | Reallocations | Bytes asked a proposal | Time a proposal, p50 [min–max] |
|---|---|---|---|---|---|---|
| 3 | before | 10,000 of 10,000 | 45 | 0 | 9,072 | 2,144 ns [1,995–2,510] |
| 3 | release | 10,000 of 10,000 | 45 | 0 | 9,440 | 2,240 ns [2,093–2,464] |
| 5 | before | 10,000 of 10,000 | 73 | 0 | 14,808 | 3,782 ns [3,534–4,120] |
| 5 | release | 10,000 of 10,000 | 71 | 0 | 14,952 | 3,925 ns [3,801–4,457] |

The leader now holds what it hears and gives it to storage as each voter does: two allocations a
proposal, measured by a build without it (five voters, 78 against 80). The series first cost more,
and each cost was found by the count and taken out: an append round of its own to tell the members
the classic commit (50 and 80 allocations a proposal with it; the next append or heartbeat carries
it now), a copy of the leader's holding when it took the entry, and a copy of a log entry to compare
a holding with (both read in place now, `Log::any_entry`). At the first count, with all three, the
time a proposal was 24–25% above the core before.

On real disks the fast quorum seldom wins: `hyper-durable-e2e`'s `fast-kill-*` scenarios (three
members, each write proposed by a member in turn, one write of each member's log out at a time)
committed no index by a fast quorum, for the fast quorum of three is all three and the last holder's
write lands behind the write it already had out, after the leader's append round (`docs/sim.md`
§15.13). The rate on a device is a race between those two writes and is reported, never asserted.

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

## slates' regression tests, R6 and R7 (R-3)

Core step R-3's first commit (`crates/hyper-raft/ORIGIN.md`, "R-3"). Measured on 2026-10-03 on the
machine above; each figure says its load. It adds a check where a campaign starts and where one is
armed by suspicion (`Raft::lead_refusal`), bounds the fast track's proposals below the last index,
and asks the rules that hold a campaign only of a member that has one armed or due.

**Allocations**: identical to `main`'s (`9893679`) on all 36 cells (the 18 workloads of the tables
at three and five voters, in place and copying), allocations, reallocations and bytes, the counting
run of seed 1 (a scratch loop over `hyper-raft-compare one <core> <workload> <voters> <batch>
<bytes> <rounds> 1 count`, `crates/hyper-raft-compare` built `--release` in both trees).

**The schedules**: every count `tests/differential.rs`, `group.rs`, `fast.rs`, `pipeline.rs`,
`repair.rs` and `suspicion.rs` print at their default seeds, 500 lines, identical to `main`'s.

**What an idle group costs, by suspicion** (`benches/idle.rs`, `suspicion 10000 <0|400>` under
`/usr/bin/time -l`, the difference taken, five interleaved rounds, load 10.5 and 14.3–14.9):

| | instructions a group a period | cycles a group a period |
|---|---|---|
| `main` | 326–333 | 156–335 |
| the check asked as `main` asked its rules, of every member at every scan | 410–411 | 202–219 |
| the rules asked only of a member with a campaign armed (as committed) | 51–54 | 32–73 |

The check alone added 84 instructions a group a period to an owner's scan: `deadline` asked every
rule that holds a campaign (`may_campaign`, the detectors' quorum, and now the room to lead) of every
member, though a follower that trusts its leader has no campaign armed and its answer was thrown
away. Asking them only where `Watch::due` finds a campaign armed, and in `wake_follower` only once
one is due, leaves the scan at three timer reads a member: a sixth of `main`'s instructions. The
answers are the same wherever they are used, so every schedule decides as on `main` (above).

## slates' tests for R4, R5, R20 and R21 (R-3)

Core step R-3's second commit (`crates/hyper-raft/ORIGIN.md`, "R-3"). Measured on 2026-10-03 on the
machine above. It adds a counter a follower ticks (`Raft::silence`) and the tests.

**Allocations**: identical to `main`'s on all 36 cells (as R6 and R7's, above).

**The schedules**: of the 500 lines the suites print at their default seeds, one moved: the
pipeline mix whose committed pages are 64 bytes (`focal, three writes out, narrow`), where a change
waits longest unapplied and so a deferred campaign keeps a lease longest under raft-rs's counter: 719
→ 715 entries committed, in 66 terms both. Every other line is `main`'s.

**A proposal's cost at any backlog** (slates' measurement tool on this core, `cargo bench -p
hyper-raft --bench backlog -- 50000`, `benches/backlog.rs`): a leader of five whose followers
answer nothing, each proposal taken and written out as an owner does and its messages dropped; three
runs, load 4.7–6.5:

| Backlog | this core, ns a proposal (three runs) | allocations, reallocations, bytes a proposal | slates before its fix | slates after |
|---|---|---|---|---|
| 1,000 | 260 / 329 / 247 | 4.00, 0.00, 160 | 129 µs | 102 ns |
| 5,000 | 229 / 293 / 228 | 4.00, 0.00, 160 | 20.0 ms | 96 ns |
| 10,000 | 264 / 222 / 216 | 4.00, 0.00, 160 | not reached | 61–67 ns |
| 50,000 | 170 / 258 / 206 | 4.00, 0.00, 160 | not reached | 61–67 ns |

Flat, as slates' fix made its own: this core's commit rule sorts the voters' matches and its
configuration is its tracker's. Its figure is the owner's whole turn (the proposal, the `Ready`, the
write to the harness's memory, `advance_append`) where slates' is `append_command` alone, so the two
compare in shape, not in nanoseconds. The allocations are counted exactly (`tests/backlog.rs` holds
them equal at backlogs of 1,000 and 49,000).

## The window a member is sent ahead of its answers (R16)

Core step R-3's third commit (`crates/hyper-raft/ORIGIN.md`, "R16"). Measured on 2026-10-03 on the
machine above (Apple M5 Max, 18 cores, 128 GB, macOS 26.4.1).

**Allocations**: the same allocations and reallocations as `main`'s on all 36 cells; the bytes are
`main`'s on 32 of them. The four snapshot cells allocate 24 bytes a round more at three voters and 40
at five: 8 bytes a member, each time the tracker is built anew at a snapshot, for `Inflights` holds
its page's cost and whether it is draining, and its `VecDeque` is a word shorter than the ring's two
indexes beside a `Vec`.

**The schedules**: of the 62 lines the suites print at their default seeds besides the members'
states (519 with them), the 31 of the differential and the other suites are `main`'s. The 31 of
`tests/pipeline.rs` moved, all of whose schedules set members' windows at run time to 1, 24, 96 or
512 bytes or a megabyte (`Mix::windows`), and most run focal's window of 256 bytes: an
append is charged its 96 fixed bytes now, so such a window holds an append or two where it held
four to eight entries, and one that filled waits for half of it. Over the 16 summaries with faults at
rest, 16,698 entries were committed and 44 of 384 groups were left waiting on a mark at a schedule's
end; with R16, 15,958 and 57; with R16 but no wait for half a window, 16,369 and 50. A window of an
append or two waits for both under the rule (its half is less than an append); no path the rule
measures has one. Each summary's own checks pass, and the durability oracle holds at every step.

**slates' measurement on this core** (`tests/timed.rs`, `tests/support/timed.rs`): five voters on
Microsoft's published P50 round trips among East US, West Europe, Japan East, Southeast Asia and
Brazil South, each path 10 Mbit/s with up to 5 ms of jitter, member 1 (East US) leading throughout,
proposals of 8 bytes at the row's rate from 2 s for 30 s, and three of the slowest round trips after
for what was proposed last to commit; 20 seeds a row, each row the median over seeds of each seed's
median and 99th percentile commit latency and commits a second, bytes and resent entries summed.
This core sends an append as each proposal is made, where slates' drive sent one batch a follower a
period: its appends are one or a few entries, and its window holds them by their records. The
simulation is deterministic, so a row's numbers are exact for its seeds; the load (10.5–12.4 for
R16's run, 287 s; 7.5–9.8 for commit 2's, 202 s) moves only its wall time. "One batch" is a window
of slates' batch budget, 4,367 bytes; "the rule" is `inflight_window` of each path's carriage, its
rate over its round trip's tail.

Paths that deliver in order, as one stream does (slates' simulation, whose appends a period apart
never reordered). "Before" is commit 2's core with the harness put on it, its window charged its
entries alone and bounded by focal's 128 places as well:

| Offered, loss | Window | before: median / p99, commits a second, MB sent, entries resent | R16: median / p99, commits a second, MB sent, entries resent |
|---|---|---|---|
| 2,000/s, none | one batch | 7,384 / 14,507 ms, 1,089/s, 299.4, 0% | 7,908 / 15,437.5 ms, 1,023/s, 79.1, 0% |
| 2,000/s, none | four batches | 125 / 153.5 ms, 2,000/s, 501.0, 0% | 135 / 170 ms, 2,000/s, 349.4, 0% |
| 2,000/s, none | one round trip's carriage | 125 / 153.5 ms, 2,000/s, 501.0, 0% | 125 / 127 ms, 2,000/s, 1,082.0, 0% |
| 2,000/s, none | the rule | 125 / 153.5 ms, 2,000/s, 501.0, 0% | 125 / 127 ms, 2,000/s, 1,082.0, 0% |
| 4,000/s, none | one batch | 11,471.5 / 22,596 ms, 1,089/s, 295.9, 0% | 11,773.5 / 23,069 ms, 1,028/s, 79.1, 0% |
| 4,000/s, none | four batches | 126 / 186.5 ms, 4,000/s, 549.7, 0% | 926.5 / 1,651.5 ms, 3,914/s, 292.3, 0% |
| 4,000/s, none | one round trip's carriage | 126 / 180 ms, 4,000/s, 626.6, 1% | 126 / 127 ms, 4,000/s, 2,162.0, 0% |
| 4,000/s, none | the rule | 126 / 180 ms, 4,000/s, 626.6, 1% | 126 / 127 ms, 4,000/s, 2,162.0, 0% |
| 2,000/s, 1 % | one batch | 7,673.5 / 15,014.5 ms, 1,053.5/s, 100.4, 8% | 8,109 / 15,728 ms, 1,008/s, 79.2, 2% |
| 2,000/s, 1 % | four batches | 200 / 340.5 ms, 2,000/s, 338.3, 24% | 197.5 / 333 ms, 2,000/s, 272.5, 19% |
| 2,000/s, 1 % | one round trip's carriage | 197.5 / 319 ms, 2,000/s, 393.3, 29% | 202.5 / 328 ms, 2,000/s, 548.4, 31% |
| 2,000/s, 1 % | the rule | 197.5 / 319 ms, 2,000/s, 393.3, 29% | 202.5 / 328 ms, 2,000/s, 548.4, 31% |
| 4,000/s, 1 % | one batch | 11,575.5 / 22,818 ms, 1,060.5/s, 100.3, 8% | 11,813 / 23,186.5 ms, 1,008/s, 79.2, 2% |
| 4,000/s, 1 % | four batches | 2,379.5 / 4,550 ms, 3,521/s, 336.0, 17% | 3,246 / 6,486.5 ms, 3,266.5/s, 267.3, 8% |
| 4,000/s, 1 % | one round trip's carriage | 232 / 387.5 ms, 4,000/s, 620.1, 32% | 246.5 / 411 ms, 4,000/s, 1,056.0, 36% |
| 4,000/s, 1 % | the rule | 232 / 387.5 ms, 4,000/s, 620.1, 32% | 246 / 418 ms, 4,000/s, 1,057.8, 36% |

slates recorded, on its drive: at 2,000 a second, one batch out at a time 7,319 / 14,378 ms and
1,029 a second, and its derived window 172 / 222 ms and 1,988 a second; with 1 % loss 7,357 ms and
201 / 321 ms; at 4,000 a second its drive, one batch a period, caps the group near 2,058 a second.
On this core one batch out at a time is 7,908 ms and 1,023 a second, and the rule 125 / 127 ms and
every proposal, 202.5 / 328 ms with 1 % loss, and 126 / 127 ms at 4,000 a second: slates'
improvement, on this core.

What R16 changed against commit 2: the rule's window was bounded before by the 128 places, not its
bytes (one round trip's carriage and two were alike), so proposals waited for a place and went
together: the same medians, a p99 of 153.5 ms at 2,000 a second and 180 ms at 4,000 against 127 ms
now, and half the bytes. With no count each proposal goes at once as its own append, its 96 fixed
bytes beside an entry of 33: this harness's owner proposes one entry a turn, where an owner that
takes every proposal of its turn into one `Ready` sends them in one append. A fixed window holds
fewer entries now that it is charged what the path carries: four batches, 17,468 bytes, held 529
entries' bytes before and holds 135 one-entry appends now, where a commit at 4,000 a second waits on
the second-nearest follower, Brazil South at 117 ms, and so needs 468 entries in flight to it, 15.4
kB of them besides their appends' fixed bytes. With 1 % loss at 4,000 a second the rule resends more
small appends (36 % of entries against 32 %) and its median is 246 ms against 232: what follows a
lost append is refused and sent again, which R17 is for.

Paths that reorder, each message after its own jitter (as datagrams and messages on streams of their
own do; focal sends a peer's frames each on its own stream, focal 27 §11), at 2,000 a second:

| Offered, loss | Window | before: median / p99, commits a second, MB sent, entries resent | R16: median / p99, commits a second, MB sent, entries resent |
|---|---|---|---|
| 2,000/s, none | one batch | 8,280 / 16,226.5 ms, 973/s, 124.9, 41% | 8,525 / 16,913 ms, 926.5/s, 104.0, 31% |
| 2,000/s, none | four batches | 171.5 / 302 ms, 2,000/s, 1,596.1, 90% | 168 / 362 ms, 2,000/s, 1,201.3, 87% |
| 2,000/s, none | one round trip's carriage | 257 / 1,026.5 ms, 2,000/s, 2,768.7, 94% | 258 / 919 ms, 2,000/s, 2,779.7, 94% |
| 2,000/s, none | the rule | 263.5 / 2,343.5 ms, 1,905/s, 3,200.4, 95% | 264 / 2,083.5 ms, 1,947/s, 3,085.3, 95% |
| 2,000/s, 1 % | one batch | 8,367 / 16,612.5 ms, 951/s, 120.8, 40% | 8,778.5 / 17,058.5 ms, 914/s, 102.0, 32% |
| 2,000/s, 1 % | four batches | 164.5 / 277.5 ms, 2,000/s, 1,384.4, 88% | 161.5 / 321 ms, 2,000/s, 1,127.1, 86% |
| 2,000/s, 1 % | one round trip's carriage | 198.5 / 326.5 ms, 2,000/s, 2,293.7, 92% | 189 / 311.5 ms, 2,000/s, 2,244.8, 92% |
| 2,000/s, 1 % | the rule | 198.5 / 312.5 ms, 2,000/s, 2,446.9, 93% | 191 / 325.5 ms, 2,000/s, 2,364.5, 93% |

Here a member refuses every append that arrives ahead of one still on its way, and the leader sends
it all again: of what is sent, 31–41 % again with one batch and 86–95 % with any larger window, before
R16 and after. That is R17's.

## What arrives ahead of a hole (R17)

Core step R-3's fourth commit (`crates/hyper-raft/ORIGIN.md`, "R17"). Measured on 2026-10-03 on the
machine above; the timed sweep ran 14:53–15:06 at load 3.9–10.3 (777 s).

**Allocations**: R16's on 28 of the 36 cells. The four snapshot cells allocate 8 bytes a member
more each time a tracker is built (`Progress::repaired`). The four catch-up cells make 40 fewer
allocations (20,680 → 20,640 at three voters, 23 kB fewer bytes) and peak 41 kB higher: the
returning member keeps what its leader sends past its end, and its leader sends the hole alone
where it probed and sent what followed again; what is kept is held until the hole fills.

**The schedules**: the 31 lines of the differential and the other suites are R16's. The 31 of
`tests/pipeline.rs` moved, R17 reached in every setting whose members keep what arrives ahead: 40 to
244 entries a line taken from what was kept (`Coverage::ahead`), each acknowledged only with the
write that holds it (the durability oracle at every step). With faults at rest 9,997 entries were
committed and 50 of 384 groups left waiting on a mark (R16: 10,179 and 57); without faults, 4,920
committed (5,131).

**slates' measurement on this core** (as R16's section above; `tests/timed.rs`). Under raft-rs's
rule (`Ahead::Refused`) this core gives R16's numbers exactly (all 80 runs of seed 0 compared). On
paths that reorder:

| Offered, loss | Window | R16: median / p99, commits a second, MB sent, entries resent | R17: median / p99, commits a second, MB sent, entries resent |
|---|---|---|---|
| 1,000/s, none | one batch | 1,531 / 3,298 ms, 923.5/s, 218.9, 69% | 190 / 256 ms, 1,000/s, 110.0, 31% |
| 1,000/s, none | the rule | 147 / 228 ms, 1,000/s, 1,091.7, 92% | 122 / 126 ms, 1,000/s, 541.9, 29% |
| 2,000/s, none | one batch | 8,525 / 16,913 ms, 926.5/s, 104.0, 31% | 7,784.5 / 15,194 ms, 1,043/s, 107.7, 26% |
| 2,000/s, none | the rule | 264 / 2,083.5 ms, 1,947/s, 3,085.3, 95% | 123 / 126 ms, 2,000/s, 1,126.0, 38% |
| 4,000/s, none | one batch | 12,154 / 23,779 ms, 925.5/s, 103.4, 31% | 11,706 / 22,941 ms, 1,044/s, 107.9, 26% |
| 4,000/s, none | the rule | 5,099.5 / 18,542 ms, 1,588/s, 4,244.3, 96% | 123 / 126 ms, 4,000/s, 2,346.6, 44% |
| 1,000/s, 1 % | one batch | 1,908.5 / 3,570 ms, 914/s, 210.0, 68% | 247 / 468 ms, 1,000/s, 108.7, 31% |
| 1,000/s, 1 % | the rule | 146 / 231 ms, 1,000/s, 1,044.9, 91% | 185.5 / 432.5 ms, 1,000/s, 459.7, 38% |
| 2,000/s, 1 % | one batch | 8,778.5 / 17,058.5 ms, 914/s, 102.0, 32% | 7,926.5 / 15,496 ms, 1,023/s, 107.6, 27% |
| 2,000/s, 1 % | the rule | 191 / 325.5 ms, 2,000/s, 2,364.5, 93% | 216 / 403 ms, 2,000/s, 995.9, 42% |
| 4,000/s, 1 % | one batch | 12,148.5 / 23,806 ms, 922/s, 102.4, 31% | 11,748.5 / 23,073 ms, 1,026.5/s, 107.5, 27% |
| 4,000/s, 1 % | the rule | 5,181 / 18,902 ms, 1,588.5/s, 4,242.5, 96% | 235 / 448.5 ms, 4,000/s, 2,076.8, 45% |

On paths that keep order, with 1 % loss:

| Offered, loss | Window | R16: median / p99, commits a second, MB sent, entries resent | R17: median / p99, commits a second, MB sent, entries resent |
|---|---|---|---|
| 1,000/s, 1 % | one batch | 443 / 737 ms, 1,000/s, 76.3, 4% | 376 / 606 ms, 1,000/s, 75.9, 3% |
| 1,000/s, 1 % | four batches | 168 / 260.5 ms, 1,000/s, 249.6, 26% | 190 / 402 ms, 1,000/s, 295.9, 12% |
| 1,000/s, 1 % | the rule | 168 / 255 ms, 1,000/s, 305.5, 28% | 200.5 / 493 ms, 1,000/s, 354.0, 15% |
| 2,000/s, 1 % | one batch | 8,109 / 15,728 ms, 1,008/s, 79.2, 2% | 8,069.5 / 15,640.5 ms, 1,016/s, 79.9, 3% |
| 2,000/s, 1 % | four batches | 197.5 / 333 ms, 2,000/s, 272.5, 19% | 176.5 / 341.5 ms, 2,000/s, 309.0, 7% |
| 2,000/s, 1 % | the rule | 202.5 / 328 ms, 2,000/s, 548.4, 31% | 227 / 462 ms, 2,000/s, 640.1, 12% |
| 4,000/s, 1 % | one batch | 11,813 / 23,186.5 ms, 1,008/s, 79.2, 2% | 11,799.5 / 23,186.5 ms, 1,013.5/s, 80.0, 3% |
| 4,000/s, 1 % | four batches | 3,246 / 6,486.5 ms, 3,266.5/s, 267.3, 8% | 1,676.5 / 2,903 ms, 3,736/s, 291.1, 5% |
| 4,000/s, 1 % | the rule | 246 / 418 ms, 4,000/s, 1,057.8, 36% | 245 / 505.5 ms, 4,000/s, 1,241.4, 12% |

Where paths reorder, a member refuses every append that overtakes one still on its way; with R17 it
keeps it, and its leader sends again only what is missing before it, so the rule's window commits
every proposal at 123 / 126 ms up to 4,000 a second, where R16 fell behind at 4,000 (1,588 a second)
sending nearly everything twice. With 1 % loss on paths that keep order, R17 sends 12 % of entries
again against 31 %, but its p99 is higher (462 ms against 328 at 2,000 a second) and its bytes too
(640 MB against 548): each repair is a message of its own, with its fixed bytes, where raft-rs's
sending of what followed a hole goes in pages and covers a second loss behind the first by chance;
and this harness sends each proposal as its own message, so a loss in a hundred messages is a hole
every hundred entries. With four batches, whose window makes proposals wait and go together, R17's
median is ahead at 2,000 a second (176.5 ms against 197.5) and its commits at 4,000 (3,736 a second
against 3,266.5), and behind at 1,000 (190 / 402 ms against 168 / 260.5).

The member's half alone, with raft-rs's leader, was measured first and collapsed on paths that
reorder: at 1,000 a second the rule's window committed 384 a second at a 4,988 ms median, 99.5 % of
what it sent sent again (20 seeds), where R16 committed every proposal at 147 ms (`docs/raft.md`
§3.2 has why, and the two leader's halves measured and rejected after it).

## A learner caught up in rounds (R13)

Core step R-3's fifth commit (`crates/hyper-raft/ORIGIN.md`, "R13"). Measured on 2026-10-03 on the
machine above, load 4.7–4.8.

**Allocations**: R17's on all 36 cells, bytes included: a leader with no learner staged counts
nothing more than a tick.

**The schedules**: every line the suites print at their default seeds is R17's: no schedule stages
a learner, and the rule asks nothing of a leader that stages none.

**slates' replay of the thesis's Figure 4.4(a)** (`a_staged_newcomer_leaves_no_availability_gap_where_a_direct_one_does`):
voters 1, 2 and 3 hold forty entries, member 4 joins with an empty log and the voters become four,
then voter 3 fails, so every commit needs member 4. Each append carries two entries and one is out
to a member at a time, as slates' drive sent its followers. A round here carries messages one way,
two to a round trip, where slates' round took its reply at once.

| Newcomer | Rounds from voter 3's loss to the first commit after it | slates |
|---|---|---|
| added directly as a voter | 45 (22½ round trips) | 21 round trips |
| caught up as a learner first (`RawNode::catch_up`), then promoted | 2 (one round trip) | 1 |

## When a log is compacted (R22)

Core step R-3's sixth commit (`crates/hyper-durable/ORIGIN.md`, "R22"; `docs/durable.md` §6.1), in
the shell: the core is unchanged, and so are its counting runs and schedules. Measured on
2026-10-03 on the machine above, load 6.9–7.2; what follows is counted, not timed, and every run
counts the same.

**slates' finding** (`a_leader_that_waits_for_its_followers_sends_them_entries_not_images`,
`tests/shell.rs`): a leader of three whose third voter takes each round a round behind the
majority, its window (eight appends) less than a round of sixteen entries of eight bytes, through
twelve rounds; expansion one, on a state that keeps every entry, so that the image grows with what
was applied. slates measured the same on its council (`fold.rs`, 2026-09-28).

| Compaction | Rounds compacted | Images sent to the third voter | slates |
|---|---|---|---|
| by the rule, which waits for the third while the log holds less than twice the threshold | 2, 5 and 11 | 0 | none |
| at the same rounds, the moment the majority holds them | 2, 5 and 11 | 3 | one at every compaction, and the third never compacted itself |

**A long history** (`a_sole_voter_that_compacts_when_due_holds_its_log_within_the_rule`, slates'
`measure_a_long_council_history`): a sole voter proposes one entry of eight bytes at a time and
compacts whenever the rule says it is due; checked after every proposal, its log never holds more
than the expansion times its image and the entry that crossed it. Without compaction it holds every
entry, 33 bytes each. Two states: one that keeps every entry (its image grows with the log, as
slates' configuration grew), and one that keeps one value (a register's: its image stays 40 bytes).

| State | Expansion | Proposals | Entries held at the end | Bytes held | Image | Compactions | Image bytes written | Entry bytes written |
|---|---|---|---|---|---|---|---|---|
| every entry | 1 | 250 | 0 | 0 | 8,032 | 8 | 16,128 | 8,283 |
| every entry | 1 | 1,000 | 25 | 825 | 31,232 | 10 | 63,200 | 33,033 |
| every entry | 1 | 4,000 | 213 | 7,029 | 121,216 | 12 | 245,952 | 132,033 |
| every entry | 4 | 250 | 11 | 363 | 7,680 | 4 | 9,632 | 8,283 |
| every entry | 4 | 1,000 | 761 | 25,113 | 7,680 | 4 | 9,632 | 33,033 |
| every entry | 4 | 4,000 | 2,830 | 93,390 | 37,472 | 5 | 47,104 | 132,033 |
| one value | 1 | 250 | 1 | 33 | 40 | 125 | 5,000 | 8,283 |
| one value | 1 | 1,000 | 1 | 33 | 40 | 500 | 20,000 | 33,033 |
| one value | 1 | 4,000 | 1 | 33 | 40 | 2,000 | 80,000 | 132,033 |
| one value | 4 | 250 | 4 | 132 | 40 | 50 | 2,000 | 8,283 |
| one value | 4 | 1,000 | 4 | 132 | 40 | 200 | 8,000 | 33,033 |
| one value | 4 | 4,000 | 4 | 132 | 40 | 800 | 32,000 | 132,033 |

- **One value**: the log is bounded whatever the history, and the images are the thesis's share of
  what is written: at four, 32,000 of 164,033 bytes, 19.5 % (the thesis: about 20 %); at one,
  37.7 %, against a half, for a compaction waits for the entry that crosses the threshold.
- **Every entry**: each image holds the whole history, so the compactions come as it doubles at
  one (twelve by 4,000), and the images written are about twice the entries; at four they are a
  third of them. This is the thesis's trade where the state grows as fast as the log.
- **The schedules**: `tests/sim.rs`'s five shapes reach the same counts at 128 seeds a shape with
  the rule's count checked after every step as without it; the check held at 1,000 seeds a shape.

## Every bound from what the owner states (`Limits::derive`)

Core step R-3's last commit (`crates/hyper-raft/ORIGIN.md`, "Limits::derive"; `docs/raft.md`
§3.2). Measured on 2026-10-03 on the machine above, load 7.7–10.3; what follows is counted, and
every run counts the same.

**Allocations**: the comparison's members state a message of 8 MiB, their group's voters and queues
of 32 MiB each; allocations, reallocations, bytes and peaks are R22's (the same core as R13's) on
all 36 cells. A bound limits what may wait; none is reserved to it but the writes out, which are as
before.

**The schedules**: every line the suites print at their default seeds is R22's but one, the group
of both cores, whose raft-rs members draw their timeouts from their thread (`tests/group.rs`): three
runs of the one tree printed 452, 416 and 455 terms led by raft-rs. The harnesses state a message
twice the largest append they send, their groups' members, and queues of four such messages; where
a bound now stands elsewhere than focal's literal (a fast window of the proposals a vote carries,
where it was 256), no schedule's outcome turned on it.

**The derivations, checked exactly** (`every_bound_is_derived_from_what_the_owner_states`, at a
message of 64 KiB, five members, queues of a MiB, three writes out): a message of
`entries_per_message` (2,617) empty entries fits the stated bytes and one more does not; a member
that holds every proposal it may has a vote that fits.

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
acted on (`F_FULLFSYNC` on macOS), driven in place. Since timing step L-4 (`docs/timing.md` §2.9,
"On real detectors") each member elects by suspicion on its own node-pair liveness stream, the
stream's timing law over its own measurements, as hyper-durable-e2e's members do; the test tells no
member what to believe and computes no time of its own.

What the test no longer has: the measured tick (the 95/95 one-sided tolerance bounds, Wilks 1941,
of a flush, a loopback round trip and a 1 ms wait, 59 samples each), `--tick-ms`, the election and
heartbeat tick counts, the answer budget in ticks, the elections each wait allowed for (Ongaro's
split chance over ten tick slots, Bonferroni across the run's caused elections), and the member's
bounds derived from that budget. What derives each count now:

- **Waits.** Every wait goes on while the group moves — any member's term, commit, applied index,
  last index or restarts seen, or, while a pair is unjudged, the heartbeats it took — and fails once
  a quiet period passes with nothing moved: the longest any member's report states for its
  detection (`η + α`) or, while a pair is unjudged, the interval its heartbeats come at
  (`PairReport::interval`), then its election's span and three rounds and an ask's three, never less than
  RFC 6298's one-second retransmission timeout (§2.1, §2.4). Quiet is time in which the test saw
  the group and nothing moved: a look that began before the quiet period ended counts as movement
  unseen, so an ask whose answer was lost spends the test's own timeout, not the group's; a look in
  which a member did not answer decides nothing (the member is waited on while its process runs,
  and its answer after counts as movement); and the time the members report their one thread spent
  in their logs' writes since the watch last heard them extends the watch by the most any one
  spent. A member's silence (from its first unanswered ask to its latest, less the timeout the
  test waited on the latest) is excused only up to the longest one write of a log any member has
  reported, or the stall the test ordered, and the quiet period; past it the wait fails, naming
  the member. An ask waits that timeout for its answer, by a peek (`wire::arrives`). A wait that
  gives up prints each member's last report and the reports of a look after. The rule is one
  module, `hyper_raft_e2e::quiet`, which hyper-durable-e2e's waits keep too (`docs/timing.md` §2.9,
  "The same rule in the other harnesses").
- **Every report** holds the member to its bookkeeping: each write it keeps waiting has an entry
  above what it applied in the log of the term it leads (`stray`, asserted zero on every report).
- **A phase** writes one entry more than an append carries: the datagram the platform allows
  (`wire::largest`) less a message's fixed bytes, over the scenario's shortest entry. A member that
  missed a phase catches up over more than one append. 136 writes on macOS (a 9,216-byte datagram),
  977 on Linux (65,507).
- **The member's bounds** are the scenario's: keys, the keys it writes; asks kept waiting, the
  writes in flight and the client's one; the log, one entry a write and one a term
  (`Wal::open`'s `max_writes`), for a leader proposes no write its log already holds and each term's
  leader appends one empty entry.
- **Writes in flight** (`leader-killed`) go from a client of their own to whoever leads, and those
  not answered are sent again while the leader's log holds fewer than all the scenario's writes,
  but only to a leader that answered since: on Linux a phase overflows a socket's buffer, and the
  leadership moves under such a burst.
- **A member restarted** takes a port the system gives, and every member up is told where it
  listens: a port freed by a process killed is the system's to give to whoever binds next. It runs
  under the next count of its starts, kept beside its log (`src/run.rs`), so its peers report one
  restart and refuse a heartbeat of its last run as stale.

| Scenario | Asserts |
|---|---|
| commits-3, commits-5 | a phase of writes answered, each read back at once by a linearizable read; every member applied the same history to the same index |
| leader-killed | writes in flight sent to the leader until its log holds them all, then the leader killed with `SIGKILL` (`TerminateProcess` on Windows); the others elect in a later term; every answered write reads back, every unanswered one reads as written or as never written; the killed member restarts on its log, every member that heard its last run reports the restart, and it applies the same history |
| follower-restarts | a follower killed, a phase answered without it, restarted on its log, its restart reported, caught up to the leader's commit with the same history |
| partition | the leader of five cut off by a drop filter inside its process, its heartbeats with its Raft messages: a read it is asked at once is never answered with a value and a write never acknowledged; the others elect; the member cut off suspects all four and all four suspect it; a key written after the cut never reads stale from it; once lifted it follows and every member applies the same history |
| all-killed | every member killed at once and restarted on its log: every answered write reads back |
| stalled-devices | every member's device answers no flush for longer than a wait can be quiet and look (the quiet period in force and two looks' retransmission timeouts); a write sent into the stall commits once the devices go on; a phase after; every answered write reads back and every member applies the same history |
| member-stopped | a follower stopped mid-scenario (`SIGSTOP`; on Windows the member holds its thread outside any write): the wait for a write it cannot apply fails, naming it, once its silence passes what the members' longest write and the quiet period excuse, rather than hanging; let go, it applies the same history as the others |

Every scenario also prints, for each member, its floors and the longest it went in one flush and
between two reads of its socket, and for each pair what the configurator was fed, the detector in
force, and its suspicions against Theorem 7's allowance (`docs/timing.md` §2.9).

Runs of the whole binary, `cargo test -p hyper-raft-e2e --test cluster`, on the code as committed
(`c4530be`, over hyper-raft `2525538`), 2026-10-03. The Linux runs are containers in Docker
Desktop's virtual machine on the same Mac (18 CPUs, Linux 6.12.76), each limited to the CPUs named,
with as many busy loops beside the test; the load is the one-minute average as each run began on
macOS and as it ended on Linux (the virtual machine's). The binaries were built once, before the
runs.

| host | runs | passed | a run |
|---|---|---|---|
| macOS 26.4.1, Apple M5 Max, load 25–66 | 10 | 10 | 51–69 s |
| Linux (Docker, rust:1.98.0, aarch64), four CPUs and four busy loops, load 8.6–11.0 | 10 | 10 | 62–163 s |
| Linux, two CPUs and two busy loops, load 4.2–8.8 | 10 | 10 | 46–106 s |
| Linux, one CPU and one busy loop, load 4.0–9.2 | 30 | 30 | 61–225 s |
| CI, the gates once on each of ubuntu-24.04, ubuntu-24.04-arm, macos-15, macos-15-intel, windows-2025 and windows-11-arm | 6 | 6 | 43, 44, 24, 38, 235 and 588 s |

A scenario took, on macOS, 3.5–5.2 s (commits-3), 3.8–6.8 s (commits-5), 5.6–9.5 s (leader-killed),
5.0–10.7 s (follower-restarts), 8.1–12.0 s (partition), 3.6–5.5 s (all-killed), 13.1–15.4 s
(stalled-devices, whose devices held their flushes 7.0 s) and 8.0–9.7 s (member-stopped); on Linux,
in the same order, 1.9–33.5 s, 2.6–19.6 s, 12.3–40.3 s, 3.4–44.3 s, 5.6–39.2 s, 1.9–39.0 s,
10.9–41.7 s and 4.9–26.7 s. CI's windows-2025 job was run twice: the first failed in
hyper-liveness's `tests/processes.rs` (a member killed in its first heartbeats: the wait gave up
after one quiet second with every member's flush in flight, the longest 372–467 ms on that runner),
before this crate's tests ran; the second passed. The macOS runs shared the machine with other
sessions' builds and with the Linux runs. One macOS run's output (58.6 s; each scenario prints its
line, the looks that did not hear every member and how far waits were extended for members in their
logs' writes, then each member's floors, longest flush and longest time between two reads of its
socket, and each pair's account; commits-3 in full, the others' first lines):

```
ok commits-3: 3 members; 136 writes answered and read back (22.66 ms per write and read, 39.58 ms the slowest); all applied index 137 alike
  looks that did not hear every member: 0, the longest silence 0.0 ms against 0.0 ms excused; waits extended 45.2 ms for members in their logs' writes, at most 23.1 ms at once
  member 1: G 3.564 ms, E[flush] 8.041 ms, T_E 78.2 ms; longest flush 16.3 ms, longest between two reads 18.2 ms
    1->2: own, 1 configurations, 148 sent, 134 taken, 0 refused unproven; fed p_L 0.0421, E(D) 0.000 ms, sd 4.586 ms; eta 19.149 ms, alpha 15.549 ms, recurrence 161.2 ms, U 4.94e-1; 3 suspicions, allowance 23.70
    1->3: own, 1 configurations, 96 sent, 162 taken, 0 refused unproven; fed p_L 0.0527, E(D) 0.000 ms, sd 5.017 ms; eta 20.994 ms, alpha 17.438 ms, recurrence 167.9 ms, U 4.99e-1; 1 suspicions, allowance 23.10
  member 2: G 5.408 ms, E[flush] 9.913 ms, T_E 89.4 ms; longest flush 18.1 ms, longest between two reads 18.2 ms
    2->1: own, 1 configurations, 137 sent, 142 taken, 0 refused unproven; fed p_L 0.0784, E(D) 0.000 ms, sd 4.390 ms; eta 26.205 ms, alpha 20.712 ms, recurrence 222.0 ms, U 4.22e-1; 4 suspicions, allowance 56.25
    2->3: own, 1 configurations, 140 sent, 158 taken, 0 refused unproven; fed p_L 0.0530, E(D) 0.000 ms, sd 4.404 ms; eta 20.668 ms, alpha 15.159 ms, recurrence 163.1 ms, U 5.74e-1; 6 suspicions, allowance 43.11
  member 3: G 5.440 ms, E[flush] 10.431 ms, T_E 96.4 ms; longest flush 20.6 ms, longest between two reads 23.3 ms
    3->1: own, 1 configurations, 163 sent, 89 taken, 0 refused unproven; fed p_L 0.1311, E(D) 0.000 ms, sd 3.986 ms; eta 10.376 ms, alpha 5.973 ms, recurrence 26.0 ms, U 3.84e0; 0 suspicions, allowance 28.81
    3->2: own, 1 configurations, 159 sent, 136 taken, 0 refused unproven; fed p_L 0.0421, E(D) 0.000 ms, sd 4.722 ms; eta 16.768 ms, alpha 11.727 ms, recurrence 95.4 ms, U 9.59e-1; 4 suspicions, allowance 38.85 [4.6 s]
…
ok commits-5: 5 members; 136 writes answered and read back (28.10 ms per write and read, 51.31 ms the slowest); all applied index 137 alike
ok leader-killed: leader 2 (term 1) killed with the 129 writes in flight in its log; 3 elected in term 2; 387 answered writes read back; 129 of them answered before the kill, and 0 of those unanswered committed by the new leader; member 2 restarted on its log, its restart reported, and applied index 390 alike
ok follower-restarts: follower 1 killed after 122 writes, 122 written without it, restarted on its log, its restart reported, caught up to index 245 alike; 244 writes read back
ok partition: leader 1 of 5 cut off; at once it answered a read with Some(NotLeader(0)) and a write with Some(NotLeader(0)), and later the read of a replaced key with Some(NotLeader(0)); it suspected all four and all four suspected it; 2 elected in term 2; after the filter lifted, 5 leads and all applied index 277 alike; 273 writes read back
ok all-killed: every member killed after 134 answered writes and restarted on its log; 1 leads in term 2; 134 writes read back; all applied index 136 alike
ok stalled-devices: every member's device held its flushes 7.0 s after 125 writes; a write sent into the stall committed once the devices went on; 250 writes read back; all applied index 253 alike
ok member-stopped: member 2 stopped after 127 writes; the wait for it failed after 3.0 s of its silence, naming it; let go, it applied index 129 with the others; 128 writes read back
```

A write and its read took 1.7–34 ms on average a run on Linux (977 writes a phase, on a 65,507-byte
datagram) and 22–46 ms on macOS (136 a phase, `F_FULLFSYNC` under load). The detectors mistake often
(`docs/timing.md` §2.9), and a mistake about a leader can cost an election. In leader-killed the
leader's term at the kill was 1–2 on macOS and 2–22 on Linux, where the scenario's first election
makes term 1; a burst of writes in flight moves the leadership, and each new leader commits what it
took: the writes in flight were all answered before the kill in 5 of 10 macOS runs and 30 of 50
Linux runs, and in the others the leader died holding 116–129 unanswered on macOS and 30–208 on
Linux, every one of which the new leader committed. The stopped member's wait failed, naming it,
after 3.0 s of its silence on macOS, 1.0–3.1 s on Linux and 1.0–3.1 s across CI. The excuse it
passed is the members' own (their longest write and the quiet period): 1.0 s on macOS and 1.0–1.9 s
on Linux; in a macOS run over `ecbf071` under load 60, whose members flushed for up to 551 ms and
stated a round of 868 ms, it was 6.9 s.

**The stall, found and fixed.** Before this change four runs in about 160 on Linux (none on macOS)
stopped with the group judged quiet while the test still had writes to land; a recurrence's last
look was a healthy idle group (one leader in term 11 trusted by both followers, every member at
commit, applied and last index 1,411, the leader's log holding 1,400 of the scenario's 1,844 writes,
the 444 left resent to it on every look and none taken). The bookkeeping lead (a write the leader
keeps waiting that no apply would answer) was checked and is not it: every report now asserts it
(`stray`, above), and it never broke. With each member reporting its longest flush and its longest
time between two reads of its socket, 101 runs in four to six one-CPU containers at once, each with
a busy loop, passed and showed the cause: flushes of up to 1.8 s, the same 1,807 ms on members of
groups in different containers at once, and each member's longest time between two reads its longest
flush, to within a millisecond, whenever that flush passed half a second. The virtual machine's
disk, one file on the host shared by every container, held every member's one thread in a write
together; the test, hearing no member, counted the silence as the group's quiet. The rule above
counts as quiet only time in which the test saw the group (`docs/timing.md` §2.9, "The stall it
found, and its cause"), and `stalled-devices` holds it: under the old rule the test failed there in
each of five runs on macOS, under the new it passed in each of five. After the fix, 50 of 50 runs
passed in six one-CPU containers at once, each with a busy loop (113–412 s a run); at `2ca93e8` 10
of 10 at four CPUs (66–219 s), 9 of 9 at two (52–89 s; the matrix stopped by hand before the tenth)
and 10 of 10 on macOS at load 52–143 (44–51 s); over `ecbf071`, before main's ordered runs (§2.8 of
`docs/timing.md`, "A restart"), the code with the silence bound passed 10 of 10 on macOS at load
33–68 (54–75 s), 10 of 10 at four CPUs, 10 of 10 at two and 30 of 30 at one (45–193 s), and the
gates on CI's six targets; then the table above. Not counted: the one-CPU row at `2ca93e8`, whose
binaries were rebuilt under it while the silence bound was written (four of its 16 runs failed in
their first 10–27 s, on that work in progress), a later matrix's two-CPU row rebuilt under it the
same way (one failure in ten), and a set in which two matrices ran at once on the same volumes by
mistake, writing over each other's logs and outputs (no failure among what was left).

On the way, 17 of 18, 28 of 30 and 15 of 15 Linux runs passed on earlier forms of this code; each
defect found was fixed at its cause before the runs above:
- the test read its own reports behind a flood of answers to the writes in flight (they now go from
  a client of their own);
- a resent burst always reached the leader in the same order, so the same first few hundred were
  taken and the rest dropped again (only the unanswered are resent);
- a look whose ask lost its answer spent the whole quiet period (the quiet period now counts only
  looks begun after it);
- a member released the askers it kept waiting only when it saw itself stop leading, so one that
  stepped down and led again between two looks kept askers whose entries its new term had replaced
  (they are now released when the term it leads changes);
- the test counted as quiet time in which it heard no member (the stall above);
- the silence bound's first form charged a member the retransmission timeout the test waited on
  its own lost ask, 2.05 s of silence against 1.05 s excused in a one-CPU probe (the timeout waited
  on the latest ask is now the test's);
- a member restarted at the port its killed run had held found the port taken ("Address already in
  use", in the first run of a matrix at four CPUs): a port freed by a process killed is the system's
  to give to whoever binds next (a member restarted now takes a port the system gives, and every
  member up is told);
- on Windows, which has no stop signal, the stopped member was held for a stated time, which ran
  out before the test's wait had judged its silence (CI's windows-2025): it is now held until the
  test releases it.

`tests/wal.rs` covers the log's torn-tail cut, its refusal of a damaged record that is not the
last, and its bound; `tests/wire.rs` damaged and cut datagrams; `tests/node.rs` a member whose
deadline is due still reading what arrived; `tests/arrives.rs` the peek. One group at a time runs,
at most five member processes of two threads each (the loop, and the one watching its parent), and
the test itself is one thread (`harness = false`).

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

# slates' regression tests, R6 and R7 (R-3): the counting runs of every cell in both trees, the
# schedules' printed counts in both, and the idle scan by suspicion, each process timed whole.
$B one hyper steady 3 1 64 20000 1 count
cargo test -p hyper-raft --test differential --test group --test fast --test pipeline --test repair --test suspicion -- --nocapture --test-threads=1
/usr/bin/time -l target/release/deps/idle-<hash> suspicion 10000 400

# slates' tests for R4, R5, R20 and R21 (R-3): as above, and slates' backlog tool on this core.
cargo bench -p hyper-raft --bench backlog -- 50000

# The window a member is sent ahead of its answers (R16): as above, and slates' pipelining
# measurement on this core; before, the harness on commit 2's tree with focal's 128 places.
HYPER_RAFT_TIMED_SEEDS=20 HYPER_RAFT_TIMED_STREAM_S=30 cargo test -p hyper-raft --release --test timed -- --ignored --exact replication_across_rates_and_loss --nocapture
HYPER_RAFT_TIMED_PLACES=128 HYPER_RAFT_TIMED_SEEDS=20 HYPER_RAFT_TIMED_STREAM_S=30 cargo test -p hyper-raft --release --test timed -- --ignored --exact replication_across_rates_and_loss --nocapture

# What arrives ahead of a hole (R17): as above, and the same sweep under raft-rs's rule, which gives
# R16's numbers.
HYPER_RAFT_TIMED_AHEAD=refused HYPER_RAFT_TIMED_SEEDS=1 HYPER_RAFT_TIMED_STREAM_S=30 cargo test -p hyper-raft --release --test timed -- --ignored --exact replication_across_rates_and_loss --nocapture

# A learner caught up in rounds (R13): the counting runs and the schedules as above; the replay of
# Figure 4.4(a) is a unit test.
cargo test -p hyper-raft --lib a_staged_newcomer_leaves_no_availability_gap_where_a_direct_one_does

# When a log is compacted (R22): slates' finding and the long history, from the shell's tests; the
# simulation with the rule's count checked after every step, at 1,000 seeds a shape.
cargo test -p hyper-durable --test shell -- --nocapture --test-threads=1 \
  a_leader_that_waits_for_its_followers_sends_them_entries_not_images \
  a_sole_voter_that_compacts_when_due_holds_its_log_within_the_rule
HYPER_DURABLE_SEEDS=1000 cargo test -p hyper-durable --test sim -- --nocapture random_schedules

# Every bound from what the owner states (Limits::derive): the counting runs and the schedules as
# above; the derivations are a unit test.
cargo test -p hyper-raft --lib every_bound_is_derived_from_what_the_owner_states

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

## The bounded view and the corrected engine (2026-10-03)

The same workload through `crates/hyper-swim-compare`'s `one hyper` points, for `main` (`2525538`),
the bounded view (`2711836`), the corrected engine on it (`40602db`), and the branch's head
(`617d70a`: a death named by its incarnation, and the engine's overflow guard). Each point a fresh
process of 400 periods, the four builds' order rotated each round, nine rounds at 16 and 64 members
and three at 256, 2026-10-03 at 00:35–01:25 PDT, load 33.5–42.4 (other sessions; no build of mine
running); ns a member a period, medians with the least and the most:

| Members | Workload | `main` | Bounded view | With the engine | Head |
|---|---|---|---|---|---|
| 16 | quiet | 398 (367–757) | 436 (372–617), +9.4 % | 387 (350–494), −2.8 % | 384 (360–572), −3.5 % |
| 16 | churning | 628 (588–668) | 686 (588–701), +9.3 % | 663 (583–1,023), +5.7 % | 638 (568–664), +1.7 % |
| 64 | quiet | 625 (548–3,455) | 722 (534–3,397), +15.4 % | 582 (481–1,410), −6.9 % | 543 (513–3,009), −13.1 % |
| 64 | churning | 1,118 (875–2,793) | 1,528 (900–5,463), +36.6 % | 1,231 (861–2,120), +10.0 % | 1,130 (871–2,752), +1.0 % |
| 256 | quiet | 980 (855–1,656) | 1,167 (874–1,303), +19.0 % | 876 (853–891), −10.7 % | 933 (850–1,535), −4.8 % |
| 256 | churning | 1,460 (1,438–1,551) | 1,468 (1,420–1,587), +0.6 % | 1,340 (1,339–1,651), −8.2 % | 1,481 (1,333–1,820), +1.4 % |

Every build allocates nothing a period at every point, and `benches/allocs.rs` counts 0 allocations,
reallocations and faults a member a period at 4 to 256 members, quiet and churning, at each commit
(load 50–52). The bounded view's periods came out slower in every median, as in an earlier series at
load 35.5–50.0 (+1 to +15 %): it had added 16 bytes to every peer's state, a count telling a death's
record from one its member had left, and the period's time is in reading the peers' map (the boxing
of the estimators above). The incarnation names a death already, so the head keeps no count, and its
medians are 1.0 to 1.7 % over `main` churning and 3.5 to 13.1 % under it quiet, within the series'
spread: the engine's two dimensions cost less a sample than eight, and its acknowledgement is 56
bytes shorter, 63 gossip entries a 1,200-byte message where there were 60.

## The split closed: the period (2026-10-03)

The same workload through `crates/hyper-swim-compare`'s `one` points for slates' detector, `main`
(`c539a19`, whose hyper-swim is `2525538`'s), the head before this work (`617d70a`'s detector) and the
head with it (`2605c89`: the directed gossip, the bound's precondition, the measurement backoff and
the digest-first exchanges, which the comparison's members run as the cluster test's do). Each point
a fresh process of 400 periods, the four builds' order rotated each round, nine rounds at 16 and 64
members and three at 256, 2026-10-03 at 04:18–04:23 PDT, load 20.6–27.1 (other sessions; none of my
builds or soaks running); ns a member a period, medians with the least and the most:

| Members | Workload | slates | `main` | Before | With the work | Against slates | Against before | Against `main` |
|---|---|---|---|---|---|---|---|---|
| 16 | quiet | 571 (567–588) | 409 (377–423) | 388 (349–396) | 431 (401–449) | −24.6 % | +11.1 % | +5.4 % |
| 16 | churning | 990 (982–1,056) | 670 (587–696) | 653 (580–660) | 732 (666–749) | −26.0 % | +12.1 % | +9.3 % |
| 64 | quiet | 700 (668–765) | 527 (522–539) | 507 (491–530) | 572 (570–597) | −18.2 % | +12.9 % | +8.5 % |
| 64 | churning | 1,280 (1,258–1,464) | 857 (846–881) | 845 (834–862) | 1,008 (987–1,019) | −21.3 % | +19.4 % | +17.7 % |
| 256 | quiet | 1,457 (1,426–1,564) | 867 (866–871) | 846 (821–853) | 1,047 (1,002–1,050) | −28.1 % | +23.7 % | +20.7 % |
| 256 | churning | 2,249 (2,239–2,255) | 1,324 (1,314–1,358) | 1,306 (1,290–1,408) | 1,866 (1,761–2,047) | −17.0 % | +42.8 % | +40.9 % |

Nine rounds at 256 members, at 04:31–04:43 at load 21.5–64.5 (another session's work arriving
midway): quiet 1,187 against `main`'s 1,008, +17.8 %, churning 2,208 against 1,717, +28.6 %, and
slates 30.7 and 12.8 % slower. The period stays under slates' at every point. Its cost over `main`
has two parts:
- the directed gossip: one more entry on every probe, applied at the target, and a lookup of the
  prober's state at every answer. Against the head before, in three series at load 20–26 with the
  directed gossip alone, it cost +4.9 to +17.1 % at 16 and 64 members, a median of +8 %, and from
  −4.0 to +20.7 % at 256, over three rounds a point;
- the exchanges: an opening of 30 bytes a member a window where views agree, 0.07, 0.06 and 0.05
  messages a member a period at 16, 64 and 256 members and no entries, which the comparison counts;
  against the directed gossip alone, interleaved, nine rounds at load 28.5–73.6, −2.4, −2.5 and
  −12.5 % quiet, nothing beyond noise. Under a refutation every period the views mostly differ, and
  they carry 1.5, 5.1 and 16.4 entries a member a period: +8.1, +10.9 and +26.0 %. A first form
  that pushed whole views at every exchange carried 2.3, 7.5 and 25.0 even in the quiet cluster,
  +72 % at 256 members quiet in an interleaved series at load 74; the digest took that away.

Every build allocates nothing a period at every point. `benches/allocs.rs`, which runs the exchanges
as an owner does, counts 0 allocations, reallocations and faults a member a period at 4 to 256
members, quiet and churning; the bytes asked under churn (0.3 a member a period at 16 and 64
members) are one-time growths, falling tenfold over 4,000 periods instead of 400.

**The findings** (`f9aff67`: a probe's stated deadline and each poll's findings, `docs/timing.md`
§2.7), against the head before them as measured above (`2605c89`), `main` and slates, the same
rotation, 2026-10-03 at 09:19–09:24 PDT at load 8.5–17.9, nine rounds at 16 and 64 members, then
nine at 256 at 09:25–09:37, load 5.0–16.0. The machine was quieter than for the series above:
sixteen busy loops an earlier load test had orphaned, running since 2026-10-02 05:08, were ended at
08:13. ns a member a period, medians with the least and the most:

| Members | Workload | slates | `main` | Before | With the findings | Against before |
|---|---|---|---|---|---|---|
| 16 | quiet | 565 (448–591) | 385 (311–428) | 367 (340–385) | 374 (335–394) | +2.0 % |
| 16 | churning | 997 (860–1,116) | 582 (540–700) | 675 (600–712) | 665 (569–970) | −1.4 % |
| 64 | quiet | 668 (623–832) | 573 (434–675) | 660 (469–1,603) | 603 (559–2,079) | −8.6 % |
| 64 | churning | 1,147 (1,057–1,430) | 779 (739–1,008) | 1,002 (834–1,443) | 978 (853–2,194) | −2.5 % |
| 256 | quiet | 1,017 (932–1,395) | 714 (687–1,126) | 913 (748–1,789) | 892 (756–1,232) | −2.2 % |
| 256 | churning | 1,566 (1,515–4,526) | 1,015 (973–1,234) | 1,363 (1,287–2,340) | 1,396 (1,282–4,421) | +2.5 % |

Nothing beyond the spread: a poll clears a vector, and a probe carries one more word; a finding is
pushed only where a probe goes unanswered, which neither workload has. Three rounds at 256 members
first read +17.7 % churning, 1,646–2,393 ns against 1,424–1,884, which the nine put at +2.5 %. The
period stays under slates' at every point, 9.7 to 33.8 % less. `benches/allocs.rs` counts 0
allocations, reallocations and faults a member a period at 4 to 256 members, quiet and churning,
the same profile as the head before, to the bytes asked.

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

**A kill under the pairs' own estimators (2026-10-03).** The series above killed once, as soon as
every pair was judged, which on loopback is mostly by the pools (a mean of 0.0 to 0.3 of the twelve
pairs judged by their own estimators at the kill), so the end-to-end test never condemned under a
pair's own detector. The test now runs five members in two phases: the kill as before, then, once
every survivor holds that victim dead, a wait until every surviving pair is judged by its own
estimator and a second kill (`docs/timing.md` §2.7). 40 runs on this machine, each a fresh
supervisor, 2026-10-03 at 00:20–00:25 PDT, load 45.2–57.6, every run passing:

| Phase | Pairs judged by their own estimator at the kill | Detection median / p95 / max | Stated bound median / max |
|---|---|---|---|
| the pools' | a mean of 1.2 of 20, at most 11 | 4.8 / 11.0 / 14.5 ms | 22.7 / 79.6 ms |
| the pairs' own | 12 of 12, every run | 15.6 / 46.8 / 87.8 ms | 84.5 / 517.5 ms |

Suspicions of live members, summed over the runs, 2 (Theorem 7 allowed 5,080), condemnations 0
(allowed 4,989): the longer run's young histories make the allowance loose, as before. A run took a
median 0.58 s, the longest 5.0 s, waiting on the pairs' evidence. Detection under the pairs' own
estimators is slower than under the pools': each pair's margin is configured at its own interval and
grows with the MTBF the run has accrued (§2.7, "Open").

**Bounded waits (2026-10-03).** Each wait now goes on while the members move toward its fact and
fails with every member's last line once the longest detection bound a live member states, never
less than RFC 6298's one second, passes with nothing moving; a pair that takes more round trips
without its own configuration than any window of its estimator holds, or a member whose output
ends, fails it at once (`docs/timing.md` §2.7). Each way of failing was checked with a fault put
into a member for the check and taken out after: a member made to exit at its 200th period failed
the wait at once with its exit status; one made to hang failed it 1.0 s after the last movement,
with its own last line 1.5 s old; and the limit lowered to 16 round trips failed it at the first
pair past it. 300 runs on this machine, each a fresh supervisor, 2026-10-03 at 01:09–01:12 PDT,
load 21.9–40.7, 298 passing:

| Wait | Median | p95 | Max |
|---|---|---|---|
| every member judges every peer | 9.1 ms | 35.7 ms | 218.3 ms |
| every survivor holds the first victim dead | 4.2 ms | 8.2 ms | 22.8 ms |
| every surviving pair is judged by its own estimator | 378.3 ms | 1,151.7 ms | 3,843.3 ms |
| every survivor holds both victims dead | 11.8 ms | 24.5 ms | 105.7 ms |

The longest any wait that held went with nothing moving was 44.9 ms, 4.5 % of its quiet period,
the one-second floor at every wait's stillest stretch (a median 2.1 ms). At the first kill a mean
of 1.1 of the 20 pairs were judged by their own estimator, at most 9, and detection took 4.2 / 8.3 /
28.9 ms (median / p95 / max) against stated bounds of a median 19.5 and at most 202.0 ms; at the
second, 12 of 12, detection 12.0 / 24.1 / 113.8 ms against 64.7 and 809.5 ms. Suspicions of live
members, summed over the runs that passed, 114 (Theorem 7 allowed 41,182), condemnations 80
(allowed 40,481). A run took a median 0.45 s, the longest 4.0 s.

The two that failed each failed the wait for every surviving pair judged by its own estimator, one
second after anything last moved, and their last lines say why: a live member had been falsely
condemned, in both by the first victim before its kill, had refuted, and one survivor had
missed the refutation, held it dead and, past the record's window, forgotten it. The others held it
alive and it probed every member, but nothing told the one that forgot it again. A further 819 runs
of a build that also printed each member's view as it changed (for the trace, not committed) failed
once the same way: member 4 condemned member 1 at incarnation 4 after member 1 had refuted at 5;
the refutation reached members 2, 3 and 5 and not member 4, which forgot member 1 17 ms later. A
refutation is a rumor, and a rumor can end known to some members and not all (Demers et al. 1987,
§1.5); hyper-swim had no anti-entropy to back it up (closed below). Before the waits were bounded,
such a run waited until CI's job limit.

**The split closed, and the soak (2026-10-03).** A probe now states its prober's own state and an
answer the answerer's suspicion or death of the prober (`e563ef1`), which closes the split where the
live member still probes the survivor that holds it dead; anti-entropy closes the one where neither
probes the other (`2605c89`, `f93e9d7`; `docs/timing.md` §2.7). The runs along the way:
- the directed gossip: a prototype 1,000 of 1,000, two further forms 999 and 998 of 1,000, no
  split; the three failures each held the first victim dead 0.2 to 2.2 ms past a bound of 2.3 to
  3.3 ms. A build that kept a ring of each member's probes, answers and views, dumped at an
  overshoot, found one 490 runs in: a live member falsely condemned 7 ms into the run, adopted by
  gossip at members whose probes were still measurement only, against a bound covering periods
  that judged nothing. Members now state no bound until every probe they make is judged, and the
  test checks the death that stands (`a3a043f`);
- two instrumented builds, 455 and 2,323 runs in, had a member 43,557 and 39,560 periods into a run
  with nothing judged, condemned 95 and 94 times: measurement periods outrun by round trips that
  had lengthened, which the detector alone reproduces; unanswered measurement periods now back off
  (`d4992fa`);
- with those three, 1,000 runs, all passing (03:03–03:25, load 20.1–65.7);
- the exchanges, pushing whole views at every exchange: 950 runs on macOS (load 31.0–118.2) and 109,
  156 and 205 on Linux at one, two and four CPUs, all passing; with digests first, 77, 26, 39 and 49,
  all passing, before a death of a member not held was found to bring a forgotten record back
  around through the exchanges (`f93e9d7`) and the soak was begun again.

The soak, on `f93e9d7`, each run a fresh supervisor, 2026-10-03 at 04:54–07:09 PDT, the macOS and
Linux series running at the same time beside other sessions' work; Linux in Docker Desktop's VM
(rust:1.98.0), each container at its CPU limit with twice as many busy loops beside the test, its
load the VM's:

| Platform | Limits and competing load | Runs | Passed | Splits | Overshoots | Load (1 min) | Suspicions (allowed) | Condemnations (allowed) | Detection, first kill, median / p95 / max | Second kill | Largest share of the stated bound |
|---|---|---|---|---|---|---|---|---|---|---|---|
| macOS 26.4.1, M5 Max | other sessions | 2,000 | 2,000 | 0 | 0 | 27.6–95.2 | 2,333 (226,062) | 41 (218,868) | 11.5 / 53.1 / 333.8 ms | 127.8 / 507.6 / 1,663.5 ms | 0.38 |
| Linux 6.12.76 (Docker Desktop) | `--cpus 1`, 2 busy loops | 500 | 500 | 0 | 0 | 3.4–52.9 | 853 (82,340) | 49 (77,052) | 60.9 / 145.7 / 571.9 ms | 898.0 / 2,312.5 / 5,256.0 ms | 0.38 |
| Linux 6.12.76 (Docker Desktop) | `--cpus 2`, 4 busy loops | 655 | 655 | 0 | 0 | 3.6–52.8 | 1,369 (103,423) | 83 (96,842) | 52.1 / 115.0 / 390.2 ms | 646.2 / 2,036.9 / 4,506.4 ms | 0.40 |
| Linux 6.12.76 (Docker Desktop) | `--cpus 4`, 8 busy loops | 638 | 638 | 0 | 0 | 3.5–52.8 | 934 (97,068) | 42 (90,989) | 53.1 / 112.9 / 316.3 ms | 532.0 / 2,341.2 / 6,741.7 ms | 0.55 |

Every survivor held each victim dead within the bound it stated, at most 0.55 of it; the stillest
any wait went was 0.27 of its quiet period. A run took a median 1.9 s on macOS and 8.4 to 12.7 s in
the throttled containers, the longest 166 s, waiting on the pairs' evidence. The suspicions and
condemnations of live members are the load's: the machine ran at up to 95, and the throttled
members stall together.

**Every finding traced (2026-10-03).** From `f9aff67` the test decides nothing by a statistical
level: it traces every suspicion, every condemnation made pending and every condemnation, from the
members' own records, to the detector's rule, and prints Theorem 7's allowance beside the counts of
live members as a report (`docs/timing.md` §2.7). Runs of that form, each a fresh supervisor, the
binaries built from the library and test `f9aff67` holds, under the machine's ambient load alone,
nothing added; macOS and Linux at the same time from 09:37 PDT, the Linux series ended at 09:49 so
as not to load another session's timing-sensitive gate:

| Platform | Limits | Runs | Passed | Load (1 min) | Traced: suspicions, pending, condemnations | Live members' answers: late (latest past its deadline), lost | Suspicions of live members (allowed) | Condemnations (allowed) | Detection, first kill, median / p95 / max | Second kill |
|---|---|---|---|---|---|---|---|---|---|---|
| macOS 26.4.1, M5 Max | none | 2,000 | 2,000 | 3.5–10.4 | 6,721, 7,593, 5,078 | 18 (9.7 ms), 0 | 15 (163,566) | 1 (160,064) | 5.9 / 9.0 / 24.6 ms | 10.4 / 20.2 / 356.4 ms |
| Linux 6.12.76 (Docker Desktop) | `--cpus 1` | 454 | 454 | 1.2–3.7 | 1,633, 1,775, 1,165 | 49 (54.9 ms), 0 | 45 (28,255) | 1 (27,390) | 19.0 / 26.9 / 71.0 ms | 51.1 / 104.1 / 576.2 ms |
| Linux 6.12.76 (Docker Desktop) | `--cpus 2` | 475 | 475 | 1.2–3.7 | 1,646, 1,936, 1,277 | 19 (73.5 ms), 0 | 16 (24,974) | 1 (24,116) | 19.6 / 27.1 / 43.3 ms | 50.0 / 105.1 / 230.9 ms |
| Linux 6.12.76 (Docker Desktop) | `--cpus 4` | 468 | 468 | 1.2–3.7 | 1,656, 1,815, 1,195 | 36 (72.1 ms), 0 | 33 (25,730) | 1 (24,858) | 19.0 / 26.8 / 61.9 ms | 49.9 / 104.7 / 239.8 ms |

No finding failed its trace, and every count a member reported was its record's at every line it
wrote. The probes the findings name were unanswered by their stated deadlines and, where relays were
asked, by theirs; every answer that missed a live member's probe came, after its period had ended,
none lost; the others were probes of the killed members, whose records hold no such ping. A run took
a median 0.34 s on macOS and 1.0 to 1.2 s in the containers. The checks bite: a detector made to
ignore one answer in five fails the trace at once (524 findings named in one run), and one that
drops its suspicions' findings fails the count at the first line that reports one.

## Commands for the detector

```sh
# The suites and the five-process kill test.
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

**Path samples stamped and aged** (2026-10-04 02:12 PDT, load 4.2 to 4.5, one build job, the
before and after runs back to back): `PathRtt::on_sample` 16.4 (16.1–16.6) ns before, 17.0
(16.4–17.3) with each sample stamped with its time and its endpoint's generation and the window's
oldest checked against its span. The first ring, whose estimator did not change, moved from 45.0
to 47.0 between the same two runs, so the difference is within what the load moves. Still no
allocation, reallocation or fault (`tests/alloc.rs`).

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

### The events' bound (2026-10-02)

The owner's unpolled events bounded (`docs/transport.md` §4a): every event holds a seat until it is
polled. The same bench, `main` (`2525538`) against this tree, each built with the time printed to
three places (a scratch edit of the bench's format, reverted), fifteen runs of each alternated in
fresh processes, 2026-10-02 at 23:55 PDT, load 48.9–51.7 (other sessions); medians, with the least
and the most:

| Workload | Allocations, reallocations, bytes | `main` | This tree |
|---|---|---|---|
| exchange, no body | 8.35, 0, 1,143: the same | 3.254 µs (3.220–3.327) | 3.261 µs (3.217–3.422), +0.2 % |
| exchange, 4 KiB bodies | 12.36, 0, 9,313: the same | 12.284 µs (12.197–12.381) | 12.249 µs (11.538–12.735), −0.3 % |
| exchange, 64 KiB bodies | 26.40, 0, 148,633: the same | 147.87 µs (141.01–150.05) | 146.70 µs (139.07–149.62), −0.8 % |
| lane frame, 512 B | 1.07, 0.09, 620: the same | 0.882 µs (0.851–0.916) | 0.897 µs (0.880–0.912), +1.7 % |
| bare hyper-quic, 16 B | the same | 2.228 µs | 2.240 µs, +0.5 % |
| bare hyper-quic, 4 KiB | the same | 9.578 µs | 9.527 µs, −0.5 % |
| bare hyper-quic, 64 KiB | the same | 124.11 µs | 123.87 µs, −0.2 % |

No allocation, reallocation or byte changed: the seats are counters in what the endpoint already
holds, and the queue keeps its capacity. The bare rows run no code this change touched and move by up
to 0.5 %, the noise of the comparison; the exchanges are within it. A lane frame is 15 ns over: it is
counted against its lane's window and its peer's, a read of the peer table when its prefix is begun
and when it is queued, and of its lane when it is polled. Two earlier series of the first form (load
31–47) had the exchanges 1.2 to 2.0 % over, traced to two table reads for each `BodyReady` and
`Writable`, a check of the flag and the hold; one read does both now (`Arena::hold_if`,
`Arena::release_with`).

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

# mantle's range replica on the shell (D-1)

mantle's D-1 moves its range replica onto this shell. `crates/hyper-durable-compare` now runs four
sides in one process on mantle's workload, the order rotated each round:
- **mantle 1c179e8**, **hyper-durable**: as in "The durable shell against mantle's" above;
- **mantle 85b9c2d**: mantle's shell at its step 1, over this repository's `687244f` snapshots, so
  it runs on the log the shell runs on;
- **mantle D-1**: mantle's range replica on this shell (mantle `crates/range` on its branch
  `shared-d1`, over the snapshots of `b3a1fd7`), driven as mantle's node is to drive a range
  (mantle `docs/design/node.md` §2.2), with mantle's settings: elections on ticks, no quiet write.

With `HYPER_DURABLE_DIAGNOSIS` each point also gives the leader's writes, submission to the answer
taken. Same machine, 2026-10-03, other sessions building (a focal gate among them); the load
average beside each point. Columns per committed entry as above, medians over the rounds; the
driving thread's counts are the shell's, the core's, the state machine's and the store's calls.

## On the device, 4 rounds of 300 entries after 50

**1 member, register entries, device; load 6.69 7.41 7.53 → 4.79 6.75 7.28**

| side | p50 µs | p99 µs | p99.9 µs | entries/s | flushes | allocs | reallocs | switches | driver's allocs | driver's reallocs | leader's write p50 µs | leader's write p99 µs |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| mantle 1c179e8 | 8655 | 14842 | 19076 | 113 | 1.00 | 38.4 | 9.08 | 15.9 | 38.3 | 9.03 | 8642 | 14824 |
| mantle 85b9c2d | 8560 | 21166 | 126517 | 108 | 1.00 | 38.4 | 9.08 | 16.5 | 38.3 | 9.03 | 8542 | 21146 |
| hyper-durable | 8596 | 17907 | 63222 | 110 | 1.00 | 30.4 | 9.08 | 16.5 | 30.3 | 9.03 | 8584 | 17896 |
| mantle D-1 | 8583 | 13673 | 18059 | 110 | 1.00 | 30.4 | 9.08 | 15.7 | 30.3 | 9.03 | 8569 | 13628 |

**3 members, register entries, device; load 4.79 6.75 7.28 → 11.03 7.70 7.44**

| side | p50 µs | p99 µs | p99.9 µs | entries/s | flushes | allocs | reallocs | switches | driver's allocs | driver's reallocs | leader's write p50 µs | leader's write p99 µs |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| mantle 1c179e8 | 65358 | 112836 | 330276 | 20 | 6.00 | 135.1 | 21.19 | 140.7 | 135.0 | 21.06 | 13030 | 29733 |
| mantle 85b9c2d | 65018 | 118407 | 353097 | 20 | 6.00 | 135.1 | 21.19 | 128.7 | 135.0 | 21.06 | 13816 | 29340 |
| hyper-durable | 37683 | 63027 | 295355 | 27 | 5.72 | 93.9 | 21.19 | 125.8 | 93.7 | 21.06 | 30669 | 54159 |
| mantle D-1 | 36989 | 68076 | 244814 | 27 | 5.78 | 93.9 | 21.19 | 137.3 | 93.7 | 21.06 | 30774 | 54707 |

**5 members, register entries, device; load 11.03 7.70 7.44 → 8.53 11.54 9.88**

| side | p50 µs | p99 µs | p99.9 µs | entries/s | flushes | allocs | reallocs | switches | driver's allocs | driver's reallocs | leader's write p50 µs | leader's write p99 µs |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| mantle 1c179e8 | 87928 | 129185 | 369212 | 12 | 10.00 | 225.8 | 33.29 | 266.8 | 225.7 | 33.08 | 18063 | 30529 |
| mantle 85b9c2d | 87973 | 148602 | 367219 | 11 | 10.00 | 225.8 | 33.29 | 285.4 | 225.7 | 33.08 | 17997 | 31578 |
| hyper-durable | 49905 | 75805 | 313985 | 19 | 9.80 | 157.0 | 33.29 | 277.8 | 156.5 | 33.08 | 41897 | 72522 |
| mantle D-1 | 49676 | 77707 | 319761 | 20 | 9.81 | 157.0 | 33.29 | 276.7 | 156.5 | 33.08 | 41066 | 75894 |

**1 member, 1 KiB put entries, device; load 8.53 11.54 9.88 → 4.36 9.02 9.09**

| side | p50 µs | p99 µs | p99.9 µs | entries/s | flushes | allocs | reallocs | switches | driver's allocs | driver's reallocs | leader's write p50 µs | leader's write p99 µs |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| mantle 1c179e8 | 22150 | 33877 | 223216 | 44 | 1.00 | 26.0 | 10.07 | 29.4 | 26.0 | 10.02 | 22125 | 33852 |
| mantle 85b9c2d | 21087 | 29924 | 240435 | 56 | 1.00 | 26.0 | 10.07 | 28.4 | 26.0 | 10.02 | 21058 | 29899 |
| hyper-durable | 19998 | 28386 | 185015 | 52 | 1.00 | 18.0 | 10.07 | 28.0 | 18.0 | 10.02 | 19976 | 28364 |
| mantle D-1 | 21200 | 28399 | 151172 | 49 | 1.00 | 18.0 | 10.07 | 28.2 | 18.0 | 10.02 | 21174 | 28359 |

**3 members, 1 KiB put entries, device; load 4.36 9.02 9.09 → 3.67 6.19 7.77**

| side | p50 µs | p99 µs | p99.9 µs | entries/s | flushes | allocs | reallocs | switches | driver's allocs | driver's reallocs | leader's write p50 µs | leader's write p99 µs |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| mantle 1c179e8 | 73622 | 134614 | 363142 | 15 | 6.00 | 96.1 | 12.16 | 162.6 | 96.0 | 12.03 | 16800 | 35881 |
| mantle 85b9c2d | 70426 | 166069 | 358973 | 14 | 6.00 | 96.1 | 12.16 | 132.7 | 96.0 | 12.03 | 14992 | 38832 |
| hyper-durable | 39046 | 69309 | 346192 | 26 | 5.77 | 54.9 | 12.16 | 138.4 | 54.8 | 12.03 | 33010 | 64949 |
| mantle D-1 | 41298 | 109634 | 292041 | 24 | 5.75 | 55.0 | 12.16 | 139.9 | 54.8 | 12.03 | 35672 | 95434 |

**5 members, 1 KiB put entries, device; load 3.67 6.19 7.77 → 20.54 14.97 11.25**

| side | p50 µs | p99 µs | p99.9 µs | entries/s | flushes | allocs | reallocs | switches | driver's allocs | driver's reallocs | leader's write p50 µs | leader's write p99 µs |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| mantle 1c179e8 | 68990 | 126718 | 376839 | 14 | 10.00 | 160.2 | 14.24 | 247.8 | 160.0 | 14.03 | 10377 | 29337 |
| mantle 85b9c2d | 74467 | 148087 | 423174 | 13 | 10.00 | 160.2 | 14.24 | 251.4 | 160.0 | 14.03 | 12901 | 32791 |
| hyper-durable | 40159 | 129713 | 375414 | 25 | 9.81 | 91.3 | 14.24 | 241.2 | 90.8 | 14.03 | 33177 | 112756 |
| mantle D-1 | 42310 | 67332 | 408772 | 23 | 9.80 | 91.4 | 14.24 | 247.1 | 90.8 | 14.03 | 34082 | 64292 |

## On the simulated device, 4 rounds of 3,000 entries

After 50 entries to warm:

**1 member, register entries, simulated device; load 5.48 7.32 7.51 → 5.68 7.33 7.51**

| side | p50 µs | p99 µs | p99.9 µs | entries/s | flushes | allocs | reallocs | switches | driver's allocs | driver's reallocs |
|---|---|---|---|---|---|---|---|---|---|---|
| mantle 1c179e8 | 11 | 31 | 121 | 87486 | 1.00 | 44.4 | 9.02 | 3.0 | 38.3 | 9.01 |
| mantle 85b9c2d | 12 | 42 | 140 | 88860 | 1.00 | 44.4 | 9.02 | 3.0 | 38.3 | 9.01 |
| hyper-durable | 11 | 40 | 145 | 85162 | 1.00 | 36.4 | 9.02 | 3.0 | 30.3 | 9.01 |
| mantle D-1 | 12 | 48 | 188 | 85463 | 1.00 | 36.4 | 9.02 | 3.0 | 30.3 | 9.01 |

**3 members, register entries, simulated device; load 5.68 7.33 7.51 → 5.68 7.33 7.51**

| side | p50 µs | p99 µs | p99.9 µs | entries/s | flushes | allocs | reallocs | switches | driver's allocs | driver's reallocs |
|---|---|---|---|---|---|---|---|---|---|---|
| mantle 1c179e8 | 58 | 178 | 324 | 15155 | 6.00 | 179.0 | 21.29 | 17.0 | 135.0 | 21.01 |
| mantle 85b9c2d | 59 | 231 | 462 | 14985 | 6.00 | 179.0 | 21.29 | 17.1 | 135.0 | 21.01 |
| hyper-durable | 46 | 97 | 211 | 20355 | 5.53 | 130.0 | 21.28 | 15.8 | 93.5 | 21.02 |
| mantle D-1 | 48 | 113 | 244 | 20403 | 5.52 | 130.0 | 21.28 | 15.3 | 93.5 | 21.02 |

**5 members, register entries, simulated device; load 5.68 7.33 7.51 → 6.11 7.40 7.53**

| side | p50 µs | p99 µs | p99.9 µs | entries/s | flushes | allocs | reallocs | switches | driver's allocs | driver's reallocs |
|---|---|---|---|---|---|---|---|---|---|---|
| mantle 1c179e8 | 96 | 261 | 492 | 9810 | 10.00 | 299.0 | 33.49 | 27.4 | 225.7 | 33.01 |
| mantle 85b9c2d | 95 | 258 | 444 | 10064 | 10.00 | 299.0 | 33.49 | 27.4 | 225.7 | 33.01 |
| hyper-durable | 65 | 130 | 214 | 14959 | 9.13 | 218.7 | 33.44 | 23.5 | 155.8 | 33.02 |
| mantle D-1 | 67 | 139 | 240 | 14793 | 9.14 | 218.7 | 33.44 | 23.5 | 155.8 | 33.02 |

**1 member, 1 KiB put entries, simulated device; load 6.11 7.40 7.53 → 6.11 7.40 7.53**

| side | p50 µs | p99 µs | p99.9 µs | entries/s | flushes | allocs | reallocs | switches | driver's allocs | driver's reallocs |
|---|---|---|---|---|---|---|---|---|---|---|
| mantle 1c179e8 | 13 | 55 | 188 | 77236 | 1.00 | 32.0 | 10.02 | 3.0 | 26.0 | 10.00 |
| mantle 85b9c2d | 13 | 36 | 169 | 81030 | 1.00 | 32.0 | 10.02 | 3.0 | 26.0 | 10.00 |
| hyper-durable | 12 | 32 | 158 | 81718 | 1.00 | 24.0 | 10.02 | 3.0 | 18.0 | 10.00 |
| mantle D-1 | 12 | 33 | 160 | 82443 | 1.00 | 24.0 | 10.02 | 3.0 | 18.0 | 10.00 |

**3 members, 1 KiB put entries, simulated device; load 6.11 7.40 7.53 → 6.10 7.37 7.52**

| side | p50 µs | p99 µs | p99.9 µs | entries/s | flushes | allocs | reallocs | switches | driver's allocs | driver's reallocs |
|---|---|---|---|---|---|---|---|---|---|---|
| mantle 1c179e8 | 60 | 212 | 354 | 15203 | 6.00 | 141.5 | 12.32 | 16.8 | 96.0 | 12.00 |
| mantle 85b9c2d | 59 | 196 | 359 | 15192 | 6.00 | 141.5 | 12.32 | 16.8 | 96.0 | 12.00 |
| hyper-durable | 47 | 116 | 255 | 20550 | 5.52 | 91.8 | 12.31 | 15.5 | 54.5 | 12.01 |
| mantle D-1 | 47 | 110 | 199 | 20209 | 5.52 | 91.9 | 12.31 | 15.9 | 54.5 | 12.01 |

**5 members, 1 KiB put entries, simulated device; load 6.10 7.37 7.52 → 6.57 7.45 7.55**

| side | p50 µs | p99 µs | p99.9 µs | entries/s | flushes | allocs | reallocs | switches | driver's allocs | driver's reallocs |
|---|---|---|---|---|---|---|---|---|---|---|
| mantle 1c179e8 | 100 | 304 | 505 | 9604 | 10.00 | 235.8 | 14.54 | 27.4 | 160.0 | 14.00 |
| mantle 85b9c2d | 95 | 280 | 461 | 9787 | 10.00 | 235.8 | 14.54 | 27.5 | 160.0 | 14.00 |
| hyper-durable | 68 | 154 | 277 | 14716 | 9.17 | 155.0 | 14.50 | 23.6 | 90.1 | 14.02 |
| mantle D-1 | 68 | 154 | 270 | 14402 | 9.16 | 154.3 | 14.50 | 23.8 | 90.1 | 14.02 |

After 3,000:

**3 members, register entries, simulated device; load 6.57 7.45 7.55 → 6.45 7.41 7.53**

| side | p50 µs | p99 µs | p99.9 µs | entries/s | flushes | allocs | reallocs | switches | driver's allocs | driver's reallocs |
|---|---|---|---|---|---|---|---|---|---|---|
| mantle 1c179e8 | 55 | 188 | 263 | 15835 | 6.00 | 202.1 | 22.21 | 16.9 | 135.0 | 21.00 |
| mantle 85b9c2d | 53 | 175 | 225 | 17417 | 6.00 | 202.1 | 22.21 | 16.8 | 135.0 | 21.00 |
| hyper-durable | 44 | 99 | 154 | 21439 | 5.55 | 152.1 | 22.20 | 15.9 | 93.5 | 21.00 |
| mantle D-1 | 45 | 105 | 145 | 21366 | 5.56 | 152.4 | 22.21 | 15.5 | 93.6 | 21.00 |

**5 members, register entries, simulated device; load 6.45 7.41 7.53 → 6.25 7.35 7.51**

| side | p50 µs | p99 µs | p99.9 µs | entries/s | flushes | allocs | reallocs | switches | driver's allocs | driver's reallocs |
|---|---|---|---|---|---|---|---|---|---|---|
| mantle 1c179e8 | 82 | 298 | 366 | 11061 | 10.00 | 337.4 | 35.01 | 27.4 | 225.7 | 33.00 |
| mantle 85b9c2d | 80 | 286 | 340 | 11929 | 10.00 | 337.4 | 35.01 | 27.4 | 225.7 | 33.00 |
| hyper-durable | 52 | 121 | 165 | 18128 | 9.16 | 256.0 | 34.95 | 23.4 | 155.8 | 33.00 |
| mantle D-1 | 54 | 132 | 215 | 18959 | 9.16 | 256.1 | 34.95 | 23.4 | 155.8 | 33.00 |

**3 members, 1 KiB put entries, simulated device; load 6.25 7.35 7.51 → 6.23 7.33 7.50**

| side | p50 µs | p99 µs | p99.9 µs | entries/s | flushes | allocs | reallocs | switches | driver's allocs | driver's reallocs |
|---|---|---|---|---|---|---|---|---|---|---|
| mantle 1c179e8 | 54 | 208 | 270 | 16661 | 6.00 | 177.4 | 13.80 | 16.6 | 96.0 | 12.00 |
| mantle 85b9c2d | 55 | 212 | 268 | 16008 | 6.00 | 177.4 | 13.80 | 16.6 | 96.0 | 12.00 |
| hyper-durable | 34 | 112 | 146 | 27227 | 5.51 | 125.0 | 13.72 | 14.3 | 54.5 | 12.00 |
| mantle D-1 | 34 | 112 | 149 | 27556 | 5.51 | 125.1 | 13.73 | 14.2 | 54.5 | 12.00 |

**5 members, 1 KiB put entries, simulated device; load 6.23 7.33 7.50 → 6.69 7.41 7.53**

| side | p50 µs | p99 µs | p99.9 µs | entries/s | flushes | allocs | reallocs | switches | driver's allocs | driver's reallocs |
|---|---|---|---|---|---|---|---|---|---|---|
| mantle 1c179e8 | 78 | 334 | 401 | 11588 | 10.00 | 295.7 | 16.99 | 26.8 | 160.0 | 14.00 |
| mantle 85b9c2d | 76 | 321 | 372 | 11794 | 10.00 | 295.7 | 16.99 | 26.5 | 160.0 | 14.00 |
| hyper-durable | 54 | 149 | 200 | 17098 | 9.16 | 210.8 | 16.82 | 23.8 | 90.1 | 14.01 |
| mantle D-1 | 54 | 150 | 194 | 17134 | 9.17 | 210.6 | 16.83 | 24.0 | 90.1 | 14.01 |

## One member, on the device, 8 rounds of 500 entries

**1 member, register entries, device; load 20.54 14.97 11.25 → 4.02 11.30 10.81**

| side | p50 µs | p99 µs | p99.9 µs | entries/s | flushes | allocs | reallocs | switches | driver's allocs | driver's reallocs | leader's write p50 µs | leader's write p99 µs |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| mantle 1c179e8 | 8725 | 48390 | 146935 | 90 | 1.00 | 38.4 | 9.06 | 16.5 | 38.3 | 9.02 | 8707 | 48362 |
| mantle 85b9c2d | 8741 | 37375 | 108733 | 89 | 1.00 | 38.4 | 9.06 | 17.3 | 38.3 | 9.02 | 8717 | 37342 |
| hyper-durable | 9440 | 57560 | 144584 | 84 | 1.00 | 30.4 | 9.06 | 18.1 | 30.3 | 9.02 | 9418 | 57526 |
| mantle D-1 | 9407 | 44444 | 169264 | 82 | 1.00 | 30.4 | 9.06 | 17.8 | 30.3 | 9.02 | 9393 | 44420 |

**1 member, 1 KiB put entries, device; load 4.02 11.30 10.81 → 3.89 8.06 9.55**

| side | p50 µs | p99 µs | p99.9 µs | entries/s | flushes | allocs | reallocs | switches | driver's allocs | driver's reallocs | leader's write p50 µs | leader's write p99 µs |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| mantle 1c179e8 | 8614 | 23116 | 27544 | 108 | 1.00 | 26.0 | 10.05 | 17.4 | 26.0 | 10.01 | 8592 | 23075 |
| mantle 85b9c2d | 8536 | 22556 | 146141 | 106 | 1.00 | 26.0 | 10.05 | 16.8 | 26.0 | 10.01 | 8514 | 22530 |
| hyper-durable | 8659 | 25197 | 268937 | 92 | 1.00 | 18.0 | 10.05 | 17.6 | 18.0 | 10.01 | 8638 | 25175 |
| mantle D-1 | 8561 | 24681 | 154813 | 102 | 1.00 | 18.0 | 10.05 | 17.1 | 18.0 | 10.01 | 8539 | 24661 |

## The losses, closed

- **Reallocations.** On the device, equal at every point, on the driving thread and in the
  process. On the simulated device the process's are at or below mantle's at every point, and the
  driving thread's mantle's once a group has run 3,000 entries (21.00, 33.00, 12.00 against the
  same; 14.01 against 14.00), 0.01–0.02 above in a group's first 3,000. A tracing allocator (a
  scratch build, its `unsafe` outside the contract script's list; every reallocation of an
  8-aligned block on the driving thread recorded by size, then by stack for the sizes that
  differ) found one cause: a follower's queue of messages growing from four slots to eight
  (`Outgoing::push_counted`, from `handle_append_entries`), 176 times in 12,000 entries at three
  members and 212 at five after 12,000 to warm, where mantle's shell grew none. The member kept one
  spare queue and dropped every other the shell gave back, while three writes out each held one.
  It now keeps a spare for each ready in flight (`crates/hyper-raft/ORIGIN.md`, "A spare queue for
  each ready in flight"): the growths fall to 1 and 22, and the driving thread reallocates 21.00
  and 33.00 an entry, as mantle's shell does. The 0.01–0.02 left in a group's first 3,000 entries
  are each circulating queue growing to its high water once, about 18 growths a group of three
  more than mantle's shell makes.
- **Allocations.** mantle's range replica on the shell allocated one more an entry per member than
  the stand-in (98.4 against 95.4 at three members), its answers built per entry; it now pushes
  one record a command into the owner's buffer, and allocates as the stand-in does (93.5; 155.8).
- **Context switches.** Per entry on the device, within 4% either way at three and five members
  (137.3 against 140.7; 276.7 against 266.8; 247.1 against 247.8) and 14% fewer at three with 1
  KiB entries (139.9 against 162.6). One side a process with `/usr/bin/time -l` (the device, three
  members, two rounds, load 3.8–7.7): voluntary 62,454 and 68,763 against mantle's 61,157 and
  60,248, involuntary 23,010 and 22,290 against 21,017 and 17,920, CPU even (1.08 and 1.01 s
  against 1.07 and 1.10), wall time a third to a half less (23.0 and 23.7 s against 50.4 and
  34.9). The overlap's, as traced above: more of the group's log threads runnable at once.
- **The one-member tail.** An entry's commit latency is its leader's write and 14–37 µs more at
  every percentile on every side, so the shell adds what mantle's adds and the tail is the write's:
  the device's flush under the load of the moment. Across three runs on the device the shells'
  one-member p50 and p99 trade places (register entries 8.58 against 8.66 ms and 9.41 against
  8.73; 1 KiB entries 21.2 against 22.2 and 8.56 against 8.61; the stand-in at `687244f` 30.8
  against 29.8 and 26.6 against 26.7). On the simulated device, where a flush costs nothing, the
  D-1 side commits 2.3% fewer register entries a second (85,463 against 87,486) and 6.7% more 1 KiB
  ones: its owner waits on a channel, mantle's harness in the log's own wait for its ticket. With
  the D-1 side polling for its log's answer instead (`HYPER_DURABLE_SPIN`, six rounds of 3,000,
  load 16–18), it commits 25,019 and 25,471 entries a second against mantle's 19,883 and 18,894,
  26–35% more; blocking, in the same minute, 4.5–9.3% fewer, its write 5–6 µs longer at the median.
  A node's shard is woken through its waker as this harness is (mantle `docs/design/node.md` §1.3).

The shell replaces mantle's: at least as fast at every point with more than one member, even with
one on the device, and allocating no more once a group has warmed. mantle's record of the same
runs is its `docs/measurements/2026-10-03-range-on-the-shell.md`.

```
# From the repository root: the comparison is a workspace of its own and fetches mantle at the
# revisions in crates/hyper-durable-compare/Cargo.toml.
cd crates/hyper-durable-compare && CARGO_BUILD_JOBS=4 cargo build --release
./target/release/hyper-durable-compare --devices sim --members 1,3,5 --shapes register,put \
  --rounds 4 --entries 3000 --warm 50
./target/release/hyper-durable-compare --devices sim --members 3,5 --shapes register,put \
  --rounds 4 --entries 3000 --warm 3000
HYPER_DURABLE_DIAGNOSIS=1 ./target/release/hyper-durable-compare --devices file \
  --members 1,3,5 --shapes register,put --rounds 4 --entries 300 --warm 50
HYPER_DURABLE_DIAGNOSIS=1 ./target/release/hyper-durable-compare --devices file --members 1 \
  --shapes register,put --rounds 8 --entries 500 --warm 50
HYPER_DURABLE_ONLY="mantle 1c,mantle 85,mantle D-1" ./target/release/hyper-durable-compare \
  --devices sim --members 1 --shapes register,put --rounds 6 --entries 3000
HYPER_DURABLE_SPIN=1 HYPER_DURABLE_ONLY="mantle 1c,mantle 85,mantle D-1" \
  ./target/release/hyper-durable-compare --devices sim --members 1 --shapes register,put \
  --rounds 6 --entries 3000
for i in 1 2; do for o in "mantle 1c" "mantle 85" "hyper" "mantle D-1"; do
  HYPER_DURABLE_ONLY="$o" /usr/bin/time -l ./target/release/hyper-durable-compare \
    --devices file --members 3 --shapes register --rounds 2 --entries 300
done; done
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
hyper-tokio's kernel-stamped socket, each liveness write a block (the size the system reports for the
file, `hyper_block::file::preferred_block`: 4 KiB on these hosts) and the platform's flush of a
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
- **A stall the device thread never took** (CI run 37087273821, ubuntu-24.04-arm: the test ran 1 h
  45 min, four members alive, until the run was cancelled). The member's device queue had room for
  one request, so a stall that came while a flush waited for the device thread was refused and
  dropped: the stalled member's disk ran on, its peers trusted it, and the supervisor waited for a
  suspicion with no end. With every wait ended on the members' law and their states dumped, the
  test looped on that runner (each supervisor 150 rounds, the queue at one request) failed in its
  31st round with the stalled member's disk stated refused (CI run 37094848890); with room for every
  request ever outstanding, the one flush in flight and the stall, it passed all 150 rounds there
  and 25 on each other target (CI run 37095270328).
- **A port taken between two supervisors** (Linux, four CPUs: run 96 of 150). The supervisor bound
  free ports, released them and handed them to its members; the other test's supervisor, starting
  at once, could be given the same port, and the member that bound second died before it was ready,
  which the old wait for readiness waited on for ever. Each member now binds its own port and the
  supervisor hands every port out at start.

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
| a block's write and full flush (4 KiB, the file's `st_blksize`) | `F_FULLFSYNC` every 10 ms: median 4.7 ms, p99 15.9 ms, most 205 ms; `BUSY`, back to back (the sweep): median 11.7 ms, p99 29.8 ms, most 54 ms | `fdatasync` every 2 ms: median 0.52 ms, p99 16.0 ms, most 3.66 s | uniform over 12–30 ms: the means hyper-durable-e2e's floors give on the runners, no shape within them measured (Jaynes 1957) |
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

## The renewal schedule (2026-10-02)

`docs/timing.md` §2.8, "Renewal once a configuration is as stale as its estimates are uncertain".
The heartbeat rule of `Pair::renewal_due`, the doubling, against three others in a scratch build
that took the rule from the environment (the `β` rule the same in all): hyper-swim's, a window's
worth of heartbeats since the last configuration; half a window, the staleness rule for an
estimate over a sliding window; and every heartbeat, Chen, Toueg and Aguilera's continuous
re-estimation. The simulation's `live_peers_configure_and_keep_their_allowance`, seeds 0–39 of the
macOS world (three nodes, run until every pair is configured and through a renewal), the
configurations counted over its pairs; then its detection tests at their gate seeds; then
`benches/allocs.rs`, three rounds, the rules alternated in each, load average 49–53.

| rule | configurations a 1,000 taken | interval moves a 1,000 taken | suspicions of live peers / allowance (40 seeds) | ns a heartbeat, 2 nodes | 4 nodes | 8 nodes | 8 nodes, 1,000 groups |
|---|---|---|---|---|---|---|---|
| doubling | 11.1 | 22.2 | 1,160 / 8,916 | 107–130 | 126–150 | 170–191 | 172–200 |
| a window | 15.3 | 22.9 | 1,202 / 8,963 | 107–109 | 138–141 | 216–222 | 214–259 |
| half a window | 23.5 | 24.4 | 1,224 / 9,072 | 155–160 | 152–155 | 210–240 | 207–247 |
| every heartbeat | 44.9 | 27.9 | 1,235 / 8,915 | 157–166 | 164–166 | 223–260 | 236–269 |

None allocates. Under every rule 96 % of the configurator's asks found the estimator without
`τ_int` at the interval its link had just moved to and returned before the configurator ran: these
are young links (a run ends at the first renewal past every pair's configuration), moving to the
interval their evidence needs every 36–45 heartbeats, each move starting the evidence again, and
the oftener rules moved them the more. The detection tests were alike under every rule: the most heartbeats a
link took to configure 424–509, the killed node suspected by its own detector 156–164 times and by
the node's evidence's margin 124–129, and the notices of a death in a link's first heartbeats the
same to the millisecond but one world's most (Linux, 1,363 ms under the doubling, 3,646 ms under
the others).

```sh
# Scratch, not committed: Pair::renewal_due's heartbeat rule read from HYPER_LIVENESS_RENEWAL
# (doubling, window, half, every), and the live-peers test printing its configurations. Then:
HYPER_LIVENESS_RENEWAL=<rule> HYPER_LIVENESS_SEEDS=40 <sim binary> --exact \
    live_peers_configure_and_keep_their_allowance --nocapture
HYPER_LIVENESS_RENEWAL=<rule> <allocs bench binary> --bench
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


## Quiet only while every member is heard: hyper-durable-e2e and the process test (2026-10-03)

`docs/timing.md` §2.9, "The same rule in the other harnesses". The code: `e5a8bc4`
(hyper-durable-e2e's heartbeat framing and run on hyper-raft-e2e's), `07f2287` (hyper-durable-e2e's
waits on `hyper_raft_e2e::quiet`, shared with hyper-raft-e2e's), `4ef052c` (a write out judged less
a look's timeout), and `2ca163b`, `a717e13`, `dd1e21e` and the commit that adds this section
(hyper-liveness's process test in its own terms), over main `77e9b6c`. The machine as above (Apple
M5 Max, macOS 26.4.1; Linux in Docker Desktop's virtual machine on it, 18 CPUs, aarch64,
rust:1.98.0), shared with other sessions' builds and soaks and with the Linux containers below;
binaries built once before each set.

**The flakes, failing as on CI before and passing after.** Each reproduction holds members in a
write, or keeps them unheard, past one quiet second; on macOS at load 19–37.

| CI's failure | reproduction | old rule | new rule |
|---|---|---|---|
| hyper-durable-e2e `stall-leader`, ubuntu-24.04 (run 37095887346): "a write was never answered; quiet 1s", the survivors heard, term 4, no leader | `stalled-devices`: the leader's disk stalled, the survivors' devices held 7 s by the fault file as they elect, once every pair is judged | 3 of 3 failed with CI's message, the survivors heard 14–88 µs before | 5 of 5 passed, 9.1–12.0 s |
| hyper-durable-e2e `stall-follower`, windows-11-arm (run 37095270328): "not suspected by every other; quiet 1s", the members up last heard 1.01 s before | one member up stopped 2.5 s by a helper process while the test waits for the suspicion (not committed) | 5 of 5 failed with CI's message, its last report 1.0–2.0 s old | 5 of 5 passed, 4.2–8.5 s |
| the process test's young victim, windows-2025 (run 37104550192, first attempt): "nothing moved for 1s", every flush in flight, the longest 372–467 ms | every flush held 450 ms more by the member's device thread (not committed) | 3 of 3 failed with CI's message, the longest flush 459–468 ms | 3 of 3 passed, 43–78 s |
| the same | every flush held 1.2 s more | 2 of 2 failed, "nothing moved for 2.4 s" | 2 of 2 passed, 75–197 s |

In the held-flush runs every member's first heartbeats were refused for want of a measured timer
(`Refusal::Unmeasured`, all six in a traced run), so the first heartbeat taken came about three
flushes in.

**The runs.** The whole binaries, `cargo test -p hyper-durable-e2e --test kill`, `-p hyper-liveness
--test processes` (`--test-threads=4`, its four tests at once) and `-p hyper-raft-e2e --test
cluster`. Linux: containers at `--cpus` 4, 2 and 1, each with as many busy loops, three at once
(each its own volume for the tests' files) unless the row says otherwise; load as each run ended
(the virtual machine's). macOS: load as each run began. "Beside the load": a container of eight busy
loops ran beside, and every member line was traced to the run's file (`HYPER_LIVENESS_TRACE`).

| host and code | kill | processes | cluster |
|---|---|---|---|
| macOS, `2ca163b`, load 34.7–52.0 | 10 of 10, 40.8–51.3 s | 20 of 20, 3.5–16.8 s | 5 of 5, 53.8–61.9 s |
| Linux, four CPUs, `2ca163b`, load 5.4–40.9 | 10 of 10, 29.1–47.8 s | 20 of 20, 3.9–19.9 s | 5 of 5, 552–878 s |
| Linux, two CPUs, `2ca163b`, load 5.4–39.2 | 10 of 10, 30.1–45.2 s | 20 of 20, 3.4–21.9 s | 5 of 5, 606–733 s |
| Linux, one CPU, `2ca163b`, load 6.4–43.9 | 10 of 10, 33.7–49.3 s | 19 of 20, 2.6–7.2 s (one failed, below), and 39 of 40 more (the stop's race, below) | 5 of 5, 364–614 s |
| Linux, `a717e13`, four, two and one CPUs, load 15.8–36.8 | | 20 of 20 each, 2.3–465.6 s | |
| Linux, `dd1e21e`, four, two and one CPUs, traced, load 7.3–10.1 | | 20 of 20 each, 2.3–4.4 s | |
| Linux, `dd1e21e` beside the load, four, two and one CPUs, load 11.6–32.4 | | 39 of 40, 38 of 40 and 16 of 16 (the 17th stopped by hand), 2.6–637.6 s: the three failures the supervisor's own lag (below) | |
| Linux, four CPUs beside the load, the rule before (`77e9b6c`) and `dd1e21e` side by side, load 22.4–42.6 | | 40 of 40, 1.0–694.7 s, against 31 of 40, 2.3–29.2 s: the nine failures the supervisor's own lag | |
| Linux, four CPUs beside the load, `dd1e21e` untraced, its members reporting any turn over 200 ms (none did; not committed), load 25.1–52.0 | | 20 of 20, 3.1–1,418.9 s | |
| Linux, `4ef052c`, four, two and one CPUs, load 41.2–52.7 | 5 of 5 each, 48.0–76.0 s | | |
| macOS, `4ef052c`, load 49.0–57.7 | 5 of 5, 47.0–54.7 s | | |
| Linux, four CPUs beside the load, this section's commit as first built (its comments and one dump field since) and `dd1e21e` side by side, load 29.3–37.6 | | 40 of 40, 3.8–259.4 s, the supervisor deaf at most 0.15–6.08 s at once, against 25 of 26 (the one failure Theorem 7's check, below) | |
| Linux, this section's commit, four, two and one CPUs, load 13.8–28.7 | | 20 of 20 each, 2.3–236.4 s | |
| macOS, this section's commit, load 57.2–69.7 (as first built: 20 of 20 at load 58.5–66.7, 4.0–19.9 s) | | 20 of 20, 3.9–16.4 s, the supervisor deaf at most 17–36 ms at once | |
| CI, the gates and then the binaries again (kill 4, processes 10, cluster 2) on each of the six targets: runs 37115140427 (`2ca163b`), 37118055070 (`a717e13`), 37119485933 (`dd1e21e`), 37122309780 (`4ef052c`) and 37125331924 (this section's commit as first built) | 149 of 150 (the one failure `4ef052c`'s cause, below) | 330 of 330 | 90 of 90 |

In hyper-durable-e2e a look missed a member only in the scenarios that stop one (the three that kill
a leader at a durability point, where the stopped leader answers nothing until the test reads its
line, and `member-stopped`): 40 of 160 scenarios on each host. Its members' longest time between two
reads of their sockets was 47 ms on macOS and 81–97 ms on Linux, and through the 7 s hold of
`stalled-devices` the waits were extended 8.0–11.5 s for the members' writes. `member-stopped`
failed its wait naming the stopped member after 1.05–4.14 s of silence against 1.02–2.12 s excused,
in 40 of 40. hyper-raft-e2e's scenarios, on the same rule now one module, took on Linux, all three
containers at once, 6–15 min a run where they took 1–4 min alone ("End to end"). The process test's
runs of many minutes are hyper-liveness's own under that load, the rule before's too (694.7 s): its
links' intervals grew to the 2–7 s their evidence needed, and the waits went on while their
heartbeats and suspicions moved.

**What failed on the way**, each a defect of this change found by these runs and fixed at its cause
before the rows after it:
- the process test's first form decided a check while a member's line was past its due by less than
  a retransmission timeout: a stopped member was still "heard" when the quiet period ended, and the
  wait gave up quiet rather than naming it (macOS, one run in ten). A check now waits for an overdue
  line or that timeout, as a look waits for its answer, each overdue member once (`dd1e21e`);
- it counted a member's writes only from the first check, so the writes before it extended nothing
  and the held-flush reproduction failed under the new rule too: they count from its first line of
  the wait;
- a member released was judged on its silence from before it was stopped (the gates, once): the test
  now waits, while its process runs, for its first line after the release, as hyper-raft-e2e's
  `thaw` waits for its first answer;
- the stopped member stated a line stamped after the test took the stop's time (Linux, one CPU, one
  run in forty): `kill -STOP` had returned before the stop took effect. The stop's time is now the
  system's word (`a717e13`);
- hyper-durable-e2e called a fresh group's first writes held (CI's windows-2025, run 37119485933,
  one run of five): every member had its first writes out 1.0–1.3 s while the longest any had
  finished took 154 ms. A write out is now judged less the timeout a look waits for its answer, as a
  silent member's silence is (`4ef052c`);
- the process test counted its supervisor's own lag as the members' silence (Linux beside the load,
  12 runs in 136, every member's latest line 2.08–2.26 s old at once, a supervisor in the same
  process printing lines 1.6 s old as one failed). A member's silence now counts only time the
  supervisor listened for lines: it reads every line already come before it judges, waits no longer
  than it must listen before the first member would be silent past the excuse, and is deaf from when
  a wait for a line ends (or the time it asked to end, if it woke past it) to when the next begins
  (this section's commit). A supervisor held three seconds once every member had stated (a scratch
  hold, not committed) failed 5 of 5 on macOS before ("member 1 stated nothing for 2.00–2.02 s past
  its due, past the 1.02–1.04 s ...") and passes 5 of 5 after, deaf 3.01 s at most.

Not of this change, as far as the runs show:
- one run of the processes binary at one CPU (`2ca163b`) failed in
  `a_stalled_disk_and_a_killed_node_are_suspected_and_no_live_one_is` at "member 4: a bound is
  stated": a survivor's suspicion of the stalled member stated no bound, which
  `Suspicion::detection` leaves unstated while a heartbeat in the window carried no echo (or an echo
  whose round trip no clock can make). It did not recur in 80 more runs of the same binary at one
  CPU, nor in 40 runs of the binary before this change; its cause is open;
- one run of `dd1e21e` beside the load failed Theorem 7's check in the same test, 15 suspicions of
  live members against an allowance of 8.2 in a run of 6.3 s: hyper-liveness's detectors under that
  load, not the waits, which all passed.

```sh
cargo test -p hyper-durable-e2e --test kill                    # stalled-devices, member-stopped among them
cargo test -p hyper-liveness --test processes -- --test-threads=4
cargo test -p hyper-raft-e2e --test cluster
# Linux: the binaries from `cargo test -p hyper-durable-e2e -p hyper-liveness -p hyper-raft-e2e
# --no-run --locked` in rust:1.98.0 with the repository at /src read-only and CARGO_TARGET_DIR on a
# volume; then each in a loop in `docker run --cpus N` with N busy loops (`while :; do :; done`),
# each container its own volume over the target's tmp directory; "beside the load", a container of
# eight busy loops (`docker run --cpus 8`) and HYPER_LIVENESS_TRACE=1 with --nocapture.
# The reproductions not committed: the held flush, `std::thread::sleep(HYPER_LIVENESS_DELAY_NS)`
# after the device thread's flush in tests/processes.rs; the stopped survivor, `kill -STOP <pid>;
# sleep 2.5; kill -CONT <pid>` from `sh -c` while stall-follower waits, once every pair is judged;
# the held supervisor, `std::thread::sleep(3 s)` in `Supervisor::until` after its reads, once.
```

## The detector model, at its causes (2026-10-03)

`docs/timing.md` §2.2 (the bound per arrival, no configuration at `U ≥ 1`), §2.8 (skipped slots as
lateness, the allowance per heartbeat taken, the evidence interval held until the first
configuration) and §3 item 11 (the history against a sliding window). Apple M5 Max (Mac17,6), 18
cores, 128 GiB, macOS 26.4.1, rustc 1.98.0, release, against main `0e746d1` (the detector code as
`c4530be`, on which the end-to-end runs below were recorded), each set run on both trees one after
the other; the machine shared with other sessions' builds and tests, no load of this work's own
making, the load average recorded with each set.

**The causes, on the recorded runs** (hyper-raft-e2e's "End to end" runs on `c4530be`, ten on macOS
and fifty in Docker's Linux at four, two and one CPUs; each pair's account line read for what the
configurator was fed and returned, each pair's two ends for what the wire lost):

| | Linux (3,398 configured pairs) | macOS (608) |
|---|---|---|
| `α = 0` | 268; `η` at most the receiver's `G` in 244 of them | 1 |
| `U > 1` | 1,475, 1,447 of them at their first configuration | 312, 311 |
| margins at the cap `η − G` (within 5 %), of `α > 0` | 1,347 of 3,130 | 387 of 607 |
| `p_L` fed, median (`U > 1` / `U ≤ 1`) | 42.4 % / 21.1 % | 12.2 % / 5.2 % |
| what the wire lost, from both ends, median (`U > 1`) | 1.9 % | 4.5 % |
| `β` at the configured margin, median (`U > 1`) | 0.936, 2.06 times `p_L` | 0.359, 3.1 times |
| `β` at the cap `η − G`, median (`U > 1`) | 0.997 | 0.422 |
| `U`'s mistake term still ≥ 1 with the wire's loss for `p_L`, the cap kept | 1,294 of 1,475 | 283 of 312 |
| a margin, uncapped, brings it below 1: `p_L` as fed / the wire's | 534 / 1,377 | 253 / 306 |

`α = 0` is the cap: `α < η − G`, a margin held to one heartbeat, is no margin where `G ≥ η`. `U > 1`
is both causes at once: the skipped slots fed as losses put every factor of `β` above `p_L`, and
the cap held the margin where `β` is near one.

**The simulation's worlds** (`tests/sim.rs`'s four, seeds 0–39 of the live-peers test's space, three
nodes, run until every pair is configured and through a renewal; 240 configured pairs a world;
load 3–6, 08:52–09:01):

| world | code | suspicions of live peers / allowance | `α = 0` | `U > 1` | `α` median (most) | `η` median (most) | fed: `p_L` or `u`, median |
|---|---|---|---|---|---|---|---|
| macOS | main | 1,163 / 8,932 | 0 | 44 | 35.6 ms (505) | 138.9 ms (2,963) | `p_L` 15.7 % |
| | this | 65 / 577 | 0 | 0 | 72.7 ms (565) | 44.8 ms (376) | `u` 0.9 % |
| macOS, its log busy | main | 157 / 1,099 | 0 | 0 | 39.4 ms (249) | 124.9 ms (965) | 1.3 % |
| | this | 84 / 565 | 0 | 0 | 127.9 ms (1,003) | 98.6 ms (671) | 0.9 % |
| Linux | main | 4,770 / 20,357 | 1 | 48 | 26.4 ms (271) | 60.5 ms (5,046) | 23.0 % |
| | this | 73 / 899 | 0 | 0 | 45.9 ms (324) | 21.5 ms (177) | 0.8 % |
| Windows | main | 1,391 / 13,951 | 0 | 44 | 138.4 ms (250) | 645.9 ms (6,053) | 22.6 % |
| | this | 0 / 553 | 0 | 0 | 264.2 ms (375) | 170.0 ms (364) | 0.9 % |

**The pair that skips slots** (`a_sender_that_skips_slots_is_late_not_lost`: a sender behind its
schedule for every other slot of a 2 ms stream, the receiver's timer 5 ms late). On main's model, the
same scenario fed `p_L` 0.5001, configured `α = 0` at the 2 ms interval with `U` 10.0 and a best of
379 ms; now a margin past the skipped slot, `U` below one, and no suspicion once configured.

**Worlds that change** (`docs/timing.md` §3, item 11): a calm macOS host whose freezes begin, and
Linux's one-way delays stepping tenfold, once every pair has configured; 16 seeds, four nodes, run
until the pairs' heartbeats have doubled twice after the change, then one node killed once every
survivor trusts it. Counted after the change. With each node's MTBF its own exposure (as the tests
run), the MTBF grew with each run's length, and the comparisons followed it; so the decision rests
on runs whose every node is seeded with a fleet history of a thousand failures over a thousand hours
or days (load 7–15, 09:10–09:20):

| world, MTBF | code | suspicions / allowance | a pair's an hour | mean spacing | a kill noticed: median / 90th / most | `U` realized |
|---|---|---|---|---|---|---|
| freezes begin, an hour | this (history) | 111 / 1,285 | 1.92 | 0.90 s | 1.52 / 1.86 / 2.09 s | 4.5·10⁻⁴ |
| | sliding window | 614 / 15,563 | 2.40 | 1.65 s | 2.92 / 5.81 / 22.6 s | 1.1·10⁻³ |
| | main | 20,399 / 150,041 | 0.65 | 14.5 s | 7.82 / 34.2 / 112 s | 4.3·10⁻³ |
| delays step, an hour | this (history) | 10 / 1,185 | 0.32 | 0.43 s | 0.65 / 1.08 / 1.40 s | 2.1·10⁻⁴ |
| | sliding window | 38 / 6,906 | 0.45 | 0.63 s | 1.02 / 1.86 / 2.83 s | 3.0·10⁻⁴ |
| | main | 4,544 / 218,153 | 0.12 | 5.80 s | 2.62 / 45.9 / 651 s | 9.1·10⁻³ |
| freezes begin, a day | this (history) | 0 / 1,482 | 0 | 2.95 s | 4.55 / 7.14 / 8.16 s | 5.7·10⁻⁵ |
| | sliding window | 365 / 24,611 | 0.16 | 6.58 s | 10.3 / 26.6 / 101 s | 1.6·10⁻⁴ |
| delays step, a day | this (history) | 16 / 1,481 | 0.06 | 1.42 s | 2.54 / 4.04 / 5.20 s | 3.0·10⁻⁵ |
| | sliding window | 25 / 19,116 | 0.02 | 3.23 s | 5.15 / 9.41 / 45.9 s | 7.4·10⁻⁵ |

Main's rows at a day's MTBF did not finish in 31 minutes of a CPU and were stopped: its intervals
asked the simulated hours to run long, and the host's freezes are replayed over all of them.

"`U` realized" is `T_E` times the suspicions a second plus the mean time to notice the kill and
`T_E` over the MTBF, `T_E` the law's election over the paths the run measured (44 ms in the frozen
world, 13 ms in the stepped one). No suspicion stated a bound the kill's notice passed, in any row.
The sliding window is the estimator of item 11 (`link.rs` with an `ArrivalWindow` of half-window
blocks at the levels' horizon, kept in the scratch notes, not committed), both with the evidence
interval held until the first configuration.

The evidence interval held past the first configuration (main's rule), on the history estimate,
each node's MTBF its own exposure (load 3–6, 08:53–09:03):

| world | evidence interval | suspicions / allowance | a pair's an hour | mean spacing | a kill noticed: median / 90th / most |
|---|---|---|---|---|---|
| freezes begin | held until the first configuration | 267 / 807 | 18.0 | 0.32 s | 0.63 / 1.30 / 1.88 s |
| | held for good, the longest of every refusal's | 1,507 / 7,096 | 0.3 | 5.60 s | 4.37 / 11.3 / 23.8 s |
| delays step | held until the first configuration | 41 / 1,375 | 3.0 | 0.16 s | 0.19 / 0.34 / 0.75 s |
| | held for good | 98 / 2,858 | 0.9 | 0.38 s | 0.28 / 0.93 / 1.29 s |

In the frozen world one link held for good sat at 31 s against its configuration's best of 0.57 s.
These runs' MTBFs grew with their length (the shorter spacing ran fewer simulated hours), so their
rates an hour are not comparable across rows; the spacing and the notice are what the rule moved.

**The process test, checked exactly** (`tests/processes.rs`, every suspicion traced as it is
made). Its first gated run failed: the stopped member suspected a peer at a freshness point after
the peer's next heartbeat had come, by the kernel's stamp, 111 ms before it. Two causes, each
shown by a scratch window of 2 ms held between the member's reads of its socket and its poll
(not committed), ten runs of the stop test each, load 6–15 (09:28–09:41):

| the member reads | the drain asks | runs failing so |
|---|---|---|
| its socket, then its clock | the reactor | 7 of 10 |
| its clock, then its socket | the reactor | 4 of 10 |
| its socket, then its clock | the kernel | 3 of 10 |
| its clock, then its socket | the kernel | 0 of 10 |

Unwindowed, the whole binary passed 10 of 10 after (load 12.4). The causes: hyper-tokio's `receive_ready`
asked the reactor whether the socket held anything, and the reactor's readiness was as of its last
turn, before the stop's datagrams came, so the drain read nothing while the socket held
heartbeats stamped before the poll; and the member read its clock after its socket, so a stop
between the two left the stop's datagrams for the next turn. `receive_ready` now asks the kernel
on a kernel-stamped socket, and the member reads its clock first.

**Every suspicion traced from the records** (2026-10-03, 11:02–11:10, load 3.9–6.0). The
simulation's nodes and the process test's members keep a record of what they fed their streams and
what the streams told them (`tests/support/record.rs`), and each suspicion is traced to NFD-E's
rule from it: its heartbeat, its freshness point as the stream held it before the call that told
it, the call at or past the point, and no heartbeat of the peer's taken after stamped before the
point; every heartbeat and poll past a held point told its suspicion; every count a report states
is its record's, at every step and every state line. The simulation, 13 tests at their default
seeds: the live peers' 10 suspicions (8 seeds) all told at a poll, each awaiting a slot its sender
skipped in a freeze, the latest answer 1.10 s past its point; allowance 122.0, reported. The process
test, one run of each supervisor: the stalled and killed members' 6 suspicions, none sent after;
the young victim's 2, each by the point the margin given in the same poll set; the stopped
member's 2, its slots skipped while it was stopped, the latest answer 1.97 s past its point. A
stream made wrong, one way at a time (scratch edits to `pair.rs`, not committed), fails at once:

| the stream made to | caught by | the live peers' simulation | the process test |
|---|---|---|---|
| suspect 20 ms early | the point | 13 suspicions named | |
| judge at half its polls | a poll past a held point told none | 16 named | 14 named |
| drop one heartbeat in five it says it took | the heartbeat judged from | 1 named | |
| leave its suspicions uncounted | the count | at the first step past one | |

**The replay** (`crates/hyper-timing/tests/replay.rs`, four simulated hours at each interval of
each shape, its stalls sent late or skipped, MTBF an hour or a month): every suspicion traced to the
rule. Suspicions against the allowance, an hour's MTBF: macOS 100 µs shape at 5 ms, 2 / 57.0 (late)
and 6 / 30.8 (skipped); at 50 ms, 54 / 146.4 both ways; the flush shape at 20 ms, 0 / 109.1 and
0 / 60.5; at 200 ms, 2 / 399.6 and 1 / 398.9. A month's MTBF: none anywhere, allowances 8.8–34.1.
13 configurations in each run of 288,000 heartbeats.

**Per heartbeat** (`benches/allocs.rs`, its world of calm links: nodes every pair of which shares a
group, ten simulated seconds past every pair's configuration; three rounds, main and this commit
alternating, load 13.8 / 9.6 / 7.9 at 09:21): nanoseconds of the crate's work a heartbeat sent or
taken, and the heartbeats of the window, which this model's shorter intervals send ten times as many
of on these links. Neither allocates, reallocates or faults a page.

| nodes, groups | main: heartbeats sent | ns | this: heartbeats sent | ns |
|---|---|---|---|---|
| 2, 1 | 216 | 76–80 | 2,362 | 71–92 |
| 4, 1 | 608 | 88–100 | 5,127 | 80–87 |
| 8, 1 | 2,952 | 122–150 | 29,089 | 97–101 |
| 8, 1,000 | 2,952 | 112–136 | 29,089 | 104–121 |

```sh
# The causes: scratch/dm-notes/causes1.py over the recorded runs' account lines
python3 causes1.py 'l4-notes/final/linux-*.txt'; python3 causes1.py 'l4-notes/final/mac*.txt'
# The worlds, the changing worlds, the fixed MTBF: scratch tests appended to tests/sim.rs and
# removed (diag_account, diag_changing_worlds, diag_fixed_mtbf, the last seeding Settings::history);
# the sliding window and the evidence held for good put back by hand. On each tree:
cargo test --release -p hyper-liveness --test sim diag_ -- --nocapture
cargo test --release -p hyper-timing --test replay -- --nocapture --test-threads=1
cargo bench -p hyper-liveness --bench allocs --no-run   # then each binary with --bench, alternating
```

## `G` from the waits the owner began (2026-10-03)

`docs/timing.md` §2.4. `Liveness::on_wait(deadline, woke)`: the owner reports each timed wait for
the stream's wake that it began before the deadline and that the deadline ended, and `G` is their
mean; a poll past a wake no longer counts. `an_owner_held_in_its_own_write_is_no_lateness_of_its_timer`
(`tests/sim.rs`): an owner whose timer ends every wait 1 ms late and whose thread is held 50 ms in
its own write at every other wake, 200 of each. Here `G` is 1 ms exactly; the same owner on main,
whose stream folded every poll past a wake, measured 25.5 ms, the mean of the two. The trace
recorder (`hyper-timing-trace run`) has always kept only waits begun before their deadline and
ended on their timeout, so the `G` of "Heartbeat traces" and "The simulation's worlds" is this
one; the members' own, before and after, are in the end-to-end accounts below.

## `G` from every wait that reached its wake (2026-10-03)

`docs/timing.md` §2.4 and §2.9 ("hyper-durable-e2e's `kill`, no `G` where messages end the
waits"). hyper-durable-e2e's `kill` in Docker's Linux at two CPUs (`docker run --cpus 2`, the
VM's load 4.3–5.5 as the runs began and ended, the Mac's 9–13), from a diagnostic build of its
member that counts, every half second, the waits it began for the stream's wake, how each ended,
what it reported and the heartbeats its stream refused (not committed: `scratch/dm-notes/diag`),
first as `dc42e0e` reports its waits, only those the deadline ended, then reporting every wait it
began before the wake that ended at or past it. Three runs each, 11:39–11:43, before and after in turn
as far as the switch of builds allowed (before, after three times, before twice):

| the member reports | runs passing | member processes a second or more without `G` | heartbeats refused as unmeasured | waits for the wake ended on the timer / reported / all |
|---|---|---|---|---|
| waits the deadline ended | 0 of 3 (`stall-leader`, `stalled-devices`, `flush-leader`) | 4 of 12, 2 of 15, 5 of 32 | 4,020, 2,575, 2,370 | 3,208 / 3,208 / 51,729; 2,603 / 2,603 / 75,882; 3,335 / 3,335 / 127,961 |
| waits that reached the wake | 3 of 3 (40.1, 23.1, 22.5 s) | 1 of 19, 1 of 19, 1 of 24, each refusing none | 51, 9, 18 | 4,273 / 6,169 / 94,726; 3,872 / 4,792 / 19,380; 4,605 / 5,798 / 20,902 |

A member's `G` came a median 79–137 ms into its start before (where it came at all, once 14 s) and
108–143 ms after; in `stall-leader`'s failed run the member that never measured it began 21,901
waits for its wake, none ended on its timer, and refused 1,360 heartbeats. The waits that reached
the wake on an ask past it are what the timer-only count missed: a fifth to a third of the reports
after. The members' `G` over their half-second lines, where measured: a median of 1.23–1.36 ms
before, the tick, which alone ended the waits counted; 0.20–0.95 ms after, the asks ending waits
past the wake sooner than the tick. `a_wait_a_message_ends_past_its_wake_measures_the_wake`
(`tests/sim.rs`) pins the rule: an owner whose every wait a message ends 300 µs past its wake, its
timer 1 ms late, has `G` 300 µs from the first wake and takes every heartbeat; counted only as the
timer ended them, it measures none and refuses every heartbeat.


## The detectors' account end to end, main against this branch (2026-10-03)

hyper-raft-e2e's `cluster`, whose members print each pair's account, once on main `9893679` and
once on this branch's head `d712a5f`, on macOS (Apple M5 Max, `F_FULLFSYNC`) and in Docker
Desktop's Linux VM at four, two and one CPUs (`docker run --cpus`; 1 ms ticks, `fdatasync` on a
disk image), under the machine's ambient load alone: other sessions' builds and tests, the load
recorded as each run began and ended (main: macOS 10:27, load 4.7–8.3; Linux 10:36–10:48, the VM's
4.3–7.8. The head: macOS 14:22, load 7.0–7.2; Linux 14:14–14:22, the VM's 4.2–7.2). Main's later
commits up to `418851a` change none of the code these runs exercise. Read by
`scratch/dm-notes/accounts2.py` (`table.py`): the configured pairs' (`own`) account lines, and the
members' `G`, `E[flush]` and `T_E`.

| | macOS main | macOS head | Linux 4 main | Linux 4 head | Linux 2 main | Linux 2 head | Linux 1 main | Linux 1 head |
|---|---|---|---|---|---|---|---|---|
| run, s | 60 | 58 | 39 | 73 | 86 | 76 | 101 | 280 |
| pairs own / pool / unjudged | 64/2/10 | 63/3/10 | 71/1/4 | 72/2/2 | 69/3/4 | 70/2/4 | 65/5/6 | 66/2/8 |
| configured with `α = 0`; `U > 1` | 0; 51 of 64 | 0; 0 of 63 | 0; 5 of 71 | 0; 0 of 72 | 1; 19 of 69 | 0; 0 of 70 | 0; 22 of 65 | 0; 0 of 66 |
| `η` median (p90), ms | 14.9 (27.7) | 12.6 (79.2) | 9.18 (19.4) | 16.6 (25.4) | 18.2 (63.2) | 13.4 (20.1) | 14 (40.3) | 75.9 (373) |
| `α` median (p90), ms | 9.75 (20.4) | 110 (200) | 4.55 (13.4) | 19.3 (1.07e+03) | 10.6 (60.4) | 15.8 (931) | 7.89 (33.2) | 86.7 (406) |
| `U` median (max) | 1.59 (4.64) | 0.0692 (0.195) | 0.0265 (5.87) | 0.0031 (0.161) | 0.236 (5.53) | 0.00228 (0.143) | 0.472 (8.23) | 0.00332 (0.0471) |
| fed `p_L` / unseen share, median | 0.102 | 0.0132 | 0.0896 | 0.0029 | 0.0632 | 0.0029 | 0.158 | 0.0037 |
| slots skipped, share median (p90) | - | 0.137 (0.259) | - | 0.0259 (0.36) | - | 0.028 (0.36) | - | 0.0275 (0.124) |
| suspicions / allowance, commits-3/5 | 147 / 772.7 | 0 / 49.5 | 216 / 851.3 | 20 / 146.3 | 434 / 1460.1 | 6 / 139.2 | 207 / 1177.8 | 0 / 135.4 |
| all scenarios | 382 / 2373.5 | 39 / 135.0 | 655 / 2756.0 | 83 / 415.9 | 1020 / 4066.3 | 85 / 547.1 | 2347 / 8535.7 | 503 / 343.9 |
| `G` median ms, commits-3/5; stalled-devices | 5.03; 47.7 | 0.439; 0.399 | 0.339; 15.5 | 0.699; 0.775 | 2.44; 18.3 | 0.646; 0.788 | 0.42; 37.8 | 0.731; 1.35 |
| `T_E` of a stopped member's peers, ms | 1.43e+03 | 46 | 2.9 | 4.8 | 340 | 2.9 | 1.23e+03 | 26 |
| leader's term at leader-killed's kill | 3 | 1 | 9 | 29 | 13 | 21 | 20 | 14 |

- **No detector that promises nothing.** Main configured `U > 1` in 51 of 64 pairs on macOS and in
  5, 19 and 22 of 65–71 in Linux at four, two and one CPUs, and `α = 0` once (two CPUs); the head
  in none (`docs/timing.md` §2.2).
- **What the configurator is fed.** Main's `p_L`, a median of 0.10 on macOS and 0.06–0.16 in
  Linux, was mostly slots its senders skipped. The head takes each skipped slot as the next
  heartbeat's lateness, and its unseen share is a median of 0.003–0.013. The senders skipped a
  median of 13.7 % of their slots on macOS, where a flush is about three quarters of the
  interval, and 2.6–2.8 % in Linux, the members' synchronous log writes holding their threads past
  a slot.
- **Wider margins, fewer mistakes.** `α` is a median of 110 ms on macOS and 15.8–86.7 ms in Linux,
  against main's 4.6–10.6 ms: the latenesses are heavy-tailed, and one Cantelli factor bounds them
  at the margin that minimizes `U`. In commits-3 and commits-5, where no member is down or cut
  off, the head's members suspected live peers 0, 20, 6 and 0 times, against main's 147, 216, 434
  and 207 (macOS; Linux at four, two and one CPUs). The runs took 58, 73, 76 and 280 s against
  main's 60, 39, 86 and 101 s: the test's waits follow the members' stated detection bounds, which
  wider margins lengthen, and at one CPU `follower-restarts`, `partition` and `stalled-devices`
  took 62–78 s each.
- **`G` is the timer's or the messages', never the owner's stalls**: 0.44 ms in commits-3/5 and
  0.40 ms through `stalled-devices` on macOS, 0.65–0.73 ms and 0.78–1.35 ms in Linux, against
  main's 5.0 and 47.7 ms, and 0.34–2.4 and 15.5–37.8 ms.
- **A stopped member's peers' `T_E`**: 46 ms on macOS and 2.9–26 ms in Linux, against main's
  1.43 s, and 2.9 ms, 340 ms and 1.23 s. Main's echoes of heartbeats that sat in the stopped
  member's socket counted the stop as the path; the kernel's stamp leaves it out (`dc42e0e`).
- **The leader's term at `leader-killed`'s kill**: 1 on macOS against main's 3, and 29, 21 and 14
  in Linux against main's 9, 13 and 20. A follower that suspects its leader campaigns, so the term
  counts the suspicions of leaders; in `leader-killed` the head's members suspected 39, 30 and 28
  times against allowances of 58.7, 85.6 and 69.1 (main 41, 229 and 138 against 120, 629 and
  332), the members killed included. One run each: on `dc42e0e`'s tree the same column read 5,
  48 and 1.
- **Suspicions against the allowance**, reported, never asserted: 39 / 135.0 on macOS, and 83 /
  415.9, 85 / 547.1 and 503 / 343.9 in Linux, against main's 382 / 2,373.5, and 655 / 2,756.0, 1,020
  / 4,066.3 and 2,347 / 8,535.7; the scenarios with a member down count its true suspicions too. At
  one CPU `follower-restarts` made 457 against 27.4: 287 by one member of the follower restarted,
  the pair at a 9 ms interval (that member skipped 707 of its 1,461 slots to the follower), and 107
  and 57 between the two live members. The exact check held (every report's `unread` zero), so each
  was the stream's, a heartbeat that came past its point by the kernel's stamp. A rerun of the
  scenario at one CPU, the members printing each suspicion (not committed), made 8 against 11.3,
  every one a heartbeat 18–188 ms past its point after a pause of every member at once, or the kill:
  the burst of the first run did not come again.
- **The members' polls past what they had read** (`docs/timing.md` §2.9, "The E2E members polled
  their streams past what they had read"): on `09e8316`, before the drain rule, `stalled-devices`
  made 465, 392 and 9 suspicions at four, two and one CPUs (main 19, 124 and 60); on the head 10,
  16 and 8.

**hyper-durable-e2e's `kill` and hyper-liveness's `processes`**, the same runs. `kill` passed on
the head on every platform, in 46.5 s on macOS and 20.6–21.0 s in Linux; main's failed once in
Linux at two CPUs (`kill-durable-leader`, a write never answered, open on main). `processes` passed
on both: the head's live members suspected 0, 0, 0 and 2 times against allowances of 2.0, 7.7, 2.0
and 1.5 (macOS; Linux at four, two and one CPUs), every suspicion traced to the rule, and main's 0,
1, 22 and 0 against 12.2, 19.9, 56.7 and 3.0.

**`G` on the traces** (`hyper-timing-trace run`; the recorder's waits are each begun before their
deadline and ended by it, nothing else waking them, so its `G` is the timer's alone): 194.5 µs on
macOS at 1 ms for 60 s (11:58–11:59, load 2.9–4.2; 59,224 waits asked a median of 753 µs, late a
median of 198.1 µs) and 932.0 µs in Docker's Linux at two CPUs at 2 ms for 60 s (14:24–14:25, the
VM's load 5.7–6.1; 26,764 waits asked a median of 668 µs, late a median of 844 µs, the VM's 1 ms
tick). The members' `G` in the table is how late past each wake the stream was polled while they
waited, by the timer or by the first message past the wake: below the recorder's in Linux, where
messages end most waits before the tick does.

**Per heartbeat** (`benches/allocs.rs`, its world of calm links: nodes every pair of which shares a
group, ten simulated seconds past every pair's configuration; three rounds, main `9893679` and the
head alternating, 14:25, load 5.8–6.0): nanoseconds of the crate's work a heartbeat sent or taken,
and the heartbeats of the window, which this model's shorter intervals send ten times as many of on
these links. Neither allocates, reallocates or faults a page.

| nodes, groups | main: heartbeats sent | ns | head: heartbeats sent | ns |
|---|---|---|---|---|
| 2, 1 | 216 | 71–83 | 2,362 | 71–74 |
| 4, 1 | 608 | 89–94 | 5,094 | 77–79 |
| 8, 1 | 2,952 | 118–123 | 29,785 | 108–112 |
| 8, 1,000 | 2,952 | 116–121 | 29,785 | 107–110 |

```sh
# Each binary once a side and platform, the load recorded (scratch/dm-notes/e2e, linux.sh and
# mac-branch.sh); the accounts read by table.py over accounts2.py:
cargo test --locked -p hyper-raft-e2e --test cluster -- --nocapture
cargo test --locked -p hyper-durable-e2e --test kill -- --nocapture --test-threads=4
cargo test --locked -p hyper-liveness --test processes -- --nocapture --test-threads=4
hyper-timing-trace run <dir> <interval-us> 60 && hyper-timing-trace analyse <dir>
cargo bench -p hyper-liveness --bench allocs   # on each tree, alternating
```

## Copa's competing mode over focal's grids (2026-10-04)

Measured on hyper-quic's congestion harness (`crates/hyper-quic/tests/congestion.rs`). Each flow is
two hyper-quic endpoints over hyper-sim's network: a dumbbell with one bottleneck each way and a queue
of one bandwidth-delay product, ECN carried. Every run is a deterministic simulation of its seed,
checked against a second run from its seed and from its trace. So the machine's load during the sweep
moves no number: 3.4 to 21.6, beside focal's gates and traced runs.

**The grid.** focal's harm grid: 1 Mbit/s and 100 ms, 10 Mbit/s and 20 ms, 10 Mbit/s and 100 ms,
100 Mbit/s and 20 ms. Each run is 30 s, seeds 1–8, under no queue manager, CoDel (5/100 ms) and a step
of one datagram. Copa runs beside NewReno and beside CUBIC. In the leave runs the incumbent stops at
the run's half. focal's alone grid runs each law alone: 1M/20, 1M/100, 10M/20, 10M/100, 100M/20,
30 s, seed 1. The bar is `min(incumbent beside its own kind, incumbent beside CUBIC)` (Ware et al.,
HotNets 2019).

**The rule**, fixed before any run of this grid. A law is admissible when:

1. with CoDel or without a manager, in every seed, neither flow carries under a tenth of the other;
2. under either manager, Copa's datagrams stay ECN-capable;
3. under CoDel, in every seed, CoDel marks Copa and each incumbent carries at least 9/10 of its bar;
4. without a manager, in every seed, each incumbent carries at least 9/10 of its bar;
5. alone, on every path, Copa carries at least 3/10 of the link and its queue's p99 is under
   NewReno's;
6. beside an incumbent that stops at the half, in every seed, Copa competed while the incumbent
   sent, and no sample in the run's last quarter finds it competing.

Of the admissible laws, the one chosen is the one under which Copa carries the most beside the
incumbents. If none is admissible, the findings stay open and the law does not change to a law
that failed.

```
cargo test -p hyper-quic --test congestion --locked every_law_alone_over_focals_grid -- --ignored --nocapture
cargo test -p hyper-quic --test congestion --locked copa_shares_a_bottleneck_over_focals_grid -- --ignored --nocapture
cargo test -p hyper-quic --test congestion --locked copa_stops_competing_over_focals_grid -- --ignored --nocapture
```

**The laws measured.** Each was run with Copa's pacing at `2·cwnd/RTTstanding` (§2.1).

- **focal**: focal's law before b18. The mode is judged over four smoothed round trips, a delay
  sample is judged by the window now, and competing raises `1/δ` a packet a round trip.
- **b18**: focal's derivations. A1 judges the mode over five round trips. A2 judges a sample by the
  window its packet was sent under. B raises `1/δ` by `d_q/RTTstanding` a round trip.
- **G**: a queue of one datagram or less at Copa's own rate counts as nearly empty. A tenth of the
  spread of a few datagrams' queue asks for an idle link.
- **H**: the competing mode ends only once the queue has stayed nearly empty for longer than the
  mode's window. That marks Copa alone's emptying, which recurs cycle after cycle (§3), not the one
  empty moment a competitor's backoff leaves (§2.2).
- **E**: competing, the window is NewReno's congestion avoidance from the window Copa entered the
  mode with, halved on a loss once a round trip.

| Law | Copa's share beside the incumbents (geomean, 128 runs) | Rule failures (runs) |
|---|---|---|
| focal, as shipped before the cut | **41.70%** | 3: 3 (bar); 4: 14; 5: 1; 6: 62 of 64 |
| GH-E: b18, G, H and E | 39.42% | 5: 2; 6: 37 of 64 |
| GH-paper: A1, A2, G and H, a packet a round trip | 36.31% | 4: 10; 6: 30 of 64 |
| GH-B: b18, G and H | 25.84% | 3: 4 (CoDel marked nothing); 5: 1; 6: 35 of 64 |
| b18 | 25.03% | 3: 3 (CoDel marked nothing); 6: 46 of 64 |

No law is admissible. The law stayed focal's from before b18, which also carries the most, until the
cut below.

**Where focal's law, shipped before the cut, fails.**

- Rules 3 and 4, an incumbent under 9/10 of its bar:
  - without a manager at 1 Mbit/s and 100 ms, CUBIC in all eight seeds (0.636 of its bar at least)
    and NewReno in seeds 1, 2 and 6 (0.735);
  - without a manager, CUBIC at 10 Mbit/s and 20 ms in seed 3 (0.884), and at 100 Mbit/s and 20 ms
    in seeds 1 and 7 (0.84);
  - under CoDel at 1 Mbit/s and 100 ms, NewReno in seeds 4–6 (0.849).

  This is focal's finding 1.
- Rule 5: at 1 Mbit/s and 20 ms Copa alone kept a queue p99 of 28.80 ms, equal to NewReno's and not
  under it. Copa judged itself competing in 59.8% of the run.
- Rule 6: Copa competed in the last quarter in 62 of 64 leave runs: every run but two seeds beside
  NewReno at 10 Mbit/s and 20 ms. This is focal's finding 3. At 100 Mbit/s and 20 ms every law
  failed all sixteen runs.

focal's law alone, focal's alone grid:

| Path | Copa carried | Queue p50 / p99 ms (NewReno's) | Copa competing, alone |
|---|---|---|---|
| 1 Mbit/s, 20 ms | 89.6% | 18.10 / 28.80 (18.02 / 28.80) | 59.8% |
| 1 Mbit/s, 100 ms | 96.6% | 24.51 / 53.31 (43.71 / 82.11) | 12.1% |
| 10 Mbit/s, 20 ms | 96.7% | 2.05 / 16.45 (9.73 / 18.37) | 15.3% |
| 10 Mbit/s, 100 ms | 97.2% | 3.65 / 11.33 (61.25 / 97.73) | 42.1% |
| 100 Mbit/s, 20 ms | 97.2% | 0.35 / 0.83 (11.10 / 19.74) | 36.5% |

**CoDel marked nothing in seven runs.** Under b18 and GH-B, at 100 Mbit/s and 20 ms, CoDel marked
none of Copa's datagrams, where Copa carried 17–23%. That fails rule 3. It is no ECN fault. Three of
the runs were replayed under b18 and Copa's datagrams stayed ECN-capable to the end. CoDel acted 11
or 12 times in 30 s, once a sawtooth of the incumbent's at that rate, and each time a datagram of
the incumbent's stood at the queue's head. A seed in which CoDel marked Copa twice was replayed
beside them.

### The cut: Copa withdraws its own bytes and looks (2026-10-04, admissible)

The design and its sources are in `docs/research/congestion.md`, "Whose queue is it". Where the
paper switches to competing (the queue not nearly empty over five round trips), Copa cuts its window
to `r·RTTmin` less two datagrams (never under three), which withdraws its own bytes in the queue.
Alone, the packets sent under the cut find the queue empty; beside another sender its backlog stays.
Competing, the window is NewReno's (E), and Copa cuts again every `CUT_INTERVAL_SRTTS` round trips,
leaving the mode only on a cut that finds the queue empty. With it, A1, A2 and G. Same harness, grid,
seeds 1–8 and rule as above, judged by the rule's script over the rows (`judge.py`). Every run is
exact for its seed, so the load moves no number: 3.5 to 20.6 over the sweeps (23:10–23:47 PDT), from
other sessions' builds and gates.

| Law | Copa's share beside the incumbents (geomean, 128 runs) | Rule failures (runs) |
|---|---|---|
| **cut, every 40 round trips competing (shipped)** | **35.48%** | none |
| cut, every 20 | 31.83% | none |
| cut, every 80 | 37.81% | 6: 4 of 64 (1 Mbit/s and 10 Mbit/s at 100 ms: the next cut came after the last quarter began) |
| cut, every 10 | 27.18% | 3: 1 (CoDel marked none of Copa's datagrams at 100 Mbit/s, 20 ms, seed 3, beside CUBIC) |

The law is the cut at 40: of the admissible, the one under which Copa carries the most.

Alone (focal's alone grid, 30 s, seed 1), against the shipped law before it:

| Path | Copa carried | Queue p50 / p99 ms (NewReno's) | Copa competing, alone (before) |
|---|---|---|---|
| 1 Mbit/s, 20 ms | 83.2% | 18.02 / 27.71 (18.02 / 28.80) | 0.0% (59.8%) |
| 1 Mbit/s, 100 ms | 96.3% | 24.51 / 53.31 (43.71 / 82.11) | 0.0% (12.1%) |
| 10 Mbit/s, 20 ms | 95.8% | 1.92 / 3.97 (9.73 / 18.37) | 0.0% (15.3%) |
| 10 Mbit/s, 100 ms | 97.2% | 2.69 / 4.61 (61.25 / 97.73) | 0.0% (42.1%) |
| 100 Mbit/s, 20 ms | 97.0% | 0.25 / 0.54 (11.10 / 19.74) | 0.0% (36.5%) |

Beside the incumbents, 30 s, seeds 1–8, Copa's carried share and the least of the incumbent's share
of its bar over the seeds (the step reported, not judged):

| Path | Manager | Incumbent | Copa carried | Incumbent's bar share, least | Copa competing |
|---|---|---|---|---|---|
| 1M 100ms | none | NewReno | 38.0–43.2% | 1.178 | 44.2–80.8% |
| 1M 100ms | none | CUBIC | 31.2–39.0% | 1.243 | 44.3–76.8% |
| 1M 100ms | CoDel | NewReno | 37.2–40.6% | 1.038 | 0% |
| 1M 100ms | CoDel | CUBIC | 38.5–41.8% | 1.063 | 0% |
| 10M 20ms | none | NewReno | 35.5–39.1% | 1.423 | 77.7–83.2% |
| 10M 20ms | none | CUBIC | 32.6–37.0% | 1.247 | 82.2–87.5% |
| 10M 20ms | CoDel | NewReno | 35.9–40.8% | 1.222 | 64.5–75.7% |
| 10M 20ms | CoDel | CUBIC | 35.2–39.8% | 1.187 | 60.5–72.0% |
| 10M 100ms | none | NewReno | 21.9–38.2% | 2.021 | 62.4–85.5% |
| 10M 100ms | none | CUBIC | 19.7–28.8% | 1.479 | 87.1–93.6% |
| 10M 100ms | CoDel | NewReno | 33.0–42.1% | 1.139 | 0% |
| 10M 100ms | CoDel | CUBIC | 30.4–41.8% | 1.173 | 0% |
| 100M 20ms | none | NewReno | 34.3–44.4% | 1.186 | 90.1–96.8% |
| 100M 20ms | none | CUBIC | 26.0–41.2% | 1.162 | 92.3–98.7% |
| 100M 20ms | CoDel | NewReno | 31.1–43.4% | 1.209 | 56.0–74.6% |
| 100M 20ms | CoDel | CUBIC | 26.9–45.2% | 1.138 | 47.6–61.5% |
| any | step | either | 1.2–26.3% | 0.870 | 0% |

- **Finding 1 is closed.** Without a manager every incumbent carries 1.16 of its bar or more in
  every seed, where the shipped law left CUBIC 0.636 at 1 Mbit/s and 100 ms.
- **Finding 3 is closed.** In all 64 leave runs Copa competed while the incumbent sent (22.5% to
  59.4% of the run) and in none of the last quarter's samples; the shipped law competed there in 62.
  `copa_stops_competing_once_its_competitor_leaves` runs in the gate again.
- **Finding 4 stands as a behaviour and no longer as a harm.** Under CoDel at 100 ms the cut finds no
  backlog of the incumbent's beyond the allowance and Copa does not compete; the incumbents still
  carry 1.04 of their bar or more, the marks answered as a classic sender answers them.
- CoDel marked Copa in all 64 CoDel runs and Copa's datagrams stayed ECN-capable in every managed
  run (the shares test's own checks).
- **With SecP384r1MLKEM1024 first between nodes and hyper-quic's handshake copies**
  (`docs/seal.md` §10, `crates/hyper-quic/VENDORED.md` §13), the harness's identity now fixed: the
  three grids again, 2026-10-05 03:08–03:23 PDT. Admissible, every rule met in every run; Copa's
  share beside the incumbents 36.23% (geomean, 128 runs).

```
cargo test -p hyper-quic --test congestion --locked every_law_alone_over_focals_grid -- --ignored --nocapture
cargo test -p hyper-quic --test congestion --locked copa_stops_competing_over_focals_grid -- --ignored --nocapture
cargo test -p hyper-quic --test congestion --locked copa_shares_a_bottleneck_over_focals_grid -- --ignored --nocapture
```

## hyper-check's checkers (S-4, 2026-10-04)

What judging a history costs (`docs/sim.md` §14.8, `docs/tails.md` §1a): CPU time, instructions and
cycles from `proc_pid_rusage(RUSAGE_INFO_V6)` (`hyper_measure::usage`, its times converted from Mach
absolute units by `mach_timebase_info` and checked against `getrusage` on this machine), allocations
and the most bytes held at once from the counting allocator (`hyper_measure::cost`), the process's
highest physical footprint beside. Apple M5 Max (Mac17,6), 18 cores, macOS, rustc 1.98.0, release,
on `line` (`2e9fd39`, the schedules' rows on `a1210ab`) with S-4; the one-minute load average beside each, from the other sessions'
builds and campaigns on the machine (none generated for the run).

**Per history, both checkers on one history** (the agreement of `docs/sim.md` §14.4), p50 / p99 /
max over the seeds by nearest rank (at 96 or fewer seeds the p99 is the most), one test at a time
(`--test-threads 1`) so the process's counts are the history's:

| Histories | Seeds | Load | Configurations | User µs | System µs | Instructions | Cycles | Allocations | Peak bytes |
|---|---|---|---|---|---|---|---|---|---|
| hyper-raft group | 96 | 82.3 | 65 / 188 / 188 | 54.0 / 106.0 / 106.0 | 6.1 / 10.8 / 10.8 | 476k / 1,165k / 1,165k | 257k / 488k / 488k | 222 / 408 / 408 | 29,056 / 64,040 / 64,040 |
| hyper-raft group | 5,000 | 67.5 | 66 / 176 / 261 | 54.9 / 112.6 / 146.8 | 5.4 / 14.5 / 55.2 | 481k / 1,095k / 1,490k | 258k / 515k / 676k | 224 / 389 / 495 | 29,056 / 61,160 / 105,552 |
| hyper-raft fast | 96 | 80.7 | 6 / 38 / 38 | 19.0 / 48.1 / 48.1 | 6.8 / 23.8 / 23.8 | 124k / 449k / 449k | 113k / 225k / 225k | 95 / 229 / 229 | 8,392 / 29,672 / 29,672 |
| hyper-raft fast | 20,000 | 15.8 | 6 / 30 / 102 | 17.0 / 38.7 / 76.3 | 4.9 / 10.4 / 36.0 | 122k / 327k / 711k | 92k / 190k / 366k | 94 / 180 / 312 | 8,392 / 18,496 / 46,816 |
| hyper-raft pipelined | 48 | 83.4 | 8 / 25 / 25 | 15.2 / 27.2 / 27.2 | 4.9 / 14.5 / 14.5 | 108k / 211k / 211k | 86k / 173k / 173k | 86 / 133 / 133 | 4,985 / 9,336 / 9,336 |
| group, 25 in 100 lost and repeated | 96 | 83.4 | 41 / 190 / 190 | 39.3 / 94.5 / 94.5 | 6.3 / 10.7 / 10.7 | 323k / 1,095k / 1,095k | 197k / 425k / 425k | 176 / 382 / 382 | 16,632 / 60,664 / 60,664 |
| group, 50 in 100 | 96 | 83.4 | 19 / 111 / 111 | 26.0 / 84.6 / 84.6 | 6.4 / 11.4 / 11.4 | 191k / 772k / 772k | 136k / 394k / 394k | 126 / 314 / 314 | 10,272 / 40,768 / 40,768 |
| group, 75 in 100 | 96 | 83.4 | 6 / 29 / 29 | 16.1 / 27.0 / 27.0 | 5.8 / 9.9 / 9.9 | 104k / 223k / 223k | 92k / 148k / 148k | 84 / 135 / 135 | 5,928 / 12,440 / 12,440 |
| mantle's range simulation | 48 | 47.9 | 58 / 58 / 58 | 44.8 / 75.7 / 75.7 | 0.8 / 49.8 / 49.8 | 667k / 899k / 899k | 201k / 541k / 541k | 671 / 700 / 700 | 19,440 / 22,035 / 22,035 |

The most any history held, 105,552 bytes, is 2.5·10⁻⁵ of the search's 4 GiB ceiling (§7); every
history is held to the ceiling by `agreed`. The process's highest footprint over a run was 4.5–6.5
MB, the test binary's own. The 5,000- and 20,000-seed rows were campaigns (each its own process, two
at once). Commands: `cargo test --release -p hyper-raft --test check -- --skip fast_track_without
--nocapture --test-threads 1`; the campaigns with `HYPER_RAFT_SEEDS=5000` (group) and `20000`
(fast) and `--exact` the test; `cargo test --release -p hyper-check --test mantle -- --nocapture
--test-threads 1`.

**Per configuration, the search alone** (`cargo bench -p hyper-check --bench search -- 3`, load
48.7, three rounds, the range over them):

| Workload | Units | Wall ns | CPU ns | Instructions | Cycles | Allocations | Bytes held at most |
|---|---|---|---|---|---|---|---|
| pass: an honest register history of 20,000 operations by three clients | 23,430 configurations | 103–133 | 103–132 | 1,610–1,658 | 394–569 | 0.014 | 3,284,301 |
| refuse: six clients, eight writes and reads each, a last read of a value nobody wrote, fingerprints then whole keys | 46,656 configurations (23,328 confirming) | 316–324 | 316–324 | 4,896–4,934 | 1,137–1,259 | 0.141 | 2,768,870 |
| witness: the honest history's 50,007 events | 50,007 events | 77–80 | 77–80 | 820–835 | 279–287 | 0.082 | 2,632,672 |

A configuration on the honest history costs about 1,600 instructions and one allocation in seventy:
the memo's table doubling and the order's growth. A refused history costs three times as much a
configuration, for every configuration is reached and then searched again on whole keys, whose
`BTreeSet` keys allocate.

## hyper-check's strategies and searches (S-5, 2026-10-04)

What a run of each strategy of `docs/sim.md` §15 costs (`docs/tails.md` §1a): CPU time from
`proc_pid_rusage(RUSAGE_INFO_V6)`, instructions and cycles, allocations and the most bytes held at
once from the counting allocator, by `hyper_measure::cost` around each run, p50 / p99 / max by
nearest rank (at 96 runs the p99 is the most). Apple M5 Max (Mac17,6), 18 cores, 128 GiB, macOS
26.4.1, rustc 1.98.0, release, branch `check-s5`; the one-minute load average beside each, from
other sessions' builds and campaigns and this work's own campaigns (four to six single-threaded
campaign processes ran beside every measurement here; none was waited out).

**A run of each strategy**, 96 runs of the harness's 4,000-step schedules and liveness phase, no
defect planted, one test at a time (`cargo test --release -p hyper-raft --all-features --test
strategies -- --ignored --exact strategy_costs --nocapture --test-threads 1`, on the tree with
§15.9's corrections), load 17.5–20.3:

| Harness | Strategy | User ms | Instructions (M) | Allocations (k) | Peak bytes (k) |
|---|---|---|---|---|---|
| group | random walk | 14.2 / 32.5 / 32.5 | 328 / 714 / 714 | 403 / 790 / 790 | 245 / 404 / 404 |
| group | swarm | 8.9 / 114.8 / 114.8 | 213 / 2,525 / 2,525 | 257 / 3,029 / 3,029 | 150 / 686 / 686 |
| group | PCT, depth 2 | 15.1 / 35.0 / 35.0 | 358 / 815 / 815 | 423 / 819 / 819 | 255 / 406 / 406 |
| group | random walk, every step held to the model | 20.1 / 51.1 / 51.1 | 462 / 1,094 / 1,094 | 452 / 845 / 845 | 247 / 415 / 415 |
| group | coverage-guided (conformance, coverage, tape) | 50.9 / 63.6 / 63.6 | 809 / 1,009 / 1,009 | 510 / 623 / 623 | 774 / 895 / 895 |
| fast | random walk | 15.3 / 26.3 / 26.3 | 339 / 519 / 519 | 401 / 646 / 646 | 292 / 461 / 461 |
| fast | swarm | 9.9 / 125.4 / 125.4 | 227 / 2,610 / 2,610 | 271 / 3,134 / 3,134 | 191 / 1,229 / 1,229 |
| fast | PCT, depth 2 | 14.7 / 30.6 / 30.6 | 338 / 719 / 719 | 380 / 741 / 741 | 289 / 608 / 608 |
| fast | random walk, every step held to the model | 21.5 / 33.6 / 33.6 | 466 / 705 / 705 | 473 / 731 / 731 | 296 / 467 / 467 |
| fast | coverage-guided | 49.5 / 73.2 / 73.2 | 717 / 944 / 944 | 431 / 615 / 615 | 812 / 946 / 946 |

System time was under 1.6 ms a run in every row. The conformance check costs a run 1.4 times the
random walk's CPU at the median (20.1 against 14.2 ms): the abstraction copies every member's log a
step (`docs/sim.md` §11 item 3). The coverage campaign's run costs 3.6 times the random walk's: the
abstraction, the coverage set's insert and the tape's record of every word. The swarm's median run is
cheaper than the random walk's (its configurations often turn off kinds of steps), its tail dearer
(losses and repeats to three in four lengthen the liveness phase). PCT costs what the walk does. The
same measurement before the corrections, at load 52–55, gave the same allocation counts but the
swarm's (its configurations' runs changed with the core's correction) and CPU times 4–20 % higher.

**The round search and shrinking** (same command and run): one exhaustive search of two rounds of
three members, 12,240 rounds played, 450 ms user (p50 of 3 searches), 8.38·10⁹ instructions, 8.71
million allocations, 92 KB held at most — 37 µs and 712 allocations a round played; shrinking seed
0's read before the first commit from 944 steps to 30, 471 runs, 173 ms user, 3.71 million
allocations, 370 KB held at most.

**The round search at four rounds** (`... --exact four_rounds_of_three_members_catch_the_older_term_commit
--nocapture --test-threads 1`, load 58–69): three rounds twice (249,840 rounds played each), four
rounds clean (4,481,280) and four with the older-term commit planted to its catch (33,621) in
512 s wall, 163 s user, 7.5 MB resident at most.

**Crash enumeration, by fork and by replay**, per schedule enumerated (one of 44: four settings ×
11 seeds of 400 steps, 1,964 crash points; `cargo test --release -p hyper-raft --all-features
--test pipeline -- --ignored --exact crash_enumeration_costs --nocapture --test-threads 1`, load
56.4):

| | User ms | Instructions (M) | Allocations (k) | Peak bytes (k) |
|---|---|---|---|---|
| by fork | 7.1 / 14.8 / 14.8 | 163 / 350 / 350 | 214 / 471 / 471 | 94 / 111 / 111 |
| by replay | 12.4 / 22.3 / 22.3 | 278 / 500 / 500 | 368 / 663 / 663 | 63 / 81 / 81 |

The fork takes 57 % of the replay's CPU and 58 % of its allocations at the median, and holds 47 %
more at its peak (the clone beside the trunk). The saving is the prefixes' 400-step schedules
(17,600 steps of trunk against 785,600 of prefix and rest); each branch's liveness phase, which the
fork does not save, is most of the rest. A clone of the group costs what its members hold (§11
item 2: a member's log, its progress and its queues; no measured cost to production, a derive adds
no code where nothing clones).

**The exhaustive model searches** (`cargo test --release -p hyper-check --test exhaustive --
--ignored --test-threads 1 --nocapture`, four workers, load 62–71):

| Search | Classes | Wall | User | Peak |
|---|---|---|---|---|
| slot model, ballots, 4, 1, 2, 4 (the default suite's) | 463,715 | 0.71 s with its suite | — | 45.6 MB counted |
| slot model, ballots, full scope: 5, 1, 2, 3; 4, 1, 3, 4; 5, 1, 2, 4; 3, 2, 2, 2 | 26,421,830 in all | 102.9 s | 167.7 s | 2.19 GB resident (2.85 GB counted) |
| prefix model, design, 3, 3, 1, 3, tail cleared | 21,771,580 | 52.7 s | 88.2 s | 2.30 GB resident (1.92 GB counted) |
| every ignored search (§15.1's tables) | — | 884.6 s | 800.8 s | 2.48 GB resident |

CI's `explore` job ran the ignored searches in 12.2 min with its build (run 37248774674, ubuntu-24.04,
four vCPUs). The 4 GiB ceiling of `docs/sim.md` §7 held every search.

# hyper-multilog

`docs/multilog.md` §11 steps 4 and 5. The machine is the one in "The machine and the runs" above (Apple
M5 Max, 18 cores, 128 GiB, macOS 26.4.1), shared with other sessions' builds and gates throughout.

## The multilog explorer

slates' explorer retargeted (`crates/hyper-multilog/tests/multilog.rs`, `docs/multilog.md` §11
step 4) on hyper-sim's free discipline: any delivery order, messages dropped and duplicated, the
network's capacity, partitions, crashes restarting from the last image, images and every log
compacted at canonical cuts, snapshots installed by laggards. Raft's invariants per log and the
merge's history across members and restarts are checked after every step; the first seed of each
shape runs through `twice`. Three voters with three logs and five with two, 3,000 steps a seed.

| Path | 3 × 3, 16 seeds | 5 × 2, 16 seeds | 3 × 3, 200 seeds | 5 × 2, 200 seeds | Floor |
|---|---|---|---|---|---|
| elections won | 531 | 255 | 6,665 | 3,167 | common |
| keyed commands applied | 5,918 | 7,687 | 72,649 | 95,938 | common |
| global commands applied | 824 | 1,046 | 8,751 | 14,892 | common |
| barriers proposed | 439 | 272 | 5,449 | 3,658 | common |
| members crashed and restarted | 751 | 735 | 9,343 | 9,177 | common |
| restarted members' replays matched | 6,170 | 8,079 | 73,771 | 103,325 | common |
| messages dropped | 4,175 | 5,096 | 55,400 | 62,515 | common |
| messages duplicated | 527 | 612 | 6,633 | 7,801 | common |
| proposals refused | 578 | 443 | 6,793 | 5,666 | common |
| images taken and every log compacted | 240 | 391 | 3,088 | 4,514 | common |
| images installed from a log's snapshot | 13 | 17 | 198 | 221 | rare |
| messages past the network's capacity | 0 | 0 | 0 | 483 | not claimed |

A floor is `docs/sim.md` §4.4's: a common path more than once a seed (above 16 and above 200), a
rare one at least once a campaign. Every path but installs ran more than once a seed at both scales
and both shapes; installs ran 0.8 to 1.1 times a seed, so they are held to the rare floor. The
network's capacity was reached only at full scale with five voters, so it is counted and not
claimed. A member that applies past a barrier without waiting is caught at seed 0, step 273
(`a_member_that_does_not_wait_at_barriers_is_caught`).

The full-scale run: release, 2026-10-04 10:29 PDT, 4.51 s, load average 2.25 at its start and 2.60
at its end; the 16-seed run is the workspace's debug run, the same minute (load 2.55–2.83).

```sh
cargo test -p hyper-multilog --test multilog
cargo test --release -p hyper-multilog --test multilog -- --ignored the_multi_log_merges_alike_at_full_scale --nocapture
```

## Allocations on the hot paths

`crates/hyper-multilog/tests/allocs.rs`, exact counts with `hyper_measure::alloc`, warm, at one
leader: the merge applies a command with no allocation and no reallocation; a proposal through the
layer allocates exactly what the core's proposal of the same bytes does (the layer reserves its
nine-byte suffix in the owner's buffer, `entry::SUFFIX_BYTES`); a barrier allocates exactly what
the core's proposal of its bytes does; handing committed entries over and screening a forwarded
message allocate nothing of the layer's own. Each is an equality the test asserts, not a measured
band.

## Against slates' MLRaft

`crates/hyper-raft-compare`, `multilog`: slates' layer at `5cce86a` and this one on slates'
workload (three voters, 64 keys, one command in eleven global, slates' timed streams' ratio; 64 B
commands), one log and three, a command a round and 64, each log led by a different voter. One
round: the commands proposed at their logs' leaders (this layer: one proposal a log,
`MultiLog::propose_in`), the group driven quiet in one thread, every member applying the round in
the merged order. Closed loop and in one process, with no network and no device: what a round
measures is the two layers' and their cores' own work. Five processes a layer and shape,
interleaved, 2,000 rounds each, pooled for the quantiles; the intervals are 95%, distribution-free
(`docs/tails.md` §3.3). CPU, instructions, cycles and energy are the process's own account over
the timed rounds (`hyper_measure::usage`, `proc_pid_rusage(RUSAGE_INFO_V6)`); allocations and wire
bytes from a separate counting process of the same seed, the wire bytes each layer's own encoding
of every message (hyper-raft's record, slates' `RaftMessage::encode`) and the log's number.
`slates-multilog+publication` copies slates' retained state out at every transition, as slates'
server does before any reply (the core's comparison above, "slates with its retained-state
publication"); it copies the whole log, so it runs a command a round only.

2026-10-04, 11:19 PDT, on a machine shared with other sessions' builds and gates: load average
95.7 at the first shape's start and 115.9 at the end (each row's start in its column). Round times
in milliseconds.

| workload | layer | load before | round p50 [95% interval] | p99 | p99.9 | max | CPU ns/command (user+sys) | instructions/command | cycles/command | energy nJ/command | wakeups/1k commands | allocs/command | reallocs/command | alloc bytes/command | messages/command | wire bytes/command | peak footprint MiB |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 1 logs b1 64B | hyper-multilog | 95.72 52.36 39.04 | 0.002 [0.002–0.002] | 0.004 [0.004–0.005] | 0.023 [0.018–0.035] | 0.144 | 1934 | 40707 | 7610 | 7330 | 0.00 | 25.00 | 0.000 | 4795 | 8.00 | 1027 | 3.1 |
| 1 logs b1 64B | slates-multilog | 95.72 52.36 39.04 | 0.002 [0.002–0.002] | 0.004 [0.004–0.005] | 0.021 [0.018–0.032] | 0.067 | 1736 | 37817 | 7445 | 4675 | 0.00 | 55.00 | 1.923 | 4278 | 6.00 | 852 | 3.1 |
| 1 logs b1 64B | slates-multilog+publication | 95.72 52.36 39.04 | 0.094 [0.093–0.096] | 8.918 [7.103–11.522] | 40.981 [34.857–67.482] | 77.382 | 102231 | 2347300 | 418822 | 467191 | 0.00 | 5077.50 | 1.923 | 768206 | 6.00 | 852 | 7.1 |
| 3 logs b1 64B | hyper-multilog | 102.91 55.31 40.25 | 0.002 [0.002–0.002] | 0.010 [0.009–0.010] | 0.034 [0.029–0.054] | 0.269 | 2952 | 54305 | 11865 | 13829 | 0.00 | 30.07 | 0.000 | 5776 | 9.81 | 1239 | 3.6 |
| 3 logs b1 64B | slates-multilog | 102.91 55.31 40.25 | 0.002 [0.002–0.002] | 0.006 [0.006–0.007] | 0.019 [0.016–0.026] | 0.275 | 2159 | 43384 | 8462 | 2165 | 0.00 | 61.24 | 2.127 | 4927 | 7.09 | 983 | 3.5 |
| 3 logs b1 64B | slates-multilog+publication | 102.91 55.31 40.25 | 0.153 [0.149–0.156] | 20.909 [17.721–27.206] | 59.315 [51.262–76.836] | 132.631 | 181706 | 3221787 | 721785 | 629961 | 0.00 | 7097.05 | 2.127 | 1006727 | 7.09 | 983 | 9.4 |
| 1 logs b64 64B | hyper-multilog | 113.29 60.75 42.55 | 0.016 [0.016–0.017] | 0.039 [0.037–0.043] | 0.225 [0.148–0.431] | 0.548 | 297 | 5551 | 1216 | 1052 | 0.00 | 9.25 | 0.000 | 1393 | 0.12 | 208 | 63.0 |
| 1 logs b64 64B | slates-multilog | 113.29 60.75 42.55 | 0.021 [0.021–0.021] | 0.047 [0.043–0.051] | 0.538 [0.286–9.083] | 17.549 | 367 | 7653 | 1495 | 1320 | 0.00 | 13.66 | 2.097 | 1411 | 0.09 | 181 | 66.4 |
| 3 logs b64 64B | hyper-multilog | 113.29 60.75 42.55 | 0.027 [0.027–0.027] | 0.096 [0.084–0.117] | 0.910 [0.550–23.228] | 50.698 | 476 | 7988 | 1886 | 1664 | 0.00 | 10.59 | 0.000 | 1670 | 0.69 | 270 | 80.8 |
| 3 logs b64 64B | slates-multilog | 113.29 60.75 42.55 | 0.026 [0.026–0.026] | 0.093 [0.080–0.112] | 2.980 [0.584–8.858] | 18.846 | 465 | 8776 | 1871 | 1606 | 0.00 | 14.52 | 2.268 | 1679 | 0.47 | 225 | 79.1 |

What it shows:
- **A command a round**, one log: even. p50 and p99 are 2 and 4 µs for both; this layer spends
  11% more CPU a command (1,934 ns against 1,736), 8% more instructions, and sends two more
  messages a command (8 against 6, not yet traced to which), with less than half slates'
  allocations (25 against 55, and no reallocation against 1.9).
- **A command a round, three logs: this layer loses.** p99 10 µs against 6, p99.9 34 against 19,
  37% more CPU a command (2,952 ns against 2,159), 25% more instructions, 9.81 messages against
  7.09 and 26% more wire bytes. Going from one log to three adds 1.81 messages a command here and
  1.09 in slates'. One difference in the barrier's path is a candidate: here every member proposes
  the barrier it owes and a follower's is forwarded to the leader, which drops it if covered
  (`docs/multilog.md` §3.1), where slates' leaders append their own and nothing else. It is not
  yet traced; the loss is open.
- **64 commands a round, one log: this layer wins.** p50 16 µs against 21, p99 39 against 47,
  p99.9 225 against 538, max 0.55 ms against 17.5; 19% less CPU, 27% fewer instructions, 32%
  fewer allocations and none reallocated. Before `propose_in` this layer proposed a command at a
  time and lost this row three to one (8 messages a command against slates' 0.09); one proposal a
  log is what slates' layer does, and what the owner contract now states (§9).
- **64 commands a round, three logs: even**, p50 27 against 26 µs and p99 96 against 93, inside
  each other's intervals; this layer's p99.9 interval reaches 23 ms, slates' 8.9 ms, on a machine
  at load 113, so neither tail is resolved against the other. CPU even (476 against 465 ns),
  instructions 9% fewer, allocations 27% fewer, but 0.69 messages a command against 0.47 and 20%
  more wire bytes, the same open loss as at one command a round.
- **slates as its server runs it** (its publication) is 50 to 60 times this layer's CPU a command
  and its p99 a thousand to two thousand times higher (8.9 and 20.9 ms against 4 and 10 µs): the
  copy of the whole retained log at every transition, which this layer's owner never makes (its
  log's storage is the durable state).
- Wakeups: none in either layer: one thread, no timer; an idle cost is the E2E's to measure.
- Peak footprint: equal at each shape (3 to 81 MiB, the 64-command rounds' logs, which neither
  compacts in this harness).

## Hostile networks

`tests/multilog_timed.rs`, `multi_log_under_hostile_networks`: slates' timed simulation (five Azure
regions at Microsoft's published P50 round trips, a keyed stream of 20 a second over 64 keys and a
global stream of two a second, elections by suspicion on per-pair heartbeat detectors, each log's
preferred voter ranked by its quorum round trip) on hyper-sim's ordered world, 70 s a run, 20 seeds,
under each condition on every path:
- **slates' paths**: as slates ran them, jitter 5 ms;
- **loss 0.1% and 1%**: ITU-T Y.1541's IP loss-ratio objective for its classes 0 to 4 is 1 × 10⁻³
  (Table 1), the edge of a network that meets it, and ten times past it; **10%**, a hundred times
  past it, where a link is failing;
- **jitter 50 ms**: Y.1541's delay-variation objective for classes 0 and 1 (IPDV 50 ms), as
  uniform jitter on each one-way delay, ten times slates';
- **duplication 1%**, hyper-sim's duplication of every message;
- **complete, partial and simplex partitions** of log 0's preferred voter for 20 s (30 s to 50 s),
  the three kinds of Alquraan et al. (OSDI 2018, §2.1): complete from every other voter; partial
  from the next-ranked voter only (the others reach both); simplex, what the others send it lost.

Every command's latency runs from its scheduled time (the streams are open loop: a command is due
whatever the group does) to its application at the member that proposed it. Quantiles pooled over
the seeds, in milliseconds, with 95% intervals; "unresolved" where the samples do not bound the
quantile from above (the global stream's 1,320 a seed). Messages and wire bytes are a command
applied's. 2026-10-04, 11:13 PDT, release, load average 14.6 at the start and 14.6 at the end.

| condition | logs | keyed p50 ms [95%] | keyed p99 | keyed p99.9 | keyed max | global p50 | global p99 | global p99.9 | global max | messages/command | wire bytes/command | unapplied/proposed | most steps |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| slates' paths | 1 | 123 [123–123] | 127 [127–127] | 127 [127–127] | 127 | 123 [123–123] | 127 [127–127] | 127 [127–unresolved] | 127 | 21.6 | 2423 | 40/26400 | 61840 |
| slates' paths | 3 | 172 [171–172] | 456 [455–456] | 461 [460–463] | 465 | 456 [456–456] | 464 [463–464] | 466 [465–unresolved] | 467 | 32.0 | 3548 | 53/26400 | 76329 |
| slates' paths | 5 | 176 [176–177] | 521 [520–522] | 530 [529–531] | 534 | 525 [524–525] | 532 [532–533] | 535 [534–unresolved] | 535 | 41.1 | 4548 | 135/26400 | 89020 |
| loss 0.1% | 1 | 123 [123–123] | 127 [127–127] | 169 [168–169] | 171 | 123 [123–123] | 127 [127–127] | 169 [165–unresolved] | 170 | 21.6 | 2423 | 40/26400 | 61743 |
| loss 0.1% | 3 | 172 [172–172] | 459 [458–460] | 840 [781–998] | 2040 | 456 [456–456] | 666 [588–802] | 1166 [890–unresolved] | 2090 | 32.0 | 3544 | 59/26400 | 76109 |
| loss 0.1% | 5 | 177 [177–177] | 534 [529–553] | 806 [778–846] | 1139 | 525 [525–525] | 825 [784–874] | 1091 [985–unresolved] | 1189 | 41.1 | 4548 | 138/26400 | 89034 |
| loss 1% | 1 | 123 [123–123] | 170 [170–170] | 228 [225–230] | 264 | 123 [123–123] | 170 [170–172] | 216 [189–unresolved] | 228 | 21.5 | 2429 | 43/26400 | 61172 |
| loss 1% | 3 | 174 [174–174] | 700 [676–721] | 1002 [972–1060] | 1288 | 457 [457–457] | 1008 [920–1098] | 1199 [1172–unresolved] | 1288 | 31.9 | 3546 | 107/26400 | 75505 |
| loss 1% | 5 | 196 [194–199] | 822 [812–836] | 1063 [1032–1107] | 1386 | 528 [527–528] | 1104 [1066–1148] | 1283 [1215–unresolved] | 1431 | 41.2 | 4570 | 156/26400 | 88320 |
| loss 10% | 1 | 204 [201–206] | 1648 [1591–1718] | 2767 [2648–2858] | 3221 | 206 [196–216] | 1691 [1448–1945] | 2857 [2448–unresolved] | 3121 | 16.8 | 2037 | 118/26400 | 53413 |
| loss 10% | 3 | 368 [362–375] | 2541 [2484–2619] | 3626 [3564–3898] | 4858 | 716 [699–740] | 2781 [2691–3164] | 4164 [3698–unresolved] | 5008 | 28.7 | 3294 | 279/26400 | 66579 |
| loss 10% | 5 | 616 [609–623] | 3068 [3013–3131] | 3900 [3833–4066] | 5118 | 993 [976–1018] | 3554 [3241–3709] | 4557 [4092–unresolved] | 5168 | 39.1 | 4457 | 710/26400 | 78478 |
| jitter 50 ms | 1 | 166 [166–167] | 205 [204–205] | 212 [211–212] | 216 | 170 [169–170] | 206 [205–208] | 212 [211–unresolved] | 215 | 20.0 | 2264 | 60/26400 | 59465 |
| jitter 50 ms | 3 | 230 [229–230] | 592 [589–596] | 656 [648–684] | 2505 | 585 [583–586] | 685 [674–701] | 770 [737–unresolved] | 798 | 28.4 | 3186 | 141/26400 | 70921 |
| jitter 50 ms | 5 | 240 [239–241] | 643 [637–649] | 705 [698–716] | 847 | 654 [652–656] | 752 [741–782] | 842 [832–unresolved] | 851 | 36.2 | 4043 | 136/26400 | 81585 |
| duplication 1% | 1 | 123 [123–123] | 127 [127–127] | 127 [127–127] | 127 | 123 [123–123] | 127 [127–127] | 127 [127–unresolved] | 127 | 21.8 | 2441 | 40/26400 | 62521 |
| duplication 1% | 3 | 172 [171–172] | 455 [455–456] | 461 [460–462] | 466 | 456 [455–456] | 464 [463–464] | 466 [465–unresolved] | 467 | 32.3 | 3578 | 53/26400 | 77393 |
| duplication 1% | 5 | 176 [176–177] | 521 [519–522] | 529 [528–530] | 534 | 525 [525–525] | 532 [532–533] | 535 [534–unresolved] | 536 | 41.5 | 4584 | 134/26400 | 90164 |
| complete partition | 1 | 173 [173–173] | 674 [587–742] | 20257 [20197–20301] | 20455 | 173 [173–174] | 424 [278–612] | 1425 [925–unresolved] | 1939 | 18.8 | 2127 | 122/26400 | 54655 |
| complete partition | 3 | 192 [192–193] | 667 [666–670] | 1422 [1305–1598] | 20384 | 458 [458–459] | 1008 [765–1229] | 1769 [1322–unresolved] | 1822 | 29.4 | 3284 | 82/26400 | 69075 |
| complete partition | 5 | 195 [194–199] | 771 [746–790] | 20704 [20512–20799] | 21228 | 527 [527–528] | 2876 [1596–20754] | 21206 [21141–unresolved] | 21239 | 38.0 | 4238 | 169/26400 | 80862 |
| partial partition | 1 | 123 [123–123] | 127 [127–127] | 127 [127–127] | 127 | 123 [123–123] | 127 [127–127] | 127 [127–unresolved] | 127 | 20.1 | 2291 | 40/26400 | 58735 |
| partial partition | 3 | 174 [174–174] | 524 [520–525] | 827 [781–922] | 20102 | 458 [458–458] | 824 [718–925] | 1000 [974–unresolved] | 1478 | 31.2 | 3468 | 53/26400 | 74217 |
| partial partition | 5 | 177 [177–177] | 525 [524–526] | 744 [699–788] | 20203 | 525 [525–525] | 812 [696–857] | 974 [944–unresolved] | 1008 | 40.5 | 4486 | 138/26400 | 87070 |
| simplex partition | 1 | 174 [174–174] | 424 [337–489] | 20380 [20355–20410] | 20516 | 174 [174–174] | 505 [272–20286] | 20380 [20310–unresolved] | 20416 | 18.8 | 2125 | 60/26400 | 56340 |
| simplex partition | 3 | 191 [190–192] | 670 [667–673] | 1484 [979–20301] | 20495 | 458 [458–459] | 947 [779–20367] | 20421 [20403–unresolved] | 20458 | 29.5 | 3287 | 53/26400 | 70719 |
| simplex partition | 5 | 195 [193–197] | 723 [716–725] | 20608 [20479–20661] | 20767 | 527 [527–528] | 20498 [20473–20711] | 20771 [20767–unresolved] | 20809 | 38.4 | 4276 | 138/26400 | 82527 |

What it shows:
- **Quiet paths, loss to 1%, duplication: the layer costs a tail.** With one log a keyed command's
  p99 is one region's round (127 ms); with three and five logs it is the global it waits behind
  (456 and 521 ms), the barrier wait slates measured. Loss and duplication add only their resends.
- **Loss 10%**: every shape's p99 passes 1.6 s, and more logs are worse at every quantile (keyed
  p99 1.65, 2.64 and 3.07 s at one, three and five logs): a keyed command outside log 0 waits for a
  barrier whose append, and whose global's, may each be lost.
- **Jitter 50 ms**: the same shape, each tail by the jitter.
- **Complete and simplex partitions of a log's preferred leader**: until its detectors suspect it,
  a cut leader's log takes commands it cannot commit; those wait the partition out (the 20 s
  maxima, every shape including one log). This is the core's, not the layer's: one log shows it as
  the others do.
- **Partial partition: the layer's own failure, found here and fixed.** Before the rule of
  `docs/multilog.md` §7.1, three and five logs had a keyed p99 of 18.8 and 18.2 s: the member cut
  from log 0's leader led log 1, its merge stopped at log 0's next global, and log 1's commands
  waited at it for the whole partition. With the rule (a member cut from a lower log's leader yields
  what it leads and is not handed it back), keyed p99 is 524 and 525 ms, the quiet paths' 456 and
  521 plus the hand-over; one log never had the failure (127 ms). A command proposed at the
  yielding leader before it yielded still waits for that member (the 20.1 s maximum).
- **Steps**: the most any run took, 90,164, sets `HOSTILE_STEPS` at four times it.

```sh
HYPER_MULTILOG_SEEDS=20 cargo test -p hyper-multilog --release --test multilog_timed -- --ignored --exact multi_log_under_hostile_networks --nocapture
```

## End to end

`crates/hyper-multilog-e2e`: each member an OS process holding every log of the group, each log a
`hyper_raft` member on its own fsynced file (`hyper_raft_e2e::wal`, the platform's full flush), Raft
datagrams over UDP tagged with their log and stamped (§7.1), one node-pair liveness stream a pair
shared by every log, elections by suspicion. The test is the core's harness
(`hyper_raft_e2e::quiet`'s waits on facts), its client routing each key to its log and following
each log's leader. Writes alternate keyed and global (`*` keys), so every keyed write outside log
0 waits at a barrier and every global waits for every other log's barrier: the workload that takes
the merge's whole path, not one that would show a gain. The client is closed loop (a write, then
its read, then the next: `docs/tails.md` §3.1). The faults its harness injects are the core's: a
member killed with `SIGKILL` and restarted on its logs, and a member cut off by a drop filter in
its process; loss, delay, jitter and duplication on real sockets wait for the impairment relay
(`docs/tails.md` §4.2, T-2) and are measured above on hyper-sim's network.

Release, 2026-10-04: the four scenarios from 11:15 PDT (load average 17.7 at the start); the tails
scenarios again from 11:25 (load 130.1 at the start, 67.0 at the end), after a fix to the accounts'
sum. A phase is 136 writes on this datagram (9,216 bytes; 129 for the longer scenario names).
Milliseconds, with 95% intervals.

| Scenario | What a client saw | Writes p50 / p99 / p99.9 / max | Reads p50 / p99 / p99.9 / max | Merged alike |
|---|---|---|---|---|
| `commits-1` (1 log) | 136 writes (68 global) answered and read back | 18.4 / 35.0 / unresolved / 38.1 | 0.20 / 18.2 / unresolved / 26.7 | 137 |
| `commits-3` (3 logs) | 136 writes (68 global) answered and read back | 50.3 / 94.9 / unresolved / 125.2 | 0.22 / 16.1 / unresolved / 21.6 | 276 |
| `member-killed` (3 logs) | log 0's leader killed after 129 writes, member 2 elected in term 5, 129 more without it; restarted on its logs, its restart reported; 258 writes read back | 35.6 / 91.2 / unresolved / 1,091 | 0.12 / 11.9 / unresolved / 16.2 | 520 |
| `partition` (3 logs) | log 0's leader cut off answered a read, a write and a later read each with `NotLeader(0)`; member 3 elected; 273 read back after the filter lifted | 40.8 / 98.1 / unresolved / 106.0 | 0.26 / 20.1 / unresolved / 55.3 | 553 |
| `tails-1` (1 log), 3,688 writes | 1,844 global | 23.2 [23.0–23.5] / 143 [113–171] / 806 [307–2,654] / 2,654 | 4.37 [3.79–4.53] / 22.9 [21.2–24.4] / 87.6 [46.1–398] / 398 | 3,691 |
| `tails-3` (3 logs), 3,688 writes | 1,844 global | 62.5 [56.8–65.4] / 409 [336–492] / 1,189 [828–1,860] / 1,860 | 0.57 [0.43–0.89] / 36.7 [25.4–82.5] / 279 [181–372] / 372 | 7,382 |

3,688 writes is the least for which a p99.9's 95% interval closes above (`0.999ⁿ ≤ 0.025`); the
smaller scenarios leave it unresolved. The members' accounts over the tails scenarios
(`proc_pid_rusage(RUSAGE_INFO_V6)` of each member process, summed), and over 10 s idle after:

| Per write, three members | 1 log | 3 logs |
|---|---|---|
| CPU (user + system) | 1,150 µs (223 + 926) | 2,780 µs (414 + 2,366) |
| instructions | 4.12 M | 6.61 M |
| cycles | 4.83 M | 11.66 M |
| energy | 2.31 mJ | 4.86 mJ |
| device bytes written per byte stored | 3,951 | 7,665 |
| peak footprint, a member | 2.7 MiB | 3.4 MiB |
| **Idle, per member** | | |
| wakeups a second | 2.6 | 2.5 |
| CPU a second | 0.369 ms | 0.415 ms |
| energy a second | 0.628 mJ | 0.742 mJ |
| footprint | 2.6 MiB | 3.2 MiB |

What it shows, plainly:
- **Three logs lose to one at every quantile of a write**: p50 62.5 against 23.2 ms, p99 409
  against 143, p99.9 1,189 against 806. A global write waits for both other logs' barriers, each
  its own fsynced append and round, and a keyed write outside log 0 waits for the global before it.
  On this workload the layer adds a barrier per global per log and the waits on them.
- **Reads are not slower at the median** (0.57 against 4.37 ms, not traced) but are at the tail
  (p99 36.7 against 22.9, p99.9 279 against 88).
- **Efficiency: three logs cost 2.4 times the CPU, 2.4 times the cycles, 2.1 times the energy and
  1.9 times the device bytes of one log per write**, for no gain in latency: two more fsynced logs
  a member and a barrier for every global in each. The device bytes per byte stored (3,951 and
  7,665) are each write's records rounded to the device's pages and flushed once per write (a write
  stores some 20 bytes of key and value), times three members.
- **Idle**: a member wakes 2.5 times a second and spends 0.4 ms of CPU and 0.6 to 0.7 mJ a second
  whatever its logs: its heartbeats are a node pair's, shared by every log (hyper-liveness), not a
  log's. A log adds about 0.3 MiB of footprint.
- **A defect the gate found under load** (load 79, `member-killed`): a member kept a write waiting
  that no apply would answer (its report's `stray`). A client asked the write of a new leader that
  already held the old leader's entry for it; the member waited for that entry but answered only
  commands it had proposed itself. A waited write is now answered when its key's value is applied
  in its log, whoever proposed it; the core's member never had the restriction. The scenarios above
  ran before the fix: it changes which member answers, not what is applied.
- No consumer should enable multilog on this evidence: it gains nothing here and costs about twice
  the resources. Its case is a workload whose logs are each saturated alone, which none of these
  is; slates measured the same for its own groups.

```sh
cargo test --release -p hyper-multilog-e2e --test cluster
cargo test --release -p hyper-multilog-e2e --test cluster -- tails
cd crates/hyper-raft-compare && cargo run --release -- multilog 5 2000
```

## hyper-quic at 500 ms one way: what the stack adds above the round trips (2026-10-04)

The owner's goal is to cut what hyper-quic adds above the physical floor at 500 ms one way by 10x,
giving up no correctness. Overhead here is a time less the floor of round trips the protocol
needs. A fresh dial's handshake needs 1 round trip; its first reply needs 2, because the request
goes with the client's Finished. A large certificate meets the amplification limit (RFC 9000 §8.1),
which adds 1 round trip to each. A resumed dial with its request in 0-RTT data needs 1 for both.
A request on a kept connection needs 1.

Machine: Apple M5 Max, 18 cores, 128 GiB, macOS 26.4.1. Other sessions were building and testing
throughout. The load average is recorded beside each run.

### On the simulated network (exact, virtual time)

`crates/hyper-quic/tests/geo.rs`: real endpoints and TLS 1.3 with X25519MLKEM768, 500 ms each way.
The large certificate has 1,000 incompressible names, so the server's flight exceeds 3x the
two-datagram ClientHello.

Clean path. The numbers are the same before (df54729) and after (5425611):

| case | handshake | first reply | kept requests |
|---|---|---|---|
| fresh, small certificate | 1,000 ms | 2,000 ms | 1,000 ms each |
| resumed, 0-RTT request | 1,000 ms | 1,000 ms | 1,000 ms each |
| fresh, large certificate | 1,999 ms | 2,999 ms | 1,000 ms each |
| resumed, large certificate, 0-RTT request (address token) | 1,000 ms | 1,000 ms | 1,000 ms each |

Every case is at its floor. The resumed dial skips the amplification round trip because the
server's NEW_TOKEN validates the client's address (RFC 9000 §8.1.3). With 0-RTT its first reply
comes at the physical floor of one round trip.

5% loss each way with ±100 ms reordering: 32 seeds, large certificate, a fresh dial and then a
resumed dial with its request in 0-RTT data. Each cell is the overhead above the floor:

| | before (df54729) | after (5425611) |
|---|---|---|
| fresh handshake, median / p90 / max | 1,059 / 2,094 / 3,026 ms | **134** / 1,890 / 2,722 ms |
| fresh first reply, median / p90 / max | 1,011 / 2,960 / 7,490 ms | 875 / 4,650 / 6,073 ms |
| fresh dials that never completed | **3 of 32** (idle timeout) | **0** |
| resumed handshake, median / p90 / max | 120 / 2,858 / 6,720 ms | 65 / 2,040 / 2,911 ms |
| resumed first reply, median / p90 / max | 120 / 7,087 / 8,706 ms | 65 / 3,899 / 4,790 ms |

- **The fresh handshake's median overhead fell from 1,059 to 134 ms (7.9x), and no dial now
  stalls to its idle timeout.** The 134 ms left is within the path's own jitter, up to 200 ms a
  round trip over two round trips. With reordering and no loss the handshake completes within two
  of the slowest round trips (`reordering_alone_costs_no_round_trip`), where it took about 2.9 s
  before.
- **The fresh first reply's p90 rose (2,960 to 4,650 ms) because the three dials that never
  completed before are now counted**, and these are the slowest that do complete. Its remaining
  overhead is tail-loss recovery: a lost request or reply waits for a probe timeout,
  srtt + 4·rttvar + max_ack_delay (RFC 9002 §6.2.1), about 2 to 3.4 s this early in a connection,
  since rttvar starts at half the first sample (§5.3).
- **Tried and not kept:** seeding a re-dial's RTT estimator with the previous connection's final
  smoothed RTT and variation (RFC 9002 §6.2.2; RFC 9040 §5's temporal sharing). The resumed first
  reply's p90 went from 3,899 to 4,146 ms. A first-flight probe then waits about 1.3 s instead of
  999 ms, and that offsets the shorter later probes, so the measurement did not justify it.

### On real sockets: the open loop

`crates/hyper-quic/examples/geo_open_loop.rs`. The client and server are threads on loopback, and
a relay holds each datagram 500 ms each way. There are five dials, the first fresh and the later
ones resumed with a 0-RTT request. Then 20,000 requests go open loop at 100 a second on the kept
connection. Each latency runs from the request's scheduled time, and its overhead is the latency
less 1,000 ms. The relay's lateness past each datagram's due time is reported beside it. Quantile
intervals are 95% distribution-free (`docs/tails.md` §3.3). The before (df54729) and after
(5425611 and d05c195, the tree measured) runs were interleaved:

| run | load (before → after) | open loop overhead p50 / p99 / p99.9 / max | relay lateness p99 up / down |
|---|---|---|---|
| before 1 | 68.9 → 62.3 | 2.067 / 4.564 / 195.2 [165.2, 242.6] / 292.6 ms | 1.95 / 1.82 ms |
| after 1 | 62.3 → 58.4 | 2.070 / 4.544 / 191.8 [151.7, 232.7] / 291.7 ms | 1.95 / 1.81 ms |
| before 2 | 58.4 → 64.1 | 1.151 / 4.111 / 194.3 [165.7, 234.3] / 293.2 ms | 1.09 / 0.99 ms |
| after 2 | 64.1 → 58.6 | 1.124 / 2.145 / 190.9 [150.9, 231.1] / 290.8 ms | 0.96 / 0.83 ms |

Dials (overhead above floor): fresh 2.6 to 6.2 ms; resumed with 0-RTT accepted 0.6 to 4.4 ms. The
fourth dial in every run fell back to 1-RTT, 0.9 to 3.6 ms over its 2-round-trip floor.

- **The steady state costs 1 to 2 ms above the round trip at p50**, at load 58 to 69. The same
  bench with no delay (`--one-way-ms 0`) gives a p50 of 2.07 ms at load 83, so this cost does not
  depend on distance. It is the host's: four threads woken in turn on a machine with about four
  runnable threads a core, two of them the relay's.
- **The p99.9 and the maximum, about 190 and 290 ms, are the open loop's first second, the same
  before and after.** The worst latency in the run's first tenth is 291 to 293 ms, and 2.5 to 26 ms
  in every later tenth. At 100 requests a second the client offers about 15 kB a round trip, above
  QUIC's initial window of 12,000 bytes (RFC 9002 §7.2, 10 × 1,200). The requests past the window
  wait for the first acknowledgements, a round trip later. At 50 a second (7.5 kB a round trip) the
  worst is 6.4 ms. This is congestion control's start, a rule of the network's safety and not a
  defect. It is the largest overhead left and it is open: an RTT-and-window carry-over for kept and
  resumed connections, such as draft-ietf-tsvwg-careful-resume, needs a research note first.
- **Why the fourth dial lost 0-RTT:** dials 2 and 3 close at their first reply, one round trip in,
  before their new session tickets arrive, half a round trip after the client's Finished
  (RFC 8446 §4.6.1). The client then used up the two tickets dial 1 left. This comes from the
  harness's dial pattern, not the stack, and it is recorded.

So, under the owner's condition with loss and reordering, the stack's handshake overhead fell by
7.9x at the median and its stalls are gone. On a clean path every protocol case was already at its
floor of round trips. What is left above the floor is the host's 1 to 2 ms, tail-loss probe
timeouts under loss, and congestion control's initial window.

```sh
# The simulated tables (the large certificate takes a few seconds per seed).
cargo test -p hyper-quic --test geo -- --ignored --nocapture print_the_overhead_table
cargo test -p hyper-quic --test geo -- --ignored --nocapture print_the_lossy_overhead_table
# Before: the same tests with the source fixes of 5425611 reverse-applied.
# Real sockets, each run preceded and followed by `uptime`, before and after interleaved.
cargo build --release -p hyper-quic --example geo_open_loop
target/release/examples/geo_open_loop --rate 100 --requests 20000 --dials 5
target/release/examples/geo_open_loop --rate 50 --requests 1500 --dials 1
target/release/examples/geo_open_loop --rate 100 --requests 3000 --dials 1 --one-way-ms 0
```

## hyper-quic at 500 ms one way: probe timeouts, tickets and Careful Resume (2026-10-04)

The second round toward the owner's tenfold cut in what the stack adds above the physical floor of
round trips at 500 ms one way (the round before: the section above). Before is 14aaea1; after is
f530fe7 (the timing runs) and its follow-up, which removes one allocation and changes no time.
Research: `docs/research/quic-overhead.md`. Changes: `crates/hyper-quic/VENDORED.md` §11 and §12,
`crates/hyper-tls/VENDORED.md` §7, `docs/transport.md` §4f.

Machine: Apple M5 Max, 18 cores, 128 GiB, macOS 26.4.1. Other sessions built and tested throughout
(workspace test runs, a release hyper-raft strategy search); the load average is beside each run.

### On the simulated network (exact, virtual time)

`crates/hyper-quic/tests/geo.rs`, the round before's harness and schedule: 500 ms each way, then 5%
loss each way with ±100 ms of reordering over the 32 seeds 1 to 32, the large certificate, a fresh
dial and then a resumed dial with its request in 0-RTT data.

Clean path: every case is at its floor before and after, as in the table above (fresh 1,000 /
2,000 ms, resumed with 0-RTT 1,000 / 1,000 ms, large certificate 1,999 / 2,999 ms, kept requests
1,000 ms each).

Lossy, overhead above the floor. With 32 samples the p99 and p99.9 are the maximum and unresolved
(`docs/tails.md` §3.3); the median, p90 and maximum are given.

| | before (14aaea1) | after (f530fe7) |
|---|---|---|
| fresh handshake, median / p90 / max | 134 / 1,890 / 2,722 ms | 134 / 1,783 / 2,190 ms |
| fresh first reply, median / p90 / max | 875 / 4,650 / 6,073 ms | 975 / 1,899 / 2,319 ms |
| resumed handshake, median / p90 / max | 65 / 2,040 / 2,911 ms | 54 / 1,177 / 1,913 ms |
| resumed first reply, median / p90 / max | 65 / 3,899 / 4,790 ms | 54 / 1,177 / 3,890 ms |
| dials that never completed | 0 of 64 | 0 of 64 |
| datagrams / bytes sent, all 32 seeds | 1,384 / 969,439 | 1,400 / 974,961 |

- **The fresh first reply's p90 fell 2.4x (4,650 to 1,899 ms) and its maximum 2.6x; the resumed
  first reply's p90 fell 3.3x (3,899 to 1,177 ms).** Two changes carry it: a probe now carries the
  lost reply or request itself rather than only control frames, saving the round trip in which the
  probe's acknowledgement used to reveal the loss; and the probe waits two smoothed RTTs at the first
  sample rather than three. A third, a Handshake probe that also carries the Data space once an RTT
  sample exists, resends a lost Finished and the request coalesced with it together.
- **The fresh first reply's median moved from 875 to 975 ms, and it is not a regression of the
  median's kind.** Sorted, the 32 values after are 0 (seven seeds), 31, 76, 114, 118, 305, 312, 354,
  875, 975, 1,024, … 2,319: fourteen seeds lose nothing on the reply's path and eighteen lose at
  least one packet there, and the median falls in the gap between the groups. A loss on the critical
  path costs at least a round trip at any stack: a lost packet is known lost only when a later one
  is acknowledged, a round trip after it was sent, and its copy then takes the one-way delay. Values
  from 875 to about 1,300 ms fit one such recovery with up to 200 ms of jitter a round trip; those
  near 2,000 ms fit two.
- **Cost:** 16 more datagrams and 0.6% more bytes over the 32 seeds, the probes' copies.
- **Ablations** (on the tree before the 0-RTT ACK fix, item 5 of VENDORED.md §11): without the
  Data-space probe the fresh first reply's maximum was 3,090 ms against 2,242 ms with it; probing
  the Data space before any RTT sample gave a resumed p90 of 961 ms against 1,177 ms, but on real
  sockets it raised the open loop's worst from 291–294 ms to 343–354 ms in single 1,500-request
  runs (the server's 999 ms first PTO fires against the 1 s round trip and repeats 0.5-RTT data
  that is not lost), so the Data space waits for a sample.

Careful Resume on the clean path (`print_the_resume_table`): a fresh dial asks for a reply of the
given size, then a resumed dial with its request in 0-RTT asks for the same. The simulated path has no
capacity limit, so the floor is one round trip and the rest is slow start:

| reply | resumed first reply, without | with Careful Resume | saved |
|---|---|---|---|
| 512 KiB | 6,631 ms | 5,886 ms | 745 ms |
| 1 MiB | 7,672 ms | 5,922 ms | 1,750 ms |
| 2 MiB | 8,722 ms | 6,038 ms | 2,684 ms |

The fresh dials are the same with it and without it (7,627, 8,685 and 9,741 ms). Under the lossy
condition, a first dial of a mebibyte on the clean path and a resumed one on the lossy path: 7 of
the 32 seeds jump and all 7 retreat at their first loss (RFC 9959 §3.5); every transfer completes.

### On real sockets: the open loop

`crates/hyper-quic/examples/geo_open_loop.rs`, the round before's bench: client, server and the
relay's two directions as threads on loopback, 500 ms each way, five dials (the first fresh, the
rest resumed with a 0-RTT request), then 20,000 requests open loop at 100 a second on the kept
connection, latencies from each request's scheduled time less 1,000 ms; quantile intervals 95%
distribution-free. The relay's own lateness is beside each run. Runs interleaved, 2026-10-04
20:20–20:48 PDT; before is 14aaea1's binary, after is f530fe7's.

The round before's condition (no warm-up):

| run | load (before → after the run) | dials falling back to 1-RTT | p50 / p99 / p99.9 / max, overhead | relay lateness p99.9 up / down |
|---|---|---|---|---|
| before 1 | 16.5 → 10.3 | dial 4 | 1.216 / 4.425 / 191.1 [151.1, 231.5] / 291.0 ms | 3.47 / 3.17 ms |
| after 1 | 10.3 → 6.3 | none | 1.174 / 2.376 / 152.3 [132.6, 181.9] / 241.1 ms | 2.69 / 1.04 ms |
| before 2 | 6.3 → 10.9 | dial 4 | 1.213 / 11.00 / 192.1 [152.0, 232.4] / 292.0 ms | 7.06 / 7.24 ms |
| after 2 | 10.9 → 7.2 | none | 1.202 / 3.223 / 152.6 [133.2, 181.5] / 241.1 ms | 1.79 / 1.97 ms |

- **0-RTT on every resumed dial.** Dials 2 to 5 all had their 0-RTT data accepted, 0.5 to 1.9 ms
  over their one-round-trip floor; before, dial 4 fell back to 1-RTT in every run (its first reply
  2,001.5 to 2,001.7 ms against a floor of 1,000) because dials 2 and 3 closed before their tickets
  arrived. The tickets now come with the server's Finished (`crates/hyper-tls/VENDORED.md` §7).
- **The start of the burst stalls 241 ms where it stalled 291 ms**, every run: p99.9 152 ms against
  191 ms. The 50 ms is the 0-RTT ACK fix (VENDORED.md §11 item 5): the server's first flight now
  acknowledges the client's 0-RTT request, so it leaves the client's flight a round trip sooner.
  What is left is the initial window: this kept connection has never measured its path (each dial
  before it carried one request), so it starts at 12,000 bytes (RFC 9002 §7.2) and the requests
  past 15 kB a round trip wait for the first acknowledgements. Every later tenth of each run is
  under 11 ms.

A path an earlier connection measured: the first dial first carries 4,000 requests at 400 a second
(about 60 kB a round trip, past RFC 9959 §3.1's four initial windows), then the four resumed dials,
then the open loop at 100 a second on dial 5. The same binary with Careful Resume off and on:

| run | load | p50 / p99 / p99.9 / max, overhead | worst in the first tenth | relay lateness p99.9 up / down |
|---|---|---|---|---|
| off 1 | 7.2 → 7.4 | 1.198 / 3.554 / 151.8 [132.0, 181.5] / 241.1 ms | 241.1 ms | 2.28 / 2.48 ms |
| on 1 | 7.4 → 7.3 | 1.235 / 2.159 / **8.93** [7.43, 11.1] / **20.5** ms | 12.8 ms | 2.02 / 1.75 ms |
| off 2 | 7.3 → 22.5 | 1.239 / 2.729 / 152.6 [133.0, 182.1] / 241.7 ms | 241.7 ms | 1.08 / 1.12 ms |
| on 2 | 22.5 → 17.4 | 1.244 / 3.106 / **24.7** [21.7, 31.7] / **64.1** ms | 1.8 ms | 4.62 / 5.10 ms |

- **With Careful Resume the burst's start no longer stalls: the first tenth's worst is 12.8 and
  1.8 ms against 241 ms, and p99.9 falls from 152 ms to 8.9 and 24.7 ms (6x to 17x).** Once dial 5's
  initial data is acknowledged its window jumps to half the 400-a-second measurement, about 30 kB,
  past the 15 kB a round trip the open loop asks. The run's worst moves to mid-run (64 ms in its
  seventh tenth in "on 2", the load average then 17 to 22), the host's, as the 1 to 2 ms p50 is.
- The warm-up itself, slow start from 12,000 bytes to 60 kB a round trip, is the same either way
  (p99 1,473 ms over the round trip): a fresh connection to an unmeasured path is not changed.

### Overhead above the floor, before and after

| condition | before | after | cut |
|---|---|---|---|
| simulated, lossy, fresh first reply, p90 / max | 4,650 / 6,073 ms | 1,899 / 2,319 ms | 2.4x / 2.6x |
| simulated, lossy, resumed first reply, p90 / max | 3,899 / 4,790 ms | 1,177 / 3,890 ms | 3.3x / 1.2x |
| simulated, lossy, resumed first reply, median | 65 ms | 54 ms | 1.2x |
| simulated, lossy, fresh first reply, median | 875 ms | 975 ms | (in the gap between seeds that lose and those that do not) |
| real sockets, fourth dial (resumed) first reply | 1,001.5–1,001.7 ms (fell back to 1-RTT) | 1.0–1.1 ms | ~1,000x |
| real sockets, kept connection's burst, p99.9 / worst (path never measured) | 191–192 / 291–292 ms | 152–153 / 241 ms | 1.25x / 1.2x |
| real sockets, resumed connection's burst after a measured connection, p99.9 / worst | 152–153 / 241–242 ms (Careful Resume off) | 8.9–24.7 / 20.5–64.1 ms | 6–17x / 4–12x |
| simulated, clean, 1 MiB resumed transfer | 7,672 ms | 5,922 ms | 1.3x (1,750 ms) |

```sh
# Simulated (deterministic): the overhead, lossy and Careful Resume tables
cargo test -p hyper-quic --test geo -- --ignored --nocapture --exact print_the_overhead_table
cargo test -p hyper-quic --test geo -- --ignored --nocapture --exact print_the_lossy_overhead_table
cargo test -p hyper-quic --test geo -- --ignored --nocapture --exact print_the_resume_table
# Real sockets, each run between two `uptime`s, before (14aaea1) and after interleaved
cargo build --release -p hyper-quic --example geo_open_loop
target/release/examples/geo_open_loop --rate 100 --requests 20000 --dials 5
target/release/examples/geo_open_loop --rate 100 --requests 20000 --dials 5 --warm-rate 400 --warm-requests 4000 [--no-careful-resume]
```

What is left of the 10x: the lossy medians sit at the cost of one recovered loss, a round trip,
which no stack avoids; the lossy maxima are two or three losses on the critical path, each still a
probe timeout of two smoothed RTTs early in a connection where the variation is still that of the
first sample. A connection to a path with no earlier measurement starts at the initial window,
RFC 9002's safety rule, so a kept connection whose earlier traffic never filled 15 kB a round trip
still waits about 240 ms at the start of such a burst. The steady state's 1 to 2 ms is the host's.

## hyper-quic at 500 ms one way: the handshake's flights twice (2026-10-05)

The third round toward the owner's tenfold cut in what the stack adds above the physical floor of
round trips at 500 ms one way (the rounds before: the two sections above). Before is `line` at
39d8195 (hyper-quic as at 4c4a199); after is that tree with `crates/hyper-quic/VENDORED.md` §13.
Research: `docs/research/quic-overhead.md` §4. Both are measured with the geo harness's certificate
made the same every run (§13's last paragraph), so before and after see the same flights.

Machine: Apple M5 Max, 18 cores, 128 GiB, macOS 26.4.1. Other sessions built and tested throughout;
the load average is beside each run.

### On the simulated network (exact, virtual time)

`crates/hyper-quic/tests/geo.rs`, the rounds before's schedule: 500 ms each way; then 5% loss each
way with ±100 ms of reordering over the seeds 1 to 32, the large certificate, a fresh dial and then
a resumed dial with its request in 0-RTT. The clean path is at its floor in every case before and
after (fresh 1,000 / 2,000 ms, resumed with 0-RTT 1,000 / 1,000 ms, large certificate 1,999 /
2,999 ms, kept requests 1,000 ms each).

Lossy, overhead above the floor (32 samples: the p99 and p99.9 are the maximum, unresolved):

| | round one's before (df54729) | before (39d8195) | after |
|---|---|---|---|
| fresh handshake, median / p90 / max | 1,059 / 2,094 / 3,026 ms | 134 / 1,783 / 2,190 ms | **0 / 116 / 994 ms** |
| fresh first reply, median / p90 / max | 1,011 / 2,960 / 7,490 ms (3 dials never completed) | 975 / 1,899 / 2,319 ms | **0 / 180 / 2,117 ms** |
| resumed handshake, median / p90 / max | 120 / 2,858 / 6,720 ms | 54 / 1,177 / 1,913 ms | **32 / 124 / 142 ms** |
| resumed first reply, median / p90 / max | 120 / 7,087 / 8,706 ms | 54 / 1,177 / 3,890 ms | **37 / 124 / 142 ms** |
| datagrams / bytes sent, all 32 seeds | | 1,400 / 970,737 | 2,453 / 1,683,483 |

- **The fresh first reply's p90 fell from 1,899 to 180 ms (10.5x), and from round two's starting
  point, 4,650 ms, 26x; the resumed first reply's p90 from 1,177 to 124 ms (9.5x), 31x from 3,899
  ms, and its maximum from 3,890 to 142 ms (27x).** A lost packet of the handshake's flights now
  has its copy in flight beside it; what remains above the floor at p90 is the path's own jitter,
  up to 200 ms a round trip. The fresh maximum, 2,117 ms, is a seed that lost an original and its
  copy.
- **Cost: 73% more bytes and 75% more datagrams on this condition**, where the large certificate's
  flight is copied too; the clean small-certificate dial sends its 2,400-byte ClientHello twice.
- **Measured and not taken** (`docs/research/quic-overhead.md` §4.1): copying every later flight
  once the connection has declared a loss (fresh p90 107 ms, resumed 112, maximum 131 ms; it doubles
  every small message on a lossy connection for good); seeding the RTT variation from the previous
  connection (resumed p90 2,100 ms without copies, against 1,177).

Careful Resume on the clean path (`print_the_resume_table`), the resumed first reply:

| reply | without, before → after | with Careful Resume, before → after |
|---|---|---|
| 512 KiB | 6,631 → 6,558 ms | 5,886 → 5,784 ms |
| 1 MiB | 7,672 → 7,599 ms | 5,922 → 5,832 ms |
| 2 MiB | 8,722 → 8,648 ms | 6,038 → 5,945 ms |

The fresh dials fell likewise (7,627 → 7,506, 8,685 → 8,542, 9,741 → 9,607 ms): slow start now
grows by every acknowledgement of a round trip in which its window was used (§13 item 4).

### On real sockets: the open loop

`crates/hyper-quic/examples/geo_open_loop.rs`, the rounds before's bench: client, server and the
relay's two directions as threads on loopback, 500 ms each way, five dials (the first fresh, the
rest resumed with a 0-RTT request), then 20,000 requests open loop at 100 a second on the kept
connection, latencies from each request's scheduled time less 1,000 ms; quantile intervals 95%
distribution-free. Before and after binaries interleaved, 2026-10-05 01:36–01:57 PDT; the third pair
02:32–02:39, after the Data space's copies were made to wait for its streams.

| run | load (before → after the run) | p50 / p99 / p99.9 / max, overhead | worst in the first tenth |
|---|---|---|---|
| before 1 | 20.8 → 6.3 | 2.173 / 4.103 / 155.0 [135.8, 184.1] / 243.1 ms | 243.1 ms |
| after 1 | 6.3 → 6.9 | 2.181 / 4.261 / **85.0** [35.0, 151.8] / 251.5 ms | 251.5 ms |
| before 2 | 6.9 → 7.0 | 2.177 / 4.030 / 155.4 [137.7, 183.8] / 243.4 ms | 243.4 ms |
| after 2 | 7.0 → 6.6 | 2.183 / 5.296 / **84.6** [42.2, 153.4] / 252.7 ms | 252.7 ms |
| before 3 | 3.3 → 2.5 | 2.143 / 4.137 / 153.4 [133.7, 182.6] / 242.1 ms | 242.1 ms |
| after 3 (the final tree) | 2.5 → 3.4 | 2.126 / 3.157 / **85.6** [42.5, 155.6] / 252.2 ms | 252.2 ms |

After a measured warm-up (4,000 requests at 400 a second on dial 1, Careful Resume on):

| run | load | p50 / p99 / p99.9 / max, overhead | the warm-up's own p99 |
|---|---|---|---|
| before | 6.6 → 8.3 | 2.190 / 7.485 / 15.5 [14.5, 17.8] / 22.9 ms | 1,477.9 ms |
| after | 8.3 → 6.6 | 2.190 / 3.318 / 11.7 [10.4, 15.0] / 20.8 ms | 1,250.7 ms |

- **The burst's p99.9 fell from 155 to 85 ms.** Before, the stall came again a round trip after
  the first (the window grew 1.8 kB in the round trip after it filled); now the window grows by
  what that round trip acknowledges, and only the first round trip waits. The warm-up, slow start
  from the initial window, gains the same way: its p99 fell from 1,478 to 1,251 ms.
- **The first round trip's worst rose 9 ms (243 to 252 ms)**: the copy of the client's Finished,
  sent as the burst starts, takes a datagram of the initial window. The worst itself is the initial
  window: the open loop offers 13.1 kB a round trip and RFC 9002 §7.2 allows 12,000 bytes before
  the first acknowledgement on a path no connection has measured (`docs/research/quic-overhead.md`
  §4.2).
- Dials: the fresh dial's first reply 5.4 to 6.5 ms over its floor, resumed dials 0.8 to 3.7 ms
  over theirs, before and after alike; 0-RTT on every resumed dial.

### Overhead above the floor, round three

| condition | before | after | cut | cut from round two's start |
|---|---|---|---|---|
| simulated, lossy, fresh first reply, p90 / max | 1,899 / 2,319 ms | 180 / 2,117 ms | 10.5x / 1.1x | 26x / 2.9x |
| simulated, lossy, resumed first reply, p90 / max | 1,177 / 3,890 ms | 124 / 142 ms | 9.5x / 27x | 31x / 34x |
| real sockets, kept connection's burst, p99.9 (path never measured) | 155 ms | 85 ms | 1.8x | 2.3x (from 192 ms) |
| real sockets, kept connection's burst, worst (path never measured) | 243 ms | 252 ms | 0.96x | 1.2x (from 292 ms) |
| real sockets, warm-up slow start, p99 | 1,478 ms | 1,251 ms | 1.2x | |

```sh
# Simulated (deterministic): the overhead, lossy and Careful Resume tables
cargo test --release -p hyper-quic --test geo -- --ignored --nocapture --exact print_the_overhead_table
cargo test --release -p hyper-quic --test geo -- --ignored --nocapture --exact print_the_lossy_overhead_table
cargo test --release -p hyper-quic --test geo -- --ignored --nocapture --exact print_the_resume_table
# Real sockets, before (39d8195) and after interleaved, each between two load readings
cargo build --release -p hyper-quic --example geo_open_loop
target/release/examples/geo_open_loop --rate 100 --requests 20000 --dials 5
target/release/examples/geo_open_loop --rate 100 --requests 20000 --dials 5 --warm-rate 400 --warm-requests 4000
```

What is left of the tenfold cut, and why: the lossy fresh dial's maximum is a seed that lost both an
original and its copy (a second loss on the critical path costs a probe timeout, as before); and a
burst's first round trip on a connection with no measurement of its path waits on the initial
window, which RFC 9002 §7.2 and §7.7 set and no standards-track mechanism lets a sender exceed
(`docs/research/quic-overhead.md` §4.2).

## Sealing at rest (hyper-seal, sealed hyper-log)

docs/seal.md §11. M5 Max, 18 cores, APFS, 2026-10-05, the machine's own load recorded with each run.

**hyper-seal** (`cargo bench -p hyper-seal --bench seal -- 10000`, 10:44Z, load 5.2), p50 / p99 / p99.9:

| | |
|---|---|
| a key made and wrapped | 1.38 / 1.50 / 1.88 µs |
| a key unwrapped | 375 / 416 / 459 ns |
| a 4 KiB segment opened, warm | 500 / 542 / 625 ns |
| a 4 KiB segment opened, cold (unwrap + commitment) | 1.21 / 1.33 / 4.00 µs |
| 4 KiB overwrite in a 64 KiB chunk: key per file / versioned | 9.88 / 10.88 / 13.71 µs vs 500 / 583 / 709 ns |
| 4 KiB overwrite in a 256 KiB chunk: key per file / versioned | 31.0 / 32.6 / 37.0 µs vs 500 / 583 / 750 ns |
| throughput a core, seal / open, 4 / 16 / 64 / 256 KiB segments | 9.25 / 10.12 / 10.45 / 10.49 GB/s; 8.42 / 9.15 / 8.98 / 9.24 GB/s |

**Group commit, plain against sealed** (`cargo bench -p hyper-log --bench log -- DIR 5 1024,16384 64
plain,sealed`, each point plain then sealed, three repetitions, 64 closed-loop replicas, load 3.9–5.3).
With the frame MAC over framing only:

| entry | appends/s plain / sealed | p50 plain / sealed | energy an append plain / sealed |
|---|---|---|---|
| 1 KiB | 6,932–7,781 / 7,099–7,748 | 8.46–8.49 / 8.43–8.49 ms | 30.2–34.2 / 32.1–37.2 µJ |
| 16 KiB | 6,894–7,536 / 6,894–7,395 | 8.49–8.54 / 8.52–8.60 ms | 47.4–48.1 / 60.1–62.0 µJ |

Latency and rate are within the flush's own spread (one F_FULLFSYNC, ~8.5 ms here): sealing adds
microseconds to a frame that waits milliseconds. Energy grows 5–9% at 1 KiB and ~27% at 16 KiB, the
cost of encrypting the bytes; before the frame MAC left sealed bytes to their tags it was ~13% and
~80% (three repetitions at 5 s, load 3.1–4.7). Single 2-second points earlier the same day moved p50
between 8.5 and 16 ms in both modes alike, the flush's steps under other I/O; they are not a
difference between the modes.

## hyper-quic at 500 ms one way under bursts: copies spaced past the burst (2026-10-05)

Before is `line` at 1a385a2 (copies of the handshake's flights sent right behind their originals);
after is that tree with `crates/hyper-quic/VENDORED.md` §14 (a copy waits `τ·ln(PTO/τ)`, 117 ms at
the first probe timeout; 1-RTT packets held until the handshake completes). Research and the burst
conditions: `docs/research/burst-loss.md`. hyper-sim's loss in time (`Loss::bursty_in_time`) is in
both trees for the simulated rows.

Machine: Apple M5 Max, 18 cores, 128 GiB, macOS 26.4.1, other sessions building and testing
throughout; load average 4.99 → 4.22 over the before table's run, 3.38 → 3.21 over the after
table's.

### On the simulated network (exact, virtual time)

`crates/hyper-quic/tests/geo.rs`, `print_the_burst_table`: 500 ms one way with ±100 ms reordering,
seeds 1 to 32, the large certificate, a fresh dial then a resumed dial with its request in 0-RTT; the
first reply above its floor, median / p90 / maximum, ms. Three conditions at the same 5% mean:
independent; bursts of correlation time 35.0 ms (mean burst 36.8 ms every 700 ms, Jiang and
Schulzrinne's trace 4); bursts of 78.7 ms (mean burst 82.8 ms every 1,574 ms, Bolot's 200 ms column).

| condition | no copies | before: copies back to back | after: copies spaced |
|---|---|---|---|
| independent, fresh | 180 / 1,947 / 2,319 | 0 / 136 / 818 | 58 / 317 / 1,168 |
| independent, resumed | 169 / 2,445 / 3,126 | 13 / 108 / 146 | 107 / 235 / 265 |
| bursts 35 ms, fresh | 118 / 2,182 / 3,229 | 0 / **2,749** / 5,776 | 0 / **394** / 10,258 |
| bursts 35 ms, resumed | 135 / 3,924 / 5,035 | 38 / 141 / 2,972 | 95 / 155 / 3,164 |
| bursts 78.7 ms, fresh | 99 / 2,005 / 9,737 | 0 / **2,004** / 5,023 | 0 / **676** / 10,258 |
| bursts 78.7 ms, resumed | 102 / 3,548 / 5,035 | 18 / 88 / 141 | 74 / 270 / 3,139 |
| bytes sent, all 32 seeds: independent / 35 ms / 78.7 ms | 1,002,927 / 998,909 / 990,242 | 1,678,139 / 1,742,656 / 1,703,399 | 1,487,099 / 1,523,386 / 1,538,742 |

- **Under bursts, copies right behind their originals died with them**: the fresh p90 with copies,
  2,749 ms, was worse than with none, 2,182 ms. **Spaced, it is 394 ms (7.0x), and 676 ms on the
  longer bursts (3.0x).** Fresh dials a probe timeout or more past their floor on the 35 ms
  condition: eight of 32 back to back, two spaced
  (`under_bursts_a_spaced_copy_clears_the_burst_its_original_met`).
- **Cost under independent loss**: the spacing is paid on each recovered loss, fresh p90 136 → 317 ms
  and resumed 108 → 235 ms. No measured trace shows independent loss at the handshake's spacings
  (`docs/research/burst-loss.md` §3).
- **Bytes: spaced copies send 11% fewer than back-to-back ones** (+48% over no copies under
  independent loss against +67%): a copy whose original is acknowledged or declared lost before it is
  due is never sent.
- **The fresh maximum, 10.3 s**, is one seed (14) where a burst took the server's whole first flight,
  ten datagrams the window and the anti-amplification limit had let go at once. Its copies wait on
  the window, and probes of two datagrams with backoff recover the flight; neither limit may be
  passed (RFC 9002 §7, RFC 9000 §8.1).
- Measured and not taken: τ = 78.7 ms as the default (fresh p90 358 / 429 ms on the two burst
  conditions, but resumed 290 ms on the 35 ms one and fresh 933 ms under independent loss).

### On real sockets: the open loop, unchanged

`crates/hyper-quic/examples/geo_open_loop.rs`, as the section above: no loss on the relay, five
dials then 20,000 requests at 100 a second on the kept connection; before and after binaries
interleaved, 2026-10-05 05:39–05:52 PDT.

| run | load (before → after the run) | p50 / p99 / p99.9 / max, overhead | worst in the first tenth |
|---|---|---|---|
| before 1 | 4.43 → 3.26 | 2.198 / 3.391 / 103.4 [42.3, 173.4] / 262.1 ms | 262.1 ms |
| after 1 | 3.26 → 3.20 | 2.162 / 3.273 / 106.5 [52.6, 176.5] / 264.8 ms | 264.8 ms |
| before 2 | 3.20 → 4.41 | 2.172 / 3.414 / 102.0 [42.4, 172.0] / 261.8 ms | 261.8 ms |
| after 2 | 4.41 → 2.95 | 2.178 / 3.335 / 102.9 [52.3, 172.9] / 262.7 ms | 262.7 ms |

Every quantile's interval overlaps its before's. The dials' first replies stay 1.7 to 9.0 ms over
their floors, 0-RTT on every resumed dial.

```sh
# Simulated (deterministic)
cargo test --release -p hyper-quic --test geo -- --ignored --nocapture --exact print_the_burst_table
# Real sockets, before (1a385a2) and after interleaved, each between two load readings
cargo build --release -p hyper-quic --example geo_open_loop
target/release/examples/geo_open_loop --rate 100 --requests 20000 --dials 5


## hyper-quic at 500 ms one way: measuring the path before its first burst (2026-10-05)

The fourth round toward the owner's tenfold cut in what the stack adds above the physical floor of
round trips at 500 ms one way. Round three left the burst on a path no connection has measured at
85 ms p99.9 and 252 ms worst, the initial window's stall. Research: `docs/research/quic-overhead.md`
§5; the change: `crates/hyper-quic/VENDORED.md` §15. Before and after are the same binary, the
warm-up off (`--no-warm-up`) and on.

Machine: Apple M5 Max, 18 cores, 128 GiB, macOS 26.4.1. Other sessions built and tested throughout;
the load average is beside each run.

### On real sockets: the open loop after the kept connection idles

`crates/hyper-quic/examples/geo_open_loop.rs`: client, server and the relay's two directions as
threads on loopback, 500 ms each way, five dials (the first fresh, the rest resumed with a 0-RTT
request), then the kept connection idles 8 s (`--idle-ms 8000`), as a node's connection to a peer
is up before load comes, then 20,000 requests open loop at 100 a second; latencies from each
request's scheduled time less 1,000 ms; quantile intervals 95% distribution-free. Runs interleaved,
2026-10-05 10:04–10:22 PDT.

| run | load (before → after) | p50 / p99 / p99.9 / max, overhead | warm-up packets, window at the burst |
|---|---|---|---|
| off 1 | 5.1 → 4.6 | 2.137 / 4.254 / 73.1 [24.7, 126.4] / 216.4 ms | 0, 12,000 B |
| **on 1** | 4.6 → 6.1 | 2.133 / 3.297 / **7.2** [6.2, 8.6] / **14.8** ms | 99, 69,659 B |
| off 2 | 6.1 → 5.8 | 2.143 / 6.154 / 75.9 [34.4, 125.9] / 215.9 ms | 0, 12,000 B |
| **on 2** | 5.8 → 6.8 | 2.138 / 2.991 / **6.2** [6.1, 6.2] / **15.9** ms | 139, 93,659 B |
| no idle (`--idle-ms 0`) | 6.8 → 6.5 | 2.140 / 4.077 / 85.1 [42.1, 152.2] / 262.2 ms | 0, 12,000 B |

- **The burst's p99.9 fell from 73–76 to 6.2–7.2 ms (about 11x), and its worst from 216 to 15–16 ms
  (about 14x).** From round three's starting point (155 ms p99.9, 243 ms worst; round two's 192 /
  292 ms): 21–25x at p99.9, 15–16x worst.
- **The no-idle run is round three's**: a burst at the first contact, before any idle round trip,
  waits on the initial window as before (85 ms p99.9, 262 ms worst), which no standard lets a sender
  exceed without a measurement (RFC 9002 §7.2).
- **Cost**: 99 and 139 warm-up packets of the path's MTU a side, about 130–184 kB, once a path a
  lifetime (one hour by default), within the budget of sixteen initial windows (232 kB at a
  1,452-byte datagram). The dials are unchanged: resumed first replies 0.8–3.5 ms over their floor
  in every run.

### On the simulated network (exact, virtual time)

`an_idle_connection_warms_its_path_up_and_a_later_burst_needs_no_second_round_trip`
(`crates/hyper-quic/tests/geo.rs`): a 24 kB reply on a kept connection after an idle first
connection, 2,000 ms without the warm-up and 1,259 ms with, the floor 1,000 ms: the jump carries
the reply within its round trip, paced over it as RFC 9959 §3.3 requires. A mebibyte reply on a
fresh resumed connection is unchanged (7,643 ms either way): the jump is declined where slow start
would beat it (`crates/hyper-quic/VENDORED.md` §15 item 3).

### Overhead above the floor, round four

| condition | before | after | cut | cut from round two's start |
|---|---|---|---|---|
| real sockets, kept connection's burst after idling, p99.9 | 73–76 ms | 6.2–7.2 ms | ~11x | 27–31x (from 192 ms) |
| real sockets, kept connection's burst after idling, worst | 216 ms | 15–16 ms | ~14x | 18–19x (from 292 ms) |
| real sockets, burst at first contact, p99.9 / worst | 85 / 252 ms | 85 / 262 ms | unchanged | the initial window |

```sh
cargo build --release -p hyper-quic --example geo_open_loop
target/release/examples/geo_open_loop --rate 100 --requests 20000 --dials 5 --idle-ms 8000 --no-warm-up
target/release/examples/geo_open_loop --rate 100 --requests 20000 --dials 5 --idle-ms 8000
target/release/examples/geo_open_loop --rate 100 --requests 20000 --dials 5 --idle-ms 0
```
