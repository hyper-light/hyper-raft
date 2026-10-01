# hyper-timing: origin

- **Source.** focal's `crates/focal-timing` at focal `a8e95f7` (`origin/slates-port`), brought in
  with its history by `git subtree`.
  - The imported tree hash equals focal's `a8e95f7:crates/focal-timing`
    (`01bdb7575584338d8b857535fc5ca68ae061e613`).
  - It holds the election-timing law (`TickPace`), both path estimators (`PathRtt`, the median and
    MAD of the latest 16; `ExchangeRtt`, RFC 9002 §5.3), and progress-charged waits
    (`ProgressDeadline`, `RoundBudget`, `DeadlineExtender`, `RoundWait`).

## Changes

1. **Package `hyper-timing`.** It inherits the workspace lint wall.
2. **No clock is read.**
   - `ProgressDeadline::begin` and `ProgressDeadline::check` read `Instant::now()` and are removed.
   - Callers use `begin_at` and `check_at` with their own clock, as the sans-io rule requires
     (CLAUDE.md §1). focal's call sites change when it takes the vendored copy.
3. **Documentation.** The 22 public items the union wall's `missing_docs` found are documented.
4. **Test opt-out.** The crate-root test block also allows `cognitive_complexity`. Four test
   functions exceed the shipped-code threshold, and no shipped function does.

## Planned

- **slates' timing law** (`cluster/src/timing.rs`) joins this crate:
  - `ElectionTiming::derive`, the base and span from the measured tail and spread;
  - `ElectionTimer` with its seeded randomization;
  - `quorum_priority`;
  - `RoundAnchors` and `round_budget`;
  - `REPAIR_ROUND_TRIPS`;
  - the pipelining window `⌈2 × tail / heartbeat⌉`.
- **The durable-acknowledgement time** joins the tail (mantle audit §11.7).
- **The estimator that feeds election timing** is decided by the timed simulation in note 32 §3.7,
  with the judged outputs fixed before the run.
