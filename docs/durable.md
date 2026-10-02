# hyper-durable: the durable shell around the Raft core

> Status (2026-10-01): design, nothing built. Sources and what each establishes are in
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

hyper-durable is built on that core, which is a core change, **R-4**, in this order:

- `RawNode::ready` may be called while earlier `Ready`s are out, and `RawNode::advance_issued(ready)`
  says the `Ready`'s write was issued; `RawNode::on_persist(number)` says every `Ready` through
  `number` is durable. `advance_append` stays as `advance_issued` followed by `on_persist` of the
  same number, so every existing caller and the raft-rs differential are unchanged.
- **The unstable log keeps its entries until they are durable** (etcd's rule, research §3), with a
  mark for how far a write was issued; a `Ready` gives only entries past that mark. raft-rs instead
  moves them to storage when the write is issued and needs storage to serve undurable entries; that
  would make hyper-log's group handle serve writes it has not answered, which it does not do by
  design (`GroupLog` answers "as of the writes answered"). `ready_in_place` and
  `advance_append_keeping` keep their zero-copy contract: the entries are given up at `on_persist`.
- **ABA.** A persist notice moves the durable index only below the first index a later issued write
  replaces and only where storage holds that term (`Log::maybe_persist`, raft-rs's rule, already in
  the core), and the issue mark falls back to a conflict's index when a leader's append truncates.
  etcd's term check on the notice is the second guard and is taken too.
- A leader counts itself toward a commit only at `on_persist` (thesis §10.2.1: "the leader uses its
  own match index to indicate the latest entry to have been durably written to its disk"); a
  follower's acknowledgements, votes and every other message the core marks as answering for a
  write are attached to the `Ready` that wrote it and released by the shell at its `on_persist`.

The gate for R-4 is the raft-rs differential unchanged at depth one (one `Ready` out), and at depth
`k` a differential against raft-rs's own `advance_append_async`/`on_persist_ready` driven by the
same schedule; the recorded fast-track seeds of `docs/raft.md` (160,000 schedules from four seeds)
pass with readies persisted at random lags; the TLA+ model gains an issued-but-not-durable write
per member and a crash that loses it.

### 2.2 One `drive`

The shell has one non-blocking entry point for work, `drive`, called by the owner when it stepped
something into a replica or when a write completion woke it. Each call does, in order:

1. **Take completions.** The log's answers for this group, in submission order. For each durable
   write: release its persisted messages; tell the core `on_persist(number)`; record the commit the
   write stated (§4). A failed write fences the replica (§2.4). Completions are taken only here, and
   a write submitted in this call is never polled in it: whether a flush happened to finish
   microseconds after submission must not decide what a call gives out (mantle's determinism
   finding, replica.md §5).
2. **Apply** what the core gives to apply and the commit fence allows (§4), answering each entry's
   callers.
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
open.

### 2.4 Failures

A write refused for room another write frees (`Backlog`, `Full`, `TooManyGroups`, `Busy`) waits,
whole, with the writes after it untaken, and the replica refuses calls `Stalled` until the owner
frees room and drives again: a member that cannot persist takes no part (mantle, audit S04). A
snapshot report that arrives meanwhile is kept, the latest per member, since replication to that
member pauses until its fate is known (a mantle simulation seed found a lost report pausing it for
good). Any other failure fences the replica: a failed flush leaves the device's contents unknown
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
  page and asks the core for no more (core step R-6, an apply pause like etcd's
  `applyingEntsPaused`), and makes sure a write states the commit: the next `Ready`'s write carries
  it, or, when none is due, a write of the hard state alone. Configuration changes are rare: the
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
  **R-7**, after R-5, behind the TLA+ model extended with a marked member and the explorer's
  schedules; until R-7 such a group waits, as mantle's does.
- **A damaged frame with later frames after it.** PAR identifies faulty entries by identifiers
  stored apart from them; hyper-log should report each group the frame touched as marked through
  the frame's range rather than refuse to open the log (a hyper-log change, open in mantle replica.md
  §7). The shell's rules above then repair each group in place.

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
(mantle's `DRIVE_BUDGET`, Shreedhar and Varghese Theorem 4.5). Applying runs on the owner, as the
state machine is the replica's own; CockroachDB and etcd move application to other threads, which
here would need shared ownership of the state machine, and whether apply time ever exceeds what a
quantum allows is measured before any such split (§13).

Every call into the core and the state machine runs inside an unwind boundary (focal's
`guarded_in`): an unwind fences the replica and is reported, never propagated (mantle `CLAUDE.md`
§1). The core does not unwind; the state machine is an application's.

## 8. Composition

**hyper-timing (L-2).** The core campaigns on its detector's suspicion of the leader's node after
the election law's randomized delay (`docs/timing.md` §2.3). The shell withholds a suspicion from the
core while the member is marked, stalled or fenced, and the core already refuses one while a
committed change is unapplied. A vote's latency is a flush (I1), which the ballot charges as the mean
flush (`Flushes`): the shell feeds that fold with each hard-state write's submit-to-durable time, on
the owner's clock passed into `drive`. A node heartbeats only after its log took a write and a flush
within the period (`docs/timing.md` §2.1, L-3): the log's completions are that evidence.

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
    fn apply_change(&mut self, index: u64, configuration: &ConfState) -> Result<(), Fatal>;
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
    pub fn suspect(&mut self, node: NodeId) -> Result<(), ReplicaError>;
    pub fn propose(&mut self, entry: &[u8]) -> Result<(), ReplicaError>;
    pub fn propose_fast(&mut self, entry: &[u8]) -> Result<u64, ReplicaError>;
    pub fn read(&mut self, context: &[u8]) -> Result<(), ReplicaError>;
    pub fn change(&mut self, change: &ConfChangeV2) -> Result<(), ReplicaError>;
    pub fn transfer(&mut self, to: NodeId) -> Result<(), ReplicaError>;
    pub fn report_snapshot(&mut self, to: NodeId, arrived: bool) -> Result<(), ReplicaError>;
    /// §2.2. Fills the owner's buffers, which it reuses: no allocation in a steady state.
    pub fn drive(&mut self, now: Instant, waker: &Waker, out: &mut Output<M::Answer>)
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
  `configuration_known` counts each voter's durable commit, which the core's answers carry once the
  shell tells it `C_d` (R-6). First test: kill a member inside the window between a replacement's
  commit and its logged commit, in the range simulation and on real processes; it fails today if
  the hole of §1 is real.
- **focal (D-2).** `DurableNode` becomes a `Replica` over hyper-log after F-1's WAL conversion; its
  memory budget is the `Budget`; `apply_on_written_commit` is `acts_at_start` for every entry of a
  control group; `PersistencePending` goes, and with it the owners' ingress queues for it; decoder
  floors and checkpoints become state-machine records written through the shell. Changes in focal
  need the owner's permission (`docs/STATUS.md`).
- **slates (X-1).** A `LogStore` over its anchor publication, depth one, `submit` publishing and
  `poll` answering at once; `SavedRaft` carries what the core's `Storage` and `InitialState` read.
  Its groups are re-founded (owner's decision 9).
- **The core.** R-4 (readies ahead of persistence), R-5 (a lost-entries refusal regresses a
  member's progress), R-6 (an apply pause; a leader's own-term entries given to apply before its
  write is durable; the durable commit carried in answers), R-7 (CTRL's leader-side recovery).
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
  schedules electing a leader once R-7 is in.
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
   simulation's unavailable runs before and after R-7.
6. **The idle confirmation flush.** Whether hyper-log can drop the confirmation a lone frame needs
   when nothing follows it, by PAR's identifiers stored apart, or whether depth makes it rare enough
   (with depth, under load, the next frame confirms).
7. **Budgets.** The reservation's cost per operation for an owner that passes `Unbounded`, which must
   be nothing.
8. **slates' store.** A transition's publication cost at slates' record sizes through the shell,
   against its own path.
</content>
</invoke>
<invoke name="Bash">
<parameter name="command">cd /private/tmp/claude-501/-Users-adalundhe-Projects-mantle/02c17d90-de60-44b8-a9b1-b42df5c5e5df/scratchpad/dur-notes/src; grep -n "may be lost\|messages may be\|does not assume the network" ongaro.txt | head