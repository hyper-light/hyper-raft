# hyper-durable: the durable shell around the Raft core

> Status (2026-10-02): the shell's D-1 core is built (`crates/hyper-durable`, §15) on core steps
> R-4 (§2.1), R-5 (§5.1), R-6 (§4.4) and R-7 (§5.2).
> Sources and what each establishes are in
> `docs/research/durable.md` ("research §n"). This replaces `docs/raft.md` §4's plan to extract
> mantle's replica: the shell is designed from all three projects' shells and the literature, then
> built and measured against each of them (`CLAUDE.md` §1a).

The core (`crates/hyper-raft`) decides; it never writes, sends or applies. A shell turns what the
core asks for into durable log writes, messages, applied entries and answers, in the order Raft's
safety needs, and turns what the log and the network report back into calls on the core. Three
shells do this today, each answering the same question with a different medium and different
rules: mantle's `Replica` over its shared device log, focal's `DurableNode` over its shared WAL, and
slates' whole-state publication to its anchor's memory (research §6). `hyper-durable` is one shell
for all three, generic over the log and the state machine, whose release rules are stated as
invariants, not as modes.

## 1. The three shells, and what each gets wrong

| | mantle `Replica` (origin/dev `1275bf8`) | focal `DurableNode` (`7a6170e`) | slates publication (`ec5e0df`) |
|---|---|---|---|
| Medium | hyper-log `GroupLog`, one log per device, group commit, persist records, confirmation by a later record | focal-log shared WAL; each group commit is data, fence file and a rename's directory entry | `SavedRaft` re-encoded and BLAKE3-hashed into one of two anchor-memory slots |
| Readies out a group | one | one | none: the publication is synchronous |
| Inputs while a write is out | held: messages to a window of appends a member, ticks to `2·election_tick` | refused `PersistencePending`; the host queues | not applicable |
| Leader's appends | sent before its own write (thesis §10.2.1) | sent early since F17 (`sendable`) | after the publication |
| Commit | volatile; restart writes the engine's applied index into the log | volatile, rides the next record; written after a quiet period; logged before a configuration change or anything in a control group applies | in the publication |
| Repair | uncertainty mark from the persist record; votes judged against it; asks for a snapshot reaching it; damaged group rebuilt under a new identity | a log damaged before its fence does not open; member replaced | publication refused when its hash differs |
| Accounting | none | memory budget reserved before every transition; staging, retained log and output charged | proposal refused when the record's region would overflow |

Known defects and costs, each from the project's own record or from reading its code:

- **mantle.** An answered update waits for two flushes, its frame's and the record that confirms
  it, because one `Ready` is out at a time: a closed-loop append went from 4.33 to 8.65 ms
  (mantle `docs/measurements/2026-09-29-log-confirmation.md`). Proposals, reads and campaigns are
  refused `Stalled` while a `Ready` is out (replica.md §7). Held ticks replay a burst of timer
  firings after a stall; focal records that a member replaying them "campaigns for a leader that
  was only as slow as itself" (focal 27 §9). Reads wait for the round out before their own leaves,
  up to a round trip more each (focal 27 §10). Repair ships a whole snapshot where CTRL ships the
  lost entries (replica.md §7; research §5). A damaged frame that is not the last fails the whole
  log. Read against focal's F17 (research §4): committed configuration changes are applied on a
  volatile commit (`Replica::apply`, `changed`), and a replacement ends when every voter reports a
  commit (`configuration_known`) that is itself volatile. The first test of D-1 (§11) decides
  whether a kill inside that window leaves a group that cannot elect.
- **focal.** Every mutation is refused while a write is out, so each owner keeps an ingress queue of
  its own. The retained log is all in memory (`RamLog`). A group commit is three device flushes, an
  open item in focal 27 §9. Measured: one voter 12.8 ms an entry, three voters on one disk 36.2 ms
  median and 28,880 entries a second (research §4).
- **slates.** A transition costs the retained record's bytes, about 0.8 ns a byte, 70 µs at 1,000
  entries and 3.1 ms at 50,000 (research §6), bounded only because compaction keeps the record
  small; its durability is a daemon crash's, not a power loss's, by design of the anchor.

## 2. The pipeline

### 2.1 Readies ahead of their persistence (core step R-4)

Every defect of mantle's and focal's above has one cause: the core takes no call while a `Ready` is
out (`RawNode::operate`), so a shell must either hold what arrives (mantle) or refuse it (focal),
and a group's next write cannot start until its last is answered, which on hyper-log is two
flushes. etcd's and raft-rs's cores both lift that restriction (research §3): a core gives the next
`Ready` before the last is durable, keeps stepping messages and taking proposals meanwhile, and is
told, in order, which `Ready` became durable. CockroachDB measured 7–38 % lower average and 9–44 %
lower p99 latency from 1,000 to 32,000 writes a second, and etcd's rafttoy 29 % lower latency and
73 % more throughput at saturation (research §3). On hyper-log it also removes mantle's second
flush under load, since the next frame's persist record confirms the last.

hyper-durable is built on that core, which is a core change, **R-4**. It is in
(`crates/hyper-raft/src/node.rs`, `src/log.rs`); as built:

- **The calls.** `RawNode::ready` and `ready_in_place` may be taken while earlier `Ready`s' writes
  are out, up to `Limits::readies_in_flight` of them (one by default, the owner that finishes each
  write before it takes the next; the shell sets its store's depth, §6); one more is refused
  `Capacity` and changes nothing. A `Ready` taken is issued by `RawNode::advance_issued(ready)`:
  until then the member takes no operation, as before, and `RawNode::to_persist` reads what it
  gives. `RawNode::on_persist(number)` (and `on_persist_keeping(number, keep)`) says every `Ready`
  through `number` is durable, in the order issued, and returns what follows (a `LightReady`).
  `RawNode::in_flight` says how many are out. `advance_append` and `advance_append_keeping` are
  `advance_issued` and `on_persist` of the same number, so every existing caller and the raft-rs
  differential are unchanged.
- **The unstable log keeps its entries until they are durable** (etcd's rule, research §3), with
  a mark for how far writes were issued (`Unstable::issued`, and whether its snapshot's was); a
  `Ready` gives only what is past the mark, and a replacement below the mark moves it back. At a
  notice, what the write vouches for leaves (`Log::take_stable_to`, `Log::take_stable_snapshot`);
  `ready_in_place` and `on_persist_keeping` keep their zero-copy contract, the entries given up at
  the notice. An issued write remembers the last entry not yet durable when it was issued: every
  entry before it was given by it or an earlier one, so all are durable with it.
- **ABA, both guards.** etcd's: an issued write remembers the term it was issued in, and a notice
  heard in a later term makes no entry or snapshot durable. Liveness needs nothing more: every change of term
  is a write of its own (the hard state), whose notice vouches for every entry before it. Then
  raft-rs's: what a notice names leaves only where the unstable log still holds it past its offset
  (`Log::take_stable_to`), and the durable index moves only below the first index not durable and
  where storage holds the term (`Log::maybe_persist`). The directed test
  (`a_notice_heard_in_a_later_term_makes_nothing_durable`, etcd's five-step schedule) fails with the
  term guard taken out.
- **What leaves when** (I1, I2, I7; refined from the plan, which attached answers to the write that
  made them). Every message of a member that does not lead is a persisted message of the `Ready`
  that takes it, as before; that write holds, with every earlier one, all the member held when the
  messages were made. A leader's leave at once only while the term and vote it leads in are durable
  (new: etcd's rule that no message leaves before the latest hard state is durable; it is reached by
  a sole voter elected with learners, whose term rides the same `Ready`). What a notice makes (a
  leader's commit, the fast track's word that a member holds a proposal) leaves with the notice only
  when the member leads with a durable term and vote, or nothing is out or left unwritten; otherwise
  it waits in the queue for the next `Ready` and leaves as that one's messages do.
- **A leader counts itself toward a commit only at the notice** (I3; thesis §10.2.1, as before), and
  may commit earlier by a majority of followers. It need not have a durable log to be elected: a
  sole voter is elected while its writes are out (`Raft::become_leader` no longer refuses that).

**The gate as run.** The raft-rs differential unchanged at depth one (every campaign of
`tests/differential.rs`). At depth `k` raft-rs is no oracle: it moves entries to storage when a
write is issued and has no term guard, so at the schedules the guard is for it counts durable what
a write still out will overwrite, and the two cores disagree there by design. Depth `k` is held
instead to a durability oracle against each member's disk at every step (`tests/pipeline.rs`, §3
below), over schedules that interleave persistence steps (take and issue, a write durable, a notice)
with everything else, crashes at every persistence step of a schedule in turn, and the fast-track
schedules with readies persisted at random lags. The TLA+ model (`docs/models/README.md`) is
unchanged: its member acts atomically on durable state, and R-4 keeps every output behind the
durability it depends on (I1, I2, I7), so a run with writes out maps to a run of the model in which a
member's step happens when the write that carries it is durable, and a crash that loses writes out
is the model's crash. A pending write as a state of its own is not modelled; adding it recounts every
configuration's budget in CI.

### 2.2 One `drive`

The shell has one non-blocking entry point for work, `drive`, called by the owner when it stepped
something into a replica or when a write completion woke it. Each call does, in order:

1. **Take completions.** The log's answers for this group, in submission order. For each durable
   write: release its persisted messages; tell the core `on_persist(number)`; record the commit the
   write stated (§4), which the core reads from the `Ready`'s hard state itself, and is told with
   `commit_durable` for a write the shell made beyond it (§4.4). A failed write fences the replica (§2.4). Completions are taken only here, and
   a write submitted in this call is never polled in it: whether a flush happened to finish
   microseconds after submission must not decide what a call gives out (mantle's determinism
   finding, replica.md §5).
2. **Apply** what the core gives to apply and the commit fence allows (§4), answering each entry's
   callers: one page a drive at most, the core's committed page (`max_committed_size_per_ready`,
   entries counted as the core counts them) or one entry larger than it, whatever the writes
   answered in step 1 gave. What is committed past the page waits as entries behind the fence do,
   with the core's apply paused (§4.4), and for the next drive only: the drive says it is due
   (`Driven::more`). So what one drive hands the state machine is bounded as one `Ready` is (§7's
   quantum), and an owner reserves a page and an entry for it before the drive (focal's hand-over
   machine, focal 27 §15.7).
3. **Take a `Ready`**, if the core has one and the group has fewer than the log's depth of writes
   out (§6): give out at once the messages that answer for no write (a leader's appends and
   heartbeats, snapshots, confirmed reads); attach the rest to the write; lay out the write
   (snapshot point, entries, hard state, fast-track proposals, commit, §2.3) and submit it with the
   owner's waker; `advance_issued`.
4. Repeat 2–3 while the core has a `Ready` and depth allows, up to the owner's quantum (§7).

Nothing is held between calls but the writes out, each with what waits for it.

### 2.3 What one write holds, and in what order

One write is one log update: the snapshot's point, the entries, the fast-track proposals, then the
hard state and the commit. Written in parts when it exceeds a frame (`GroupLog::parts`), entries
first and hard state last (mantle replica.md §3), so a crash between parts leaves only entries no
one acknowledged; the write is durable once every part is, and each part counts against the
group's depth (§6). A snapshot's state is made durable by the
state machine before the write that moves the log's start to it (mantle's rule: the log never
starts past what the state machine holds), and an install the log never recorded is finished at
open. The snapshot a leader sends a member behind its log's start is the state machine's image,
at a point it applied, with the configuration the group held there (research §1, the Raft paper's
§7): a machine that keeps its owner's checkpoints, as focal's does, serves the latest, behind what
it applied, and the member applies the changes after it from the log.

### 2.4 Failures

A write refused for room another write frees (`Backlog`, `Full`, `TooManyGroups`, `Busy`) waits,
whole, with the writes after it untaken (hyper-log refuses each one sent behind it, `Behind`), and
the replica refuses calls `Stalled` until room may have been freed: its own compaction durable, or
its owner's word (`Replica::resume`); made again before, the writes would only be refused again, a
refusal a drive. They are made again as one write of everything the core holds not yet durable,
the fast track's proposals among it (`RawNode::issued_proposals`: the core keeps those of every
write issued until its notice), whose notice is the last refused `Ready`'s: a member that cannot persist takes no part (mantle, audit S04). A
snapshot the core took meanwhile, which no `Ready` gave, is not in it, nor the entries after it: the
state machine has not installed it, and the log would start past the state machine (I8); its own
`Ready` installs it and writes it, and the write made again states no commit past what earlier
writes hold. A
snapshot report that arrives meanwhile is kept, the latest per member, since replication to that
member pauses until its fate is known (a mantle simulation seed found a lost report pausing it for
good).

A write the store holds for its owner (`Fault::Held`) waits the same way, whole. It is not a
refusal for room: the store refuses it until its owner meets a precondition of the store's own,
which the store names in its own type (`LogStore::Hold`, read through `Replica::held`). focal's
store holds the first entry that needs a successor decoder until the group's record of that floor
is durable (focal 27 §15.5, O2). The owner meets the precondition outside the log and gives the
store what it met (`Replica::release`), which resumes the replica. The owner may learn of a hold
from the store as the write is submitted and release it before the shell has taken the refusal:
the release is kept then, for the store holds nothing when the refusal is taken, and the writes
are made again at the next drive. The shell counts such writes (`Writes::held`). A store that holds a write refuses every write submitted behind it
(`Fault::Behind`), as hyper-log does behind a refusal, so no later write is durable before the
held one. Nothing that depends on the held write leaves before it is durable (I1, I2), for it is
one of the refused writes made again.

Any other failure fences the replica: a failed flush leaves the device's contents unknown
and the page cache marked clean (Rebello et al., research §5), so the shell drops every write out
with what waited for it, answers every call `Fenced`, and the node reopens the log and the member
from what is durable.

## 3. The invariants

Stated over a member's durable state `D` (what a restart would read) at the moment an output leaves.

- **I1 (promises).** A message that carries or depends on a term or vote leaves only once `D` holds
  that term and vote (thesis §3.8). A candidate's requests and every vote leave after their hard
  state is durable, and a candidate's request names only a last entry `D` holds.
- **I2 (acknowledgements).** A follower's acknowledgement of index `i` leaves only once `D` holds
  every entry through `i` with the terms the acknowledgement names. The fast track's "held" answer
  for a proposal leaves only once `D` holds the proposal (focal 27 §4.4).
- **I3 (self-count).** A leader counts itself toward the commit of `i` only once `D` holds `i`
  (thesis §10.2.1); it may commit `i` earlier by a majority of followers ("still safe", §10.2.1).
- **I4 (apply).** An entry is applied only once committed. It need not be in `D` (§4.2).
- **I5 (restart-acted state).** A change of configuration, and any entry its state machine says a
  member acts on at its next start, is applied only once `D` states a commit covering it (§4.1).
- **I6 (answers).** A client's answer leaves only once its entry is applied, which by I4 is after
  its commit: a commit is durable on a quorum whether or not this member's record of it is.
- **I7 (order).** Writes become durable in submission order, and a write's persisted messages leave
  after every earlier write's. A commit a write states names only entries that write or an earlier
  one holds.
- **I8 (start).** The log's start never passes what the state machine holds durably, and the state
  machine never opens past what the group committed without the log being told (§4.3).

I1–I3 and I7 are etcd's and raft-rs's contracts (research §3); I5 is focal's F17 (research §4); I8 is
mantle's.

Where each is kept, as of R-4:

| | Enforced | Tested |
|---|---|---|
| I1 | `RawNode::ready_given` (a leader's messages leave at once only with its term and vote durable; every other member's wait for the write); `RawNode::releases_now` for what a notice makes | `tests/support/lagged.rs`, `check_message` and `Lagged::take`, at every write's durability and every release; `a_leader_sends_nothing_at_once_before_its_term_is_durable` |
| I2 | the same, and the write that takes a message holds everything the member held when it was made | `check_message` (acknowledgements, the fast track's word); `a_followers_answer_waits_for_the_write_that_holds_what_it_says` |
| I3 | `Raft::on_persist_entries` moves a leader's own progress, reached only from a notice; `Raft::reset` starts it at what is durable | `Cluster::check_durable` (its own entry on its disk; each commit held durably by a majority of each half of its configuration); `readies_are_taken_while_writes_are_out` |
| I4 | `Log::next_entries_since` and `next_range_since` give only what is committed and durable here, or a leader's own committed entries where it applies before its write is durable (`Log::unpersisted_after`, §4.4) | `Lagged::apply`, every entry given to apply; `a_leader_applies_its_own_committed_entries_before_its_write_is_durable` |
| I5 | the shell's commit fence (§4.1), with the core's apply pause (`RawNode::pause_apply`) and durable commit (`RawNode::durable_commit`, `commit_durable`), R-6 | `Lagged::release`: the schedules' harness keeps the fence as a shell does, stating a commit only as `Ready`s give one, holding a change and every entry after it until its disk states a commit covering it, and paused meanwhile. Before R-6 it recorded the commit before every apply; without that, a 4,000-step schedule (seed 19, `random_interleavings_keep_every_invariant` at three writes out) reproduced §4.1's hole, a sole voter that restarted without its commit reverting to the configuration before changes it had applied, electing itself by it, and leaving a group that cannot elect. `an_owner_that_pauses_apply_is_given_nothing_more` |
| Answers (R-6) | `node::state_durable_commit`: an answer to an append or a heartbeat states no commit beyond what is durable when it leaves | `check_commit` at every release; `an_answer_a_notice_releases_states_the_durable_commit` (fails on R-4); `an_answer_states_no_commit_that_no_durable_write_stated` (mantle's case) |
| I6, I8 | the shell's (§4; D-1) | §15's table |
| I7 | `RawNode::on_persist` takes notices in issue order; a `Ready` gives only what no earlier one gave | `Lagged::make_durable`: once a write is durable the disk holds the term, vote and log the member held when it took the `Ready` |


Where the shell keeps each, as built (D-1, `crates/hyper-durable`; the oracle and its checks are
`tests/support/cluster.rs`, run by `tests/sim.rs`):

| | Enforced in the shell | Tested |
|---|---|---|
| I1 | a `Ready`'s persisted messages leave with its write's answer, answers taken in submission order (`Replica::take_answers`, `durable`); a write refused for room releases nothing until it is made again (`refused`, `make_again`); a marked member's refusal flagged lost is a persisted message of the core's like any other (R-5) | `check_promises` on every release, against the sender's disk; mutated to release a `Ready`'s persisted messages at once, the schedules fail ("MsgRequestVote of term 1 left with term 0 durable") |
| I2 | the same | `check_acknowledgement` on every release |
| I3 | the core's; the shell tells it a write is durable only from that write's answer | `check_leader_commit`: every commit new to the group, held by a majority of each half of the configuration that decided it, through steps in which the leader stepped down |
| I4 | the shell applies only what the core gives (`walk`), reading a leader's own entries past the store where the core holds them (`Config::apply_unpersisted`) | `check_applied`: every member applies the same entry at an index; `a_leader_applies_its_own_term_before_its_write_is_durable` |
| I5 | `Replica::walk` stops at a change or an entry `StateMachine::acts_at_start` past `C_d`, holds the page (`behind_fence`) and pauses the core (`pause_apply`); `Ready`s go on and state the commit, or a write of the commit alone does (`commit_write`); `release_fence` applies once `C_d` covers it | `check_applied` at every drive; `Cluster::restart`: every entry acted on before a crash is applied again from the member's own durable state before it hears from anyone; `tests/directed.rs`, the four cases of mantle `1c179e8` and focal's two F17 cases with the power cut at every device operation in turn, all of which fail with the fence taken out |
| I6 | answers are what `StateMachine::apply` returns as it applies; reads leave once applied through their index (`release_reads`) | `reads_past_the_bound_are_refused`; the schedules' reads |
| I7 | every write states the core's commit, or the last entry where the member decides alone; a write of the commit alone states no more than the entries written (`written_through`); a commit stated past what the log holds once the write is durable fences (`durable`); hyper-log refuses writes sent behind a refused one (`LogError::Behind`) | `Cluster::durable`: once a write is durable its disk's commit is an entry it holds; a 600-seed soak found a commit-only write stating entries not yet written, fixed by `written_through` |
| I8 | `install` before the write that moves the start; a compaction never passes the state machine's durable index, what the log holds, nor a start still out (`compact`); `repair_at_open` | `check_start` after every step; `an_install_the_log_never_recorded_is_finished_at_open`, `a_state_machine_past_the_logs_commit_raises_it`, `a_state_machine_past_the_logs_last_entry_moves_its_start`, `a_log_that_starts_past_the_state_machine_does_not_open` |

## 4. The commit fence

### 4.1 Apply only on a commit the log holds, where a restart acts on it

A member's commit is volatile (thesis §3.8) and is re-learned from its group after a restart. In a
core whose configuration takes effect when applied (hyper-raft's, as etcd's), the configuration
reverts with it, and a member that restarted without the commit counts the members the change
removed: two voters of which one was removed and stopped leave one that cannot elect itself
(focal's `cli_network`; etcd's doc states the same hole for two-member removal, research §3). And a
member that acts on applied state at its next start, before its group tells it anything, acts on
state that ran ahead of what it will reopen with (focal's `cli_upgrade`).

The shell keeps `C_d`, the durable commit: the greatest of the commit the last durable write stated
and the index the state machine reports durable (`StateMachine::durable`). Then:

- An ordinary entry is applied on the core's commit.
- A change of configuration, and an entry the state machine marks as acted on at start
  (`StateMachine::acts_at_start`; a whole group says so for every entry, as focal's control groups
  do), is applied only once `C_d` covers it. Until then the shell holds that `Ready`'s committed
  page and asks the core for no more (core step R-6, `RawNode::pause_apply`, an apply pause like
  etcd's `applyingEntsPaused`), and makes sure a write states the commit: the next `Ready`'s write
  carries it, or, when none is due, a write of the hard state alone, which the shell then tells the
  core (`RawNode::commit_durable`). Configuration changes are rare: the
  cost is one flush each (focal 27 §9).
- A member campaigns only once it has applied every change it has committed (the core's rule), so a
  member holding a committed change behind the fence does not campaign by the configuration before
  it.

For a fenced entry `C_d` is always the logged commit: the state machine cannot be durable past an
entry it has not applied. And the logged commit names only entries the same or an earlier write
holds (I7), so a fenced entry waits for its own durability too, which §4.2 never skips.

Otherwise the commit costs no write of its own (focal F17): every write states the latest commit
the entries it or an earlier write holds allow; a member that alone decides (it leads, it is the one
voter of a configuration that is not joint, the entries are of its term) states `commit = last` in
the very write that holds the entries, which is true exactly when that write is durable, and the
core's commit is checked against it after; and a commit no write has carried while the applied index
ran past `C_d` for a whole owner period is written then, one such write out at a time, waited for by
no one, so a member that stops reopens with what it applied. Here `C_d`'s state-machine part is what
saves the write: mantle's engine, durable on its own schedule, covers what it applied, and the quiet
write is due only when the engine has not persisted for a whole period; focal's state machine is the
log, and the rule is exactly focal's.

### 4.2 Apply before local durability, on a leader in its own term

A committed entry is durable on a quorum; waiting for this member's copy adds nothing to its
durability (TiKV RFC 0112; DeWitt et al.'s pre-commit, research §2–§3). The core gives a leader the
entries of its own term to apply once committed, whether or not its own write is durable yet
(R-6, raft-rs's `max_apply_unpersisted_log_limit` with the bound that is already there: such
entries are the core's unstable entries, bounded by `Limits`). A leader whose own disk is the
slowest of its quorum then answers at the quorum's pace instead of its own.

Kept from TiKV's conditions, with their reasons: only a leader, since a follower's apply serves no
answer; only entries of its term, which its own appends made, so no write in flight from another
leader can replace them (the ABA case of §2.1 is about entries a member took from others); never a
restart-acted entry (§4.1 needs it logged), and never compaction past the log's own durable index.
A member that restarts with its state machine past its log is the case TiKV argues safe: it
acknowledged nothing it had not made durable (I2), so it was not counted in the quorum that
committed what it applied, and its shorter log cannot elect it; §4.3 opens it.

### 4.3 What a member opens with

At open, from the log's view and the state machine's durable point `(index, term)`:

1. A snapshot the state machine installed and the log never recorded: the log starts there, holds
   nothing past it, and records it committed (mantle's `complete_install`).
2. The state machine past the log's commit and within its entries: the log's commit is raised to
   the state machine's index (mantle's `commit_applied`).
3. The state machine past the log's last entry (§4.2, or a lost last frame): the log starts at the
   state machine's point, as after an install. The state machine reports the term of its durable
   index, so this needs no inference from terms along the log; mantle inferred it and rebuilt the
   member where it could not (`ReplicaError::Damaged`), and `StateMachine::durable` returning the
   term removes that case.
4. The core is opened at the state machine's applied index and configuration.

### 4.4 As built (core step R-6)

In `crates/hyper-raft` (`src/node.rs`, `src/raft.rs`, `src/log.rs`):

- **The durable commit.** `RawNode::durable_commit` is the logged part of `C_d` as the core knows
  it: the commit storage stated when the member opened; then, at each notice, the commit each
  durable `Ready`'s hard state stated (the issue mark keeps it; it is durable whatever the term the
  notice is heard in, for it names only entries that write or an earlier one holds and a committed
  entry is never replaced, so etcd's term guard does not apply to it); and any commit the shell
  made durable by a write of its own, which it says with `RawNode::commit_durable(commit)`: the
  commit a `LightReady` gave (raft-rs's owners write it with the next record, mantle's never), the
  fence's write of the hard state alone, `commit = last` in the write of a member that alone
  decides, or the state machine's durable index folded in. It never goes back, and a commit beyond
  the log is refused, fatally. The core rests on one rule of the shell's, which was already the
  contract: a `Ready`'s hard state is written as given, its commit with it.
- **Answers carry it.** `MsgAppendResponse` and `MsgHeartbeatResponse` are made with the commit
  the member knows, as raft-rs's are, and held to the durable commit when they may leave: a
  `Ready`'s persisted messages at that `Ready`, to `C_d` or the commit its own hard state states,
  whichever is greater, for they leave once that write is durable; what a notice releases from a
  member that does not lead, to `C_d` then. A leader's messages are not walked: an answer that
  states a commit is made only by a member that follows, a member leads only in a later term, and
  it sends at once only once the write of that term is durable, which took every answer it made
  before. An answer states the lesser of the commit it was made with and the commit durable when
  it leaves, never a commit no durable write stated. Where the durable commit covers the member's
  commit nothing is walked at all, which is every `Ready` of an owner that writes every commit it
  is given. Where the rule it
  replaces said one:
  - a heartbeat moves a follower's commit while its write is out, and that write's notice
    releases the answer at once, nothing being left to write; no `Ready` states that commit, for
    the notice reports it as `LightReady::commit_index` and folds it into the hard state the next
    `Ready` is compared against. `an_answer_a_notice_releases_states_the_durable_commit` fails on
    R-4 (it says 3 with 2 durable), and the schedules' oracle fails at once with the old rule in
    ("member 1: MsgAppendResponse said commit 1 with 0 durable");
  - a shell's own answers, such as mantle's lost-entries report, state `C_d` likewise.
  mantle's case (`1c179e8`): a member commits as leader at a notice its owner never writes, and
  steps down in the same term by check-quorum. It answers nothing in that term, no other member
  leading it; its next answer is to a later term, and leaves with that term's write, whose hard
  state names the commit. So the answer states the commit only as that write makes it durable,
  and the core does not take the volatile commit for durable meanwhile
  (`an_answer_states_no_commit_that_no_durable_write_stated`). A leader's own
  `Progress::committed_index` stays its commit, as raft-rs's; its durable one is
  `RawNode::durable_commit`.

  **Against raft-rs.** raft-rs states the commit it knows. The two say the same wherever the owner
  writes every commit it is given, as the differential's owners do (each `Ready`'s hard state and
  each `LightReady` commit, told with `commit_durable`), so the differential compares unchanged at
  every setting and `tests/support/convert.rs` translates nothing. They differ only where an owner
  leaves a `LightReady` commit unwritten or hears notices behind its writes (depth above one),
  which the differential never runs; `tests/pipeline.rs` holds this core there.
- **The apply pause.** `RawNode::pause_apply`, `resume_apply` and `apply_paused`. While paused, no
  `Ready` or notice gives committed entries (nor a range), `has_ready` does not count them, and
  everything else goes on; `advance_apply_to` reports what the shell applied of the page it holds,
  and a snapshot given meanwhile replaces that page. etcd pauses by the bytes given and not yet
  acknowledged (`maxApplyingEntsSize`, research §3); here the shell pauses, since only it knows
  its fence holds a page. With it the fence holds at most one `Ready`'s committed page (§6).
- **Apply before local durability (§4.2).** `Config::apply_unpersisted`, off by default as
  raft-rs's `max_apply_unpersisted_log_limit` is zero. A leader that has it notes the last index
  before its term's entries (`Log::unpersisted_after`); once everything through it is durable here
  the apply bound is the commit, not the lesser of the commit and what is durable. It is cleared at
  every change of role (raft-rs PR #561). Given in place, a range's tail past storage is read where
  the log holds it (`Log::next_range_since` over `Log::any_entry`). The shell's part: a
  restart-acted entry still waits for `C_d`, which cannot cover an entry not durable here (I7); and
  no compaction passes what the disk states committed (the schedules' harness compacts only what
  its disk's commit covers, stating it first).

**The gate as run** is `docs/raft.md` §3's R-6 row; counts in `crates/hyper-raft/ORIGIN.md` and
`docs/benchmarks.md`, "The durable commit and the apply pause (R-6)".

## 5. Repair

The log tells crashes from damage (persist records and confirmation, hyper-log; PAR's CLSTORE,
research §5) and reports each group's health: whole; **marked** (a lost last frame whose persist
record survived: the term and vote are restored and the last acknowledged `(index, term)` is known);
**damaged** (a promise is lost: a vote or a fast-track approval no peer can restore). What each
calls for:

- **Damaged: rebuild under a new identity.** A member that lost what it promised "cannot safely
  rejoin the cluster with its prior identity" (thesis §3.8); PAR keeps two copies of the metainfo for
  the same reason. The node retires the identity, removes the group's records and joins a new member
  that a membership change swaps for the old one (mantle's `rebuild` and `membership::Replacement`).
- **Marked, as follower.** It judges a vote request against its mark, not its shorter log, refuses
  a transfer and suppresses its own campaigns, as mantle does: a candidate behind the mark could lack
  an entry this member helped commit. Once a leader shows it counts entries the member lost (a
  commit past the member's log), the member reports what it lost: an append refusal flagged `lost`,
  naming its durable last entry. The leader takes it as a regression of that member's progress, its
  match falling to what the member holds, and resends from there (core step **R-5**). This is CTRL's
  follower repair (research §5, §3.4): the lost entries, not a snapshot (PAR measured 1.2 ms and
  7 KB against 1.24 s and 32 MB). Lowering a member's match revokes no commit (commit is monotone and
  the entries are held by the leader); a forged flag only costs resends from an authenticated peer.
  A snapshot is sent only where the leader no longer holds the entries.
- **Marked, and its log is the most up to date** (PAR Figure 4(b), where mantle's rule elects no
  one). CTRL lets such a member lead and recover its own lost entries before it serves: for each
  lost `⟨term, index⟩` it asks the voters, fixes it from any `have`, discards it and everything
  after on a majority's `dontHave`, and waits on `haveFaulty` (§3.4.2–3.4.3). This is core step
  **R-7** (§5.2): with the suffix marks hyper-log keeps, the vote itself is the question, and a
  marked member is elected on its log, without its own vote, by a quorum of the others that answer
  for no more than it holds.
- **A damaged frame with later frames after it.** PAR identifies faulty entries by identifiers
  stored apart from them; hyper-log should report each group the frame touched as marked through
  the frame's range rather than refuse to open the log (a hyper-log change, open in mantle replica.md
  §7). The shell's rules above then repair each group in place.

### 5.1 As built (core step R-5)

In `crates/hyper-raft` (`src/raft.rs`, `src/progress.rs`, `src/node.rs`, `src/proto.rs`,
`src/wire.rs`):

- **The core keeps the mark.** `Config::lost: Option<Lost>` is what storage found when the member
  opened (`Lost { index, term }`: hyper-log's uncertainty mark, entries through `index`, of terms up
  to `term`, that the log may lack of what it acknowledged); `Raft::lost` reads it. It ends by
  hyper-log's own rule (`Lost::resolved_by`, mantle `docs/design/raft-log.md` §6) read from storage
  at open and at every notice (`Raft::settle_lost`): the durable log reaches the mark's index, or
  holds an entry of a later term from a leader, whose log holds every entry committed in the
  mark's terms before that one (Leader Completeness; terms never fall along a log). mantle's rules
  for a marked member (§5) move from the shell into the core: it judges a vote request against
  its claim, the mark (`Raft::claim`, in `step_vote`, for the vote and for `Precedence`), refuses
  to campaign (`Error::Lost`, and its deadline is none), forgets the leader whose silence asked it
  to (so it holds no lease on one that is gone), and refuses no one for priority (a voter that
  refuses for priority must be one the group could elect instead; a schedule of seed 560 found a
  marked member of the highest priority refusing every candidate as long as it was marked). The
  mark costs an unmarked member a test of an `Option` at each notice and each heartbeat, and nothing on an
  acknowledgement: the flag is read only on a refusal, and priority is judged where votes are.
- **The follower's word.** While marked, a member that is sent a heartbeat whose commit passes its
  log, or an append after a point past its log, answers with a refusal flagged lost
  (`Message::lost`, the format's flag bit 2, `docs/raft.md` §3.1), naming the last entry it holds
  (`reject_hint`, `log_term`) and its commit (held to the durable commit as every answer is, R-6).
  It is an ordinary refusal besides, so a leader that counts none of the lost entries probes as it
  always did.
- **The leader's repair** (`Raft::take_lost`, `Progress::lost`). A leader whose progress for the
  member is past the named entry takes it back there: `matched` falls to it, the member is probed
  from the entry after, its window and any snapshot under way are dropped, and its stated commit is
  the one the refusal names. The named entry must be the leader's own where the leader still holds
  it (what the member holds is a prefix of what it acknowledged, which was this leader's log, and a
  leader never cuts its own); a mismatch is a violation, dropped. It then sends from there: the lost
  entries, paged as any append; a snapshot only where it compacted them. Lowering `matched` revokes
  no commit (the commit never goes back, and the entries are the leader's), so a false word costs
  resends from an authenticated peer and nothing else (§5). Without the flag the refusal is one for
  an index the leader counts as held, which it takes for a stale answer
  (`Progress::maybe_decrease_to`): the group reaches a fixed point where the member never holds the
  entries again (`without_the_flag_a_leader_never_sends_the_lost_entries_again`).
- **The shell** (`crates/hyper-durable`, `Replica`) passes the store's health into `Config::lost` at
  open and keeps nothing of the mark: `Replica::mark` reads the core's. Its own rules (`judged_by_mark`,
  `refresh_mark`, the campaign filter in `emit`, the snapshot request `ask_repair` and its leader's
  `serve_requests`) are gone; `Replica::campaign` maps the core's `Error::Lost` to `ReplicaError::Marked`.

**The gate as run** is `docs/raft.md` §3's R-5 row; counts in `crates/hyper-raft/ORIGIN.md` and
`crates/hyper-durable/ORIGIN.md`, cost in `docs/benchmarks.md`, "Repair by entries (R-5)".

### 5.2 As built (core step R-7): a marked member's election

**The rule.** For a member `u` let `ω_u` be the `(term, index)` of the last entry of its durable log,
`μ_u` its mark while it is marked, and `κ_u` its claim: `μ_u` while marked, else `ω_u` (`Raft::claim`;
a mark not yet ended is always past `ω_u`). Pairs are ordered as Raft orders logs, by term and then
index (thesis §3.6.1). Then:

- a voter `u` grants a candidate `C` only if `ω_C ≥ κ_u` (R-5's judgment, kept);
- a marked `C` campaigns on its log: its requests name `ω_C`, never its mark, and its own vote,
  though cast and durable as every vote is, is not counted toward its quorum (`Raft::campaign`
  polls itself as a refusal); it campaigns only where the others can be a quorum of each half of its
  configuration (`Raft::may_campaign`; a sole voter and one of two wait, refused `Error::Lost`), and
  not in a fast group;
- elected, its mark ends (`Raft::become_leader`).

So a marked candidate is elected exactly when a quorum of each half of its configuration, without
it, answers for no more than its log holds.

**Why it is safe.** Two assumptions about marks, which are the store's:

- (M1) a member that durably acknowledged an entry `w` holds `w` in its log, or holds a later entry
  of `w`'s term or an entry of a later term (it took a leader's log past `w`, or compacted it into
  a snapshot past it), or is marked with `μ ≥ w`. hyper-log keeps this: a persist record stored
  apart from its frame survives the frame's loss and names the frame's last index and term, and two
  marks merge to the greater index and term (`docs/research/durable.md` §5, PAR §3.3.3–§3.3.4); a
  frame lost before its persist record was written was never acknowledged.
- (M2) the term and vote are never lost. A member that loses them is damaged and rebuilt under a
  new identity (§5); PAR keeps two copies of its metainfo for the same reason (§3.3.1).

Under (M1), `κ_u ≥ w` from the moment `u` acknowledges `w`, for good. Let `e` be committed in term
`T` at index `i`: the leader of `T` counted a quorum `Q` of its configuration holding an entry `w =
(T, j)` with `j ≥ i` (Raft commits only entries of its term, earlier ones by implication). Let `C`
be elected in a term `U > T` by grants `G`, a quorum of each half of its configuration, `C ∈ G` only
if `C` is unmarked. The quorums of the configurations a change passes through meet as Raft's do
(thesis §4.1 for a change by one voter, §4.3 for the joint configuration): the rule changes which
members' judgment counts, not the quorums' shapes, so there is `u ∈ G ∩ Q`. If `u = C`, `C` is
unmarked and its own log holds `w`. Otherwise `u` granted, so `ω_C ≥ κ_u ≥ w`. Either way `ω_C ≥ w`,
and Raft's Leader Completeness argument (thesis §3.6.3) gives the rest: if `C`'s last entry is of
`T`, `C` took it from `T`'s leader and its log matches that leader's through an index past `j`; if of
a later term `T′`, from `T′`'s leader, which held `e` (by induction on terms), and matches it through
an index past `i` (terms never fall along a log). So `C`'s log holds every committed entry. Its lost
entries then carry nothing committed that its log does not hold, and its mark ends. Election Safety
is untouched: `G` is a quorum, each member votes once a term (M2), `C`'s own vote included.

What the proof needs of the self-exclusion is the case `u = C` with `C` marked: `C` may be the only
member of `G ∩ Q`, and its log may lack `w` (`a_marked_log_that_may_lack_a_committed_entry_is_never_elected`;
with its own vote counted, `faults_at_rest_*` and the directed tests commit a second entry at an
index, and `MarkedSelf.cfg` must be refused). What it needs of the voters' judgment by the mark is
`u ≠ C` marked and lacking `w` (`MarkedWhole.cfg`, and the same tests mutated).

**Which configurations, which marks.** The argument covers every configuration hyper-raft's change
protocol makes on the classic track: one voter added or removed by one entry, and a joint
configuration entered and left by `ConfChangeV2` (a candidate counts a quorum of each half without
itself). A fast group's marked member does not campaign: a fast commit counts what a voter holds
beside its log, which a lost frame takes with it and no mark names. Marks are suffix marks: what a
log may lack is its tail past `ω_u`, through `μ_u`. A damaged frame with intact frames after it
(PAR's Figure 4(b) as drawn, a faulty entry below good ones) is a hole, which hyper-log does not
report today (it refuses to open such a log, §5); the schedules' disk cuts at the first damaged
entry and marks through what it held (§12), which makes a hole a suffix at the cost of the good
entries after it, then repaired by R-5.

**CTRL, mapped.** PAR's leader asks the voters about each faulty `⟨term, index⟩` and hears `have`,
`dontHave` or `haveFaulty` (§3.4.2–§3.4.3). With suffix marks the vote is that question for the
whole lost range at once: a grant (`κ_u ≤ ω_C`) is `dontHave` (no witness above `C`'s log), a
whole voter's refusal is `have` or more (its log goes past `C`'s), a marked voter's refusal is
`haveFaulty` (its mark covers what `C` lacks). Case 2, a majority of `dontHave`, is `C` elected
without its own vote, its lost entries discarded; case 3, wait on `haveFaulty`, is a refused
campaign drawn again. Case 1, fixing `C`'s entries from a `have`, is not needed: hyper-log names
only the last lost entry, `μ_C`, so a `have` is a voter `v` holding `μ_C`, whose log then matches
`C`'s lost range whole (Log Matching), and `ω_v ≥ μ_C`. Every voter that would grant `C` on its
mark, as CTRL elects it, then grants `v` (`κ_u ≤ μ_C ≤ ω_v`, `C` included), so `v` is a candidate the
rule admits, elected without copying anything to `C`. A partial fix (some lost entries from a
`have`, the rest discarded) needs each lost entry's identity, which hyper-log does not keep. PAR
names each faulty entry from identifiers stored apart; with the last's alone, the lowest term a lost
entry may have is `ω_u`'s, and a marked voter whose mark's term is below the candidate's lost
entries' cannot yet answer `dontHave`. A store that kept the first lost entry's term would let it
(§14).

**Where a group must wait.** A marked member of a group of one or two voters, or of a joint half of
two, is never part of a quorum of the others: the other alone may lack entries that the marked
member and a voter since removed committed under an earlier configuration, and only the operator,
rebuilding the marked member, can end the wait. Where every copy of a committed entry is lost, the
group waits for good, as PAR's does ("the system will remain unavailable", §3.4.2). The schedules
count both (`docs/raft.md` §3's R-7 row).

**By the log's precedence.** That a group with a member the rule admits elects rests on
`Precedence::Log`, under which a voter refuses a candidate for priority only where it could be
elected instead. Under raft-rs's precedence of length, which the core keeps only to compare itself
with raft-rs, a voter of higher priority refuses a candidate whose log is shorter however much more
current, and with a mark no member need be away for the group to elect no one: at 960 seeds of the
faults at rest (seed 740), the voter of the longest log, of an older term, refused the candidate of
the later term for priority, the marked voter refused it for its mark, and neither could be elected.
The schedules hold the rule's election to the log's precedence and count such a group under the
other as waiting (`tests/pipeline.rs`).

**The model.** R-7 changes who counts toward an election, which is what the TLA+ model checks, so
the model gains the marked member (`docs/models/README.md`): `Lose`, a member losing its log's tail
at rest and marking through its last entry; a voter judging by its claim (`Claim`); a marked
candidate's own vote uncounted (`Own`); the mark ending as the log reaches it or at election. Every
configuration before it has the states it had (`Losers = {}`); three new ones pass at two terms and
two indexes (three voters, a change by one voter, a joint configuration: 196,484, 76,173 and 909,876
states), and five must be refused (the candidate's own vote counted, voters judging by their logs,
and for each of the three the claim that no marked member is ever elected), counted by CI.

Ganesan et al.'s findings are the tests' checklist (research §5): every fault detected (checksums on
every record and payload, R-2's CRC-32C), crash and corruption never conflated, redundancy always
used before a member is given up, and no protocol path (election, catch-up) that spreads a local
loss.

## 6. Bounds

Every hold has a bound derived from a quantity the system already has:

| Hold | Bound | Derivation |
|---|---|---|
| Writes out per group | the log's depth: `PIPELINE_FRAMES` (3) | A group's write is useful in each of the log's three pipeline frames (confirming, writing, gathering); a fourth cannot be written before the third flush from now and adds only waiting (Little's law, hyper-log `lib.rs`, mantle research/11 §4). hyper-log's `GROUP_SUBMISSIONS` becomes `PIPELINE_FRAMES` plus one for a compaction. A store that completes synchronously (slates') has depth one. |
| Unstable entries in the core | `Limits` (uncommitted bytes at a leader; at a follower the leader's inflight window to it, since nothing past it is sent before acknowledgement) | existing core bounds; R-3's `Limits::derive` |
| Persisted messages waiting on writes | the core's pending-message bound for each `Ready`, times the depth | core `Limits` × depth |
| Committed entries behind the fence | one `Ready`'s committed page (`max_committed_size_per_ready`), and no more while it waits | R-6's apply pause |
| Committed entries applied in one drive | one page (`max_committed_size_per_ready`), or one entry larger than it | §7's quantum: the rest waits for the next drive, the core's apply paused |
| Committed entries waiting past a drive's page | the rest of the one range that crossed it, a page at most, and no more while they wait | R-6's apply pause: once they wait, no notice or `Ready` gives a range |
| Snapshot reports while stalled or fenced | one per member of the configuration | at most `MAX_MEMBERS` |
| Reads | the core's read bounds (rounds are the core's since F43; the shell keeps none) | core `Limits` |
| Inputs while a write is out | none held: the core steps them | R-4 |
| Inputs while stalled for room | none held: refused `Stalled`, as the network may drop them, and retried by Raft (thesis §3.3: messages may be lost) | — |

Memory is reserved before a transition from the owner's budget (focal's R28): an operation whose
reservation is refused changes nothing. The budget is a trait; mantle's and slates' owners pass one
that admits all, and the reservation then costs nothing on their paths (measured in §12).

## 7. Threading and ownership

No thread per group, no `Arc`, no lock (`CLAUDE.md` §1). A node runs a fixed set of owner threads
(mantle node.md's shards; focal's session owners); each owns its replicas in an arena by
generational handle, each replica owning its core, its state machine and its group's log handle.
Each device's log runs its owner and device threads (hyper-log), later its issuer. The shell never
blocks: a write is submitted with the owner's waker (`GroupLog::submit_waking`; focal's
`notify_persisted`, F45) and the owner drives the replica when woken, so one flush serves every
group its owner submitted before it (the commit group, research §2), and nothing polls on a timer.
The owner shares itself among replicas by deficit round robin with a quantum of one `Ready`'s work
and one page applied (mantle's `DRIVE_BUDGET`, Shreedhar and Varghese Theorem 4.5). Applying runs on the owner, as the
state machine is the replica's own; CockroachDB and etcd move application to other threads, which
here would need shared ownership of the state machine, and whether apply time ever exceeds what a
quantum allows is measured before any such split (§13).

Every call into the core and the state machine runs inside an unwind boundary (focal's
`guarded_in`): an unwind fences the replica and is reported, never propagated (mantle `CLAUDE.md`
§1). The core does not unwind; the state machine is an application's.

## 8. Composition

**hyper-timing (L-2, built).** The core elects by suspicion (`docs/timing.md` §2.8): it campaigns
when it trusts no leader, after the election law's randomized delay, and takes no ticks; the shell
opens a core so when its owner says so (`Settings::elections`), and gives it no tick (an owner on
ticks, below). The owner tells a replica what its
node's detectors believe (`Replica::suspect`, `trust`, `restarted`) and its group's measured timing
(`Replica::set_timing`); `drive` wakes the core at the owner's clock, and `Driven::wake` and
`Replica::deadline` say when to drive again though nothing arrives, none for a group with nothing in
flight. The clock is the owner's monotonic clock in nanoseconds, a `u64`, as the core's and
hyper-liveness's are (`docs/timing.md` §2.9: one type a simulated world drives every sans-io crate
by; an owner with an `Instant` converts at its edge). The node's liveness stream (L-3) is wired by
the owner: `Owner::pairs` keeps the stream told which peers each replica's group has,
`Owner::believe` takes each of its changes to the replicas with a member on that node (a
suspicion, trust again, a restart to `Replica::restarted`), `Owner::measure` derives each group's
timing from what the stream measured (`Replica::measure`) and charges every pair its groups'
expected election (`docs/timing.md` §2.9: a pair never charged never configured), and
`Driven::flushed` is the flush each heartbeat proves (`crates/hyper-durable/tests/liveness.rs`).
The stream's run (`hyper_liveness::Settings::run`) is the node's, not the shell's: a count of the
node's starts it keeps whole beside its other records and raises before the stream's first
heartbeat, so runs are ordered and a superseded run's heartbeat is refused (`docs/timing.md` §2.8,
"A restart"); the shell has no record of the node's starts (its `Kind::Start` is a compaction's new
start of one group's log), and a node's replicas come and go. The shell holds the core's campaigns while the member is stalled
(`RawNode::hold_campaigns`); the core holds them itself while the member is marked (§5.1) or a
committed change is unapplied.
It does not withhold the detectors' words, as the plan here said it would: a marked follower that
kept trusting a leader its detector suspected would refuse every pre-vote for that leader's group,
and a group whose other voter is a candidate could not elect; words change nothing durable, so a
stalled member takes them too, and a fenced one takes nothing, its owner telling the reopened
replica what its detectors believe. A vote's latency is a flush (I1), which the ballot charges as
the mean flush (`Flushes`): the shell feeds that fold with each hard-state write's
submit-to-durable time, on the owner's clock passed into `drive`. A node heartbeats only after its log took a write and a flush
within the period (`docs/timing.md` §2.1, L-3): the log's completions are that evidence.

**Ticks, until each owner elects by suspicion.** focal elects on ticks until its timing and
liveness step (focal 27 §14), and mantle until its owner carries the node-pair stream (§11, D-1).
The shell runs the core's tick path as an owner's setting (`Settings::elections`):
- It is fixed when the replica opens and never changed while it runs. On ticks the shell refuses
  the detectors' words (`suspect`, `trust`, `restarted`, `set_timing`), as the core does
  (`ticks_and_suspicion_do_not_mix`): there is no mixed mode.
- The owner ticks it once a period (`Replica::tick`), and may send a leader's heartbeats between
  ticks (`Replica::beat`) and set the randomized election timeout from its own pace
  (`Replica::set_randomized_election_timeout`), as focal-consensus's `DurableNode` does.
- A stalled replica is not ticked: a member that cannot persist takes no part. The ticks it missed
  are never replayed, for mantle's replay of them was a defect (§10).
- A replica whose writes are out is ticked as its owner's period comes, its core campaigning as its
  clock says; each campaign supersedes the vote requests of the one before that have not left
  (`crates/hyper-raft/ORIGIN.md`, "A campaign supersedes the requests still waiting"), so a member
  whose device held its writes through many timeouts sends at most one campaign's requests for each
  write it had out and its last campaign's when the device goes on: before the rule, mantle's range
  group sent 234 requests after a hundred of the longest timeouts with three writes out.
- `Replica::deadline` says nothing of ticks, which the owner's period drives.
- It goes once the last owner elects by suspicion.

The core's semantics on ticks are raft-rs's, which the differential holds it to. focal's election
suites run on the shell to show that its own (pre-vote, check-quorum, the timeout from its pace)
are unchanged.

**hyper-transport.** Messages leave as the shell releases them; the transport may lose or reorder
them, which Raft tolerates. A message from the network is bound to its authenticated sender before
the core sees it (focal's `step_authenticated`). Snapshots go on the bulk class and their stream's
fate is the `report_snapshot`. The window a leader keeps in flight to a member is twice what the
transport's congestion window to it holds (focal 27 §11), passed to the core each round
(`set_inflight_bytes`).

**hyper-log.** The shell is generic over the log (§9); hyper-log is the disk implementation.

## 9. The API

```rust
/// Where a group's writes become durable: hyper-log's group handle, a RAM publication.
pub trait LogStore: hyper_raft::Storage {
    /// Writes this store gives a group at once (§6).
    fn depth(&self) -> usize;
    /// Copies `write` into the store's buffer and returns once it is on its way; the waker is
    /// woken when its answer comes. Refused writes changed nothing.
    fn submit(&mut self, write: &Write<'_>, waker: &Waker) -> Result<(), LogRefusal>;
    /// The oldest write's answer, if it has come.
    fn poll(&mut self) -> Option<Result<(), LogFault>>;
    /// What the store found of this group when it opened (§5).
    fn health(&self) -> Health;
}

/// What the shell asks of a state machine.
pub trait StateMachine {
    type Answer;
    fn apply(&mut self, entry: &Entry, answers: &mut Vec<Self::Answer>) -> Result<(), Fatal>;
    /// The change as its entry stated it, and the configuration it left.
    fn apply_change(&mut self, at: Point, change: &ConfChangeV2, configuration: &ConfState)
        -> Result<(), Fatal>;
    /// The index and term a restart opens at (§4.3).
    fn durable(&self) -> Point;
    /// Whether a member acts on this entry at its next start (§4.1).
    fn acts_at_start(&self, entry: &Entry) -> bool;
    fn image(&mut self, at: Point) -> Result<ImageSource, Fatal>;
    fn install(&mut self, image: ImageSink, at: Point) -> Result<(), Fatal>;
}

pub struct Replica<L: LogStore, M: StateMachine, B: Budget = Unbounded> { /* core, writes out, C_d */ }

impl<L: LogStore, M: StateMachine, B: Budget> Replica<L, M, B> {
    pub fn open(config: &Config, log: L, machine: M, budget: B) -> Result<Self, OpenError>;
    pub fn step(&mut self, message: Message) -> Result<(), ReplicaError>;
    /// What the node's failure detectors believe of a member's node (timing step L-2).
    pub fn suspect(&mut self, member: NodeId) -> Result<(), ReplicaError>;
    pub fn trust(&mut self, member: NodeId) -> Result<(), ReplicaError>;
    pub fn restarted(&mut self, member: NodeId) -> Result<(), ReplicaError>;
    /// The group's span and round tail, from hyper-timing's law over the measured paths.
    pub fn set_timing(&mut self, timing: Timing) -> Result<(), ReplicaError>;
    /// When to drive though nothing arrives; none for a group with nothing in flight.
    pub fn deadline(&self) -> Option<u64>;
    pub fn propose(&mut self, entry: &[u8]) -> Result<(), ReplicaError>;
    pub fn propose_fast(&mut self, entry: &[u8]) -> Result<u64, ReplicaError>;
    pub fn read(&mut self, context: &[u8]) -> Result<(), ReplicaError>;
    pub fn change(&mut self, change: &ConfChangeV2) -> Result<(), ReplicaError>;
    pub fn transfer(&mut self, to: NodeId) -> Result<(), ReplicaError>;
    pub fn report_snapshot(&mut self, to: NodeId, arrived: bool) -> Result<(), ReplicaError>;
    /// §2.2. Fills the owner's buffers, which it reuses: no allocation in a steady state.
    pub fn drive(&mut self, now_ns: u64, waker: &Waker, out: &mut Output<M::Answer>)
        -> Result<Driven, ReplicaError>;
    pub fn compact(&mut self, keep: u64) -> Result<(), ReplicaError>;
}
```

Errors are the core's three kinds and the shell's three states: refused (nothing changed),
`Stalled` (waiting for room, §2.4), `Fenced` (a failed write; reopen), and fatal. mantle's
`begin`/`drive`/`wait_persisted` and focal's `try_drain`/`drain`/`sendable`/`wait_persisted` collapse
into `drive` and the waker: with readies ahead of persistence there is nothing to finish
separately. A blocking owner (a test, a single-group tool) waits on its waker.

`Write` borrows the snapshot point, entries, proposals and hard state from the core in place
(`RawNode::to_persist`); the store copies them once, into its frame.

## 10. What comes from each project

| From | Taken | Why |
|---|---|---|
| mantle | the shared device log as the store; leader sends before its write (R31); a stall that waits whole and a member that takes no part while stalled (R33); fencing on a failed write; open-time repair of install and commit; uncertainty marks, vote judgment and rebuild under a new identity (R34); one `Ready`'s work as the scheduling quantum; never polling a write just submitted | the log is measured against the other two (`docs/benchmarks.md`, "hyper-log against mantle-log and focal-log": focal-log at 13–69 % of mantle-log's rate, hyper-log above mantle-log's median at 11 of 15 points); the repair rules are PAR's and survived 20,000 simulated seeds with damage at rest (replica.md §5) |
| focal | the commit rides the next write; `commit = last` in a sole voter's append; a quiet group's commit written after a period; configuration changes and restart-acted groups applied only on a logged commit (F17, generalized by `C_d`); budgets reserved before transitions (R28); the unwind boundary (R27); decoder floors (R29) and checkpoint images as state-machine features; the owner woken by the log's answer (F45) | each is a measured or SIGKILL-found fix (focal 27 §9); a volatile state machine needs the logged commit |
| slates | a store that completes synchronously is a depth-one `LogStore`; out-of-order acknowledgement within a term needs its window slots in the write before the acknowledgement (R17, I2); the thesis's compaction rule as the shell's policy (R22) | slates' explorer and models back them (note 32 §2.13) |
| literature | readies ahead of persistence with an unstable log true to its name and the ABA guards (etcd, raft-rs); apply before local durability on a leader (TiKV, DeWitt); targeted entry repair and leader-side recovery (CTRL) | research §2, §3, §5 |

Rejected: mantle's held messages and ticks (no longer needed, and the tick replay is a defect);
focal's `PersistencePending` refusal (the same); mantle's one read round out at a time (focal
measured the cost; rounds are the core's since F43); focal's whole retained log in memory (the
log's cache and fetches replace it); slates' whole-state publication per transition for a disk log
(cost linear in the retained log; kept only as slates' own RAM store, where compaction bounds it and
slates measured a delta format not worth a second recovery path).

## 11. What each consumer changes

- **mantle (D-1).** `RangeMachine: StateMachine` holds the engine and layer: `apply` is
  `apply_entry`; `durable` returns the engine's durable index with its term, which the engine now
  keeps beside the index; `acts_at_start` is false for its layers. The replica's `Held`, `Rounds`,
  `Staged`, `commit_applied`, `complete_install` and repair move into the shell or go.
  `configuration_known` counts each voter's durable commit, which the core's answers carry since R-6
  (§4.4); mantle's replica is to tell the core the commits it writes beyond its `Ready`s' hard
  states with `commit_durable`. First test: kill a member inside the window between a replacement's
  commit and its logged commit, in the range simulation and on real processes; it fails today if
  the hole of §1 is real.
  It elects on ticks (§8) until mantle's owner carries the node-pair stream.
- **focal (D-2).** Designed with F-1 as one step (focal 27 §15, reviewed by focal's session on
  2026-10-03). `DurableNode` becomes a `Replica` over hyper-log, one log per data directory:
  - its memory budget is the `Budget`;
  - `apply_on_written_commit` is `acts_at_start` for every entry of a control group;
  - `PersistencePending` goes, and with it the owners' ingress queues for it;
  - each group's image and records (identity, fast track, decoder floor and transition) are files
    of their own, ordered by rules rather than written together;
  - focal's store wraps `GroupStore` and holds the first entry that needs a successor decoder
    until the floor's record is durable (`Fault::Held`, §2.4);
  - it elects on ticks (§8) until focal's timing and liveness step.
- **slates (X-1).** A `LogStore` over its anchor publication, depth one, `submit` publishing and
  `poll` answering at once; `SavedRaft` carries what the core's `Storage` and `InitialState` read.
  Its groups are re-founded (owner's decision 9).
- **The core.** R-4 (readies ahead of persistence; built, §2.1), R-5 (a lost-entries refusal
  regresses a member's progress; built, §5.1), R-6 (an apply pause; a leader's own-term entries given to apply
  before its write is durable; the durable commit carried in answers; built, §4.4), R-7 (CTRL's
  leader-side recovery; built, §5.2).
- **hyper-log.** `GROUP_SUBMISSIONS` from `PIPELINE_FRAMES`; health per group; a damaged frame
  reported as marks through its range for the groups it touched.

## 12. Tests

- **Durability windows, enumerated.** The simulation numbers every durability event of a run (a
  submit, a part durable, a flush, a confirmation, a state-machine persist, a message release) and
  crashes a member at each in turn, not only at random: between a leader's early send and its own
  write; between a follower's write and its acknowledgement; inside a multi-part write; between a
  commit known and a commit logged, with a configuration change and with a restart-acted entry
  behind it; between a state machine's install and the log's start; between a leader's
  apply-before-durability and its own write; inside a compaction. A durability oracle checks every
  released message against the sender's `D` at release (I1–I3) and every applied entry against I4–I5.
- **Recorded seeds.** mantle's range simulation (`MANTLE_SIM_SEEDS`, linearizability per key by
  WGL, every put once, the same rows everywhere, every mark ended) and focal-consensus's simulation
  suites run on the shell. Ready sequences differ by construction (readies now overlap), so the gate
  is the outcome: the same committed entry at every index on every member, the same answers, the
  same rows, every invariant of each suite. `docs/raft.md`'s fast-track seeds run with readies
  persisted at random lags.
- **F17's two cases** on every consumer's shape: a founder that removed its only peer elects itself
  after a kill inside the window; a restart-acted entry never regresses across a kill.
- **Faults at rest** (Ganesan et al.'s checklist, research §5): a flipped bit in each kind of record,
  the last frame and an earlier one, a lost write, a misdirected write; every fault detected, every
  marked member repaired in place by entries (R-5), every damaged one rebuilt, and Figure 4(b)'s
  schedules electing a leader once R-7 is in. As built for the core (`crates/hyper-raft/tests`): the
  schedules' disk keeps a checksum beside every entry and verifies it when the member opens
  (`Disk::verify`), cutting at the first mismatch and marking through what it held; a fault is a
  bit flipped in an entry of the last write or an earlier one, or the last writes lost though
  acknowledged with their persist record kept (`Fault::Flip`, `Fault::Lose`, `Op::Corrupt`), one
  marked member at a time. The oracle counts a member's mark for what it acknowledged before the
  fault (I3), and once settled every member holds every committed entry as it was committed
  (`Cluster::check_kept`); a group that does not settle is accepted only where no member's election
  the rule admits (`Cluster::electable`), or under the precedence of length (§5.2), and counted.
  Whether a group settles is judged by its progress, never a count of rounds: on ticks a group
  that moves no term, commit, applied index or last index for twice the longest timeout a member
  draws has had every member campaign with no lease left, and wins no election later.
- **Real processes.** `hyper-raft-e2e` members on hyper-log and real disks, SIGKILL at named
  durability points (a hook that stops the process at the point and the harness kills it) and at
  random, every acknowledged write recovered, F17's two cases, a failed flush injected on a device.
- **Bounds.** Each bound of §6 driven to its edge, refused with its typed error, and the thread count
  asserted constant as groups grow.

## 13. Measurement

`hyper-durable-compare` drives each project's shell and this one on the same workloads, same host,
recorded commands, load recorded (`CLAUDE.md` §1a), into `docs/benchmarks.md`:

- **mantle**: its range group on `GroupLog`, 1, 3 and 5 members, closed-loop 1 KiB entries, and
  `mantle bench log`'s 1–256 replicas; against `Replica` at origin/dev `1275bf8`.
- **focal**: `cargo bench -p focal-consensus --bench commits`'s workloads (one voter, three voters an
  entry at a time, three voters pipelined); against `DurableNode` at `7a6170e`, after F-1.
- **slates**: `publication_cost`'s workload through a RAM store; against its publication at
  `ec5e0df`.

Each reports commit latency p50, p99 and p99.9, entries a second, device flushes per committed entry,
allocations, reallocations and minor and major faults per committed entry (`hyper-measure`), context
switches, and the threads alive. The shell replaces a project's only where it is at least as fast
and allocates no more on that project's workload.

## 14. Open, to be measured

1. **Depth.** The flushes a committed entry costs, and the latency, at depths 1–3 on hyper-log,
   closed and open loop: whether the third pipeline frame earns its place for one group as it does
   for the log.
2. **Apply before local durability.** p99 commit-to-answer on a leader whose disk is the slowest of
   its quorum, with and without §4.2, on one disk per member and on a shared disk.
3. **The quiet commit write.** How long a volatile state machine's applied state runs ahead of its
   logged commit under focal's one-period rule, and what the write costs a quiet group.
4. **Apply on the owner.** Apply time per `Ready` against the owner's quantum under mantle's metadata
   workload; a split only if it is exceeded.
5. **Repair.** Bytes and time to repair a marked member by entries (R-5) against a snapshot, and the
   simulation's unavailable runs before and after R-7. Measured for R-5 (`docs/benchmarks.md`,
   "Repair by entries (R-5)"): one lost entry of 30,000 repaired in 1.8 KB and 32 µs in process
   against a snapshot's 30.7 MB and 8.9 ms; the bytes stay below the snapshot's until nearly the
   whole log is lost, and in-process time crosses near 2,000 entries. Open: whether a leader should
   choose a snapshot past a crossover measured on a real path. The schedules' waits on a mark
   (`crates/hyper-raft/ORIGIN.md`, R-5 and R-7): with one marked member at a time, 97 to 192 of
   1,000 schedules a setting end waiting before R-7 and after it alike, every one inspected in a
   configuration of two voters or one; with two of three marked at once, 1,950 of 4,000 before R-7
   and 1,627 after. Open: a first lost entry's term in the mark (§5.2), and what an operator's
   rebuild of a marked member of two costs.
6. **The idle confirmation flush.** Whether hyper-log can drop the confirmation a lone frame needs
   when nothing follows it, by PAR's identifiers stored apart, or whether depth makes it rare enough
   (with depth, under load, the next frame confirms).
7. **Budgets.** The reservation's cost per operation for an owner that passes `Unbounded`, which must
   be nothing.
8. **slates' store.** A transition's publication cost at slates' record sizes through the shell,
   against its own path.

## 15. As built (D-1)

`crates/hyper-durable` (`ORIGIN.md` records where each rule came from):

- **`LogStore`** (`src/store.rs`) is not a supertrait of the core's `Storage`: the replica wraps
  the store in `Held` (`src/held.rs`), which adds what the shell keeps beside the log, the
  configuration it opened at and the snapshot it prepared, and answers the core's walks of the
  log with one entry whose buffers it reuses. A store reads as its answered writes left it, and
  adds `bounds`, `visit` (entries where the store holds them, borrowed: the state machine applies
  from the log's own buffers), `room` and `write_now` (the writes of §4.3, before the core opens).
  `GroupStore` (`src/hyperlog.rs`) is hyper-log's group handle, each write one update cut into
  frame-sized parts and answered once every part is; an owner with nothing else to do for the
  group waits on it for the oldest write's answer (`GroupStore::wait`, the log's own wait for each
  part) and drives after, as focal's `wait_persisted` does. `RamStore` (`src/memory.rs`)
  completes each write as it is submitted, depth one, slates' case and the simplest test store.
- **A store's hold** (`src/store.rs`): `LogStore::Hold`, `held` and `release`, and
  `Fault::Held` (§2.4). hyper-log's handle, the RAM store and the tests' stores hold nothing
  (`Infallible`). `tests/shell.rs` holds a write whose entry needs a precondition until its
  owner meets it, and stops a member between the release and the write made again.
- **`StateMachine`** (`src/machine.rs`) applies an `EntryRef`, borrowed, with no copy, and each
  change with the change its entry stated (its context among it); its `durable` point carries its
  term (§4.3); `image` (with the configuration held at its point, §2.3), `install` (durable
  before it returns) and `persist` are its snapshot and compaction.
- **`Replica`** (`src/replica.rs`): `open`, `step`, `suspect`, `trust`, `restarted`,
  `set_timing`, `deadline`, `campaign`, `propose`, `propose_fast`, `change`, `read`, `transfer`,
  `report_unreachable`, `report_snapshot`, `drive`, `compact`, `resume`, `held`, `release`;
  the owner's policy, `set_priority` and `set_inflight_bytes` (the core's, as focal's owners set
  them from placement and from what a path carries); what the owner reads to admit and account,
  `reads_held` (reads confirmed that wait for their apply, which an owner bounding reads counts
  beside the core's) and `budget_mut` (an owner whose budget charges by lane says which before a
  call); and on ticks (§8), `tick`, `beat`, `set_randomized_election_timeout`, `set_patience`.
  `Settings::elections` is stated by every owner, with no default. Every call runs inside the unwind boundary. A drive takes the log's answers first and
  only then, applies what the fence allows, one page at most (§2.2), takes at most one `Ready`
  (§7's quantum), and writes the
  commit alone when the fence or a quiet period asks. `Settings::quiet` is the owner's period.
  Elections are by suspicion (L-2, §8), or on the owner's ticks until it elects so: by suspicion, no
  ticks; a drive wakes the core and `Driven::wake` says
  when to drive again; a stalled replica's campaigns are held by the shell, a marked one's by the
  core (§5.1).
- **`Owner`** (`src/owner.rs`): an arena by generational handle, one waker a slot made by the
  embedder, turns by deficit round robin with a quantum of one `Ready`. The crate spawns nothing.
- **`Budget`**: reserved before an input, settled to the replica's resident bytes after each call;
  `Unbounded` compiles both out.

What waits on core steps not built:

- **R-5** (built, §5.1): a marked member's repair is the core's, by entries; the shell's snapshot
  request and its serving are gone.
- **R-7** (built, §5.2): a marked member campaigns on its log, without its own vote, where the
  others can be a quorum; the shell holds only a stalled member's campaigns.
- **Asked of the core besides:** `RawNode::into_store`, so a closed replica gives back its store;
  and the fast track's proposals not yet durable, so a refused write that held them is made again
  instead of fencing the replica (no owner enables the fast track yet).

hyper-log changed with it (`crates/hyper-durable/ORIGIN.md`): writes sent behind a refused one are
refused (`LogError::Behind`), and `GroupLog::depth` and `has_room`. `GROUP_SUBMISSIONS` is still two,
not `PIPELINE_FRAMES` plus one (§11): at four, a group of frame-sized writes holds all three frames'
bytes and a cold group's room goes; a group's third write waits in the log for its own room, which
§14 item 1 measures.

Tests (`crates/hyper-durable/tests`):

- `sim.rs`: five shapes (three voters at depth three with a durable state machine; at depth two
  with a replayed one; a control group, every entry acted on at start; a leader applying before its
  write is durable, its disk the slowest; five voters at depth one), 128 seeds of 5,000 steps each
  by default, with refusals, failed writes, crashes, compactions and changes; and a crash after
  every step of a schedule that did something, in turn (`a_crash_after_every_durability_event_loses_nothing_durable`).
  Members elect by suspicion: a tenth of the steps are a detector's word about a peer (right nine
  in ten of a member that is down, wrong one in ten of one that is up), the clock moves a
  millisecond a step, a member reopened after a crash is told to the others as restarted, and the
  group settles with every detector trusting every member. The round tail and the span are half a
  second each, as the ticks before waited ten ticks of fifty steps and drew over ten more. Soaked
  by suspicion at 1,000 seeds a shape and the crash after every event at 64 seeds; the soak found
  one harness defect, a settle that resumed a stalled replica once and never again while a refusal
  taken after it stalled the replica anew (seed 5, crash 4), now resumed each round.
  A crash after a drive is the harder case for every event inside it: nothing durable changes
  within a drive, and what it released has left. Soaked at 1,000 seeds a shape; three shell
  defects were found and fixed so (a commit-only write naming entries not yet written; a
  compaction behind a snapshot's start still out; a refused write made again before room was
  freed, a refusal a drive), and one in the harness's own model of hyper-log's refusals. At 1,000
  seeds a shape on R-3's core, one more (seed 600 of five voters at depth one): a refused write
  made again with a snapshot the core took while it was out, which no `Ready` had given, started
  the log past the state machine (I8), so that a member stopped there would not open; fixed in
  `make_again`, test `a_write_made_again_never_starts_the_log_past_the_state_machine`. It is in
  `main`'s shell; R-3's out-of-order acknowledgement (R17) changed the schedules that reach it.
- `directed.rs`: mantle's four cases and focal's two, on hyper-log over simulated devices with the
  power cut at each write and flush in turn, both ways of taking readies. The founder campaigns on
  its owner's word; the members are given their timing once it leads, as no span is chosen before
  their paths are measured, and a member whose power was cut reopens timed, told to the others as
  restarted.
- `crates/hyper-durable-e2e`, real processes: each member a `Replica` on hyper-log over a real,
  fully flushed file (`DeviceFile`), over UDP in hyper-raft-e2e's datagrams, woken by its log's
  answers through a datagram to its own socket (five threads a process, whatever it holds, with the
  one that watches for its test to go). The members elect by suspicion on their own detectors:
  each runs the node-pair liveness stream, its heartbeats proved by its replica's writes or, idle,
  by an empty update of a group of the stream's own on the same log, in a run it keeps beside its
  log and raises at each start (`hyper_raft_e2e::run`, a record kept whole), and takes the stream's changes
  to its replica and its group's timing from what the stream measured, as `Owner` does
  (`docs/timing.md` §2.9, "On real detectors"). The test tells no member what to believe and
  derives nothing: it waits on facts while the group moves, for a quiet period of the members'
  own law, quiet only while the test hears every member, by hyper-raft-e2e's rule
  (`hyper_raft_e2e::quiet`): a look that did not hear a member decides nothing; the time a member
  had a write of its log out extends the wait, for the log's threads make its writes and its group
  moves through it only as they become durable; and a member silent, or heard with its oldest write
  out, past the longest one write any member has reported (or the hold the test ordered) and the
  quiet period, each counted less the timeout a look waits for its answer, fails the wait, named
  (`docs/timing.md` §2.9, "The same rule in the other harnesses"). The test (`tests/kill.rs`) kills the leader and a follower with `SIGKILL` at each named durability
  point (a write submitted; a write durable whose answer was not taken; messages released), and at
  seeded random points and counts; focal's F17 cases (the founder killed once it applied the
  removal of its only peer, the peer stopped for good, elects itself alone; killed with the removal
  behind its fence, the group finishes it; a host that acted on a fence, restarted told of no one,
  acts on it again from its own log); a failed flush, after which the member fences, exits
  and rejoins; a stalled disk, whose member every other suspects and the group elects without;
  the survivors of a stalled leader electing while their devices hold every flush past a quiet
  period (the fault file's hold); and a member stopped, which the wait for what it cannot do names
  before it lets it go.
  At every restart, every member that heard the last run reports the restart its stream saw. Every answered write reads back linearizably and every member applies the same
  history. With the fence taken out the fence host reopens below the fence it acted on; the
  founder's window is too narrow between processes to fail there (its write is on its way when it
  applies), which `directed.rs` covers deterministically.
- Measured against mantle's shell (`crates/hyper-durable-compare`, `docs/benchmarks.md`, "The
  durable shell against mantle's", "The three losses ... traced" and "mantle's range replica on the
  shell (D-1)"): faster with three and five members and 18–36% fewer allocations. Of the losses:
  - the reallocations were the core's unstable log and outgoing queue giving up their capacity
    when readies are taken ahead, and the member keeping one spare queue while three writes out
    each held one; the core keeps its capacity and a spare for each ready in flight, and the
    driving thread reallocates as mantle's shell does once a group has warmed;
  - the extra switches are involuntary, the price of the overlap, for the same CPU time; per
    entry they are within a few percent of mantle's either way;
  - the one-member tail is the device's: an entry's latency is its write's and the same few tens
    of microseconds on both shells.
- **mantle (D-1), switched** (mantle branch `shared-d1`, 2026-10-03): its range replica is this
  shell with its engine and layer as `RangeMachine`, `durable` the engine's durable index with
  the term the engine keeps beside it, its applied answers one record a command; elections on
  ticks, no quiet write. Measured on its workload, it commits a group of three or five at a median
  39–44% lower than mantle's shell on real files, with 30–43% fewer allocations and the same
  reallocations, and one member evenly (`docs/benchmarks.md`, "mantle's range replica on the
  shell (D-1)"). The move found two core faults, fixed: a campaign's vote requests left waiting
  through many timeouts (`ORIGIN.md`, "A campaign supersedes the requests still waiting"), and
  the one spare queue (`ORIGIN.md`, "A spare queue for each ready in flight").
- `liveness.rs`: the shell on the node-pair liveness stream (L-3), three nodes on one simulated
  clock, 64 seeds by default: elected from nothing, silent while idle but after a detector's change,
  and the leader's node killed, suspected by every survivor within its stated bound and replaced
  (`docs/timing.md` §2.9).
- `shell.rs`, `hyperlog.rs`, `threads.rs`: each bound of §6 at its edge, the open repairs of §4.3,
  marks (a marked member of three campaigning on its log, one of two asking no one), the unwind
  boundary, the owner's turns, parts and refusals on hyper-log, and the threads
  an owner's sixty-four groups cost (none of their own).

