------------------------------- MODULE Reconfig -------------------------------
(***************************************************************************)
(* hyper-raft's classic core across changes of its configuration, and the *)
(* configuration an election counts by (docs/raft.md §3.4,                 *)
(* docs/research/reconfiguration.md).                                      *)
(*                                                                         *)
(* A configuration names voters, the voters it leaves when it is joint,   *)
(* and learners.  The group goes through Chain, one configuration after   *)
(* another: an entry of the log names the configuration it moves to.  A   *)
(* leader proposes the next one only once no configuration entry waits    *)
(* past its commit (pending_conf_index) and everything it held when it    *)
(* was elected is committed.  The entry that leaves a joint configuration *)
(* is the next of Chain, which a leader writes once the joint one is      *)
(* committed (auto-leave).                                                 *)
(*                                                                         *)
(* An election counts by Elections, and a leader counts commitment by    *)
(* Commits, each:                                                          *)
(*   "newest"   the newest configuration in the member's log, committed  *)
(*              or not (the thesis's §4.1, the rule the core keeps);      *)
(*   "applied"  the configuration of its committed log (raft-rs's, the    *)
(*              core's before: refused, ReconfigApplied.cfg, and mixed    *)
(*              with "newest", ReconfigMixed.cfg).                         *)
(* Who campaigns is Stand:                                                 *)
(*   "voter"    a voter of the configuration it counts by;                *)
(*   "needed"   that, or, while the newest configuration entry in its log *)
(*              is past its commit, a voter of the configuration before   *)
(*              it, for the group may still need it (thesis §4.2.2, the   *)
(*              core's); its own vote counts only where it is a voter.     *)
(* A configuration entry a later leader overwrites is gone from the log,  *)
(* and the newest configuration falls back to the one before (thesis      *)
(* §4.1): the definitions read the log as it is.                           *)
(*                                                                         *)
(* A leader's append carries what follows a point through any index of its*)
(* log, and the member learns the leader's commit with it, or nothing of  *)
(* it: an append that arrived ahead of a hole, or late, brings entries     *)
(* without the commit the leader has since.  Messages are otherwise as in *)
(* FastTrack.tla: what was said stays said, and elections are one step.   *)
(*                                                                         *)
(* Nothing grows without a bound: StateBudget, as in FastTrack.tla.        *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets, Sequences, TLC

CONSTANTS Servers, Values, Noop, Nobody, MaxTerm, MaxLen, Elections, Commits, Stand,
          A, B, C, D,  \* the four members the scenarios name
          Scenario,    \* "promote" | "joint"
          StateBudget

\* A configuration: voters, the voters it leaves (joint), learners.
Conf(v, o, l) == [voters |-> v, outgoing |-> o, learners |-> l]
\* "promote": D a learner made a voter, then A a voter made a learner.
\* "joint": A replaced by D through a joint configuration, left by itself.
\* "single": a sole voter A adds B by one entry; counting commitment by what
\* it applied, A commits by {A} until the entry is committed, while B
\* counts its elections by {A, B}.
\* "singlejoint": the same through a joint configuration.
Chain ==
  CASE Scenario = "promote" ->
         << Conf({A, B, C}, {}, {D}), Conf({A, B, C, D}, {}, {}), Conf({B, C, D}, {}, {A}) >>
    [] Scenario = "joint" ->
         << Conf({A, B, C}, {}, {}), Conf({B, C, D}, {A, B, C}, {}), Conf({B, C, D}, {}, {}) >>
    [] Scenario = "single" ->
         << Conf({A}, {}, {B}), Conf({A, B}, {}, {}) >>
    [] Scenario = "singlejoint" ->
         << Conf({A}, {}, {}), Conf({A, B}, {A}, {}), Conf({A, B}, {}, {}) >>

Indexes == 1..MaxLen
Configs == 2..Len(Chain)
Entries == [term : 1..MaxTerm, value : Values \cup {Noop} \cup Configs]

VARIABLES term, vote, role, log, commit, acks, chosen
vars == <<term, vote, role, log, commit, acks, chosen>>

NotChosen == [value |-> Noop, term |-> 0]
Min(x, y) == IF x < y THEN x ELSE y
Max(x, y) == IF x > y THEN x ELSE y
LastTerm(l) == IF Len(l) = 0 THEN 0 ELSE l[Len(l)].term

Voters(c) == c.voters \cup c.outgoing
Members(c) == Voters(c) \cup c.learners
Majority(S, H) == 2 * Cardinality(H \cap S) > Cardinality(S)
QuorumOf(c, H) == Majority(c.voters, H) /\ (c.outgoing = {} \/ Majority(c.outgoing, H))

IsConf(e) == e.value \in Configs
\* The configuration the log l states through index upto.
ConfThrough(l, upto) ==
  LET at == {i \in 1..upto : IsConf(l[i])}
  IN IF at = {} THEN Chain[1]
     ELSE Chain[l[CHOOSE i \in at : \A j \in at : j <= i].value]
ConfIndex(l, upto) ==
  LET at == {i \in 1..upto : IsConf(l[i])}
  IN IF at = {} THEN 1 ELSE l[CHOOSE i \in at : \A j \in at : j <= i].value
Applied(s) == ConfThrough(log[s], commit[s])
Newest(s) == ConfThrough(log[s], Len(log[s]))
ElectionConf(s) == IF Elections = "newest" THEN Newest(s) ELSE Applied(s)
CommitConf(s) == IF Commits = "newest" THEN Newest(s) ELSE Applied(s)
Pending(s) == \E i \in (commit[s] + 1)..Len(log[s]) : IsConf(log[s][i])

TypeOK ==
  /\ term \in [Servers -> 0..MaxTerm]
  /\ vote \in [Servers -> Servers \cup {Nobody}]
  /\ role \in [Servers -> {"follower", "leader"}]
  /\ \A s \in Servers : Len(log[s]) <= MaxLen /\ commit[s] <= Len(log[s])
                        /\ \A i \in 1..Len(log[s]) : log[s][i] \in Entries

Init ==
  /\ term = [s \in Servers |-> 0]
  /\ vote = [s \in Servers |-> Nobody]
  /\ role = [s \in Servers |-> "follower"]
  /\ log = [s \in Servers |-> << >>]
  /\ commit = [s \in Servers |-> 0]
  /\ acks = [s \in Servers |-> [t \in 0..MaxTerm |-> 0]]
  /\ chosen = [i \in Indexes |-> NotChosen]

Write(l, v) ==
  /\ Len(log[l]) < MaxLen
  /\ log' = [log EXCEPT ![l] = Append(@, [term |-> term[l], value |-> v])]
  /\ UNCHANGED <<term, vote, role, commit, acks, chosen>>

\* A leader takes an entry: its own first, or a value.
Take(l, v) == role[l] = "leader" /\ Write(l, v)

\* A leader writes the next configuration once none waits past its commit
\* and what it held when elected is committed.
Reconfigure(l) ==
  LET k == ConfIndex(log[l], commit[l]) IN
  /\ role[l] = "leader"
  /\ k < Len(Chain)
  /\ ~Pending(l)
  /\ \A i \in 1..Len(log[l]) : log[l][i].term < term[l] => i <= commit[l]
  /\ Write(l, k + 1)

\* A quorum of the leader's committed configuration holds its entry at i.
Commit(l, i) ==
  LET c == CommitConf(l) IN
  /\ role[l] = "leader"
  /\ i > commit[l]
  /\ i <= Len(log[l])
  /\ log[l][i].term = term[l]
  /\ QuorumOf(c, {m \in Voters(c) : m = l \/ acks[m][term[l]] >= i})
  /\ commit' = [commit EXCEPT ![l] = i]
  /\ chosen' = [j \in Indexes |->
                 IF j > commit[l] /\ j <= i /\ chosen[j] = NotChosen
                 THEN [value |-> log[l][j].value, term |-> term[l]]
                 ELSE chosen[j]]
  /\ UNCHANGED <<term, vote, role, log, acks>>

\* A member of the leader's configuration takes what follows the point p,
\* through k, and the leader's commit with it if learns.
Replicate(l, m, p, k, learns) ==
  /\ l # m
  /\ role[l] = "leader"
  /\ m \in Members(Applied(l)) \cup Members(Newest(l))
  /\ term[m] <= term[l]
  /\ p <= k
  /\ k <= Len(log[l])
  /\ \/ p = 0
     \/ p <= commit[m]
     \/ p >= 1 /\ p <= Len(log[m]) /\ log[m][p].term = log[l][p].term
  /\ LET from    == Max(p, commit[m]) + 1
         differs == {i \in from..k : i > Len(log[m]) \/ log[m][i].term # log[l][i].term}
         taken   == IF differs = {}
                    THEN log[m]
                    ELSE LET c == CHOOSE i \in differs : \A j \in differs : i <= j
                         IN SubSeq(log[m], 1, c - 1) \o SubSeq(log[l], c, k)
     IN /\ log' = [log EXCEPT ![m] = taken]
        /\ commit' = [commit EXCEPT ![m] =
                        IF learns THEN Max(@, Min(commit[l], k)) ELSE @]
  /\ term' = [term EXCEPT ![m] = term[l]]
  /\ vote' = [vote EXCEPT ![m] = IF term[m] = term[l] THEN @ ELSE Nobody]
  /\ role' = [role EXCEPT ![m] = "follower"]
  /\ acks' = [acks EXCEPT ![m][term[l]] = Max(@, k)]
  /\ UNCHANGED chosen

Current(c, m) ==
  \/ LastTerm(log[c]) > LastTerm(log[m])
  \/ LastTerm(log[c]) = LastTerm(log[m]) /\ Len(log[c]) >= Len(log[m])
\* The index of the newest configuration entry in the log l, 0 for none.
NewestAt(l) ==
  LET at == {i \in 1..Len(l) : IsConf(l[i])}
  IN IF at = {} THEN 0 ELSE CHOOSE i \in at : \A j \in at : j <= i
Stands(c) ==
  \/ c \in Voters(ElectionConf(c))
  \/ /\ Stand = "needed"
     /\ Elections = "newest"
     /\ commit[c] < NewestAt(log[c])
     /\ c \in Voters(ConfThrough(log[c], NewestAt(log[c]) - 1))
Campaigns(c) == term[c] < MaxTerm /\ Stands(c)
Asked(c, Q) ==
  /\ c \notin Q
  /\ \A m \in Q : /\ term[m] < term[c] + 1 \/ (term[m] = term[c] + 1 /\ vote[m] = Nobody)
                  /\ Current(c, m)
Quorums(c, Q) ==
  LET counted == ElectionConf(c) IN
  {{}} \cup {V \in SUBSET ((Q \cup {c}) \cap Voters(counted)) :
            /\ c \in Voters(counted) => c \in V
            /\ QuorumOf(counted, V)}

\* The members of Q vote for c in its next term; it leads if V, a quorum of
\* the configuration it counts by, is among them.
Elect(c, Q, V) ==
  LET t == term[c] + 1
      voted == Q \cup {c}
  IN
  /\ Campaigns(c)
  /\ Asked(c, Q)
  /\ V \in Quorums(c, Q)
  /\ term' = [m \in Servers |-> IF m \in voted THEN t ELSE term[m]]
  /\ vote' = [m \in Servers |-> IF m \in voted THEN c ELSE vote[m]]
  /\ role' = [m \in Servers |-> IF m \in voted
                                THEN IF m = c /\ V # {} THEN "leader" ELSE "follower"
                                ELSE role[m]]
  /\ UNCHANGED <<log, commit, acks, chosen>>

Next ==
  \/ \E l \in Servers : /\ role[l] = "leader"
                       /\ \/ \E v \in Values \cup {Noop} : Take(l, v)
                          \/ Reconfigure(l)
                          \/ \E i \in Indexes : Commit(l, i)
                          \/ \E m \in Servers \ {l}, p \in 0..Len(log[l]), k \in 0..Len(log[l]),
                                learns \in BOOLEAN : Replicate(l, m, p, k, learns)
  \/ \E c \in Servers : /\ Campaigns(c)
                       /\ \E Q \in SUBSET (Servers \ {c}) :
                            /\ Asked(c, Q)
                            /\ \E V \in Quorums(c, Q) : Elect(c, Q, V)

Spec == Init /\ [][Next]_vars

OneLeader ==
  \A l, m \in Servers :
    (role[l] = "leader" /\ role[m] = "leader" /\ term[l] = term[m]) => l = m
Agreement ==
  \A m, n \in Servers : \A i \in 1..Min(commit[m], commit[n]) : log[m][i] = log[n][i]
Committed ==
  \A m \in Servers : \A i \in 1..commit[m] :
    chosen[i] # NotChosen /\ log[m][i].value = chosen[i].value
LeaderHolds ==
  \A l \in Servers : role[l] = "leader" =>
    \A i \in Indexes : (chosen[i] # NotChosen /\ chosen[i].term < term[l]) =>
      i <= Len(log[l]) /\ log[l][i].value = chosen[i].value
LogMatching ==
  \A m, n \in Servers : \A i \in 1..Min(Len(log[m]), Len(log[n])) :
    log[m][i].term = log[n][i].term => \A j \in 1..i : log[m][j] = log[n][j]

\* The scenario reaches what it is for: a member leads while its log holds a
\* configuration entry of an earlier term past its commit, so that the
\* newest configuration in its log is not the one it committed when it was
\* elected.  A configuration that checks the rule must reach it
\* (ReconfigReached.cfg, refused), or it checks nothing of the rule.
ElectedOnPending ==
  \E l \in Servers :
    /\ role[l] = "leader"
    /\ \E i \in (commit[l] + 1)..Len(log[l]) : IsConf(log[l][i]) /\ log[l][i].term < term[l]
NoElectedOnPending == ~ElectedOnPending

\* A member leads that the newest configuration in its log names no voter:
\* the campaign of one the group may still need (Stand = "needed").  A
\* configuration that checks that rule must reach it (ReconfigStood.cfg,
\* refused).
ElectedUnnamed ==
  \E l \in Servers : role[l] = "leader" /\ l \notin Voters(ElectionConf(l))
NoElectedUnnamed == ~ElectedUnnamed

WithinBudget == TLCGet("distinct") <= StateBudget
=============================================================================
