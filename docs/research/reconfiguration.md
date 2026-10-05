# Research: which configuration an election counts by

Source notes for the core's election rule across changes of its voters (`docs/raft.md` §3.4). Each
entry says what the source establishes, read on 2026-10-05 (the dissertation and the MongoDB paper as
PDFs, etcd's `raft.go` at `main`, the raft-dev thread as archived).

## 1. The defect

hyper-check's random walk found two leaders of one term (fast seed 135,923 of `tests/check.rs`'s
fast schedules, 2026-10-05), and the core before any change does the same in a scripted run
(`tests/group.rs`,
`a_member_that_holds_a_change_it_has_not_committed_leads_no_term_another_leads`), where it goes on
to commit a second entry at an index. Five voters go to `{2, 3, 5}` through a joint configuration
and then to `{2, 3}`. Member 1 holds the joint configuration's two entries in its log with a commit
of 1: an append that arrived ahead of a hole was kept (R-3's R17) and a stale one, sent with a
commit of 1, filled the hole. A configuration here takes effect when it is applied (raft-rs's and
etcd's rule), so member 1 counts by the five voters. Members 4 and 5, whose logs are no longer than
its own, elect it: three of five. Member 3 leads the same term by `{2, 3}`, and member 1 commits its
own entry at the index `{2, 3}` committed the second change at. No restart and no fault is needed,
and the harness's stopping of members that left is beside the point: member 1's own configuration
names it a voter, and no owner can be relied on to stop a process the core does not know was
removed. A core whose safety needs its removed members stopped is unsafe while they are not.

## 2. Sources

**Ongaro, *Consensus: Bridging Theory and Practice*, PhD dissertation, Stanford, 2014.**
- **§4.1.** "The new configuration takes effect on each server as soon as it is added to that
  server's log ... each server always uses the latest configuration found in its log." "Servers
  always use the latest configuration in their logs, regardless of whether that configuration entry
  has been committed." The reason given: "If servers adopted Cnew only when they learned that Cnew was
  committed, Raft leaders would have a difficult time knowing when a majority of the old cluster had
  adopted it ... the servers would need to persist their commit index to disk." And the cost: "a log
  entry for a configuration change can be removed (if leadership changes); in this case, a server
  must be prepared to fall back to the previous configuration in its log." "Servers without Cnew
  cannot be elected leader" once Cnew is committed.
- **§4.1, voting.** "A server also grants its vote to a candidate that is not part of the server's
  latest configuration (if the candidate has a sufficiently up-to-date log and a current term)": a
  voter does not judge a candidate by configuration.
- **§4.2.2, a leader removed.** "A leader that is removed from the configuration steps down once the
  Cnew entry is committed"; meanwhile "it replicates log entries but does not count itself in
  majorities". And: "a server that is not part of its own latest configuration should still start new
  elections, as it might still be needed until the Cnew entry is committed (as in Figure 4.6). It does
  not count its own vote in elections unless it is part of its latest configuration." Figure 4.6: S1
  removes itself from {S1, S2}; until S2 holds Cnew it needs S1's vote, which S1 refuses for S2's
  shorter log, so only S1 can lead.
- **§4.2.3, disruptive servers.** A removed server can start elections; Pre-Vote does not stop all of
  them; the rule is that "if a server receives a RequestVote request within the minimum election
  timeout of hearing from a current leader, it does not update its term or grant its vote." It is
  about disruption (liveness), not safety: under the thesis's rule a removed server cannot win.

**Ongaro, "bug in single-server membership changes", raft-dev, 10 July 2015**
(<https://groups.google.com/g/raft-dev/c/t4xj6dJTP6E>). Found by Zhang and Amos formalizing
single-server changes: with leader changes, two uncommitted single-server changes from different
terms can be adopted by different members and their majorities need not meet, and a committed entry
can be overwritten. The fix: a new leader commits an entry of its own term before it starts a change.
The principle it restates is the one at stake here: a decision taken by one quorum must be visible to
every later quorum, across changes.

**etcd raft, `raft.go` (`hup`, `hasUnappliedConfChanges`).** A configuration takes effect when
applied ("the inputs usually result from restoring a ConfState or applying a ConfChange"), and a
member refuses to campaign while a configuration entry is "committed but unapplied": the scan runs
over `(applied, committed]` only. An entry past the member's commit is not considered, which is the
case of §1: etcd's rule leaves it open, and so did this core's, which has the same rule
(`Raft::hup`, `Raft::has_pending_conf`).

**Schultz, Zhou, Dardik, "Design and Analysis of a Logless Dynamic Reconfiguration Protocol"
(MongoRaftReconfig), OPODIS 2021, arXiv:2102.11960.** §3.4: "If a replica set server is a candidate
for election in configuration Ci, then a prospective voter in configuration Cj may only cast a vote
for the candidate if Ci is newer than or equal to Cj", and a configuration is "deactivated" before a
newer one is installed (Q1, Q2, P1 of §3.3). The protocol carries configurations outside the log, so
it needs the voter's check to deactivate old ones; a log-based Raft gets the same from the log
comparison once the candidate counts by the newest configuration its log holds (§3, option a).

## 3. The options

Write `C*` for the newest configuration committed in the group. This core allows at most one
configuration entry the leader has not applied (`pending_conf_index`), and a new leader proposes no
change before the end of its log at its election is applied, which takes committing an entry of its
own term (Figure 3.7's rule): the 2015 fix holds already. So every log holds at most one configuration
entry past `C*`.

**(a) Elections count by the newest configuration in the candidate's log; commitment by the one the
leader applied** (the thesis's rule for elections, raft-rs's for commits). The argument first written
here for it: a candidate's log holds every committed configuration entry, so it counts by `C*` or
`C*+1`, adjacent configurations' quorums meet, and a leader commits by `C*` or the one before it.
The model refutes it (§5): `LeaderHolds` fails, at three terms with two changes, both through a joint
configuration and by one voter at a time. A leader that counts commitment by an older configuration
than the one later candidates count elections by commits by a quorum that a later election's quorum
need not meet: the adjacency holds between the configurations, not between the one a commit used and
the one an election two changes on uses. **Refused.**

**(a') Elections and commitment count by the newest configuration in the member's log** (the thesis's
rule, §4.1, for both), both halves of a joint one, falling back when the entry is replaced; a leader
the configuration leaves out leads, counting itself nowhere, until the entry is committed; a voter of
the configuration before an uncommitted change that leaves it out may campaign, its own vote counting
nowhere (§4.2.2).
- *Safety.* The thesis's §4.1 argument: each decision, an election or a commit, is taken by a quorum
  of the configuration its taker's log states, and that configuration and every one a later taker
  counts by are one change apart in turn, because a change is proposed only once the one before it is
  applied (so committed) and a new leader's first change waits for an entry of its term. The model
  passes it (§5).
- *Liveness.* The thesis's. A member added counts once its log holds the entry that adds it and may
  be seeded by a snapshot older than that entry, so a snapshot that does not name the member is taken
  (§4.1: servers process requests "without consulting their current configurations"). A member that
  holds its own removal campaigns only while the removal is past its commit and it voted before it:
  without that, seed 11 of the group schedules had no member that could be elected (Figure 4.6's case
  through a joint configuration). A leader answers a read alone only when it is the one voter: a
  leader the newest configuration leaves out leads on, and that configuration's one voter may have
  been elected and committed since.
- *For the consumers.* No byte of the wire or of storage changes. A leader commits by the newest
  configuration (a sole voter that adds a second no longer commits alone past the entry), the owner is
  told what it applies as before, and an owner that holds a committed change no longer holds its
  member's campaign. A group mixing this core and raft-rs must not change its configuration: raft-rs
  counts by what it applied.

**(b) A member refuses to campaign while its log holds a configuration entry past its commit** (etcd's
rule, over the uncommitted entries too). Safe: the candidate holds every committed entry, and here it
campaigns only when none past its commit remains, so it counts by `C*`. Not live: if the leader
commits `C*+1` and fails before any member hears of the commit, every member holds `C*+1` uncommitted
and none campaigns, for ever. **Refused.**

**(c) A voter refuses a candidate while its own log holds a configuration entry past its commit that
excludes or changes the electorate.** The same loss of liveness when every member holds such an entry,
and it contradicts §4.1's rule that a voter does not judge a candidate by configuration. **Refused.**

**(d) MongoRaftReconfig's voter check: a voter grants only a candidate whose configuration is as new
as its own.** With configurations applied late, the voters of §1 may all hold `C1` unapplied and
compare equal, so the check alone does not reach the defect; with each side's configuration taken as
the newest in its log, it is implied by (a')'s log comparison. Not needed beside (a').

## 4. Chosen

(a'): the thesis's rule, for elections and for commitment, with §4.2.2's campaign of a member that may
still be needed, and §4.1's processing of what a leader sends without consulting the configuration.
Built in `Raft::refresh_configuration`, `Raft::promotable` and `Raft::restore`; `docs/raft.md` §3.4
has the rule as built, every surface and the configuration it counts by, and the tests.

## 5. The model

`docs/models/Reconfig.tla`, and its Rust mirror `crates/hyper-check/tests/models/reconfig.rs`
searched by `hyper_check::explore`: four servers through two changes (scenario "promote": D a learner
made a voter, then A a voter made a learner; "joint": A replaced by D through a joint configuration,
left by itself), and a sole voter adding a second ("single", by one entry; "singlejoint", through a
joint configuration). An append carries the leader's commit or not, so a member can hold a change past
its commit, as §1's member 1 did. The constants `Elections` and `Commits` name the configuration each
counts by, `"applied"` or `"newest"`, and `Stand` who campaigns.

The Rust search's counts are TLC's distinct states for the same configuration: it holds the
specification's variables, actions and invariants with no symmetry and no reduction, and
`docs/models/README.md` lists each configuration with the count both reach and the run that counted
it. Beyond those, where a full search would hold more states than this machine's memory, the Rust
search counts a state by what any step or invariant can still read of it, and by orbit under swapping
B and C:
- *What for whom a member voted.* `Asked` reads only whether a member's vote is `Nobody`; no
  invariant reads `vote`. Two states that differ only in whom non-`Nobody` votes name have the same
  successors up to that field and the same invariant values, so they are bisimilar.
- *What members acknowledged to a leader of term `t`, once no member leads `t` and every member's
  term is at least `t`.* Only `Commit` reads `acks[·][t]`, and only for a leader of term `t`. Terms
  never fall, and an election into `t` needs a candidate whose term is below `t`, so no leader of `t`
  exists in any successor: those fields are never read again, and zeroing them is a bisimulation.
- *B and C.* Every configuration of the promote and joint chains names B and C in the same roles, no
  action or invariant names a server but through a configuration, and once votes are only whether
  one was cast no variable holds a server's name: swapping B's and C's variables maps every behavior
  to a behavior and every invariant to itself, a symmetry (Ip and Dill, "Better verification through
  symmetry", Formal Methods in System Design 9, 1996).
A violation reachable in the full search is reachable in the reduced one and conversely; the reduced
counts are classes, not TLC's states, and are reported as such.
