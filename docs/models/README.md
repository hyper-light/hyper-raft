# The core's model

`FastTrack.tla` is a TLA+ model of `crates/hyper-raft` as it is built: the classic track, a change
of the voters by one entry or through a joint configuration, the fast track with both of its
rules (`docs/raft.md`, "The fast track's election defect, and its fix"), and members whose log loses
its tail at rest and reopens marked, with a marked member's election (core steps R-5 and R-7,
`docs/durable.md` §5.2). TLC checks it in CI only
(`CLAUDE.md` §1, "Model checking"): the `model` job of `.github/workflows/ci.yml` runs
`scripts/check-model.sh`, which runs every configuration below and fails on any that does not end
as it states. `scripts/gates.sh` does not run it: the gates run on the owner's machine, and TLC does
not (the same rule).

It is focal's model (focal `docs/models/FastTrack.tla` at `7ea6f63`), which had the first rule and
no change of configuration, with three things added: the configuration a member counts by, the
entries that change it, and the second rule. With no change configured (`Initial = Target`) no
step of the additions is enabled and every one of focal's configurations has exactly the states
focal counted, which is the check that the additions changed nothing else.

## What it checks

| Invariant | Raft's property (Ongaro, thesis Figure 3.2) | Statement |
|---|---|---|
| `OneLeader` | Election Safety | no term has two leaders |
| `LogMatching` | Log Matching | two logs that hold an entry of one term at one index hold the same entries through it |
| `LeaderHolds` | Leader Completeness | a leader of a later term holds what was committed, at its index |
| `Agreement`, `Committed` | State Machine Safety | no two members commit different entries at one index; what a member committed is what was first committed there |
| `TypeOK` | | the variables' types, what a member holds is above its log, a follower notes no configuration |
| `WithinBudget` | | the checker has found no more states than the configuration states |

An entry committed by the fast quorum is stamped with the term of the leader that took it, and
the next leader takes it again with its own, so the invariants compare what entries state, not
their terms.

## The actions and the code

| Action | What it is in `crates/hyper-raft` |
|---|---|
| `Hold(m, i, v)` | A voter holds a proposal beside its log: `Raft::hear_proposal` → `Raft::hold` (`track.rs`) → `Proposals::hold` (`fast.rs`); refused unless the member votes in its configuration and the index is above its log. Its vote to the leader: `Raft::send_vote`, `Raft::on_persist_proposals`. |
| `Say(m, i)` | A held entry said again to a leader of a later term: `Raft::heard_leader`. |
| `Take(l, v)` | The leader takes an entry for the next index: `Raft::leader_hears` → `Raft::decide` → `Raft::take`; its own proposals; its first entry, `Raft::become_leader`. |
| `Reconfigure(l)` | A change accepted by `Raft::propose`: one at a time (`pending_conf_index`, set to the end of the log at `Raft::become_leader`), and out of a joint configuration before into another; the entry that leaves a joint configuration by itself, `Raft::commit_apply` (auto-leave). |
| `ConfigurationOf(s)`, `Stands(c)` | The configuration a member counts by, the newest its log states (`Raft::refresh_configuration`, `docs/raft.md` §3.4), and who may campaign (`Raft::promotable`). |
| `FastCommit(l)` | `Raft::fast_commit` (`track.rs`). `OfTheRound` is the first rule, `self.log.term(progress.matched) == self.term`; `FastOfTheTerm` is the second, `Raft::fast_quorum_of_the_term` over the voters in force (`Tracker::has_fast_quorum`) and those `Raft::note_term_configuration` and `Raft::note_term_change` noted; `~Pending(l)` and `c.out = {}` are `Raft::has_pending_conf` and `Configuration::is_joint`. |
| `ClassicCommit(l, i)` | `Raft::maybe_commit`: the tracker's quorum index (`Tracker::quorum_index`, both halves of a joint configuration) and `Log::maybe_commit`, which commits only an entry of the leader's term. |
| `Replicate(l, m, p)` | `Raft::bcast_append` to the members of the leader's configuration; `Raft::handle_append_entries` → `Log::maybe_append` and `Raft::release_proposals` at the member; the answer, `Raft::handle_append_response` (`Progress::matched`, the model's `acks`). |
| `Elect(c, Q, V)` | `Raft::hup` (`Raft::promotable`), `Raft::campaign`, `Raft::step_vote` (the log comparison, `Log::is_up_to_date`, and the held entries sent with the vote), `Raft::poll`, `Raft::hear_report`, then `Raft::become_leader` → `Raft::note_term_configuration` and `Raft::recover` (the entry most held among the voters, at every index above the log). |
| `Lose(m, k)` | A member of `Losers` whose log lost its tail after `k` at rest and reopened: hyper-log's uncertainty mark (`Health::Marked`), passed to the core as `Config::lost` (`Lost`). It keeps its term and vote, marks through its last entry, the greater index and term where a mark had not yet ended (hyper-log's merge), and its commit falls to what it holds. The mark ends as `Lost::resolved_by` says, in `Replicate` (`Raft::settle_lost` at every notice) and at its election (`Raft::become_leader`). |
| `Claim(m)`, `Current(c, m)` | `Raft::claim` in `Raft::step_vote`: a voter grants only a log at least as current as its mark while marked. `Marks = "whole"` judges by the log alone (refused). |
| `Own(c)`, `Quorums(c, Q)` | `Raft::campaign` polling a marked candidate's own vote as a refusal (R-7): its quorum is of the others. `Marks = "self"` counts it (refused). `Raft::may_campaign`'s further limits (the others can be a quorum; not in a fast group) only refuse campaigns the model may take and win nothing by. |

What the model leaves out, and why it is sound to:
- **What a member keeps ahead of a hole** (core step R-3's R17, `Ahead::Kept`). The core's member
  takes kept entries into its log once an append of the same term fills the hole before them; the
  entries are the leader's own log at those indexes, which a leader never rewrites in its term, and
  the answer acknowledges them with the write that holds them. That leaves the member where one
  append carrying the hole's entries and the kept ones would, sent when the leader sent the last of
  them and delivered late: what `Replicate(l, m, p)` stands for whenever an append arrives late. No
  action changes.
- **Messages** are not modelled one by one: what was said stays said and may be acted on at any
  later time or never, which is every order, delay, repetition and loss.
- **A crash** is a member that does nothing for a while; what is durable is all a member has.
- **Readies ahead of their persistence** (core step R-4, `docs/durable.md` §2.1). A member of the
  model acts on durable state and its action is durable at once; the core may hold state that is not
  durable yet and decide on it. It never lets an output depend on that state before it is durable:
  a member that does not lead sends nothing before the write holding everything it held when it made
  the message is durable, a leader sends at once only while its term and vote are durable and counts
  itself only for what its own writes made durable, and writes are durable in the order issued (I1
  to I3 and I7). So a run of the core with writes out maps to a run of the model in which each
  member's step happens when the write that carries it is durable, and a crash that loses the writes
  out is the model's crash. No action changes; the pending write is not a state of the model.
- **The durable commit, the apply pause and applying before durability** (core step R-6,
  `docs/durable.md` §4.4). An answer's commit is what a leader learns of a member's commit; no
  action of the model reads it, nor any decision of the core, so holding it to the durable commit
  changes no run. The apply pause only defers applying, which no configuration a member counts
  by waits for (`docs/raft.md` §3.4). A leader that applies its own committed entries before its write is durable applies
  what a quorum holds durably, which `ClassicCommit` and `FastCommit` already require; the model has no
  state machine but the configuration, and a change waits for the durable commit (I5), which
  cannot cover an entry not durable at the member (I7). No action changes.
- **Pre-vote, check-quorum, priority and leader transfer** only refuse or bring forward a
  campaign, which the model may take at any time. So do **elections by suspicion** (timing step
  L-2, `docs/timing.md` §2.9): a campaign started by a detector's suspicion after a drawn delay,
  refused while a member and those it trusts are no quorum, brought forward by a hand-over's order;
  a leader stepping down when its detectors suspect a majority, which is a leader that stops acting;
  a lease that refuses votes while the leader is trusted rather than while it is heard; a follower
  forgetting its leader, which no action reads. None grants a vote the log comparison would refuse,
  so `Elect` is unchanged and so is every configuration's state count. **Learners** do not vote and are not counted.
  **ReadIndex** commits nothing. **A snapshot** stands for a committed prefix of a log.
- **A member's word that it lost entries** (R-5's refusal flagged lost, `Raft::take_lost`) lowers
  what its leader counts it holding; the model's leader keeps counting what it was told (`acks`)
  and so commits in more runs than the core. A lost write that was never acknowledged is a crash,
  which the model has; a mark covers what was acknowledged, which hyper-log's persist record, kept
  apart from its frame, names. Losing the term or the vote is a damaged member, rebuilt under a new
  identity, not modelled.
- **The leader's first entry** is an entry it takes like any other, so the model's leader may
  write a change before it; the core writes its first entry at once. The model has every run of
  the core and some more.
- **One change** is made at most, from `Initial` to `Target`.
- **A held proposal** is held only at the indexes `HeldAt` names; the empty set is the classic core.

## The configurations

Each configuration states its distinct states (`StateBudget`). A configuration that passes must
have exactly that many; one that is refused must be refused at exactly that many (the checker runs
it with one worker, so it stops at the same state every time). A refused configuration is the model
with a rule of the core taken out, or with a claim that must be false: if the checker cannot find
the defect the rule is for, the model checks nothing of the rule.

Measured on 2026-10-01 with TLC 1.7.4 on OpenJDK 26.0.2.1 (Homebrew), Apple M-series, macOS 26.4,
one worker, `TLC_MEMORY_MB=256`, beside other work (load average about 30 on 18 cores), by
`bash scripts/check-model.sh` on this tree:

| Name | Configuration | Bounds | Distinct states | Result | Time |
|---|---|---|---|---|---|
| `one` | `FastTrack.cfg` | 3 voters, 3 terms, 1 index, 2 values | 560,563 (focal: 560,563) | passes | 1 min 20 s |
| `round` | `FastTrackRound.cfg` | 3 voters, 2 terms, 2 indexes, 1 value | 2,462,010 (focal: 2,462,010) | passes | 6 min 33 s |
| `four` | `FastTrackFour.cfg` | 4 voters, 3 terms, 1 index, 1 value | 3,207,204 (focal: 3,207,204) | passes | 16 min 25 s |
| `change` | `FastTrackChange.cfg` | 3 voters, a change removes s3, 2 terms, 2 indexes (held at 2), 2 values | 2,841,943 | passes | 12 min 56 s |
| `grow` | `FastTrackGrow.cfg` | 3 voters, a change adds s4, 2 terms, 2 indexes (held at 2), 1 value | 5,228,729 | passes | 14 min 14 s |
| `classic` | `Classic.cfg` | classic core, 2 voters, a change adds s3, 3 terms, 3 indexes | 5,013,585 | passes | 7 min 2 s |
| `joint` | `ClassicJoint.cfg` | classic core, s1 replaced by s3 through a joint configuration, 3 terms, 3 indexes | 798,339 | passes | 1 min 4 s |
| `reached` | `FastTrackReached.cfg` | `round` and the claim `NoFastByHeld` | 367,248 (focal: 367,248) | refused: `NoFastByHeld` | 46 s |
| `anyround` | `FastTrackAnyRound.cfg` | `four` without the first rule | 190,662 (focal: 190,662) | refused: `LeaderHolds` | 37 s |
| `least` | `FastTrackWrong.cfg` | 5 voters, 2 terms, 1 index, the least-held entry recovered, no first rule | 89,337 (focal: 89,337) | refused: `LeaderHolds` | 2 min 58 s |
| `growreached` | `FastTrackGrowReached.cfg` | `grow` and the claim `NoFastByHeldAfterChange` | 239,653 | refused: `NoFastByHeldAfterChange` | 2 s |
| `marked` | `Marked.cfg` | classic core, 3 voters, any losing its log's tail at rest, 2 terms, 2 indexes | 196,484 | passes | 14 s (CI, 4 workers) |
| `markedchange` | `MarkedChange.cfg` | classic core, 2 voters and s3 added by one entry, any losing its tail, 2 terms, 2 indexes | 224,402 | passes | 5 s (CI, 4 workers) |
| `markedjoint` | `MarkedJoint.cfg` | classic core, s3 removed through a joint configuration, any losing its tail, 2 terms, 2 indexes | 784,034 | passes | 51 s (CI, 4 workers) |
| `markedself` | `MarkedSelf.cfg` | `marked` at 2 terms and 1 index, a marked candidate's own vote counted | 979 | refused: `LeaderHolds` | 1 s (CI) |
| `markedwhole` | `MarkedWhole.cfg` | `marked` at 2 terms and 1 index, voters judging by their logs | 1,187 | refused: `LeaderHolds` | 1 s (CI) |
| `markedreach` | `MarkedReached.cfg` | `marked` and the claim `NoMarkedLeader` | 241 | refused: `NoMarkedLeader` | 1 s (CI) |
| `changereach` | `MarkedChangeReached.cfg` | `markedchange` and the claim `NoMarkedLeader` | 1,023 | refused: `NoMarkedLeader` | 2 s (CI) |
| `jointreach` | `MarkedJointReached.cfg` | `markedjoint` and the claim `NoMarkedLeader` | 575 | refused: `NoMarkedLeader` | 1 s (CI) |

What each shows:
- `one`, `round`, `four`: focal's three, unchanged. `reached` shows that `round` commits an index
  by what members hold beside their logs; with one index and the first rule no index is ever so
  committed, which is why `round` has two.
- `anyround` is seed 9843's defect (`an_election_never_commits_a_second_entry_at_a_committed_index`):
  a member that holds the entry beside a log of an older term votes for a candidate that keeps
  its own entry at the index. `four` is the same four voters with the first rule.
- `anyconfig`, seed 54104's defect with three voters for five, was refused while a member counted
  by the configuration its committed log stated; since it counts by the newest its log states
  (`docs/raft.md` §3.4) that configuration passes with the rule out, and was retired
  (2026-10-05): the rule's catch is the core's (`tests/check.rs`, fast seed 1,483). `change` is
  the configuration with the second rule. After a change that
  removes a voter of three no fast quorum of the two is one of the three, so under the rule the
  leader commits by the classic quorum until its term ends.
- `grow` checks the second rule where it leaves the fast track open: after a change that adds a
  voter, three of the four that are the three of before. `growreached` shows that `grow` commits
  an index by what members hold after the change.
- `least` is recovery of the least-held entry, which the core does not do.
- `classic` and `joint` are the classic core alone, across a change by one entry and through a
  joint configuration (both halves' majorities, Ongaro's thesis §4.3).

**The resources.** The largest configuration, `change`, completed in 256 MB of heap (`-Xmx`) and
256 MB of direct memory (`-XX:MaxDirectMemorySize`, where TLC keeps its fingerprints), and its
largest live heap after a full collection was 108 MB (`-Xlog:gc`, the same run alone); no run's
resident set passed 514 MB. The whole suite took 65 min 31 s at one worker (3,931 s, `time -l`); its states, on disk while a configuration runs, are removed when it ends, however it ends.
CI's timeout is twice that, 131 minutes, for a runner core as slow as half of one here; CI runs
the configurations that pass with the runner's four workers (GitHub's standard Linux runner for a
public repository: 4 vCPUs, 16 GB), which the count of a complete run does not depend on, and the
refused ones with one. The first CI run's time is the measurement to replace this one with.

**What is not run.** focal measured these past 20 million states and none is run here: 3 voters
at 3 terms and 2 indexes, 4 voters at 2 terms and 2 indexes, 5 voters at 2 terms and 1 index; the
model before an election became one step took 208 million states and 26 GB of disk at 3 voters,
3 terms and 2 indexes without ending. A change of five voters to four, as seed 54104 had, is
five voters at two indexes: past that bound too, so `change` checks the rule at three.

**The marked members (R-7, 2026-10-02).** The model gained `Lose`, `Claim`, `Own`, the variables
`mark` and `markedLed` and the constants `Losers` and `Marks`. With `Losers = {}` no step of them is
enabled and `mark` and `markedLed` keep their first values: every configuration above has the
states it had (CI run 37044106956, all twelve at their counts), and each states `Losers = {}` and
`Marks = "core"`. TLC was not run on the owner's machine: the eight new configurations were counted
by CI (run 37048934354, the job's whole suite 35 minutes at four workers, the eight together under
two). Each is at the smallest scope that still does what it is for:
- `marked`, `markedchange`, `markedjoint`: every invariant at two terms and two indexes, the fewest at
  which a marked member's log holds an entry below what it lost and one is elected in the second
  term. Three voters at three terms and two indexes had passed 3.3 million states with 659,491 on
  the queue when its first ceiling stopped it (run 37044106956).
- `markedreach`, `changereach`, `jointreach` show each of the three elects a marked member, or it
  checks nothing of R-7: s1 leads term 1, writes an entry, loses it at rest, and is elected in term 2
  by the two others vouching for its log; after the change by one voter the same under the three
  voters it made. In `markedjoint` the halves are of two and three voters: no member of the half of
  two can be elected while marked (the other alone is no quorum), and the marked member elected is
  elected under the three voters before the change, which the run then passes through.
- `markedself` is the self-exclusion taken out: s1 commits an entry by s2, s2 loses it at rest, and
  s2 counts its own vote with s3's, whose log is empty, and leads without it.
- `markedwhole` is the voters' judgment by the mark taken out: s2, having lost the entry, votes for
  s3 by its own empty log.

`scripts/check-model.sh` now runs every configuration named and fails at the end if any did not end
as it states, so one run reports every count.

**The newest configuration in the log (2026-10-05).** `ConfigurationOf(s)` now reads a member's
whole log, and `Stands(c)` lets a voter of the configuration before an uncommitted change campaign
(`docs/raft.md` §3.4). The configurations with a change took new counts, counted by CI's model job
(runs 37296371662 and 37305116263) and, for those of one value, equal to the Rust mirror's
(hyper-check `tests/fasttrack.rs` on the release series); those without a change kept theirs.
The Reconfig rows' passing counts are the Rust search's, which TLC matched in those runs.

**The configuration a member counts by (`Reconfig.tla`, 2026-10-05).** A second module, of the
classic core alone across two changes of four servers (and a sole voter adding a second), with the
configuration elections and commitment count by as constants (`docs/research/reconfiguration.md`
§5, `docs/raft.md` §3.4). Its Rust mirror, hyper-check's `tests/models/reconfig.rs`, searches the
same specification with no symmetry and no reduction, so its class counts are the distinct states
the configurations state; CI's explore job holds the mirror to them and searches past them with
the reductions that note argues sound.

| Name | Configuration | Bounds | Distinct states | Result |
|---|---|---|---|---|
| `rjoint` | `ReconfigJoint.cfg` | the core's rule, A replaced by D through a joint configuration, 2 terms, 2 indexes | 4,945,526 | passes |
| `rpromote` | `ReconfigPromote.cfg` | the core's rule, D promoted then A demoted, 2 terms, 2 indexes | 6,496,567 | passes |
| `rsingle` | `ReconfigSingle.cfg` | the core's rule, a sole voter adds a second by one entry, 3 terms, 3 indexes | 31,203 | passes |
| `rsinglejoint` | `ReconfigSingleJoint.cfg` | the same through a joint configuration | 24,460 | passes |
| `rapplied` | `ReconfigApplied.cfg` | `rjoint` by the configuration applied (raft-rs's rule) | 1,171,121 | refused: `OneLeader` |
| `rpending` | `ReconfigPending.cfg` | `rjoint` and the claim `NoElectedOnPending` | 603 | refused: `NoElectedOnPending` |
| `rstood` | `ReconfigStood.cfg` | `rjoint` and the claim `NoElectedUnnamed` | 27,962 | refused: `NoElectedUnnamed` |
| `rremove` | `ReconfigRemove.cfg` | the core's rule with priority (A, then C) by the log's precedence, B removed through a joint configuration, 2 terms, 3 indexes | 667,123 | passes |
| `rlength` | `ReconfigRemoveLength.cfg` | `rremove` by raft-rs's precedence of length | 848 | refused: `Elects` |

Elections by the newest configuration and commitment by the one applied, the thesis's rule for
elections alone, is refused for `LeaderHolds` only at three terms, past what TLC runs here at one
worker in the job's time; the mirror refutes it there with the reductions, in eleven steps
(`elections_by_the_newest_and_commits_by_the_applied_lose_a_committed_entry`).

**Holdings kept until a classic commit (2026-10-05).** `docs/raft.md` §3.5: a fast quorum counts
what members hold by themselves, and a member holds what it holds until it knows the index
committed by a classic quorum (`Releases = "classic"`, `Votes = "held"`, `Reports = "held"` in every
configuration below that does not name another). Every configuration took new counts, counted by
CI's model job (runs 37336523719, 37358361974 and 37365272958) and, for those that pass, equal to hyper-check's
Rust model where it searches them (`tests/fasttrack.rs`, `the_model_counts_what_tlc_counts`):

| Name | Distinct states | Result |
|---|---|---|
| `one` | 573,160 | passes |
| `round` | 3,974,278 | passes |
| `four` | 3,602,968 | passes |
| `change` | 4,067,274 | passes |
| `grow` | 9,551,710 | passes |
| `classic` | 841,050 (two indexes now; three were 5,013,585) | passes |
| `joint` | 2,626,941 | passes |
| `reached` | 285,888 | refused: `NoFastByHeld` |
| `anyround` | 315,519 | refused: `LeaderHolds` |
| `least` | 264,569 | refused: `LeaderHolds` |
| `growreached` | 1,571,732 | refused: `NoFastByHeldAfterChange` |
| `marked` | 303,762 | passes |
| `markedchange` | 431,168 | passes |
| `markedjoint` | 1,182,158 | passes |
| `markedself` | 1,005 | refused: `LeaderHolds` |
| `markedwhole` | 1,247 | refused: `LeaderHolds` |
| `markedreach` | 250 | refused: `NoMarkedLeader` |
| `changereach` | 1,165 | refused: `NoMarkedLeader` |
| `jointreach` | 586 | refused: `NoMarkedLeader` |
| `roundballot` | 3,974,278 | passes |
| `oneb` | 854,746 | passes |

New with it:
- `restamp` (`FastTrackRestamp.cfg`): three voters, two terms, three indexes, the entry held at
  index 2, 3,808,625 states, passes. A member keeps the stamp of an entry it committed that a later
  leader's election took again under its own term, so `LogMatching` compares terms only where
  neither member has committed; `restampreach` (`FastTrackRestampReached.cfg`, 1,230,055 states,
  refused: `NoRestamp`) shows the scope reaches such a pair, which no other configuration does.
- The scenario of swarm fast seed 41,345 (`FastTrackScenario.tla`, searched whole from the end of
  its term 1): `scenario` 3,870,308, passes; `before` (both rules the core had) 2,549, `logs`
  (holdings counted from logs) 2,553 and `covered` (the release on log coverage) 2,549, each refused: `LeaderHolds` (`covered` starts
  where `before` does: under the release on log coverage the leader let go of its holding once it
  took the entry, CI run 37365272958); `ballot` (slates' highest
  ballot) 495, refused: `LeaderHolds`; design B `designb` 3,066,656 and `designblog` 1,097,636,
  pass.

The suite no longer fits one job's timeout at four workers (run 37336523719 passed 131 minutes), so
CI runs it in five parts side by side, each configuration naming its part in
`scripts/check-model.sh`; in run 37358361974 the longest part took 55 minutes.

**Priority and the liveness it is for (2026-10-05).** A voter of higher priority votes for a
candidate of lower only where the candidate holds what `Precedence` asks: by `"log"`, the core's,
a more current log; by `"length"`, raft-rs's, more entries (`Grants`). A member's priority is in
force once it has a term and while it may campaign, as `Raft::settle_priority` has it. `Elects`
states the liveness the precedence is for as a state predicate: with every member up, some member
that may campaign is granted, at a term past every member's, the votes of a quorum of the
configuration it counts by. The core's configurations check it (`rjoint`, `rpromote`, `rsingle`,
`rsinglejoint`, with no member ranked above another, and `rremove`, ranked). `rlength` is
`rremove` by raft-rs's rule, and the checker must refuse it: A, of the highest priority, holds the
longest log, of an older term, refuses both candidates of the later term, one of which counts by
the joint configuration that needs A's vote, and can never be elected itself, the shape of
hyper-check's swarm, group seed 9,657, which no model checked before, as none had priority or a
liveness property (`docs/raft.md` §3.3). CI's TLC counted both (run 37387647725: `rremove` at the
mirror's 667,123, `rlength` refused at 848). The mirror finds the same
(`by_the_logs_precedence_a_group_with_every_member_up_can_always_elect`).

**To change the model.** A change that makes a configuration larger or smaller fails the check
until its states are counted again and stated; a new configuration is first run with a
`StateBudget` it cannot exceed without being stopped, and its count recorded here with the run.
