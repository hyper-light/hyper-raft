# The core's model

`FastTrack.tla` is a TLA+ model of `crates/hyper-raft` as it is built: the classic track, a change
of the voters by one entry or through a joint configuration, and the fast track with both of its
rules (`docs/raft.md`, "The fast track's election defect, and its fix"). TLC checks it in CI only
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
| `ConfigurationOf(s)` | The configuration a member counts by: `Raft::apply_conf_change` applies a committed change, and `Raft::hup` campaigns only once every committed change is applied. |
| `FastCommit(l)` | `Raft::fast_commit` (`track.rs`). `OfTheRound` is the first rule, `self.log.term(progress.matched) == self.term`; `FastOfTheTerm` is the second, `Raft::fast_quorum_of_the_term` over the voters in force (`Tracker::has_fast_quorum`) and those `Raft::note_term_configuration` and `Raft::note_term_change` noted; `~Pending(l)` and `c.out = {}` are `Raft::has_pending_conf` and `Configuration::is_joint`. |
| `ClassicCommit(l, i)` | `Raft::maybe_commit`: the tracker's quorum index (`Tracker::quorum_index`, both halves of a joint configuration) and `Log::maybe_commit`, which commits only an entry of the leader's term. |
| `Replicate(l, m, p)` | `Raft::bcast_append` to the members of the leader's configuration; `Raft::handle_append_entries` → `Log::maybe_append` and `Raft::release_proposals` at the member; the answer, `Raft::handle_append_response` (`Progress::matched`, the model's `acks`). |
| `Elect(c, Q, V)` | `Raft::hup` (a voter of its own applied configuration), `Raft::campaign`, `Raft::step_vote` (the log comparison, `Log::is_up_to_date`, and the held entries sent with the vote), `Raft::poll`, `Raft::hear_report`, then `Raft::become_leader` → `Raft::note_term_configuration` and `Raft::recover` (the entry most held among the voters, at every index above the log). |

What the model leaves out, and why it is sound to:
- **Messages** are not modelled one by one: what was said stays said and may be acted on at any
  later time or never, which is every order, delay, repetition and loss.
- **A crash** is a member that does nothing for a while; what is durable is all a member has.
- **Pre-vote, check-quorum, priority and leader transfer** only refuse or bring forward a
  campaign, which the model may take at any time. **Learners** do not vote and are not counted.
  **ReadIndex** commits nothing. **A snapshot** stands for a committed prefix of a log.
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
| `change` | `FastTrackChange.cfg` | 3 voters, a change removes s3, 2 terms, 2 indexes (held at 2), 2 values | 3,304,320 | passes | 12 min 56 s |
| `grow` | `FastTrackGrow.cfg` | 3 voters, a change adds s4, 2 terms, 2 indexes (held at 2), 1 value | 3,084,745 | passes | 14 min 14 s |
| `classic` | `Classic.cfg` | classic core, 2 voters, a change adds s3, 3 terms, 3 indexes | 1,661,802 | passes | 7 min 2 s |
| `joint` | `ClassicJoint.cfg` | classic core, s1 replaced by s3 through a joint configuration, 3 terms, 3 indexes | 575,442 | passes | 1 min 4 s |
| `reached` | `FastTrackReached.cfg` | `round` and the claim `NoFastByHeld` | 367,248 (focal: 367,248) | refused: `NoFastByHeld` | 46 s |
| `anyround` | `FastTrackAnyRound.cfg` | `four` without the first rule | 190,662 (focal: 190,662) | refused: `LeaderHolds` | 37 s |
| `least` | `FastTrackWrong.cfg` | 5 voters, 2 terms, 1 index, the least-held entry recovered, no first rule | 89,337 (focal: 89,337) | refused: `LeaderHolds` | 2 min 58 s |
| `anyconfig` | `FastTrackAnyConfig.cfg` | `change` without the second rule | 755,201 | refused: `LeaderHolds` | 1 min 22 s |
| `growreached` | `FastTrackGrowReached.cfg` | `grow` and the claim `NoFastByHeldAfterChange` | 12,451 | refused: `NoFastByHeldAfterChange` | 2 s |

What each shows:
- `one`, `round`, `four`: focal's three, unchanged. `reached` shows that `round` commits an index
  by what members hold beside their logs; with one index and the first rule no index is ever so
  committed, which is why `round` has two.
- `anyround` is seed 9843's defect (`an_election_never_commits_a_second_entry_at_a_committed_index`):
  a member that holds the entry beside a log of an older term votes for a candidate that keeps
  its own entry at the index. `four` is the same four voters with the first rule.
- `anyconfig` is seed 54104's defect (`a_member_that_counts_by_the_configuration_before_commits_no_second_entry`)
  with three voters for five: the leader commits the change by s2, which does not hear that it is
  committed and counts by all three; the leader commits the next index by the fast quorum of the
  two voters left, s2 holding the entry beside its log; s3 holds another entry there, and s2 is
  elected by s3 and takes it. `change` is the same with the second rule. After a change that
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
five voters at two indexes: past that bound too, so `anyconfig` and `change` show the same defect
and rule at three.

**To change the model.** A change that makes a configuration larger or smaller fails the check
until its states are counted again and stated; a new configuration is first run with a
`StateBudget` it cannot exceed without being stopped, and its count recorded here with the run.
