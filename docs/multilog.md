# hyper-multilog: one group's log as `n` Raft logs, merged into one application order

> Status (2026-10-04): designed. Sources, and what each establishes, are in
> `docs/research/multilog.md` ("research §n"). The plan's starting point is `docs/raft.md` §4's
> `hyper-multilog` row and mantle note 32's R25 (§2.13, §3.2): slates' MLRaft layer, moved as a layer
> crate over the core and enabled by no consumer until an owner shows a gain. This design is not
> slates' code moved: it is drawn from slates' MLRaft, the core it now runs on, and the literature
> on several logs merged into one order, and it departs from slates where §10 says why.

## 1. What it is

A group's log divided into `n` Raft logs over the same voters (MLRaft, research §1). Each log is a
`hyper_raft::RawNode` of its own on every member: it elects its own leader, replicates and commits
on its own, and persists in its own storage. Commands go to one log each, and every member applies
the logs' committed entries in **one application order**: one *command history* in Lamport's sense
(research §3), a partial order that orders every pair of commands that interfere and leaves
commands that commute unordered. Every member's sequence of applied commands is a member of that
history, so every member reaches the same state, and every command sees the same state on every
member.

Two kinds of command, as slates defines them (research §8):
- a **keyed** command reads and writes the state of one key, and reads the group's global state;
- a **global** command may read and write anything.

Two keyed commands of different keys commute. Two of the same key interfere, and so does a global
command with every command.

**What it costs, and why no consumer enables it.** A global command is ordered against every log,
so it waits for every log to say where it falls (§3, §4.5): it is only as available as the least
available log, which is the price every multi-leader total order pays (Mencius §4.4, EPaxos §3 on
Mencius; research §4). slates measured MLRaft across five regions and kept one log for both of its
groups (research §8). The layer exists, built and checked to this repository's standard, for an
owner whose commands are keyed and whose measurements show a gain; until one does, nobody enables
it (`docs/raft.md` §4).

**`n = 1` is the single log, by the same code.** With one log every command is in log 0, no log
holds a barrier, a global command waits for no other log, and the merge applies log 0 in its order.
Nothing in the layer tests `n`; each rule below reads "every other log", and at `n = 1` there is
none. An owner at `n = 1` gets the core's own application order, a tag on each command (§2.2) and
nothing else (§11, "n = 1 against the bare core").

## 2. Routing and the entry format

### 2.1 Routing

`Route::Global` goes to log 0. `Route::Key(k)` goes to log `log_of(k, n)`: SplitMix64's finalizer of
`k + γ` modulo `n` (Steele, Lea and Flood, OOPSLA 2014; `γ = 0x9e3779b97f4a7c15`, the multipliers
`0xbf58476d1ce4e5b9` and `0x94d049bb133111eb`, the shifts 30, 27 and 31). The finalizer is a
bijection on 64-bit words with full avalanche, so consecutive keys spread over the logs, and the
remainder's bias toward the low logs is under `n / 2^64`. A key's log is a function of the key and
`n` alone: it is part of the format, the same on every member and every version. `n` changes only
by a resize (§3.5), at one point of the merged order, and a key's log is a function of the key and
the `n` in force where the merge consumes the command.

The owner's keys are 64-bit words. An owner whose keys are byte strings hashes them first, with a
hash of its own that every member computes alike; two keys that hash alike share a log, which costs
order and nothing else.

### 2.2 What an entry states

The layer's commands ride in the core's normal entries. Its tag is the **last** byte of the entry's
data, so that a proposal's bytes are moved into the log as the owner gave them, and the tag and key
are appended into room the owner may reserve (`SUFFIX_BYTES`, 9): an owner that reserves it
allocates nothing more for a proposal than its command.

| Data | States |
|---|---|
| `command ‖ 0x01` | a global command |
| `command ‖ key (u64, little-endian) ‖ 0x02` | a keyed command |
| `n (u64, little-endian) ‖ 0x04` | a resize to `n` logs, `1 ≤ n ≤ u32::MAX` (log 0 only) |
| `index (u64, little-endian) ‖ 0x03` | a barrier naming log 0's `index` |
| empty | the core's own entry (a new leader's first) |
| any other | malformed |

An entry whose type is a change of configuration is the log's own (§3.4). An entry is **out of
place** where a global command stands in a log other than log 0, a barrier in log 0, or a keyed
command in a log its key does not route to. A malformed or out-of-place entry is *refused*: the
merge consumes it as nothing and reports it to the owner (`Applied::Refused`), alike on every
member, since every member reads the same committed entry. slates applied a keyed entry in whatever
log held it; applied, a key's commands in two logs would not commute (research §8).

## 3. Barriers

A global command at log-0 index `g` must take one place in every other log's order: after the
entries that log ordered before it, and before those it orders after. A log states that place with
a **barrier**: an entry naming `g`, after which its entries are applied only once log 0 has been
applied through `g` (§4).

### 3.1 Who proposes one, and when

A member owes log `k` (`k ≥ 1`) a barrier when its replica of log 0 has given to apply (committed
and durable here, the core's I4) a global command at an index `g` that no barrier it knows of in log
`k` covers. Known barriers are those its merge has read in log `k` (applied, or standing at the
head of what it applied), and those it proposed in log `k` during the current term of log `k` as it
knows it. It proposes `Barrier(latest)`, `latest` the highest such `g`: one barrier covers every
global before it. The owner asks for this after every drive (`MultiLog::barriers`); a member owes at
most one proposal a log for each pair of a term and a `latest`, so a call proposes nothing twice.

Every member proposes what it owes, the leader by appending and every other member through the
core's forwarding of proposals to its leader (`raft.rs`, `step_follower`, `MsgPropose`). slates let
leaders alone append. The members are needed for a group whose connectivity is partial: a leader of
log `k` that cannot hear log 0's leader never learns that `g` committed, while it still commits log
`k`'s entries, and every log's entries behind a barrier for `g` wait until a partition heals. A
member that hears both relays the barrier.

**The leader keeps one barrier a global.** A leader of log `k` takes a forwarded barrier only when
no barrier it appended in its term, and none its merge has read in log `k`, names as much;
otherwise it drops the message, which is no refusal for its sender (it proposes again only on a new
term or a new `latest`). So a barrier is appended at most once for each global and term, whoever
proposes it first, and the members' proposals cost messages, not entries.

**A barrier names a committed index.** A member proposes only an index its own log 0 holds
committed. Safety does not rest on it (§4.3: the order is a function of whatever the logs hold);
liveness does: every member's log 0 commits through any committed index eventually, so the merge
always reaches what a barrier names. A barrier naming an index log 0 has not committed would hold
its log until log 0 reaches that index, which an idle log 0 never does.

### 3.2 What a leader refuses

A leader takes a forwarded proposal (`MultiLog::step` on `MsgPropose`) only if every entry is one a
member may propose there: a global command in log 0, a keyed command in its key's log, a barrier in
a log other than log 0 (with §3.1's rule), the core's own change of configuration. Anything else is
refused before the core sees it (`Error::Violation`) and nothing changes. A member's own proposals
go through `MultiLog::propose`, which builds only well-formed entries.

### 3.3 n = 1

There is no log `k ≥ 1`, so nothing is owed and `barriers` proposes nothing.

### 3.4 Changes of configuration

Each log is a group of its own, with its own configuration; a change of the members is a change of
each log, `n` changes, each committed in its log. A change is applied to its log's member **as the
core gives it** (`MultiLog::hand_over`), in that log's order, not when the merge reaches it. The core
holds a campaign while a committed change waits to be applied (research §7). Were changes applied
at the merge, a log whose application waits behind a barrier would hold its members' campaigns, and
a group can then lose every log's progress: log 0's members that committed past a global cannot
campaign while their changes wait behind it, the member that leads log `k` cannot learn the global
committed without a log-0 leader, and the global waits for log `k`'s barrier. Applied as given, a
log's elections never wait on the merge. The merge consumes a change as nothing.

### 3.5 Changing `n`

A group changes its count of logs while it runs by a **resize**: an entry of log 0 naming the new
count (`MultiLog::propose_resize`), ordered as a global command is. It is Elastic Paxos's dynamic
subscription (research §9) with the place of the change fixed by the merge rather than computed
from two streams: every log stands at a barrier naming the resize before the merge takes it
(§3.1, §4.1), so the resize is a cut every member reaches alike, and at it:
- the logs past the new count **end**. What they hold before their barrier for the resize was
  consumed before it; nothing after it is read. Elastic Paxos removes a stream on one ordered
  request the same way (research §9).
- the logs it adds **begin**, each read from its first entry, under log 0's configuration as of
  the merge. The merge stops right after the resize, and each member opens the new logs on stores
  of its own (`MultiLog::open_logs`) before it reads on. A log that begins holds nothing ordered
  before the resize, so its entries need no barrier for it.
- every keyed command the merge consumes after it is routed by the new count. One proposed under
  the old count and committed after the resize, in a log its key no longer routes to, is refused
  as out of place (§2.2), alike on every member, and its owner proposes it again, as Elastic Paxos's
  clients resend a command a split sent to the wrong partition (research §9).

**Generations.** A log number can end and begin again (three logs to one, then to three), and the
new log must take nothing the old one sent. Each log has a generation, the count of resizes applied
when it began, kept in the cut and the image (`Cut::generations`, point version 2). Every message a
log's member sends carries its generation in the upper half of its priority (`MultiLog::stamp`),
and a member drops a message of another generation (`MultiLog::step`), as the network may drop it.
A resize is refused past `MAX_RESIZES` (`i32::MAX`), which keeps the stamp a non-negative priority.

**An ended log's last word.** The leader of an ended log drives it once more: its last heartbeats
tell the others the commit that holds the log's barrier for the resize, so their merges reach the
resize too (`MultiLog::take_ended`, which hands the owner the ended members to drive and stamp,
`MultiLog::stamp_ended`). A member that misses them reaches the resize through an image past it,
which log 0's leader sends with a snapshot once it compacts past the member (§5.3); the owner keeps
an ended log's storage until an image past the resize is durable, for a member that reopens from an
older image reads the log again.

**An image across a resize.** An image of other logs, or of other generations, is not installed in
place: the member reopens from it (`MultiLog::runs_generations_of`, §5.5).

**Evidence.** `tests/merge.rs`: `a_resize_that_adds_a_log_routes_what_follows_it_by_the_new_count`,
`a_resize_that_ends_logs_reads_nothing_past_their_barrier`, `a_resize_outside_log_0_is_refused`;
`tests/layer.rs`: `a_group_grows_and_shrinks_its_logs_and_every_member_applies_alike` (two logs to
three to one under writes, and a member reopened from an image two resizes back reaching the same
history), `a_message_of_another_generation_of_a_log_is_dropped`. The explorer runs both shapes with
members proposing resizes among one log and one more than the group began with, beside crashes,
loss, duplication and partitions: over its 59 seeds, 466 and 951 resizes applied and 151 and 90
keyed commands refused as a resize moved their key, every member's history alike
(`tests/multilog.rs`).

## 4. The merge

### 4.1 The rules

The merge holds, for each log, the index `next[k]` of its next entry to consume, and the log-0
index `epoch` of the last global command it applied (0 before any). It reads each log's entries
where its storage holds them (`Storage::any_entry`), from `next[k]` through what the core has given
to apply (`RawNode::given_to_apply`), and copies none: the owner's state machine is handed each
command's bytes where storage holds them.

It consumes, repeatedly and until no log moves:
- **log `k ≥ 1`**, in order: a keyed command, applied in the current `epoch`; a barrier naming `h`,
  passed once log 0 has been consumed through `h` (`next[0] > h`), and otherwise the log stops
  there; the core's own entries and changes, as nothing; a refused entry (§2.2), reported;
- **log 0**, in order: a keyed command, applied in the current `epoch`; a global command at `g`,
  applied only when every log `k ≥ 1` stands at a barrier naming `g` or later, and then `epoch`
  becomes `g`; and otherwise the log stops there; the rest as for log `k`.

A barrier naming a global passes when that global has been applied, slates' rule (`global <=
epoch`). A barrier naming an index that holds no global passes once log 0 is consumed through it,
where slates' rule waits for an epoch that never comes and holds its log forever. The two agree on
every barrier a correct member proposes.

The owner hands a budget of bytes to each call, and the merge stops once the commands it consumed
reach it, after one at least, so that a call's work is bounded (§6).

### 4.2 The order it applies in

Define, over the committed entries of all logs, the relation `≺` as the transitive closure of:
- (i) **log order**: within a log, an entry precedes every later entry;
- (ii) **after a barrier**: for a barrier `B(h)` at position `q` of log `k`, every log-0 entry at
  index `≤ h` precedes every entry of log `k` after `q`;
- (iii) **before a global**: for a global command at log-0 index `g`, every entry of log `k ≥ 1`
  before `q*_k(g)` precedes it, where `q*_k(g)` is the position of the first barrier in log `k`
  naming `g` or later.

**Lemma 1 (`≺` is a partial order, whatever the logs hold).** Give each entry a rank: a log-0
entry at index `i` ranks `i`; an entry at position `p` of log `k ≥ 1` ranks `M(p) + ½`, where `M(p)`
is the greatest index any barrier before `p` in log `k` names (0 if none). No edge lowers a rank,
and every edge but log order within a log `k ≥ 1` raises it:
- (i) in log 0 raises it; in log `k`, `M` never falls along the log;
- (ii) runs from `x ≤ h` to an entry after `B(h)`, whose `M ≥ h`, so `x < h + ½`;
- (iii) runs from an entry before `q*_k(g)`, after barriers that all name less than `g` (the first
  naming `g` or later is `q*_k(g)`), so its rank is at most `g − ½`, to the global ranked `g`.

A cycle returns to its rank, so it could hold only log-order edges of one log, which has none. No
property of the barriers' contents is used.

**Lemma 2 (`≺` orders every pair that interferes).** Two keyed commands of one key are in one log
(§2.1): (i). A global at `g` and an entry `p` of log `k ≥ 1`: if `p` is before `q*_k(g)`, (iii)
puts `p` first; otherwise `p` follows the barrier at `q*_k(g)`, which names `h ≥ g`, and (ii) puts
the global first. A global and a log-0 entry, or two globals: (i).

**Lemma 3 (the merge applies in an order that extends `≺`, at every member, whatever order the
logs' commits arrive in).** (i): each log is consumed in order. (ii): a barrier is passed only once
log 0 is consumed through what it names. (iii): when log 0's global at `g` is applied, each log
`k ≥ 1` stands at a barrier naming `g` or later; it cannot stand past `q*_k(g)`, since passing that
barrier (naming `h ≥ g`) needs log 0 consumed through `h`, the global among it; and it cannot stand
before `q*_k(g)` at a barrier, since every barrier before `q*_k(g)` names less than `g`. So it
stands exactly at `q*_k(g)`, every entry before consumed and none after.

**Theorem 1 (one application order).** Every member's sequence of applied commands is a member of
one command history (Lamport §4.4: the order of every interfering pair fixed, research §3): by
Lemmas 2 and 3, each orders every interfering pair as `≺` does, and `≺` is a function of the logs'
committed contents alone. So every member reaches the same state after applying the same commands,
and every command produces the same result on every member. In particular a keyed command at
position `p` of log `k` is applied in epoch `E(p)`, the greatest global index `≤ M(p)`, where
`M(p)` is the greatest index any barrier before `p` in log `k` names (and in log 0, the greatest
global before `p`): the merge cannot pass those barriers before log 0 is consumed through `M(p)`,
and no global past `M(p)` can be applied while `p` waits, since that would need log `k` standing
at a barrier before `p` naming more than `M(p)`. EPaxos calls Theorem 1 execution consistency
(research §3).

**What "one application order" does not mean.** Two members may apply keyed commands of two logs
in different interleavings. They commute, so no state and no result differs; a member that keeps a
running digest over the commands it applies, in the order applied, keeps it per log, or over a
structure whose digest does not depend on the order of commuting commands.

### 4.3 Linearizability

A client's command is invoked before it is proposed, and answered by a member only after that
member applied it. Let `a(c)` be the first time any member applies `c`.
- If `c1 ≺ c2`, every member applies `c1` first (Lemma 3), in particular the member that first
  applies `c2`: `a(c1) < a(c2)`.
- If `c1`'s answer precedes `c2`'s invocation, `a(c1) ≤ answer(c1) < invocation(c2) ≤ a(c2)`:
  `c2` is applied nowhere before it is proposed.

So `≺` and the real-time order of operations are both contained in the order of `a`, and their
union has no cycle. Any total order of the commands by `a`, ties broken freely, extends `≺`, so it
is equivalent to every member's execution (Theorem 1, interfering pairs in `≺`'s order), and it
extends the real-time order: Herlihy and Wing's L1 and L2 (research §5). The history of every
command, global ones among them, is linearizable; the proof is their Theorem 1's construction with
`≺` in place of the per-object orders. EPaxos's execution linearizability is the special case of
two interfering commands. Neither the barrier's naming a committed index nor anything else of
§3 is needed; Lemma 3 alone carries it.

### 4.4 Liveness

Every committed command is applied at every member that stays up, provided each log keeps a leader
long enough to commit, and some member that leads or relays to log `k` hears log 0's commits (§3.1):
- log 0's globals are committed, so every member's log 0 gives them to apply;
- for the least global `g` not yet applied, every log `k ≥ 1` either stands at a barrier naming `g`
  or later, or its next entries are consumable (no barrier for `g` before them): such a log moves
  until it holds a barrier for `g`, which some member proposes once its log 0 has `g` (§3.1), and
  which commits in log `k`;
- then `g` is applied, and every barrier naming `g` passes; by induction on the globals, every
  entry is consumed.

A log whose application waits never holds its own elections (§3.4), so no log's progress waits on a
log that waits on it.

### 4.5 What a lost leader costs

A global command waits for every log's barrier. A log without a leader adds no barrier, so every
global waits for its election, and every other log's entries behind a barrier for those globals
wait with them. That is a Mencius turn every log must take (research §4); slates measured it at
3,713–6,354 ms across five regions for a crash of any log's leader but log 0's, where with one log
only a crash of its one leader pauses commands (research §8). Keyed commands of logs without such a
barrier, and every keyed command while no global is in flight, do not wait.

## 5. Cuts, images and compaction

slates owed compaction across logs (research §6). An owner compacts a log by taking an image of its
state machine and dropping the log's entries the image holds; a member far behind is sent the image.
With `n` logs, an image covers a prefix of each, and must be one that any member can install
whatever its own state: a prefix of the one history (Theorem 1) that every state a member can reach
is comparable with, or a member could be handed an image ahead of it in one log and behind it in
another.

### 5.1 Canonical cuts

A **cut** is the merge's position: `next[k]` for every log and the `epoch`. The **canonical cut**
`C(g)` of a global command at `g` is the position right after it is applied: `next[0] = g + 1`,
`next[k] = q*_k(g)` for `k ≥ 1`, `epoch = g`. By Lemma 3 it is the same at every member. With
`n = 1`, every position is canonical.

**Lemma 4 (a canonical cut is comparable with every state a member reaches).** Let `S` be a merge
state some member reaches. If `S` applied `g`, it holds `C(g)`: `g` was applied with each log at
`q*_k(g)` (Lemma 3). If not, `S` holds no log-0 entry past `g`, and no entry of log `k` at or after
`q*_k(g)`, since passing that barrier needs `g` applied: `S ⊆ C(g)`. With `n = 1` both are prefixes
of one log. A cut at a log-0 index that holds no global is not canonical when `n > 1`: a member
whose log 0 runs ahead of the cut while a log `k` lags is neither ahead of it nor behind
(§11, step 2, holds this as a test).

Comparing an image's canonical cut `C` with a member's state `S` needs one number: `S` holds `C` if
and only if `S.next[0] ≥ C.next[0]` (with `n > 1`, holding log 0 past `g` is having applied it).

### 5.2 Taking an image

The owner may take an image only at a canonical cut: when the merge has just applied a global
command and the owner stops it there (the apply callback returns `Flow::Stop`), or anywhere when
`n = 1`. `MultiLog::point` gives the cut and, for each log, the term of its entry at `next[k] − 1`
and its configuration as of that entry (§5.4). An owner that wants an image where no global is due
proposes an empty global command: the cost of compaction is one global's barriers, at the rate the
owner compacts (`docs/durable.md` §6.1's rule, R22).

Then, in this order: the image (state and point) is made durable; each log `k` is compacted
through `next[k] − 1`, its snapshot's index, term and configuration from the point and its data the
image. The image is the base every log's snapshot shares.

### 5.3 Installing an image

A member's log installs a leader's snapshot only past its commit there (the core's rule), so the
image's cut is past what the member's merge read of that log, unless an image the member installed
through another log's snapshot already moved its merge there. The image is canonical, so by Lemma 4
it is then either ahead of the member's whole state or held by it already (`MultiLog::install`
says which, by log 0's position alone). Ahead, the member persists it as its new base, replaces its
state machine with it, and the merge moves to its cut. Its other logs keep their members as they
are: each goes on taking its entries (and changes) from its leader, the merge reading only from its
new position, or is sent a snapshot of its own, of this image or a later one.

### 5.4 Configurations at a cut

A change is applied as given (§3.4), ahead of the merge, so a log's configuration as of a cut is
not its member's latest. The layer keeps, for each log, its configuration as of the merge's
position and the changes applied since, each with its index: the merge's passing a change moves the
first into the second. They are at most the changes between the merge and what the log has given,
one for each such entry (§6).

### 5.5 Restart

A member restarts from its last image. Each log opens with its storage's configuration as the
image states it, and `Config::applied` at its cut's `next[k] − 1`; the core gives it the entries
past that again, and the layer applies their changes again, to a configuration that has not seen
them, so that none is applied twice. A log whose storage ends before its cut is first reset to the
image's snapshot for it. The merge resumes at the cut and reads from storage what the logs hold;
what a member applied after its image it applies again, alike (Theorem 1). `MultiLog::open` refuses
storage that does not hold the cut (`first ≤ next[k] ≤ last + 1`).

## 6. Bounds

| What | Bound | At the bound |
|---|---|---|
| Logs | stated by the owner: `n ≥ 1`, at most `u32::MAX` (a log's number travels as a `u32`) | refused at open |
| A log's member | its `hyper_raft::Limits`, derived from what the owner states of it (`docs/raft.md` §3.2) | the core's refusals |
| What a log holds beyond its merge | `Limits::unmerged`, entries, stated by the owner: the retained log's bound between images | a leader refuses a client's proposal there (`Error::Capacity("unmerged")`) and takes nothing from the fast track past it (`RawNode::cap_takes`, §9.1); barriers are always taken, being what lets the merge move |
| The merge's work a call | the owner's budget in bytes, one command at least | the call stops; `Advance::more` says so |
| The configurations kept (§5.4) | one for each change between the merge and what the log gave, so within `unmerged` and the log's own retained entries | grows with them only |
| Barrier proposals | one a log for each pair of a term and a `latest` | nothing more is proposed |
| The merge's state | `n` positions, `n` heads, the epoch | fixed at open |

Every queue of the core keeps its own bound. The layer keeps no copy of any entry: the merge reads
storage, and the owner's state machine is handed each command where storage holds it.

**Why `unmerged` is needed.** A log whose merge waits (a barrier whose global waits for a lost log,
§4.5) keeps committing: its leader does not know the merge waits. Without a bound, the log grows
until the stall ends, and its images cannot move past the stall (§5). Multi-Ring Paxos names the
same growth for a merge whose inputs run at different rates (research §4). The leader judges its
own merge, which every member's reaches alike (Theorem 1), up to what it has been given.

## 7. Leaders spread

MLRaft spreads the logs' leaders "by priority election and dynamic transfer" (research §1); slates
preferred the voter ranked `k` for log `k`; focal spreads preferred leaders across its many groups
and returns leadership to them (focal 27 §5). Here:
- `MultiLog::spread(ranked)`: the owner ranks the voters, best first (by its measured quorum round
  trips, `hyper_timing::quorum_priority`, or by placement); log `k`'s ranking is that list rotated
  by `k`, and a member's priority in log `k` is how many ranked voters it is ahead of or level with
  there (the preferred voter all `r` of them, the last one; a member not ranked none), so each log
  prefers a different voter and falls back through the same ranking. The core's elections then
  favour it (a voter of higher priority grants a lower one only a more current log, `docs/raft.md`
  §3.3).
- `MultiLog::hand_off(k)`: a leader of log `k` that is not its preferred voter names it, once its
  tracker shows that voter active and holding the leader's whole log. When to hand over is the
  owner's leadership-return policy (note 32 R11, a shell policy: focal's fit, quiet and rest rules),
  and the core's transfer catches the voter up and asks it to campaign.

### 7.1 A leader cut from a lower log's leader yields

In one Raft group a follower cut from its leader by a partial partition is only stale: the leader
still has a quorum through the others, and no client waits on the follower. In a multilog every
member is a follower of every log it does not lead, and its merge cannot pass a log's next barrier
or global without that log's entries, which only its leader sends. So a member cut from one log's
leader applies nothing past that log's next barrier or global, and if it leads another log, the
commands proposed there are never applied at it while the cut lasts. Measured (`docs/benchmarks.md`,
"Hostile networks"): a partial partition cutting log 0's preferred voter from the next-ranked one
for 20 s left one log untouched (keyed p99 127 ms, as with no cut) and three and five logs at a
keyed p99 of 18.8 and 18.2 s: the partition's length. Partial partitions are 29% of the 136
partition failures Alquraan et al. studied (OSDI 2018; `docs/research/tails.md`).

The rule, exact and local to each member:
- A member is *cut below* log `k` when it suspects the leader its share of some log `j < k` last
  named, and has heard of no other since (a member suspecting its leader campaigns and names none
  meanwhile, but is cut all the while). Log 0, and a single log, are never cut below.
- A member cut below `k` stands for election in `k` at priority zero, and, leading `k`, yields it
  (`MultiLog::hand_off`) to the first voter in `k`'s order of preference that it does not suspect,
  that did not state itself cut below `k`, that its leader hears and that holds its whole log.
- Every message a member's share of `k` sends other than a vote states whether it is cut below `k`
  (`MultiLog::stamp`: the message's priority field, which the core reads only in a vote and sets
  there itself, carries `CUT`); a leader hands `k` to no voter whose last message there stated it.
  When the cut heals the voter's messages say so, and `k` goes back to its preferred voter.

Of two leaders cut from each other, only the one leading the later log is cut below its log, so
exactly one yields and nothing is handed back and forth; the member that yielded leads nothing its
clients wait on while the cut lasts. With the rule, the same partial partition left three and five
logs at a keyed p99 of 524 and 525 ms. What remains above it is a command proposed at the yielding
leader before it yielded: its client waits for that member, cut, to apply it (the maximum, 20.1 s).

## 8. Reads

A read of a key asks log `log_of(k)` (`RawNode::read_index`) and is served once the merge has
consumed that log through the index the read state names (`MultiLog::merged_through`). A read of the
global state asks every log, and is served once the merge has consumed each through its own index:
every write answered before the read began is then applied (a write is answered only once applied,
§4.3). With `n = 1` both are the core's ReadIndex.

## 9. The owner's contract

The layer is sans-io, like the core: no clock, no disk, no network; time reaches each log's member
as the owner gives it (ticks, or `wake` and its detectors by suspicion), and the owner carries each
log's messages tagged with the log's number in a frame its transport checksums (the E2E's
datagram's CRC-32C, hyper-transport's AEAD). For each log, the owner drives its member as it drives
a single group (`node_mut`: `ready`, persist, send, `on_persist`), with these differences:
- the committed entries a `Ready` gives go to `MultiLog::hand_over`, which applies their changes
  and tells the member they are applied; the state machine is not handed them;
- the state machine's commands come from `MultiLog::apply`, in the merged order;
- `MultiLog::barriers` after every drive; `MultiLog::step` for every message, in place of the
  member's `step`;
- proposals through `MultiLog::propose`, or, for a batch of commands routed to one log,
  `MultiLog::propose_in`, which makes them one proposal of the core, carried to each follower by
  one append (measured: one proposal a command sent 8 messages a command at a batch of 64, one a
  batch 0.12, `docs/benchmarks.md`, "Against slates' MLRaft");
- each message a log's member sends stamped by `MultiLog::stamp` (§7.1), and `MultiLog::hand_off`
  asked by the owner's leadership policy for every log it leads;
- images at canonical cuts only, and their installation through `MultiLog::install` (§5).

Refused at open, with its reason: `Config::apply_unpersisted` (a leader's entries given before they
are durable are not in storage, where the merge reads).

### 9.1 The fast track

Each log may run the fast track (`Config::fast`, `docs/raft.md` §3.5): a member proposes a command
by `MultiLog::propose_fast` to every voter of the command's log, each holds it beside its log, and
the leader takes it into the log. The merge is unchanged: it reads what the logs committed, and an
entry taken from the fast track is a log entry like any other once taken. Three rules keep the
layer's other ways into a log what they were:
- **Commands only, in place.** A fast proposal is a global command in log 0 or a keyed command in
  its key's log. Anything else is refused at every member before the core sees it
  (`Error::Violation`), so no member holds it: a barrier goes by the leader alone, whose one
  barrier a global (§3.1) the fast track would bypass, and a misplaced command no member may
  vote for.
- **The bound holds.** A leader takes from the fast track only as far as `Limits::unmerged` past
  its merge (`RawNode::cap_takes`, set at each fast-track message and after each merge call):
  votes past the cap are kept within the core's bound on them and taken as the merge moves, so a
  stalled merge stops the fast track's growth of a log as it stops a client's proposals there.
  A leader that dropped such votes instead lost them for its term (a member says what it holds
  once a term), and the log's fast track stalled until another entry took those indexes.
- **The owner proposes again what was displaced.** A proposal another entry took the index of
  comes back in its log's `Ready` (`Ready::displaced`); no member applies it, and the owner
  proposes it again or answers that it was not taken, as for a single group.

Each log's storage keeps the core's contract for what a member holds (`docs/raft.md` §3.5,
"Storage").

**Evidence.** `tests/layer.rs`: `a_command_proposed_by_the_fast_track_is_applied_alike_everywhere`
(a keyed command and a global, each committed by the fast quorum, applied alike on every member),
`a_fast_proposal_out_of_place_is_refused_and_no_member_holds_it`, and
`a_leader_at_its_unmerged_bound_takes_nothing_by_the_fast_track` (the leader holds 2 entries past
its merge at a bound of 2, and 3 without the cap; once the merge moves it takes what was voted).
The explorer runs both shapes on the fast track beside the classic ones, every command proposed by
it and each displaced one proposed again, under loss, duplication, partitions and crash-restarts:
over its 59 seeds, 22 and 76 indexes committed by a fast quorum and 65 and 164 proposals displaced
(three voters with three logs, five with two), every member's history alike (`tests/multilog.rs`,
`docs/benchmarks.md`, "The multilog explorer"). `tests/allocs.rs`: a fast proposal through the layer allocates what the
core's own does.

## 10. What comes from where

| Piece | From |
|---|---|
| `n` logs over one set of voters, a leader each, leaders spread by priority and transfer | MLRaft (research §1), through slates |
| Keyed and global commands, routing by SplitMix64 of the key, globals in log 0, barriers, the merge's rules, `n = 1` as one code path, the explorer and the timed measurement | slates' MLRaft (research §8) |
| One application order as a command history; interference | Lamport's Generalized Consensus §4.4; EPaxos §4.1–§4.2 (research §3) |
| Commands of disjoint state in any order, of one state in log order | ParallelRaft §5.2 (research §3) |
| What a total order across logs costs a lost leader | Mencius §4.3–§4.4, Calvin §3.1, Multi-Ring Paxos §IV-A, Scalog §3 (research §4) |
| Linearizability across objects | Herlihy and Wing §2.2, §3.1 (research §5) |
| Several logs between the same nodes share their per-pair costs | TiKV, CockroachDB (research §2); hyper-liveness's one stream per node pair |
| The rule a barrier passes by (log 0 consumed through what it names), the refusal of what is out of place, members relaying barriers, changes applied as given, canonical cuts, the bound on what a log holds beyond its merge, reading storage in place | here, each for the reason given where it is stated |
| The core, its forwarding, priority, transfer, ReadIndex, changes | `hyper-raft` (focal's core with slates' enhancements, `docs/raft.md`) |
| Leadership-return timing | the owner's (note 32 R11; focal 27 §5) |

## 11. Tests and gates

Each step is one gated commit (`bash scripts/gates.sh` on its final tree).

1. **This design** and its sources.
2. **The crate.** `crates/hyper-multilog` over `RawNode`, held to the workspace's lints, no `unsafe`.
   - *The merge's determinism, exhaustively at small scope.* Every history of up to three logs over
     a small alphabet (keyed commands of two keys, globals, barriers naming each log-0 index and one
     past it), and for each every interleaving of the logs' commits arriving one entry at a time,
     with the merge called after each arrival, and again called only at the end: every run applies
     the same command history (each interfering pair in one order, each keyed command in one
     epoch), the same set of commands, and an order that extends `≺` computed independently from
     §4.2's definition (an oracle written from the definition, not from the merge).
   - *Beyond it, by property tests*: longer histories, random interleavings, random budgets and
     stops, restarts from a canonical cut, against the same oracle; and canonical cuts comparable
     with every state any interleaving reaches, a non-canonical one shown incomparable.
   - *n = 1 against the bare core*: one log through the layer and a bare `RawNode` on one schedule
     apply the same commands in the same order.
   - *The format*: every entry round-trips; malformed and out-of-place entries are refused on every
     member alike; a forwarded barrier the leader covers is dropped, an out-of-place proposal is
     refused at the leader.
3. **slates' tests, retargeted** (`tests/multilog.rs`, `tests/multilog_timed.rs`), each as slates
   states it: the unit tests of slates' `multilog.rs` (the codec, the routing's spread, one log in
   its own order, a global waiting for every barrier, replicas learning commits in any order, three
   logs led apart merging alike on every voter, each log handing off to its preferred voter); the
   explorer; the failure-path measurement across five regions. A test slates passes and this crate
   fails is a defect here, fixed at its cause. Where slates' test decides by a measured margin, the
   check is restated exactly (the mechanism the margin stood for, held for every command of the
   run), and the measured numbers are recorded beside slates' (the owner's rule: exact checks
   only).
4. **The explorer on hyper-sim** (`docs/sim.md`, S-1 and S-2): 59 seeds (Wilks's one-sided 95/95)
   at a scale measured down from slates' (`docs/benchmarks.md`, "The multilog explorer"), every
   member's real logs on the world and its network (free discipline: any
   delivery order; loss, duplication, partitions; crashes restarting from the last image; images
   and compaction at canonical cuts and snapshots sent to laggards), Raft's invariants per log and
   the merge's history across members and restarts after every step; the first seed through
   `twice` (`docs/sim.md` §3.9); named coverage counters, each with a floor set from its measured
   count (§4.4 there); a planted defect (a global applied without its barriers) caught.
5. **The law** (`CLAUDE.md` §1a): allocations, reallocations and page faults on the hot paths
   (a proposal, a merge step, a barrier) measured with `hyper-measure` and driven down; the layer
   against slates' MLRaft on slates' workloads in `hyper-raft-compare`; an E2E of real processes
   over UDP with fsynced logs running a multilog group, a member killed and a partition healed.
   Results in `docs/benchmarks.md` with hardware, date, load and command, said plainly whatever
   they show.
6. **Docs**: `docs/raft.md`'s row, `docs/STATUS.md`'s row, the crate's `ORIGIN.md`.

## 12. Every number

| Number | Value | Why |
|---|---|---|
| Tags | `0x01`, `0x02`, `0x03` | the format's (§2.2); a later version adds tags, never reuses one |
| `SUFFIX_BYTES` | 9 | a key's 8 bytes (`u64`) and the tag |
| A barrier's data | 9 bytes | an index's 8 and the tag |
| A resize's data | 9 bytes | a count's 8 and the tag |
| `MAX_RESIZES` | `i32::MAX` | a generation fills the upper half of a non-negative `i64` priority |
| Routing constants | §2.1 | SplitMix64 (Steele, Lea and Flood, OOPSLA 2014) |
| `n` | the owner's | configuration; at most `u32::MAX` (§6) |
| `Limits::unmerged` | the owner's | configuration: the retained log it allows between images (§6) |
| The merge's budget | the owner's, a call | configuration, as the core's `max_committed_size_per_ready` |
| Spread priorities | `1 ..= r` for the `r` voters ranked, `0` for one not ranked | how many ranked voters a member is ahead of or level with in the log's rotation of the ranking (§7) |
| A point's encoding | version 2 (the generations, §3.5), CRC-32C | the core's record format's conventions (`docs/raft.md` §3.1; RFC 3720 §B.4) |

The tests' shapes (seeds, steps, stretches, bags, keys, rates, regions) are slates', each stated
where it is used with slates' reason, and the coverage floors are set from measured counts.

## 13. Open

- Whether any owner gains: slates measured one log better for its groups (research §8); the
  measurements of §11 step 5 say what this core and layer show.
