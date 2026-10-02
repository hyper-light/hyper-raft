# hyper-durable: origin

hyper-durable is new code. It is not extracted from one project's shell: its design is
`docs/durable.md`, drawn from the three shells that exist and from the literature
(`docs/research/durable.md`), and each rule below names where it comes from.

## The shells read

- **mantle**, `crates/range/src/{replica.rs,store.rs}` at origin/dev `1c179e8`
  (`1c179e8a734d40640b82b6bddf8cad2030d0b55f`), its commit fence included, and its range
  simulation's four directed cases (`crates/range/tests/sim.rs` at the same revision).
- **focal**, `crates/focal-consensus` at `aa1f162` (F17: `persistence.rs`, `sole_commit`,
  `settle_commit`, `apply_on_written_commit`) and `7a6170e` (`DurableNode`, `guarded_in`,
  `memory.rs`), as `docs/research/durable.md` §4 and §6 record them.
- **slates**, its anchor publication at `ec5e0df` (`docs/research/durable.md` §6).

## What came from where

| Rule | From | Here |
|---|---|---|
| The shared device log as the store; a group's handle that reads where the replica is | mantle | `GroupStore` over hyper-log's `GroupLog` (`src/hyperlog.rs`); entries encoded as mantle's store encodes them (`encode_entry`, `ENTRY_OVERHEAD`) |
| A leader sends before its own write | mantle R31, thesis §10.2.1 | the core gives them (`Ready::messages`), `Replica::take_ready` emits them at once |
| A write refused for room waits whole; the replica takes no part while it does | mantle (audit S04) | `Replica::refused`, `make_again`, `ReplicaError::Stalled`; ticks dropped, not held |
| A snapshot report kept while stalled | mantle (a simulation seed) | `Replica::report_snapshot`, one a member of the configuration |
| A failed write fences the replica | mantle; Rebello et al. | `Cause::Write`; every call answers `Fenced` |
| Open: finish an install the log never recorded; raise the commit to what the state machine holds | mantle (`complete_install`, `commit_applied`) | `repair_at_open`, with the state machine's point's term from `StateMachine::durable`, so mantle's `Damaged` inference is gone |
| Marks: votes judged against the mark, no campaign, repair asked of the leader | mantle (R34; PAR) | `Replica::judged_by_mark`, `emit`, `ask_repair` |
| One `Ready` a drive, the owner's quantum | mantle (`DRIVE_BUDGET`) | `Replica::drive`, `Owner::turn` |
| A write submitted in a drive is never polled in it | mantle (replica.md §5) | `Replica::take_answers` runs first and only first |
| The commit rides the next write; `commit = last` where the member decides alone; a quiet group's commit written after a period | focal F17 | `Replica::stated_commit`, `decides_alone`, `quiet_commit` |
| A change, and what the state machine acts on at start, applied only on a logged commit | focal F17, generalised by `C_d` | `Replica::walk`'s fence, `StateMachine::acts_at_start` |
| Memory reserved before a transition | focal R28 | `Budget`, `Replica::reserve` and `settle`; `Unbounded` compiles it out |
| The unwind boundary | focal R27 (`guarded_in`) | `Replica::guarded`, `Cause::Unwound` |
| The owner woken by the log's answer | focal F45 | `LogStore::submit`'s waker, `Owner::woken` |
| A store that completes synchronously is a store of depth one | slates | `RamStore` |
| Readies ahead of persistence; the unstable log true to its name; the ABA guards | etcd, raft-rs (core R-4) | `Limits::readies_in_flight` set to `LogStore::depth` |
| Apply before local durability on a leader | TiKV RFC 0112; raft-rs #537, #561 (core R-6) | `Config::apply_unpersisted` passed through; `Replica::walk` reads the leader's own entries past the store where the core holds them |

## Core steps

- **R-4** (readies ahead of their persistence) and **R-6** (the durable commit, the apply pause,
  applying before durability; hyper-raft `94a3a6a`) are built and used: `RawNode::ready_in_place`,
  `to_persist`, `advance_issued`, `on_persist`, `durable_commit`, `commit_durable`, `pause_apply`,
  `resume_apply`.
- **R-5** (a lost-entries refusal regresses a member's progress): not built. Until it is, a marked
  member asks its leader for a snapshot reaching its mark, as mantle does (`Replica::ask_repair`);
  R-5 replaces that message with the refusal flagged lost, and nothing else in the shell changes.
- **R-7** (CTRL's leader-side recovery): not built. Until it is, a marked member does not campaign,
  as mantle's does, and a group whose only up-to-date log is marked waits.
- **L-2** (elections on the detector's suspicion): not built. The shell takes `tick`; its
  `suspect` waits on L-2, and will be withheld while the member is marked, stalled or fenced, as
  ticks are dropped while stalled now.

Asked of the core, not built here: `RawNode::into_store`, so that an owner gets its store back from
a replica it closes (`Replica::into_machine` gives back the state machine only); and an accessor
for the fast track's proposals not yet durable, so that a write refused for room that held them is
made again rather than fencing the replica (`Replica::make_again`).

## hyper-log changes made with it

- **Writes sent behind a refused one are refused** (`LogError::Behind`): a group's handle carries
  an epoch it moves on every refusal it takes, and the log refuses a write of the group sent in an
  epoch at or before a refused one's. Without it a write that held only a hard state, sent behind
  entries refused for the group's bound, was written: the group then stated a commit past the
  entries it held (`tests/log.rs`, `a_write_sent_behind_a_refused_one_is_refused_too`, fails
  without the owner's check).
- `GroupLog::depth` (the log's `PIPELINE_FRAMES`, three) and `GroupLog::has_room`.
- Not made: `GROUP_SUBMISSIONS` from `PIPELINE_FRAMES` (`docs/durable.md` §11). At four a group of
  frame-sized writes would hold all three frames' bytes and the log's tests of a cold group's room
  fail; a group's third write waits in the log for its group's room instead, never refused, and
  `docs/durable.md` §14 item 1 measures whether the third frame earns its place.

## L-2: elections by suspicion, on the node-pair stream

Timing step L-2 (`docs/timing.md` §2.9, `docs/durable.md` §8). The shell opens every core electing
by suspicion and has no `tick`; `suspect`, `trust`, `restarted`, `set_timing` and `deadline` reach
the core, and `drive` wakes it. The time every call takes is the owner's monotonic clock in
nanoseconds (`u64`), as the core's and hyper-liveness's. The campaigns are held while the member is
marked or stalled (`RawNode::hold_campaigns`); the detectors' words are not withheld, which the plan
had said, since a marked follower that kept trusting a suspected leader would refuse every
pre-vote of its group. The owner wires the node's `hyper_liveness::Liveness`: `Owner::pairs`,
`Owner::believe`, `Owner::measure`, `Replica::measure`, `Replica::believe_all`, `Replica::peers`,
`Driven::flushed`. The simulation's settle resumed a stalled replica once and never again, so a
refusal taken after it stalled the replica for good; found by the suspicion soak (seed 5, crash 4),
it now resumes each round.
