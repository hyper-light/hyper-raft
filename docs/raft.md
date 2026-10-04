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
>
> Step R-5 is done (2026-10-02): a member whose log lost at rest what it acknowledged is repaired by
> its leader resending the lost entries, not a snapshot (§3, `docs/durable.md` §5.1).
>
> Step R-7 is done (2026-10-02): a marked member is elected on its log, without its own vote, by a
> quorum of the others that answer for no more than it holds (§3, `docs/durable.md` §5.2); the
> TLA+ model has the marked member, eight configurations at the states CI counted
> (`docs/models/README.md`).
>
> Timing step L-2 is done (2026-10-02): elections started by the owner's failure detectors, not by
> ticks (`Config::elections`, `docs/timing.md` §2.9).
>
> Step R-3 is done (2026-10-03, §3.2): slates' regression tests for R6, R7, R4, R5, R20 and R21
> pass; three defects at the end of what a member counts and a lease kept by a member's own timer,
> which their siblings found, are fixed; the window a member is sent ahead of its answers is one
> rule from focal's and slates' (R16); a member keeps what arrives ahead of a hole and acknowledges
> it with the write that holds it (R17); a learner is caught up in rounds before it is promoted
> (R13); a log is due for compaction by the thesis's rule, the shell's policy (R22); and every bound
> is derived from what the member's owner states, the members a configuration names among them
> (`Limits::derive`). mantle's and focal's suites run when each takes a snapshot of this crate.

## 1. What `hyper-raft` is

`hyper-raft` is the Raft core that slates, focal and mantle share. It is a state machine with no
clock, no disk and no network:
- `RawNode::tick` says that time has passed, for a member that elects on ticks (raft-rs's rule,
  kept for the differential); one that elects by suspicion (timing step L-2, `docs/timing.md` §2.9)
  is told what its owner's detectors believe of the other members (`suspect`, `trust`,
  `restarted`), given its group's measured `Timing`, and woken at its owner's clock (`wake`,
  `deadline`): an idle group is woken for nothing;
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
- pre-vote and check-quorum; and, by suspicion, campaigns started by the owner's failure detectors
  after the election law's draw, check-quorum from the detectors, a leader that beats only while
  its group has work in flight, and a lead that ends in its term handed over (`docs/timing.md`
  §2.9);
- election priority with `Precedence::Log`;
- learners and joint consensus (`ConfChangeV2`);
- leader transfer;
- an inflight window with conflict hints, bounded per member in bytes, each append charged its
  record (`Config::max_inflight_bytes`, `RawNode::set_inflight_bytes`: what the owner says the path
  to the member carries over the two round trips a lost append takes to repair, R16), and in
  messages unless the window counts none (`Config::max_inflight_msgs`); a window that filled sends
  again at room for a whole append or half of it; and a heartbeat's answer that says how far the member's
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
| **R-3** (done, 2026-10-03) | slates' enhancements, one commit each, in this order. First, regression tests for R6 and R7, which are expected to pass. Then tests for R4, R5, R20 and R21, with a patch only on failure. Then the R16 byte-bounded pipeline window, R17 out-of-order acknowledgement within a term, R13 learner catch-up rounds, and R22's compaction rule. `Limits::derive` replaces `Limits::default`. | Run, for each (§3.2, `crates/hyper-raft/ORIGIN.md`, "R-3"; `docs/benchmarks.md`): slates' tests on this core, each failing with its rule taken out; slates' recorded improvements reproduced (pipelining at 2,000 proposals a second across five regions, 125 / 127 ms against one batch out falling behind; reordered paths at 4,000 a second; Figure 4.4(a), 45 rounds without a commit against one round trip; three images against none at three compactions); the raft-rs differential unchanged, R17's divergence in §3.3 with its own tests; every suite of this repository, the simulations at 1,000 seeds; allocation counts on all 36 cells. Still to run: mantle's and focal's suites, each when it takes a snapshot. |

The R-numbers are note 32 §2.13's ledger.

The durable shell needs four more core steps, R-4 to R-7. Their design and gates are in
`docs/durable.md` §2.1, §4, §5 and §11.

| Step | What changes | Gate |
|---|---|---|
| **R-4** (done, 2026-10-02) | `Ready`s taken ahead of their persistence: `advance_issued`, `on_persist`, `on_persist_keeping`, `Limits::readies_in_flight`; the unstable log keeps its entries until they are durable, with an issue mark; etcd's term guard and raft-rs's `maybe_persist` against ABA; a leader sends at once only while its term and vote are durable; what a notice makes leaves with it only when nothing is out or unwritten (`docs/durable.md` §2.1, where each invariant is kept in §3). | Run: the raft-rs differential unchanged at depth one; the recorded-seed equivalence of the synchronous path; `tests/pipeline.rs` (random interleavings of proposals, ticks, deliveries and persistence steps at depths two and three, held to a durability oracle against each member's disk; a crash at every persistence step of a schedule in turn; the fast-track schedules at random lags); allocation counts identical on every workload; `benches/pipeline.rs` (`docs/benchmarks.md`, "Readies in flight"). raft-rs is no oracle at depth `k` (`docs/durable.md` §2.1). |
| **R-5** (done, 2026-10-02) | A member's mark (`Config::lost`, `Lost`; hyper-log's uncertainty mark) is the core's: it judges votes by it, takes no part in campaigns, holds no priority, and ends it by hyper-log's rule as its durable log reaches it. Marked, it answers a heartbeat or an append that counts entries past its log with a refusal flagged lost (`Message::lost`, flag bit 2 of §3.1); a leader whose progress counts them takes the member's `matched` back to the entry it names and resends from there, a snapshot only where it compacted them (`Raft::take_lost`, `Progress::lost`). CTRL's follower repair (`docs/durable.md` §5.1). hyper-durable's snapshot request (`ask_repair`) and its serving are gone. | Run: `tests/repair.rs` (lost entries resent exactly, once each, no snapshot; the fixed point without the flag; a snapshot only past the leader's compaction; a damaged entry before the last cut, marked and repaired); `tests/pipeline.rs` with faults at rest (a checksum beside every entry, verified at open; a bit flipped in the last write or an earlier one, the last writes lost though acknowledged), the durability oracle counting marks, every committed entry held as committed once settled, 1,000 schedules of 4,000 steps at four settings and the crash at every persistence step of 40 schedules with faults; every schedule without faults prints R-6's and L-2's counts exactly; the differential, group, fast and suspicion suites; golden vector of the lost refusal; hyper-durable's suites and `a_member_whose_last_writes_were_lost_at_rest_is_repaired_by_entries`; `docs/benchmarks.md`, "Repair by entries (R-5)". |
| **R-6** (done, 2026-10-02) | An apply pause (etcd's `applyingEntsPaused`): `RawNode::pause_apply`, `resume_apply`; a leader's own-term entries given to apply before its own write is durable (`Config::apply_unpersisted`, off by default as raft-rs's limit is zero); and the durable commit carried in answers: `MsgAppendResponse` and `MsgHeartbeatResponse` state no commit beyond the durable commit (`C_d`, `docs/durable.md` §4.1) when they leave, not `log.committed()`. The core knows `C_d` from each durable `Ready`'s hard state and from `RawNode::commit_durable`, by which the shell states every other commit it writes (`docs/durable.md` §4.4). The case found in mantle (`1c179e8`, its F17 commit fence): a member commits alone as leader in `advance_append`, then steps down in the same term (check-quorum), and holds a commit no write states; a shell's `configuration_known` must not count it. R-4 did not carry it: the core was told which writes are durable, not which commit a write stated. | Run: the directed tests `an_answer_states_no_commit_that_no_durable_write_stated` (mantle's case), `an_answer_a_notice_releases_states_the_durable_commit` (fails on R-4), `an_owner_that_pauses_apply_is_given_nothing_more`, `a_leader_applies_its_own_committed_entries_before_its_write_is_durable`; `tests/pipeline.rs` with the oracle holding every answer's commit to the sender's disk and the harness keeping the commit fence with the pause, at four settings (the fourth a leader applying before its write, its disk the slowest) and the crash at every persistence step with and without it; the raft-rs differential unchanged with no translation (`docs/durable.md` §4.4, "Against raft-rs"); allocations identical on every workload; `benches/pipeline.rs` (`docs/benchmarks.md`, "The durable commit and the apply pause (R-6)"). |
| **L-2** (timing, done, 2026-10-02) | Elections by suspicion: `Config::elections` (`Elections::Ticks`, raft-rs's, the default and the differential's; `Elections::Suspicion`), `RawNode::suspect`, `trust`, `restarted`, `set_timing`, `hold_campaigns`, `wake`, `deadline` (`src/watch.rs`; `docs/timing.md` §2.9 has the rules and their sources). On ticks nothing changes: every tick-path suite passes unchanged. | Run: the raft-rs differential, `tests/group.rs`, `tests/fast.rs` and `tests/pipeline.rs` on ticks, unchanged; `tests/suspicion.rs` (directed tests in time: the delay as the law's draw, a fresh draw at every arming, and each crash's first round split exactly when the law's event holds on the members' delays); `tests/pipeline.rs` by suspicion: the durability oracle at the four settings (10,000 schedules of 4,000 steps from seed 1,000, and 1,000 from 0), the fast track (2,000 schedules), the crash at every persistence step (20 seeds); counts in `crates/hyper-raft/ORIGIN.md`. |
| **R-7** (done, 2026-10-02) | CTRL's leader-side recovery with the suffix marks hyper-log keeps (`docs/durable.md` §5.2): a marked member campaigns on its log and its own vote is not counted (`Raft::campaign`), where the others can be a quorum of each half and not in a fast group (`Raft::may_campaign`); voters judge by their claims (R-5); elected, its mark ends (`Raft::become_leader`). The vote is CTRL's question for the whole lost range: a grant is `dontHave`, a refusal `have` or `haveFaulty`; `have`'s fix is the have-er's own election. The safety argument, the configurations and the marks it covers are §5.2's. | Run: `tests/repair.rs` (Figure 4(b) in suffix form: the only current logs marked, mantle's rule electing no one, which fails on R-5, and R-7 electing the one the others vouch for; a marked log that may lack a committed entry never elected, which fails with the candidate's own vote counted or the voters judging by their logs; a marked member of one or two voters asking no one); `tests/pipeline.rs` with faults at rest by suspicion as well as on ticks, and with two of three voters marked at once, the harness's `Cluster::electable` holding the rule; the schedules' waits before and after R-7; the TLA+ model's `Lose`, `Claim`, `Own` and eight configurations, three that pass and five refused, at the states CI counted; hyper-durable's suites with the shell's marked member campaigning; `docs/benchmarks.md`, "A marked member's election (R-7)". |

A campaign supersedes its own vote requests still waiting to be taken (2026-10-03, found by mantle's
D-1): a member whose writes stay out through many election timeouts, ticked meanwhile, sends at most
one campaign's requests for each write it had out and its last campaign's, not one campaign's for
every timeout (`crates/hyper-raft/ORIGIN.md`, "A campaign supersedes the requests still waiting").
The raft-rs differential compares unchanged. A member keeps a spare queue of messages for each
`Ready` whose write may be out (`Limits::readies_in_flight`), so the queues an owner gives back
once each write is durable are not dropped while others are out (2026-10-03, found by mantle's
D-1; `ORIGIN.md`, "A spare queue for each ready in flight").

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

- **Message body**: kind (1), flags (1: bit 0 reject, bit 1 snapshot present, bit 2 a refused
  append's answer from a member whose log lost what it acknowledged, core step R-5, bit 3 a refused
  append's answer from a member that kept the append ahead of a hole, R-3's R17; any other bit set
  is refused), then nine `u64` (to, from, term, log term, index, commit, commit term, request snapshot,
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

An owner that prices what a committed change will add before the core applies it reads the change
in place (`wire::changes_stated`): each change's kind and member from the entry's record, nothing
allocated, after the whole record is checked as decoding checks it (focal's memory accounting,
which priced raft-rs's encoding the same way before its core moved here).

### 3.2 slates' enhancements (R-3)

Each of note 32's R-numbers comes in as slates states it: its regression test run on this core, or
a rule designed from slates', focal's and the literature where this core lacked one. The record of
each, with the tests that failed before, is `crates/hyper-raft/ORIGIN.md`, "R-3".

**The end of what is counted (R6).** A message naming `u64::MAX` is beyond what is counted
(`counts_beyond_bound`), so the last term and the last index a member reaches are `u64::MAX - 1`
(`raft::LAST`). The step past either is refused before anything changes, never saturated (slates'
AUD-29-26: a term saturated at its last value let two leaders share it):
- a campaign is refused `Capacity("terms")` at the last term, and `Capacity("the log's indexes")`
  where the log has no index left for the entry a leader's term begins with (`Raft::lead_refusal`,
  asked in `Raft::hup` before anything moves); by suspicion no campaign is armed that it would
  refuse (`Raft::may_lead` in `deadline` and `wake_follower`), since an owner's wake that returned
  the refusal fences the replica;
- an append whose entries would run past the last index is refused whole, and a leader at the last
  index refuses every proposal and change;
- the fast track proposes and holds nothing at the last index (`track::proposable`,
  `Raft::propose_fast`): a leader that recovered such an entry at its election would have no index
  for its own first entry after it. With that and `lead_refusal`, a leader's first entry always has
  an index, and its append failing for room is a fatal `Invariant`, not a refusal.

**Independent election draws (R7).** On ticks each member draws its timeout from its own SplitMix64
stream at every reset (`Config::seed`); by suspicion every arming draws anew from hyper-timing's law
(`Watch::draw`). Two members that drew alike draw apart at their next campaign, whatever their
seeds: slates' `(id + attempt) mod span` kept members congruent modulo the span together for good.

**A lease lapses at the minimum election timeout (R4).** A member refuses a vote only within the
minimum election timeout of hearing its leader (thesis §4.2.3, etcd's `inLease`), whatever its own
timer does: a member that waits past its timeout (its owner's patience, slates' yield to a more
central voter) or whose campaign waits for a committed change to apply (`Raft::hup`; an owner's
commit fence makes that common at a leader's loss) grants the voter that campaigns. On ticks the
lease reads `Raft::silence`, the ticks since the member last heard its leader, apart from its own
election timer, which its campaigns restart; one counter for both kept such a member's lease alive.
By suspicion the lease is the detectors' trust in the leader, which no timer touches.

**Votes across a change (R5).** A member votes without consulting its own configuration (thesis
§4.1): one that missed its promotion grants the candidate whose configuration names it, and a
candidate counts only its own voters' grants (`Tracker::record_vote`). A learner's lease lapses as
a voter's does.

**Replication against compaction (R20, with R19's hints).** An append anchored below a member's
commit is answered with the commit and changes nothing (raft-rs's rule); a refusal carries the
conflict hint of §5.3, so an empty member or a stale term's run costs one refusal; progress never
moves back on a late answer; a snapshot's recipient is credited with what it says it holds.

**A proposal's cost at any backlog (R21).** The commit rule sorts the voters' matches
(`Tracker::quorum_index`) and the configuration is the tracker's, never a scan of the log: a
proposal costs a leader the same allocations at any backlog (`tests/backlog.rs`), and its time is
flat (`benches/backlog.rs`, `docs/benchmarks.md`).

**The window a member is sent ahead of its answers (R16).** One rule, of which focal's (F41,
focal 27 §11) and slates' (consensus-enhancements §3.5) are two readings: the bytes a leader keeps
in flight to a member are what the path to it carries over the two round trips a lost append takes
to repair (`hyper_timing::inflight_window`, `REPAIR_ROUND_TRIPS`). One round trip's carriage, the
path's bandwidth-delay product, keeps a path that loses nothing full; a lost append is answered
only once its resend is, a round trip for the refusal of the append after it to come back and
another for the resend's answer. focal reads the carriage as the transport's congestion window to
the member's node, and says twice it (as Linux's `tcp_sndbuf_expand` sizes a send buffer at twice
the window); slates' drive sends one batch a period, carries a batch for each period of a round
trip, and keeps ⌈2·tail/period⌉ batches (`ElectionTiming::window_budget`: the rule exactly on a
tail of whole periods, at most a batch more between them). hyper-durable takes the carriage from
its owner and gives the core the rule's window (`Replica::set_carriage`). What a member is sent
while a lost append is repaired is refused, as raft-rs refuses it, until R17 keeps it.

The core keeps the window in what the path carries: each append is charged its record, the
message's fixed bytes and its entries' (`wire::MESSAGE_RECORD_FIXED_BYTES`, 96), as RFC 9002 §B.2
counts a packet's bytes in flight. focal and slates counted the entries alone: for an append of one
entry of eight bytes, a quarter of what the path carries (33 of 129 bytes). A window may count no
messages of its own (`Config::max_inflight_msgs` of `usize::MAX`): its bytes bound it, for every
append costs at least its fixed bytes and an entry's, and its ring grows to what they admit. A fixed
count, focal's 128 or raft-rs's 256, is no measure of a path (note 32 R16); raft-rs's count is kept
for the differential, which runs with no byte bound. A window bounded by neither is refused.

A window that filled sends again once it has room for a whole append (its fixed bytes and
`max_size_per_msg` of entries) or half of it is answered, whichever comes first (`Inflights`): the
sender's avoidance of the silly window syndrome (Clark, RFC 813; RFC 1122 §4.2.3.4, which sends "if
a maximum-sized segment can be sent" or "at least a fraction Fs of the maximum window", `Fs` "a
fraction whose recommended value is 1/2"). Charged what the path carries, a window held at its bound
by a path that carries no more opens by one small answer at a time, and each opening draws one
small append, its fixed bytes beside one entry, so most of what the path carries is fixed bytes
(found by `tests/timed.rs`). Below its bound a window sends at once, as before: this is no Nagle's
rule (RFC 896), which would hold every small append behind an answer. A window of about two appends
or fewer, which no path the rule measures has, sends them in turn and waits for both. raft-rs's
rule frees a full window's first message at a heartbeat's answer and sends the next whatever the
window holds (`HeartbeatAnswers::Bare`), and keeps doing so.

Measured on slates' five Azure regions in time (`tests/timed.rs`, `docs/benchmarks.md`, "The window a
member is sent ahead of its answers (R16)"), on paths that keep order as slates' did: at 2,000
proposals a second one batch out at a time commits 1,023 a second at a 7,908 ms median (slates:
1,029 at 7,319 ms) and the rule every proposal at 125 / 127 ms median and p99 (slates' derived
window: 172 / 222 ms), 202.5 / 328 ms with 1 % loss (slates: 201 / 321 ms); at 4,000 a second the
rule commits every proposal at 126 / 127 ms. Against focal's window of 128 places the medians are
the same and the p99 lower (127 ms against 153.5 and 180), and with no count of places each proposal
goes at once as its own append, so this harness, whose owner proposes one entry a turn, sends twice
the bytes at 2,000 a second and four times at 4,000. On paths that reorder, a member refuses what
arrives ahead of an append still on its way, and 86–95 % of what a larger window sends is sent
again: R17.

**Out-of-order acknowledgement within a term (R17).** A member that receives a leader's append past
the end of its log refuses it, as Raft's consistency check does (§5.3), and keeps its entries beside
its log (`Ahead::Kept`, `crate::ahead`): as many as its log may hold not yet durable
(`Limits::unstable_entries`), those nearest the hole first. Once an append of the same term fills
the hole, the kept entries that continue it are taken into the log with it, as the leader's next
append would have carried them, and the answer to it acknowledges them all. This is slates' reading
of ParallelRaft-CE (Gu et al., IJSI 2021, after PolarFS's ParallelRaft, Cao et al., VLDB 2018 §5),
checked by its prefix model: acknowledgement out of order within one leader's term, commitment and
application in order; commitment out of order lost a committed entry in twelve steps and stays out
(note 32 R18).

What is kept is the leader of this term's own log: a leader never rewrites its log within its term,
so the entry it sent for an index is its entry there whenever it is taken, and the append that fills
the hole has checked the member's log against the leader's through its end. So this core keeps
whatever entries of the leader's log arrive, where slates kept only those of the leader's own term,
behind its sync rule: nothing here commits or applies out of order, which is what that rule guards.
Nothing kept is acknowledged before it is in the log: a refusal acknowledges nothing, and the answer
that covers a kept entry leaves with the write that holds it as a log entry (`docs/durable.md` I2
and §10: the window's slots are in the write before the acknowledgement), so they need no write of
their own, and a member that restarts has lost only resends. A change of term or role, or a snapshot,
forgets what was kept, and a marked member (R-5) keeps nothing. A member that takes what it kept is
where one append carrying the hole's entries and the kept ones would leave it, sent when the leader
sent the last of them and delivered late, as any append may be: the TLA+ model's `Replicate` stands
for it as it stands for a late append, and the model is unchanged (`docs/models/README.md`).
raft-rs's rule is `Ahead::Refused`, which the differential runs.

The leader's half. A member's refusal names the end of its log (its hint) and the append it
refused; where that append began past the end, a member that keeps what arrives ahead kept it, and
says so (`Message::kept`, bit 3 of §3.1). Its leader acts on the member's word, not on its own
setting: a member that keeps nothing (raft-rs's, or one of `Ahead::Refused`, in a group of both
cores or across an upgrade) is probed as raft-rs probes it. The group of both cores found this:
taking every refusal past a member's end for a kept append, a leader of this core marked arrived what
a raft-rs follower had dropped and never sent it again, and seed 40 did not settle whenever raft-rs's
own random timeouts made such a leader. The leader keeps a scoreboard in its window, as TCP's selective acknowledgement does (RFC 2018, RFC
6675): the kept append is in flight no more and leaves the window's bytes, keeping its place
(`Inflights::delivered`); every message sent before it and neither answered nor kept is a hole, lost
or late, and goes again, once (`Outbox::repair_before`, `Progress::repaired`), as RFC 6675 sends again
every segment its `IsLost` names once data sent after it arrived, and its pipe counts a
retransmission in the lost segment's place. Each goes anchored past what the member answered or was
sent again, so that a resend arriving ahead of an earlier hole is kept too, and every hole of a
window is repaired in the round trip its refusals arrive in. Where the window holds no message that
began there (it was emptied since), what follows the member's end goes again as far as the refused
append's start. A resend that goes a beat with no answer is probed from the member's match, as a
full window that goes unanswered is: it may itself have been lost. raft-rs's leader probes from the
member's match at every refusal and sends what followed again, which it must where its member kept
none of it. Where its member keeps it, that sending made the member's half cost more than it saved
on paths that reorder: with the member keeping and the leader probing, the group collapsed at 1,000
proposals a second, 99.5 % of what was sent sent again and 384 committed a second, where R16
committed every proposal at a 147 ms median. Counted in one run, most refusals were older than what
the leader knew the member held yet named an append past its match, so raft-rs's staleness rule took
each for a new hole, and each probe and the replication after it sent the window again. Two earlier
leader's halves were measured and rejected, on paths that reorder with 1 % loss at 2,000 a second:
one that sent a hole only at a refusal and once for each end of the member's log left every later
hole of a window waiting a beat (a 717 ms median against R16's 191); one that sent the next hole at
the answer filling the first repaired one hole a round trip, behind a loss every hundred entries, and
fell further behind (1,574 ms).

Measured on slates' five regions in time (`docs/benchmarks.md`, "What arrives ahead of a hole (R17)";
`tests/timed.rs`, `on_paths_that_reorder_what_arrives_ahead_is_not_sent_again`): on paths that
reorder, with no loss, the rule's window commits every proposal at 123 / 126 ms at 2,000 and at
4,000 a second, sending 38 % and 44 % of entries again, where R16 took 264 / 2,084 ms at 2,000 and
fell behind at 4,000 (1,588 a second), sending 95 % and 96 % again; with 1 % loss, 216 / 403 ms at
2,000 a second against 191 / 325.5 ms, with 42 % of the bytes. On paths that keep order with
1 % loss, 227 / 462 ms at 2,000 a second against 202.5 / 328 ms, 12 % of entries sent again against
31 %: raft-rs's sending of what followed a hole covers a second loss behind it by chance, at two and
a half times the resends, where the scoreboard repairs each loss on its own refusals; and this
harness sends each proposal as its own message, so a loss in a hundred messages is a hole every
hundred entries.

**A learner caught up in rounds (R13).** A member joins as a learner, which votes on nothing and
counts toward no quorum, and its owner promotes it once it has caught up (thesis §4.2.1, "Catching up
new servers": a voter added with an empty log can leave its group unable to commit until it catches
up, Figure 4.4(a)). `RawNode::catch_up(member)` says where catching it up stands: replication to it
goes in rounds, each to what the leader held when the round began; a round that lasts less than an
election is the last, and the learner is `Ready`; one that lasts longer begins the next with what
the leader holds then. A learner whose lag behind the leader did not shrink over a whole election is
given up, `Aborted`, said once (slates' rule for the thesis's "unavailable, or so slow that it will
never catch up", which needs no count of rounds where the thesis says "such as 10"); asked again it is
staged afresh. An election is the member's own measure of one: on ticks the minimum election timeout
(the thesis's "an election timeout"), by suspicion the time hyper-timing's law expects an election to
take (`Timing::election`, `T_E`), the gap the group already accepts at its leader's loss. Each
learner is judged alone, so one that cannot catch up holds back none that has (slates
`docs/bugs/2026-09-30-one-lagging-member-held-back-every-council-promotion.md`). The rounds are the
leader's alone: a leader that steps down forgets them (`crate::catchup`).

Measured on slates' replay of Figure 4.4(a) (`a_staged_newcomer_leaves_no_availability_gap_where_a_direct_one_does`):
voters 1, 2 and 3 hold forty entries, member 4 joins empty and voter 3 fails, two entries an append
and one out at a time as slates' drive sent them. Added directly, the group cannot commit for 45
rounds while 4 catches up (a round here carries messages one way; slates counted 21 round trips);
staged first, it commits in the first round trip after the loss, as slates measured.

**When a log is compacted (R22).** The shell's policy, not the core's (`docs/durable.md` §6.1): a
log is due once the applied entries it holds exceed the image it was last compacted to times the
owner's expansion factor (Ongaro's thesis §5.1.2), and a leader waits for a member that lacks what
it applied while the log holds no more than twice the threshold, reading the member's progress from
the tracker (slates `fold.rs`). slates measured a leader that compacted the moment a majority held
its entries sending its third voter an image at every compaction; replayed on the shell, three
images against none at the same three compactions (`docs/benchmarks.md`, "When a log is compacted
(R22)").

**Every bound from what the owner states (`Limits::derive`).** focal's bounds were literals carried
unchanged (65,536 messages and unstable entries, 4,096 reads, 16,384 entries a message, 256
proposals and a window of 256, `8 MiB − 64 KiB` of proposals, 64 MiB of votes; mantle note 32
§2.10), and a configuration named at most `MAX_MEMBERS`, 1,024. Each is now derived from what the
member's owner states (`Stated`): the largest message its transport carries, the members a
configuration of its group names, the bytes one of its queues may hold, and its store's depth.
- A message carries no more entries than its bytes past its fixed record hold at an entry's fixed
  bytes each (`entries_per_message`; `wire::ENTRY_FIXED_BYTES`, `MESSAGE_RECORD_FIXED_BYTES`).
- A member holds proposals of no more bytes than a message carries past its fixed record, so that
  its vote carries every one (`proposal_bytes`). They are counted resident (`crate::fast`), each at
  least an entry, so that no more are held than those bytes hold entries (`proposals`), and none is
  proposed further above the commit than a vote could carry (`fast_window`).
- A leader is told what each member holds, a message's bytes from each at the most (`vote_bytes`).
- Each queue holds what the stated memory holds of its least element: an entry for the entries not
  yet durable, a message's allowance for the messages not yet taken (`proto::MESSAGE_ALLOWANCE`), a
  read for the reads that wait. What their buffers hold is bounded besides: the window to each
  member, the bytes a leader holds uncommitted, the owner's budget.
- The writes out (`readies_in_flight`) and the members (`members`) are as stated.

A statement that admits nothing is refused: a message that carries no entry, memory that holds fewer
entries than one message carries (a member could not take a leader's message whole), no member.
The members bound is a configuration's, and every member of a group states the same, as each opens
with the same fast track. A leader proposes no change past it: the entry keeps its place and states
nothing, as a second change does while one waits. A member given a configuration past it, from its
log, a snapshot or its storage at open, stops, an `Invariant`, for its group's other members hold
the configuration and going on without it would leave this one in another. What a member counts
for each member (its progress, a read's askers and confirmations, the holders of a fast entry, the
members its detectors suspect) is held to the same bound. The tests check each derivation where
it shows: a message of `entries_per_message` empty entries fits the stated bytes and one more does
not, and a member that holds all the proposals it may has a vote that fits
(`every_bound_is_derived_from_what_the_owner_states`); a change past the members is not proposed
(`a_leader_proposes_no_change_past_the_members_a_configuration_names`); a configuration past them
stops the member (`a_configuration_past_the_members_a_member_was_opened_for_stops_it`).

### 3.3 Where this core and raft-rs differ

`tests/differential.rs` runs this core and raft-rs on one schedule and compares them field by field
(§1). Where the two decide differently it is by decision, and each decision is held by a test of its
own; where a schedule can reach one, the differential runs raft-rs's rule, which this core keeps for
that purpose, or loses for both cores the message that would reach it. focal 27 §4.5 began this
table for focal-raft; it lives here since the core moved (R-1), with every decision since.

| raft-rs 0.7 | This core | Test |
|---|---|---|
| Asserts; the shell contains the unwind and stops the replica | An error of one of three kinds: a refusal that changed nothing, a peer's message that contradicts the member, or a state that no longer adds up, which alone stops the replica (`Error`) | `src/tests.rs`: `what_a_peer_may_not_say_is_refused_and_changes_nothing`, `storage_that_fails_stops_no_one_and_is_said` |
| Its generated accessors unwind on an enumeration value they do not know, which a peer chooses | Its own types and wire format (R-2, §3.1): an unknown kind, flag or version is refused | `src/wire.rs`: `unknown_kinds_flags_and_versions_are_refused`, `arbitrary_bodies_never_panic` |
| A leader that applies a change which leaves it no voter leads on, and unwinds when it next commits | It tells the voter that holds the whole log to campaign, and follows | `tests/group.rs`: `a_leader_a_change_leaves_no_voter_hands_the_group_over_and_follows` |
| One told to campaign while a change it committed is not applied forgets it was told | It campaigns once the change is applied, unless it heard of a leader since | `one_told_to_campaign_before_it_applied_a_change_campaigns_once_it_has` |
| One told to campaign while it asks whether it could be elected ignores it, and the leader waits an election timeout | It campaigns; the differential loses that message for both cores | `one_told_to_campaign_while_it_asks_whether_it_could_does` |
| A voter refuses a candidate of lower priority unless the candidate has more entries | Unless the candidate's log is more current, by its last term and then its length (`Precedence::Log`); raft-rs's rule is kept as `Precedence::Length` | `tests/group.rs`: `priority_yields_to_a_log_that_is_more_current` |
| Priority judges the vote a transfer asks for; a member without a term, or one that left, may refuse for priority | Priority never judges a transfer, and is in force only for a member with a term that may campaign | `tests/group.rs`: `priority_orders_an_election_and_never_judges_a_transfer`, `a_member_that_has_no_term_refuses_no_one_for_priority`, `a_member_that_left_refuses_no_one_for_priority` |
| One that is no voter may campaign, and unwinds when it wins | Refused, `NotPromotable` | `only_a_voter_campaigns` |
| Election timeouts from the thread's generator | From a seed the owner gives (`Config::seed`): a run is its seed | `election_timeouts_are_drawn_from_the_seed` |
| Queues without a bound of their own | `Limits`, derived from what the owner states (`Limits::derive`, §3.2); a member takes of a message what it may hold, and answers with the last entry taken | `what_waits_to_be_taken_has_a_bound`, `what_is_not_durable_has_a_bound`, `reads_that_wait_have_a_bound`, `every_bound_is_derived_from_what_the_owner_states` |
| A leader proposes any change, and a configuration names any number of members | A leader proposes no change past the members a configuration of its group may name (`Limits::members`): the entry keeps its place and states nothing; a member given a configuration past them stops; the differential's groups stay within them | `a_leader_proposes_no_change_past_the_members_a_configuration_names`; `src/progress.rs`: `a_configuration_past_the_members_a_member_was_opened_for_stops_it` |
| A round of heartbeats for each read as it is asked, and again as it is asked again | One round when the member is next asked what there is to do, for every read since (`ReadRounds::Shared`, focal F43); raft-rs's rule is kept as `ReadRounds::Each` | `reads_asked_together_leave_in_one_round_and_one_answer_confirms_them`; `tests/group.rs`: `a_round_confirms_no_read_asked_after_it_left` |
| A window of messages alone | Of bytes, each append charged its record, what the owner says the path carries (`RawNode::set_inflight_bytes`, focal F41; R16's rule, §3.2), and of messages unless it counts none; a window that filled waits for room for a whole append or half of it; the differential runs with no byte bound | `a_member_is_sent_no_more_bytes_ahead_of_its_answers_than_its_path_carries`; `src/progress.rs`: `a_window_that_filled_waits_for_a_whole_append_or_half_of_it`; `tests/timed.rs` |
| A follower refuses an append that begins past the end of its log and keeps nothing; at the refusal its leader probes from the member's match and sends what followed again | It keeps the entries, as many as its log may hold not yet durable, and takes them in when an append of the same term fills the hole, acknowledging them with it; its refusal says it kept the append (`Message::kept`), and its leader takes what the member kept out of its window and sends every hole before it again, once, and probes only a resend a beat leaves unanswered (`Ahead::Kept`, R17, §3.2); raft-rs's rule is kept as `Ahead::Refused`, which the differential runs | `a_lost_append_costs_its_own_resend_and_what_was_kept_is_not_sent_again`, `a_refusal_sends_the_hole_alone_and_what_was_kept_leaves_the_window`, `a_hole_sent_again_and_unanswered_for_a_beat_is_probed`, `a_refusal_older_than_the_members_progress_sends_what_it_lacks_and_no_more`, `every_hole_before_what_was_kept_goes_again_at_once`, `a_member_that_keeps_nothing_ahead_is_probed_and_caught_up`, `what_was_kept_is_acknowledged_only_with_the_write_that_holds_it`; `src/wire.rs`: the kept refusal's golden vector |
| A heartbeat's answer says nothing of the log, and a full window frees its first message at every answer | The answer says how far the log goes and is taken as an append's answer (`HeartbeatAnswers::Position`, focal F42); raft-rs's rule is kept as `HeartbeatAnswers::Bare` | `a_heartbeats_answer_gives_back_what_the_member_holds_and_nothing_more` |
| A follower's lease reads its one election counter, which its own campaign restarts: one whose campaign waits for a committed change to apply keeps its lease another election timeout, and refuses the voter that campaigns | The lease reads the ticks since the member heard its leader (`Raft::silence`), apart from its own timer (R4, §3.2); the differential's owner applies every change at once and never reaches it | `a_member_whose_campaign_waits_for_a_change_holds_no_lease` |
| A term or an index may be counted to `u64::MAX` | The last of each is `u64::MAX − 1`; a campaign, an append or a proposal past it is refused before anything changes, by suspicion no campaign is armed that would be, and the fast track holds nothing at the last index (R6, §3.2) | `a_term_with_no_successor_cannot_campaign_and_keeps_one_leader`, `a_member_with_no_index_for_a_leaders_first_entry_does_not_campaign`, `by_suspicion_a_member_with_no_successor_is_due_for_no_campaign`, `the_fast_track_proposes_and_holds_nothing_at_the_last_index` |

Kept although it could be otherwise, as focal kept it: a member that a change removes and adds
again is known anew, and a member added by a change is first probed one entry before the log's
end. Both are raft-rs's, harmless, and keep the comparison free of an exception for them.

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

## 4. The crates that follow

| Crate | What | Steps and gate |
|---|---|---|
| `hyper-durable` | The shell, designed from all three projects' shells and the literature, not extracted from one: `docs/durable.md`, sources in `docs/research/durable.md`. Readies ahead of their persistence, the commit fence over the durable commit, protocol-aware repair, derived bounds. | The core steps it needs first (R-4 to R-7, `docs/durable.md` §11); then mantle (D-1), focal (D-2) and slates (X-1) onto it, each gated by its own suites' outcomes and measured against its own shell (`docs/durable.md` §12–§13). |
| `hyper-block` | mantle-disk's block layer, on which the log is written: `BlockFile`, the device file with its direct I/O and full flush, aligned buffers, group commit's wait, the device issuer, the simulated device (`crates/hyper-block/ORIGIN.md`). | Moved with L-1, from mantle `147f035`; on `main` once the `log` branch lands. |
| `hyper-log` | mantle's per-device log, with its `BlockFile` trait. It is the one crate that owns its device writer and files (`CLAUDE.md` §1). | **Done on branch `log`** (`crates/hyper-log/ORIGIN.md`): L-1 and L-2 as below, with one change of plan: the owner never waits on the device, so the log runs two threads, the owner and a device thread that owns the file, until its writes go through hyper-block's issuer. **L-1**: move it. Gate: mantle's `crates/log/tests/log.rs`, `fairness.rs` and the range simulation; bytes on the simulated device identical per seed. **L-2**: an owner thread in place of `Arc`, `RwLock`, `Mutex` and `Condvar`, and `Waker` tickets in place of the mpsc `Pending`. Gate: L-1's suite; a test that a completion wakes only its submitter; mantle's log benchmark against its recorded baseline; frame contents per seed identical. **F-1**: focal's WAL converted to it, one way. Gate: recorded focal data directories converted and read back equal, and focal's restart and restore runbooks on real processes. |
| `hyper-sim`, `hyper-check` | The deterministic simulation and the checks every crate's tests and every consumer's simulations run on (§5, `docs/sim.md`). | Steps S-1 to S-8 (`docs/sim.md` §9); this core's harnesses move at S-6; its known defects (seeds 9843 and 54104) become planted mutants each strategy must catch. |
| `hyper-multilog` | slates' MLRaft layer: one group's log divided into `n` logs with barriers (note 32 R25). | Moved with X-1. Gate: slates' `tests/multilog*.rs` and its explorer (200 seeds × 3,000 steps), retargeted at this crate. No consumer enables it until an owner shows a gain; slates measured it worse for its own groups. |

slates' consensus moves onto `hyper-raft` in **X-1**. Gate: slates' explorer, conformance, prevote,
priority, wan_election, pipelining and fast_track suites; the prefix and slot models; the KIND
succession lane; slates' N=1 differential. Its groups are re-founded on a fresh start, since slates has
no release to keep (owner's decision 9).

## 5. Shared test infrastructure the core will use

The allocation-count bench (focal-memory's counting global allocator) came back as `hyper-measure`,
rewritten without its lock and with its `unsafe` listed in the contract script; with it,
`hyper-raft-compare` measures this crate against each core it replaces, and `hyper-raft-e2e` runs it
as real processes (`docs/benchmarks.md`).

**The harness's receive.** A member of `hyper-raft-e2e` or `hyper-durable-e2e`, and each test that
asks one, waits for a datagram by peeking with a timeout (`wire::arrives`) and takes what the peek
found without waiting; it never waits in a receive. On Windows a receive that times out can lose the
datagram that arrives as it times out. Microsoft's `setsockopt` reference says of `SO_RCVTIMEO`: "If
a blocking receive call times out, the socket is left in an indeterminate state, and should not be
used; TCP sockets in this state have a potential for data loss, since the operation could be
canceled at the same moment the operation was to be completed." Measured on GitHub's windows-11-arm
runner (2026-10-02, 20,000 numbered datagrams a run, each sent at a random moment and acknowledged,
the receiver waiting a millisecond at a time): the receive lost 46 and 19 of them, 65 of 40,000 over
4,664 waits that timed out; the peek lost none of 40,000 over 3,799. ubuntu-24.04-arm lost none
either way (20,023 and 20,062 timeouts), and windows-2025, whose waits of a millisecond never timed
out before the next datagram came, none. In `hyper-durable-e2e` the receive lost the test's asks:
each one lost cost the test its whole retransmission timeout, one second, which is its quiet period
at the start of a scenario, so the wait gave up while the members moved (the stall scenarios failed
18 of 26 runs there, and the members' records of the asks they took showed that no timed-out ask had
reached its member); it lost heartbeats, wakes and Raft messages too. A peek takes no error either:
after a send to a closed port, both Windows runners' peeks reported the reset three times in a row,
and the receive after it took it, the next receive finding nothing; so whatever the peek found, a
datagram or an error, is taken by a receive that does not wait (`wire::take`), never by peeking
again (a first form of this change peeked again, and spun on the reset to the end of each wait: on
both Windows runners the E2E did not finish within fifteen minutes once a member had exited).
That no wait in the workspace is a timed receive the lint holds: `clippy.toml` disallows
`UdpSocket::set_read_timeout`, and each site that sets one states why its wait cannot lose a
datagram (a peek: `wire::arrives`, the real-socket tests of `hyper-transport`, `hyper-swim` and
`hyper-datagram`; or a socket whose receives drop what arrives). A test that sent 4,251 datagrams
to catch a loss with a picked probability is gone: a peek removes nothing, so none is lost to a
cancelled one, and the count measured nothing the lint does not hold. `tests/arrives.rs` holds the
taking of a reset and the datagram behind it.

The simulation and the checks are two crates, designed in `docs/sim.md` (sources in
`docs/research/sim.md`), not yet built:
- **`hyper-sim`**, the world under a test's control: one seeded generator with a stream per source,
  virtual time and node clocks, a network of focal's and slates' path model with VOPR's partitions,
  a write-queue device and a block device with the storage faults of the literature (lost and
  misdirected writes, torn sectors, failed flushes), processes that crash, pause and restart, and
  forks of a world.
- **`hyper-check`**: the oracles every project checks today, made one set, among them this core's
  durability oracle (`tests/support/lagged.rs`, generalized); the witness and search linearizability
  checkers; liveness bounds derived from the election law; and the strategies — random under swarm
  configurations, PCT, coverage over this core's TLA+ action map, slates' exhaustive search.

This crate's harnesses (`tests/support`, `pipeline.rs`, `group.rs`, `fast.rs`, the differential)
move onto them at step S-6 (`docs/sim.md` §8–§9), gated by outcome over the same seed counts.
Both crates are held to the production lints, as focal requires of `focal-sim`.
