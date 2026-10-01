# The consensus crates: the Raft core, its durable shell and the shared log

> Status (2026-09-30): **step R-1 done in this repository.** focal's Raft core is `crates/hyper-raft`,
> with its history and conformed to this repository's lint wall. Its tests, the raft-rs differential
> and focal's allocation counts are identical before and after (`crates/hyper-raft/ORIGIN.md`).
> - R-1's consumer gates have not run: focal's full suite and mantle's `crates/range` suite against
>   the new crate.
> - No consumer has taken a snapshot yet.
>
> The plan and its evidence are mantle's `docs/research/32-shared-transport-and-raft.md` ("note 32").
> Its §6 records the owner's decisions.

## 1. What `hyper-raft` is

`hyper-raft` is the Raft core that slates, focal and mantle share. It is a state machine with no
clock, no disk and no network:
- `RawNode::tick` says that time has passed;
- `RawNode::step` gives it what arrived;
- one `Ready` at a time says what to persist, send and apply;
- `advance_append` and `advance_apply_to` report back.

A `Ready` comes in two forms that decide alike (`tests/differential.rs`, "in place"):
- `RawNode::ready` copies what it gives, as raft-rs's `Ready` does: the entries to persist and the
  entries to apply are the owner's to take.
- `RawNode::ready_in_place` copies nothing the owner can read where it is. The owner writes the
  snapshot and entries out from `RawNode::to_persist`, applies the range
  `Ready::committed_range` names from its own storage, and keeps the very entries and snapshot
  the member gives up at `RawNode::advance_append_keeping`. A disk-backed owner copies no entry
  at all. `docs/benchmarks.md` measures both forms.

A leader's proposals and a follower's appends move into the log uncopied (`Log::append_owned`,
`Log::append_after_owned`).

It is Raft as Ongaro's thesis states it, with these extensions:
- pre-vote and check-quorum;
- election priority with `Precedence::Log`;
- learners and joint consensus (`ConfChangeV2`);
- leader transfer;
- an inflight window with conflict hints, bounded per member in messages and in bytes
  (`Config::max_inflight_bytes`, `RawNode::set_inflight_bytes`: what the owner says the path to
  the member carries before it answers), and a heartbeat's answer that says how far the member's
  log goes (`HeartbeatAnswers::Position`, the default; `HeartbeatAnswers::Bare` is raft-rs's,
  which the differential runs);
- ReadIndex (quorum-confirmed, no lease), with one round of heartbeats for every read asked since
  the last `Ready` (`ReadRounds::Shared`, the default; `ReadRounds::Each` is raft-rs's round per
  read, which the differential runs), and snapshots;
- the fast track (Fast Raft, Castiglia, Goldberg and Patterson, ICDCS 2020) for a group that enables it.

Errors are of three kinds:
- a refusal changed nothing;
- a violation is a peer message that contradicts this member, and is dropped;
- a fatal error means the member's own state no longer adds up. Only this kind stops the replica.

Every queue has a bound in `Limits`.

It keeps raft-rs's wire and log types through `raft-proto`, at the raft-rs revision focal and mantle
pin (`8e4cef172421bf77b2ae1c26628a9531b0be41f0`). So a member on this core and a member on raft-rs
exchange the same bytes. `tests/differential.rs` runs both cores on one schedule and compares them after
every step. raft-rs is a dev-dependency only.

The crate is sans-io, as every crate here is (`CLAUDE.md` §1). A durable shell drives it: today focal's
`focal-consensus` and mantle's `crates/range` replica, and later `hyper-durable` (§4).

## 2. Provenance

`crates/hyper-raft` is focal's `crates/focal-raft` at focal `a8e95f7` (`origin/slates-port`,
2026-09-30), moved with its eight commits of history. `crates/hyper-raft/ORIGIN.md` records:
- the source revision;
- the commit mapping;
- every change since: the rename, the manifest, the conformance to the lint wall, the bench left in
  focal, and the evidence that behaviour did not change.

Note 32 chose focal's core as the base for four reasons (§2.2, §3.8):
- it is already sans-io with typed errors and bounded queues;
- its differential against a deployed library is the strongest oracle any of the three cores has;
- it already carries joint consensus v2 and the fast track;
- focal and mantle both run raft-rs's types, so the core speaks their existing wire and log.

slates' core stays in slates until X-1. It carries no enhancement this one lacks, apart from those that
R-3 brings in.

## 3. The core's next steps

Each step is one gated commit, and each can be undone by reverting it (note 32 §5.2). R-1's gate also
includes the consumers' suites. They run when each consumer takes a snapshot under its own rules
(`CLAUDE.md` §3).

| Step | What changes | Gate |
|---|---|---|
| **R-1** (done here) | focal-raft moved and conformed; no behaviour change. | focal-raft's own tests and the raft-rs differential, identical before and after (done, `ORIGIN.md`). Still to run: focal's full suite with focal on a snapshot of this crate, and mantle's `crates/range` suite including `sim.rs`. |
| **R-2** | Own message types. `raft-proto` is replaced by the same messages declared as Rust structs with `prost` derive. No protoc, no build script, and no second git source in production. | Golden vectors equal for every message and entry type. The differential unchanged: raft-rs on its own types, this core on the new ones, compared by encoded bytes. focal's WAL replay of a recorded data directory. |
| **R-3** | slates' enhancements, one commit each, in this order. First, regression tests for R6 and R7, which are expected to pass. Then tests for R4, R5, R20 and R21, with a patch only on failure. Then the R16 byte-bounded pipeline window, R17 out-of-order acknowledgement within a term, R13 learner catch-up rounds, and R22's compaction rule. `Limits::derive` replaces `Limits::default`. | For each: the slates test or bench that motivated it, run against this crate and showing slates' recorded improvement. The raft-rs differential; any intended divergence joins focal 27 §4.5's divergence table with its own test. mantle's and focal's suites. |

The R-numbers are note 32 §2.13's ledger.

**The fast track** stays focal's algorithm until note 32 §3.8's tests decide, and no owner enables it
until then. The tests:
- slates' exhaustive search applied to it;
- the reconfiguration and liveness the TLA+ model lacks;
- slates' five-region crossover;
- mantle's admitted-request accounting.

TLC runs in this repository's CI only (owner's decision 7), beside `hyper-check`'s explorer. The model
`FastTrack.tla` is still in focal (`docs/models/`) and moves here with that work.

**Open against the rules until R-3.** These values are carried unchanged and are literals, not
derivations:
- `Limits::default`: mantle note 32 §2.10;
- `MAX_MEMBERS`.

## 4. The crates that follow

| Crate | What | Steps and gate |
|---|---|---|
| `hyper-durable` | The shell: mantle's Ready pipeline with focal's accounting (note 32 §3.8). `Replica<L: LogStore, M: StateMachine>` with `begin` and `finish`; leader appends sent before the leader's own write; held inputs bounded by bytes and ticks; protocol-aware repair; the commit fence decided by `StateMachine::durable_applied`. | **D-1**: extract the shell from mantle's replica, leaving `RangeMachine: StateMachine` in mantle. Gate: mantle's `crates/range/tests/group.rs` and `sim.rs`. Recorded simulation seeds replay to identical Ready sequences and identical histories. **D-2**: focal's shell onto it (`guarded_in`, reservations before transitions, decoder fences, checkpoint images, nonblocking receipts). Gate: focal-consensus's tests and focal's real-process suites. Recorded seeds replay with identical committed output. The one intended timing change, leader appends sent early, is checked by the safety explorer. |
| `hyper-log` | mantle's per-device log, with its `BlockFile` trait. It is the one crate that owns its device writer and files (`CLAUDE.md` §1). | **L-1**: move it. Gate: mantle's `crates/log/tests/log.rs`, `fairness.rs` and the range simulation; bytes on the simulated device identical per seed. **L-2**: an owner thread in place of `Arc`, `RwLock`, `Mutex` and `Condvar`, and `Waker` tickets in place of the mpsc `Pending`. Gate: L-1's suite; a test that a completion wakes only its submitter; mantle's log benchmark against its recorded baseline; frame contents per seed identical. **F-1**: focal's WAL converted to it, one way. Gate: recorded focal data directories converted and read back equal, and focal's restart and restore runbooks on real processes. |
| `hyper-multilog` | slates' MLRaft layer: one group's log divided into `n` logs with barriers (note 32 R25). | Moved with X-1. Gate: slates' `tests/multilog*.rs` and its explorer (200 seeds × 3,000 steps), retargeted at this crate. No consumer enables it until an owner shows a gain; slates measured it worse for its own groups. |

slates' consensus moves onto `hyper-raft` in **X-1**. Gate: slates' explorer, conformance, prevote,
priority, wan_election, pipelining and fast_track suites; the prefix and slot models; the KIND
succession lane; slates' N=1 differential. Its groups are re-founded on a fresh start, since slates has
no release to keep (owner's decision 9).

## 5. Shared test infrastructure the core will use

The allocation-count bench (focal-memory's counting global allocator) came back as `hyper-measure`,
rewritten without its lock and with its `unsafe` listed in the contract script; with it,
`hyper-raft-compare` measures this crate against each core it replaces, and `hyper-raft-e2e` runs it
as real processes (`docs/benchmarks.md`). The same
applies to the timed simulation, the safety explorer and the exhaustive models from slates, and to
mantle's linearizability checker. Each joins under the production lints, as focal requires of
`focal-sim`.
