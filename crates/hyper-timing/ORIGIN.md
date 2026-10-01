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

5. **slates' timing law joins the crate** (`src/election.rs`, from slates
   `crates/cluster/src/timing.rs` at `c4e2c52`).
   - Ported: `ElectionTiming` (derive, floor, window_budget, timeout_periods), `ElectionTimer`,
     `FollowerStep`, `ElectionPriority` (from slates' `raft.rs`), `quorum_priority` and
     `REPAIR_ROUND_TRIPS`.
   - The derivations take any `PathEstimate` (`PathRtt` or `ExchangeRtt`), so the choice of
     estimator for election timing is a measured input, not a fork.
   - `ElectionTiming::derive` takes `durable_tail_ns`: the durable-acknowledgement time the path
     samples lack (mantle audit §11.7). It is added once a path is measured.
   - Local ids are `u64`.
   - slates' 15 tests are ported on `ExchangeRtt` and pass unchanged in their numbers. That
     confirms focal's `ExchangeRtt` and slates' `RttEstimator` compute the same RFC 9002 values.
   - New tests: the durable term, the law on either estimator (three late answers of 16 move the
     median-and-MAD base nowhere and the smoothed base past 100 periods), and
     `ElectionPriority::outranks`.
6. **One round-budget law.** `RoundBudget::derive(&RoundAnchors, tail_ns, ceiling_ns)` replaces
   focal's `derive(period, tail, ceiling)` and slates' `round_budget(anchors, tail)`.
   - From focal: the caller's ceiling bounds everything; an unmeasured round gets the whole ceiling,
     hard; and extensions are capped at `ELECTION_MARGIN` and at the room left under the ceiling.
   - From slates: the stall window is `stall_periods` periods (derived from the SWIM suspicion span,
     where focal fixed it at two), the lookahead and poll rate come from the anchors, and
     `RoundAnchors::poll_interval_ns` is kept.
   - focal's tests pass with its anchors (stall 2, lookahead 3/4).
   - slates' measured cases are identical under a ceiling of `max(heartbeat, tail) +
     ELECTION_MARGIN × heartbeat`.
   - slates' unmeasured case changes from one period to the whole ceiling. A round against a peer
     nothing is known about is never cut off before the caller's bound; slates' own WAN-round bug
     was such a cut-off.

## Planned

- **The estimator that feeds election timing** is decided by the timed simulation in note 32 §3.7,
  with the judged outputs fixed before the run.
