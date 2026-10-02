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
>
> Step R-4 is done (2026-10-02): `Ready`s are taken ahead of their persistence (§3,
> `docs/durable.md` §2.1).
>
> Step R-6 is done (2026-10-02): the durable commit carried in answers, an apply pause, and a
> leader's own entries applied before its write is durable (§3, `docs/durable.md` §4.4).

## 1. What `hyper-raft` is

`hyper-raft` is the Raft core that slates, focal and mantle share. It is a state machine with no
clock, no disk and no network:
- `RawNode::tick` says that time has passed;
- `RawNode::step` gives it what arrived;
- a `Ready` says what to persist, send and apply; one is taken at a time, and once its write is
  issued (`advance_issued`) the member takes operations again and the next may be taken, up to
  `Limits::readies_in_flight` writes out;
- `on_persist` says which writes are durable, in the order issued, and `advance_apply_to` how far
  the application applied; `advance_append` is `advance_issued` and `on_persist` at once;
- `commit_durable` says a write the owner made beyond a `Ready`'s hard state states a commit, and
  `pause_apply` that the owner holds what it was given to apply and takes no more.

A `Ready` comes in two forms that decide alike (`tests/differential.rs`, "in place"):
- `RawNode::ready` copies what it gives, as raft-rs's `Ready` does: the entries to persist and the
  entries to apply are the owner's to take.
- `RawNode::ready_in_place` copies nothing the owner can read where it is. The owner writes the
  snapshot and entries out from `RawNode::to_persist`, applies the range
  `Ready::committed_range` names from its own storage, and keeps the very entries and snapshot
  the member gives up at `RawNode::advance_append_keeping` (or `on_persist_keeping`, once the
  write is durable). A disk-backed owner copies no entry at all. `docs/benchmarks.md` measures both forms.

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

It declares its own message and log types and writes them in its own format (§3.1). raft-rs, at the
revision focal and mantle pin (`8e4cef172421bf77b2ae1c26628a9531b0be41f0`), is a dev-dependency only:
`tests/differential.rs` runs both cores on one schedule and compares what they say after every step,
field by field through an adapter between the two sets of types.

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
| **R-2** | Own message types and **its own wire format** (the owner's decision, 2026-10-01: hyper-raft speaks its own protocol, not raft-rs's). The types are plain Rust structs with typed kinds; the encoding is §3.1's. No protobuf, no `prost`, no `raft-proto` in production. | Golden vectors of the new format pinned for every type. The differential unchanged in what it compares: raft-rs on its own types, this core on its own, compared field by field through the test adapter. Decoding refuses every truncation and corruption of every golden vector, never panics on arbitrary bytes. focal's WAL is translated by F-1's conversion. |
| **R-3** | slates' enhancements, one commit each, in this order. First, regression tests for R6 and R7, which are expected to pass. Then tests for R4, R5, R20 and R21, with a patch only on failure. Then the R16 byte-bounded pipeline window, R17 out-of-order acknowledgement within a term, R13 learner catch-up rounds, and R22's compaction rule. `Limits::derive` replaces `Limits::default`. | For each: the slates test or bench that motivated it, run against this crate and showing slates' recorded improvement. The raft-rs differential; any intended divergence joins focal 27 §4.5's divergence table with its own test. mantle's and focal's suites. |

The R-numbers are note 32 §2.13's ledger.

The durable shell needs four more core steps, R-4 to R-7. Their design and gates are in
`docs/durable.md` §2.1, §4, §5 and §11.

| Step | What changes | Gate |
|---|---|---|
| **R-4** (done, 2026-10-02) | `Ready`s taken ahead of their persistence: `advance_issued`, `on_persist`, `on_persist_keeping`, `Limits::readies_in_flight`; the unstable log keeps its entries until they are durable, with an issue mark; etcd's term guard and raft-rs's `maybe_persist` against ABA; a leader sends at once only while its term and vote are durable; what a notice makes leaves with it only when nothing is out or unwritten (`docs/durable.md` §2.1, where each invariant is kept in §3). | Run: the raft-rs differential unchanged at depth one; the recorded-seed equivalence of the synchronous path; `tests/pipeline.rs` (random interleavings of proposals, ticks, deliveries and persistence steps at depths two and three, held to a durability oracle against each member's disk; a crash at every persistence step of a schedule in turn; the fast-track schedules at random lags); allocation counts identical on every workload; `benches/pipeline.rs` (`docs/benchmarks.md`, "Readies in flight"). raft-rs is no oracle at depth `k` (`docs/durable.md` §2.1). |
| **R-5** | A lost-entries refusal regresses a member's progress (CTRL's follower repair, `docs/durable.md` §5). | `docs/durable.md` §12. |
| **R-6** (done, 2026-10-02) | An apply pause (etcd's `applyingEntsPaused`): `RawNode::pause_apply`, `resume_apply`; a leader's own-term entries given to apply before its own write is durable (`Config::apply_unpersisted`, off by default as raft-rs's limit is zero); and the durable commit carried in answers: `MsgAppendResponse` and `MsgHeartbeatResponse` state no commit beyond the durable commit (`C_d`, `docs/durable.md` §4.1) when they leave, not `log.committed()`. The core knows `C_d` from each durable `Ready`'s hard state and from `RawNode::commit_durable`, by which the shell states every other commit it writes (`docs/durable.md` §4.4). The case found in mantle (`1c179e8`, its F17 commit fence): a member commits alone as leader in `advance_append`, then steps down in the same term (check-quorum), and holds a commit no write states; a shell's `configuration_known` must not count it. R-4 did not carry it: the core was told which writes are durable, not which commit a write stated. | Run: the directed tests `an_answer_states_no_commit_that_no_durable_write_stated` (mantle's case), `an_answer_a_notice_releases_states_the_durable_commit` (fails on R-4), `an_owner_that_pauses_apply_is_given_nothing_more`, `a_leader_applies_its_own_committed_entries_before_its_write_is_durable`; `tests/pipeline.rs` with the oracle holding every answer's commit to the sender's disk and the harness keeping the commit fence with the pause, at four settings (the fourth a leader applying before its write, its disk the slowest) and the crash at every persistence step with and without it; the raft-rs differential unchanged with no translation (`docs/durable.md` §4.4, "Against raft-rs"); allocations identical on every workload; `benches/pipeline.rs` (`docs/benchmarks.md`, "The durable commit and the apply pause (R-6)"). |
| **R-7** | CTRL's leader-side recovery of a marked member's own lost entries (`docs/durable.md` §5). | `docs/durable.md` §12, behind the TLA+ model extended with a marked member. |

**The fast track** stays focal's algorithm, with the safety fix below, until note 32 §3.8's tests
decide, and no owner enables it until then. The tests:
- slates' exhaustive search applied to it;
- the liveness the TLA+ model lacks (it has a change of configuration since it moved here: `docs/models/README.md`);
- slates' five-region crossover;
- mantle's admitted-request accounting.

TLC runs in this repository's CI only (owner's decision 7), beside `hyper-check`'s explorer, once
that is here. The model is `docs/models/FastTrack.tla`, moved from focal (`7ea6f63`) with a change of
configuration and both rules below; `docs/models/README.md` maps its actions to this crate's functions
and records every configuration's states.

### 3.1 The wire format (R-2)

hyper-raft writes its messages, entries, hard states, configurations, changes and snapshots in a
format of its own. It is not raft-rs's: no member of mantle, focal or slates talks to a raft-rs
member; mantle's and focal's logs keep entries in their own frames, and focal's WAL is translated
once by F-1's conversion tool; the differential against raft-rs compares the two cores' outputs as
values, not as bytes.

**What it departs from, and why.** raft-rs writes `eraftpb` as protocol buffers
(<https://protobuf.dev/programming-guides/encoding/>): every field a key and a varint or a
length-delimited value, defaults omitted, fields in any order with the last occurrence winning. That
suits a schema that evolves across many writers. A Raft message is the opposite case: a small,
fixed set of fields, every one read on every message, on the path every entry takes. The format
here follows the design of FIX's Simple Binary Encoding (SBE, FIX Trading Community, *Simple Binary
Encoding Technical Specification* 1.0, §2: fixed-length fields at fixed offsets, little-endian,
variable-length data after the fixed block, a version in the header) and of Cap'n Proto's
argument against parse-heavy encodings (<https://capnproto.org/encoding.html>): a reader takes each
field from a known offset, with no key dispatch and no merging, and knows every length before it
takes any byte, so it allocates each buffer once at its exact size and can later lend entries out
of the received bytes instead of copying them.

**The layout.** Every integer is little-endian at its full width. A top-level value is a record:

| Bytes | Field |
|---|---|
| 1 | format version, 1 |
| 1 | the value's kind: message 1, entry 2, hard state 3, configuration 4, snapshot 5, change 6, joint change 7 |
| … | the value's body |
| 4 | CRC-32C (Castagnoli, RFC 3720 §B.4) of everything before it |

The checksum is mantle `CLAUDE.md` §6's rule (every on-disk record and network payload carries a
checksum verified on read); a mismatch is a typed corruption, never a value. Inside a record, nested
values (a message's entries, a snapshot's configuration) are bodies without their own header or
checksum.

- **Message body**: kind (1), flags (1: bit 0 reject, bit 1 snapshot present; any other bit set is
  refused), then nine `u64` (to, from, term, log term, index, commit, commit term, request snapshot,
  reject hint) and an `i64` priority, then the entry count (`u32`) and context length (`u32`), the
  context bytes, the entries, and the snapshot body when its flag is set.
- **Entry body**: kind (1), term and index (`u64`), data and context lengths (`u32`), the data, the
  context.
- **Hard state**: term, vote, commit (`u64`).
- **Configuration**: auto-leave (1), four counts (`u32`: voters, learners, outgoing voters, next
  learners), then the ids (`u64`).
- **Snapshot**: presence (1: bit 0 metadata, bit 1 its configuration; any other bit refused), the
  index and term (`u64`) and the configuration body when present, the data length (`u32`), the data.
- **Change**: kind (1), member (`u64`), context length (`u32`), context.
- **Joint change**: transition (1), change count (`u32`), each change as kind (1) and member
  (`u64`), context length (`u32`), context.

Every count and length is checked against the bytes that remain before anything is taken, an
unknown kind, version or flag is refused, and a record whose bytes run past its body is refused.
raft-rs's `deprecated_priority`, `sync_log` and change `id` are not carried: hyper-raft never read
them; the test adapter folds `deprecated_priority` into the priority as raft-rs does.

### The fast track's election defect, and its fix

**The defect.** In a fast group an election could commit a second, different entry at an index that
already held a committed one. Found by focal's schedules (`FOCAL_RAFT_SEEDS=40000
FOCAL_RAFT_SEED=3000`, seed 9843) and reproduced here on `main` (`e1e292c`) before any change:
`HYPER_RAFT_SEEDS=40000 HYPER_RAFT_SEED=3000 cargo test -p hyper-raft --release --test fast
a_group_with_the_fast_track`, "seed 9843: member 3 committed another entry at 32". Traced: the leader
of term 3 committed index 32 by the fast quorum {1, 2, 4} of four voters, where members 2 and 4 held
the entry beside their logs and the leader knew nothing of their logs (`matched` 0). Member 3's log
held an entry of an older term at 32. Members 2 and 4, whose logs were no more current than member
3's, elected it; recovery takes the most-held entry only above the candidate's log, so it kept its
own entry at 32 and committed it.

**The cause.** A fast quorum's members hold the committed entry beside their logs, but vote by the
classic comparison of logs. Their vote for the committing leader is a vote in that leader's round, and
nothing an election reads records it.

**The literature.**
- Fast Raft (Castiglia, Goldberg and Patterson, arXiv:2004.06215, ICDCS 2020) §IV-C: "the definition
  of up-to-date is modified to only include leader-approved entries", and the elected leader recovers
  self-approved entries from its voters. Its safety proof (Lemma 2) shows that "a follower never
  overwrites a chosen entry", and that a new leader inserts the most-voted entry, but not what an
  elected leader does with a leader-approved entry of an older term at the same index. That is this
  defect: the paper's rule has it too.
- Fast Paxos (Lamport, Distributed Computing 19(2), 2006) §3.3 (picking a value in phase 2a, condition O4): a value is chosen in round
  `k` only by a quorum of votes cast in round `k`, and a coordinator's recovery reads each acceptor's
  round of its last vote (`vrnd`), which the acceptor keeps durable, and considers only the highest.
  Here the round is the leader's term, and the only durable record an election reads is the log.
- Raft (Ongaro, thesis §3.6.2, Figure 3.7): a leader counts replicas only for an entry of its own
  term, for the same reason — a count of an older term's replicas says nothing a later election
  will respect.

**The fix.** A member that holds the entry beside its log counts toward a fast quorum only once the
leader knows its log holds an entry of the leader's term (`self.log.term(progress.matched) ==
self.term`, `Raft::fast_commit`). Then every member of the fast quorum R has a last log term at or
above the leader's, and stays so: what it holds through the leader's first entry of the term is held
by a majority, so no later leader's append truncates below it. By the classic rule each refuses any
candidate whose last term is older. A candidate whose last term is the leader's or later holds, up to
its last entry, the log of a leader that holds the committed entry: it holds the entry, or its log
ends below the index, and then the entry is the most held among its voters (Fast Raft's own
argument, which now applies). It is Fast Paxos's same-round condition, with the round recorded where
elections read it, and it needs no message, no field and no state: the leader's first entry of its
term reaches every member with its first append.

**The alternatives, and why not.**
- A voter refuses a candidate whose log is older than its own latest fast vote: the term of that
  vote must be durable, which raft-proto's `HardState` has no room for before R-2, and the
  comparison is of one term for the whole log. A candidate whose log is stale at the committed index
  can have voted later entries beside its log to the same leader, so its own latest fast vote is as
  recent as its voters' and it is elected with the stale entry: the rule must be per index.
- What a member holds carries the term it was voted in, and an election weighs it at every index
  above the candidate's commit (Fast Paxos's recovery, per index): it needs that term durable for
  every held entry (a write each time a held entry is voted to a new leader), the candidate truncating
  its own log at election, and a vote's term set against an entry's term that is only a lower bound
  of the round it was accepted in. The fix above makes the classic comparison carry the round instead.

**A second defect the schedules found once the first was mended: a configuration not yet
applied.** With the rule above in, 40,000 schedules from seed 43,000 and from seed 200,000 each
failed once (seeds 54104 and 203544: "member 2 committed another entry at 11", "member 1 committed
another entry at 12"). Traced: the leader had applied a change that demoted a voter to a learner and
committed an index by three of its four voters, a fast quorum of four. One of those three had not
heard the change committed, so it counted by the five voters before it (a member campaigns by the
configuration it has applied, as raft-rs does); it was elected by itself and two members of the five
that held another entry, which was the most held among them. Raft's classic argument holds across a
change because majorities of two configurations one change apart meet; a fast quorum of the new
configuration need not be a fast quorum of the old one, and the guard the fast track had (no change
committed and not applied, no joint configuration, both at the leader) says nothing of the members.

The fix: a member that took an entry of the leader's term took the leader's commit with it (an
append commits to the lesser of the leader's commit and its own last entry), and that commit covers
the configuration the leader was elected under; a member campaigns only once it has applied every
change it has committed. So it counts by the configuration the leader was elected under or by one
the leader applied since. A member that took no entry of the term has an older last term than every
member of the fast quorum, which refuse it, and no majority of a configuration one change away
avoids three quarters of this one. The leader notes the voters it was elected under and the one
other set of voters a change since named (a joint configuration's two halves are the two sets), and
counts a fast quorum only where it is a fast quorum of each (`Raft::note_term_configuration`,
`Raft::note_term_change`, `Raft::fast_quorum_of_the_term`). After a change that names a third set it
commits by the classic quorum until its term ends. This assumes, as raft-rs does, that a member
writes a `Ready`'s entries and its hard state's commit durably together. The directed test is
`a_member_that_counts_by_the_configuration_before_commits_no_second_entry` (seed 54104).

**What it costs.** Fast commits that counted members whose logs were not yet of the leader's term
become classic ones, one round later: those at a new leader's first indexes, before its first append
is answered, chiefly the entries it recovered at its election, which Fast Paxos also commits by a
classic round. After a change of the configuration, a fast quorum must also be one of the voters
before it, until the next term; after a second change, there is none until the next term.

| Fast schedules | Fast commits, before | with the first rule | with both rules |
|---|---|---|---|
| 96 from seed 0 (the default) | 418 | 322 | 188 |
| 40,000 from seed 3,000 | fails at seed 9843 | 140,198 | 93,864 |

The schedules change leaders and configurations far more often than a group does in service (about
five terms and many changes in every 4,000 steps); in the comparison's fast workload, a group with
one leader and no change, the rules commit every proposal by the fast quorum as before
(`docs/benchmarks.md`).

**Why the model missed them, and what it checks now.** focal's model had no step by which a deposed
leader campaigns again with the log it led with, so no member whose log holds what no other took was
ever elected again, and the first defect was unreachable at any bound; it had no change of
configuration, so the second was outside it. focal added the step and the first rule (`7ea6f63`), and
the model came here with both (`docs/models/FastTrack.tla`), with the configuration a member counts
by (the one its committed log states), a change by one entry or through a joint configuration, and
the second rule. TLC finds each defect with its rule taken out and passes the same bounds with it in:
`FastTrackAnyRound.cfg` (four voters, three terms) is refused for `LeaderHolds` and
`FastTrackFour.cfg` passes, 3,207,204 states; `FastTrackAnyConfig.cfg` (three voters, of which a
change removes one; seed 54104's run with three voters for five) is refused for `LeaderHolds` and
`FastTrackChange.cfg` passes, 3,304,320 states. `docs/models/README.md` has every configuration,
its states and the run that counted them.

**Evidence.** The directed test `an_election_never_commits_a_second_entry_at_a_committed_index` runs
seed 9843's schedule with the rules it was found under (a round of reads for each read, no byte
bound, bare heartbeat answers); it fails without the fix and passes with it. `a_member_that_counts_by_the_configuration_before_commits_no_second_entry`
runs seed 54104's schedule; it fails without the second rule and passes with it. With both rules,
160,000 fast schedules of 4,000 steps pass, 40,000 from each of the seeds 3,000, 43,000, 100,000 and
200,000 (`HYPER_RAFT_SEEDS=40000 HYPER_RAFT_SEED=<seed> cargo test -p hyper-raft --release --test
fast a_group_with_the_fast_track`): every member commits the same entry at every index, every
answered read saw what was committed when it was asked, and every group settles.

**Open against the rules until R-3.** These values are carried unchanged and are literals, not
derivations:
- `Limits::default`: mantle note 32 §2.10;
- `MAX_MEMBERS`.

## 4. The crates that follow

| Crate | What | Steps and gate |
|---|---|---|
| `hyper-durable` | The shell, designed from all three projects' shells and the literature, not extracted from one: `docs/durable.md`, sources in `docs/research/durable.md`. Readies ahead of their persistence, the commit fence over the durable commit, protocol-aware repair, derived bounds. | The core steps it needs first (R-4 to R-7, `docs/durable.md` §11); then mantle (D-1), focal (D-2) and slates (X-1) onto it, each gated by its own suites' outcomes and measured against its own shell (`docs/durable.md` §12–§13). |
| `hyper-block` | mantle-disk's block layer, on which the log is written: `BlockFile`, the device file with its direct I/O and full flush, aligned buffers, group commit's wait, the device issuer, the simulated device (`crates/hyper-block/ORIGIN.md`). | Moved with L-1, from mantle `147f035`; on `main` once the `log` branch lands. |
| `hyper-log` | mantle's per-device log, with its `BlockFile` trait. It is the one crate that owns its device writer and files (`CLAUDE.md` §1). | **Done on branch `log`** (`crates/hyper-log/ORIGIN.md`): L-1 and L-2 as below, with one change of plan: the owner never waits on the device, so the log runs two threads, the owner and a device thread that owns the file, until its writes go through hyper-block's issuer. **L-1**: move it. Gate: mantle's `crates/log/tests/log.rs`, `fairness.rs` and the range simulation; bytes on the simulated device identical per seed. **L-2**: an owner thread in place of `Arc`, `RwLock`, `Mutex` and `Condvar`, and `Waker` tickets in place of the mpsc `Pending`. Gate: L-1's suite; a test that a completion wakes only its submitter; mantle's log benchmark against its recorded baseline; frame contents per seed identical. **F-1**: focal's WAL converted to it, one way. Gate: recorded focal data directories converted and read back equal, and focal's restart and restore runbooks on real processes. |
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
