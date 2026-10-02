------------------------------- MODULE FastTrack -------------------------------
(***************************************************************************)
(* hyper-raft's consensus core as it is built (docs/raft.md): the classic  *)
(* track, a change of the voters by one entry or through a joint           *)
(* configuration, and the fast track (crates/hyper-raft/src/track.rs).     *)
(* It is focal's model (focal docs/models/FastTrack.tla, 7ea6f63) with the *)
(* configuration a member counts by, the change that moves it, and the     *)
(* core's second rule.  docs/models/README.md maps each action to the      *)
(* function of the core it stands for.                                     *)
(*                                                                         *)
(* A member holds beside its log, at indexes above it, entries it approved *)
(* by itself: one an index, whatever proposer it came from.  It says what  *)
(* it holds, as of its term.  The log holds what a leader approved, and    *)
(* every entry of it bears the term of the leader that took it.            *)
(*                                                                         *)
(* A leader takes any entry for the next index of its log: the first it    *)
(* hears of, or one of its own.  It commits an index of its own term       *)
(*   by the classic quorum: a quorum of its configuration holds its log    *)
(*     through the index;                                                  *)
(*   by the fast quorum: the index is the one after its commit, no change  *)
(*     of the configuration waits in its log, the configuration is not     *)
(*     joint, and three quarters of its voters hold the entry, from the    *)
(*     leader, or by themselves beside a log that holds an entry of the    *)
(*     leader's term (the first rule, Counts = "round"), and they are also *)
(*     three quarters of the voters it was elected under (the second rule, *)
(*     Configs = "term").                                                  *)
(*                                                                         *)
(* The first rule is the round of a vote, kept where an election reads it. *)
(* A member votes by its log alone, so one that holds the entry beside a   *)
(* log of older terms votes for a candidate whose log fills the index with *)
(* an older entry, and that candidate keeps its own.  Counts = "any" is    *)
(* the rule without it (FastTrackAnyRound.cfg, refused).                   *)
(*                                                                         *)
(* The second rule is for a member that counts by a configuration older    *)
(* than the leader's: one that has not committed the change.  A fast       *)
(* quorum of the new voters need not be one of the old, and such a member  *)
(* is elected by old voters most of whom held another entry.  Configs =    *)
(* "current" is the rule without it (FastTrackAnyConfig.cfg, refused).     *)
(*                                                                         *)
(* A member counts by the configuration its committed log states: the     *)
(* core applies a change once it is committed, campaigns only once it has *)
(* applied what it committed (Raft::hup), and a leader that has a change   *)
(* committed and not applied commits nothing by the fast quorum            *)
(* (Raft::has_pending_conf).  A change is one entry from Initial to Target *)
(* (Joint = FALSE), or an entry that enters the joint configuration of     *)
(* both and one that leaves it for Target (Joint = TRUE, ConfChangeV2 with *)
(* auto-leave).  One change is made at most.                               *)
(*                                                                         *)
(* A member votes for a candidate whose log is at least as current as its  *)
(* own, and says with its vote what it holds by itself.  One that is       *)
(* elected takes, for every index above its log that a voter of its        *)
(* quorum holds an entry at, the entry most held among them, and for an    *)
(* index none of them holds anything at, an entry that states nothing;     *)
(* then it writes its own first entry.                                     *)
(*                                                                         *)
(* A member takes from a leader what follows a point both hold.  What is   *)
(* committed at the member is taken to be what the leader holds there,    *)
(* whatever term it bears: an entry committed by the fast quorum bears the *)
(* term of the leader that took it, and the leader after it, which took it *)
(* again at its election, gave it its own.                                 *)
(*                                                                         *)
(* Messages are not lost one by one here: what was said stays said, and   *)
(* an action may act on it at any later time or never, which is every      *)
(* order, delay, repetition and loss.  A leader that was deposed and does  *)
(* not know it goes on leading its term, and a member that stopped is one  *)
(* that does nothing for a while: what is durable is all a member has      *)
(* here.  One that learns it was deposed campaigns again, with the log it  *)
(* led with: without that step no member whose log holds what no other     *)
(* took is ever elected again, and the model misses the run the first rule *)
(* is for.  A leader's own first entry is an entry it takes like any       *)
(* other, so a leader may write a change before it (the core writes its    *)
(* first entry at once): the model has every run of the core and more.    *)
(*                                                                         *)
(* Nothing here grows without a bound.  A configuration states how many    *)
(* distinct states it has (StateBudget), the checker stops at one more     *)
(* (WithinBudget), and scripts/check-model.sh gives the checker the memory *)
(* that many states take and no more.  A change that makes a model larger  *)
(* or smaller is refused until its states are counted and stated again.    *)
(*                                                                         *)
(* To keep the states few enough to visit them all, a member's word is     *)
(* kept as the last it gave: what it last said it holds at an index, when  *)
(* it came to hold it, with its vote, or again in a later term.  A leader  *)
(* sends its log through its end.  An election is one step (Elect).        *)
(* Proposals are held only at the indexes HeldAt names: the empty set is   *)
(* the classic core alone.                                                 *)
(*                                                                         *)
(* Checked:                                                                *)
(*   Agreement     no two members commit entries that state different     *)
(*                 things at one index (state machine safety)              *)
(*   Committed     what a member has committed at an index is what was     *)
(*                 first committed there                                   *)
(*   LeaderHolds   a leader of a later term holds what was committed       *)
(*                 (leader completeness)                                   *)
(*   OneLeader     no term has two leaders (election safety)               *)
(*   LogMatching   two logs that hold an entry of one term at one index    *)
(*                 hold the same entries through it                        *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets, Sequences, TLC

CONSTANTS Servers,   \* every member, voter or not
          Initial,   \* the voters at the start
          Target,    \* the voters a change makes; Initial for no change
          Joint,     \* whether the change goes through a joint configuration
          Values,    \* what proposers propose
          Noop,      \* what a leader's own first entry states
          Change,    \* the entry that changes Initial to Target
          Enter,     \* the entry that enters the joint configuration
          Leave,     \* the entry that leaves it for Target
          Nothing,   \* no entry
          Nobody,    \* no vote
          MaxTerm,
          MaxLen,    \* how long a log grows
          HeldAt,    \* the indexes at which proposals are held
          StateBudget \* the distinct states the checker may find

Stated  == Values \cup {Noop}
Changes == {Change, Enter, Leave}
Indexes == 1..MaxLen
Entries == [term : 1..MaxTerm, value : Stated \cup Changes]

\* A configuration: the voters, and in a joint one the voters it leaves.
Configuration(in, out) == [in |-> in, out |-> out]
NoConfiguration == Configuration({}, {})
Voters(c) == c.in \cup c.out
Majority(C, H) == 2 * Cardinality(H \cap C) > Cardinality(C)
\* A classic quorum: a majority of each half (Ongaro's thesis, 4.3).
ClassicOf(c, H) == Majority(c.in, H) /\ (c.out = {} \/ Majority(c.out, H))
\* A fast quorum: ceil(3M/4) of the M voters (quorum.rs).
FastOf(C, H) == 4 * Cardinality(H \cap C) >= 3 * Cardinality(C)

VARIABLES
  term,     \* [Servers -> 0..MaxTerm]
  vote,     \* [Servers -> Servers \cup {Nobody}]
  role,     \* [Servers -> {"follower", "leader"}]
  log,      \* [Servers -> Seq(Entries)]
  held,     \* [Servers -> [Indexes -> Values \cup {Nothing}]]
  commit,   \* [Servers -> 0..MaxLen]
  says,     \* [Servers -> [Indexes -> [term, value]]]: what a member last
            \* said it holds by itself, when it came to hold it, with its
            \* vote, or again in a later term
  acks,     \* [Servers -> [0..MaxTerm -> 0..MaxLen]]: through which index a
            \* member said it holds the log of the leader of a term
  under,    \* [Servers -> configurations]: of a leader, the configuration
            \* it was elected under (Raft::note_term_configuration); of a
            \* follower, none
  chosen    \* [Indexes -> [value, term]]: what was first committed, and by a leader of which term

vars == <<term, vote, role, log, held, commit, says, acks, under, chosen>>

NotChosen == [value |-> Nothing, term |-> 0]

Min(a, b) == IF a < b THEN a ELSE b
Max(a, b) == IF a > b THEN a ELSE b
LastTerm(l) == IF Len(l) = 0 THEN 0 ELSE l[Len(l)].term

\* The configuration a member counts by: the one its committed log states.
States(s, x) == \E i \in 1..commit[s] : log[s][i].value = x
ConfigurationOf(s) ==
  IF States(s, Change) \/ States(s, Leave) THEN Configuration(Target, {})
  ELSE IF States(s, Enter) THEN Configuration(Target, Initial)
  ELSE Configuration(Initial, {})
\* A change written and not committed (Raft::has_pending_conf).
Pending(s) == \E i \in (commit[s] + 1)..Len(log[s]) : log[s][i].value \in Changes

\* What a member holds by itself is above its log.
Release(h, length) == [i \in Indexes |-> IF i <= length THEN Nothing ELSE h[i]]

TypeOK ==
  /\ term \in [Servers -> 0..MaxTerm]
  /\ vote \in [Servers -> Servers \cup {Nobody}]
  /\ role \in [Servers -> {"follower", "leader"}]
  /\ \A s \in Servers : /\ Len(log[s]) <= MaxLen
                        /\ \A i \in 1..Len(log[s]) : log[s][i] \in Entries
                        /\ commit[s] <= Len(log[s])
  /\ held \in [Servers -> [Indexes -> Values \cup {Nothing}]]
  /\ \A s \in Servers : \A i \in Indexes : i <= Len(log[s]) => held[s][i] = Nothing
  /\ \A s \in Servers : role[s] = "follower" => under[s] = NoConfiguration

Init ==
  /\ term   = [s \in Servers |-> 0]
  /\ vote   = [s \in Servers |-> Nobody]
  /\ role   = [s \in Servers |-> "follower"]
  /\ log    = [s \in Servers |-> << >>]
  /\ held   = [s \in Servers |-> [i \in Indexes |-> Nothing]]
  /\ commit = [s \in Servers |-> 0]
  /\ says   = [s \in Servers |-> [i \in Indexes |-> NotChosen]]
  /\ acks   = [s \in Servers |-> [t \in 0..MaxTerm |-> 0]]
  /\ under  = [s \in Servers |-> NoConfiguration]
  /\ chosen = [i \in Indexes |-> NotChosen]

----------------------------------------------------------------------------
\* A proposal reaches a voter, which holds it if it holds nothing there,
\* and says so as of its term.  What it says a leader may act on at any
\* later time or never: one that holds and has not said is one whose word
\* no leader has acted on.
Hold(m, i, v) ==
  /\ m \in Voters(ConfigurationOf(m))
  /\ i > Len(log[m])
  /\ held[m][i] = Nothing
  /\ held' = [held EXCEPT ![m][i] = v]
  /\ says' = [says EXCEPT ![m][i] = [value |-> v, term |-> term[m]]]
  /\ UNCHANGED <<term, vote, role, log, commit, acks, under, chosen>>

\* A member says again what it holds, as of a term it has come to since.
Say(m, i) ==
  /\ held[m][i] # Nothing
  /\ says[m][i].term # term[m]
  /\ says' = [says EXCEPT ![m][i] = [value |-> held[m][i], term |-> term[m]]]
  /\ UNCHANGED <<term, vote, role, log, held, commit, acks, under, chosen>>

Write(l, v) ==
  /\ log' = [log EXCEPT ![l] = Append(@, [term |-> term[l], value |-> v])]
  /\ held' = [held EXCEPT ![l] = Release(@, Len(log[l]) + 1)]

\* A leader takes an entry for the next index of its log.
Take(l, v) ==
  /\ role[l] = "leader"
  /\ Len(log[l]) < MaxLen
  /\ Write(l, v)
  /\ UNCHANGED <<term, vote, role, commit, says, acks, under, chosen>>

\* A leader writes the change, or the entry that leaves the joint
\* configuration once the one that entered it is committed.  It writes none
\* while a change waits in its log, nor before what it held when it was
\* elected is committed: the core's pending_conf_index, set to the end of
\* the log at the election (Raft::become_leader).  A leader the change
\* removes does not write it.
Reconfigure(l) ==
  LET c == ConfigurationOf(l)
      older == {i \in 1..Len(log[l]) : log[l][i].term < term[l]}
      v == IF c.out # {} THEN Leave ELSE IF Joint THEN Enter ELSE Change
  IN
  /\ role[l] = "leader"
  /\ Initial # Target
  /\ l \in Target
  /\ c # Configuration(Target, {})
  /\ Len(log[l]) < MaxLen
  /\ ~Pending(l)
  /\ \A i \in older : i <= commit[l]
  /\ Write(l, v)
  /\ UNCHANGED <<term, vote, role, commit, says, acks, under, chosen>>

CONSTANT Counts   \* "round" | "any"
\* A member's log is of the leader's round: it said it holds the leader's
\* log through an entry of the leader's term.
OfTheRound(l, m) ==
  LET a == acks[m][term[l]]
  IN a >= 1 /\ a <= Len(log[l]) /\ log[l][a].term = term[l]
\* Who holds the entry a leader has at an index, as the leader was told.
\* "round" is what the core does.
HoldsByItself(l, m, i) ==
  /\ Counts = "any" \/ OfTheRound(l, m)
  /\ says[m][i] = [value |-> log[l][i].value, term |-> term[l]]
HoldsFromLeader(l, m, i) ==
  \/ m = l
  \/ acks[m][term[l]] >= i

Choose(i, l) ==
  [chosen EXCEPT ![i] = IF @ = NotChosen
                         THEN [value |-> log[l][i].value, term |-> term[l]]
                         ELSE @]

CONSTANT Configs  \* "term" | "current"
\* The fast quorum of the term (Raft::fast_quorum_of_the_term): of the
\* voters in force, and of those the leader was elected under, which is
\* not joint.  "term" is what the core does.
FastOfTheTerm(l, H) ==
  /\ FastOf(ConfigurationOf(l).in, H)
  /\ Configs = "current" \/ (under[l].out = {} /\ FastOf(under[l].in, H))

FastCommit(l) ==
  LET i == commit[l] + 1
      c == ConfigurationOf(l)
  IN
  /\ role[l] = "leader"
  /\ i <= Len(log[l])
  /\ log[l][i].term = term[l]
  /\ ~Pending(l)
  /\ c.out = {}
  /\ FastOfTheTerm(l, {m \in c.in : HoldsByItself(l, m, i) \/ HoldsFromLeader(l, m, i)})
  /\ commit' = [commit EXCEPT ![l] = i]
  /\ chosen' = Choose(i, l)
  /\ UNCHANGED <<term, vote, role, log, held, says, acks, under>>

ClassicCommit(l, i) ==
  LET c == ConfigurationOf(l) IN
  /\ role[l] = "leader"
  /\ i > commit[l]
  /\ i <= Len(log[l])
  /\ log[l][i].term = term[l]
  /\ ClassicOf(c, {m \in Voters(c) : HoldsFromLeader(l, m, i)})
  /\ commit' = [commit EXCEPT ![l] = i]
  /\ chosen' = [j \in Indexes |->
                 IF j > commit[l] /\ j <= i /\ chosen[j] = NotChosen
                 THEN [value |-> log[l][j].value, term |-> term[l]]
                 ELSE chosen[j]]
  /\ UNCHANGED <<term, vote, role, log, held, says, acks, under>>

\* A member of the leader's configuration takes from it what follows the
\* point p, through k.
Replicate(l, m, p) ==
  LET k == Len(log[l]) IN
  /\ l # m
  /\ role[l] = "leader"
  /\ m \in Voters(ConfigurationOf(l))
  /\ term[m] <= term[l]
  /\ p <= k
  /\ \/ p = 0
     \/ p <= commit[m]
     \/ p >= 1 /\ p <= Len(log[m]) /\ log[m][p].term = log[l][p].term
  /\ LET from     == Max(p, commit[m]) + 1
         differs  == {i \in from..k : i > Len(log[m]) \/ log[m][i].term # log[l][i].term}
         taken    == IF differs = {}
                     THEN log[m]
                     ELSE LET c == CHOOSE i \in differs : \A j \in differs : i <= j
                          IN SubSeq(log[m], 1, c - 1) \o SubSeq(log[l], c, k)
     IN /\ log' = [log EXCEPT ![m] = taken]
        /\ held' = [held EXCEPT ![m] = Release(@, Len(taken))]
        /\ commit' = [commit EXCEPT ![m] = Max(@, Min(commit[l], k))]
  /\ term' = [term EXCEPT ![m] = term[l]]
  /\ vote' = [vote EXCEPT ![m] = IF term[m] = term[l] THEN @ ELSE Nobody]
  /\ role' = [role EXCEPT ![m] = "follower"]
  /\ under' = [under EXCEPT ![m] = NoConfiguration]
  /\ acks' = [acks EXCEPT ![m][term[l]] = Max(@, k)]
  /\ UNCHANGED <<says, chosen>>

Current(c, m) ==
  \/ LastTerm(log[c]) > LastTerm(log[m])
  \/ LastTerm(log[c]) = LastTerm(log[m]) /\ Len(log[c]) >= Len(log[m])

\* How many of the voters V hold v at i by themselves, as their votes say.
Count(V, i, v) ==
  Cardinality({m \in V : held[m][i] = v})
\* MostHeld is what the core does.  LeastHeld is what it does not, kept to
\* show that the properties fail without the rule.
MostHeld(V, i, v) ==
  /\ Count(V, i, v) > 0
  /\ \A w \in Values : Count(V, i, w) <= Count(V, i, v)
LeastHeld(V, i, v) ==
  /\ Count(V, i, v) > 0
  /\ \A w \in Values : Count(V, i, w) > 0 => Count(V, i, v) <= Count(V, i, w)

CONSTANT Rule   \* "most" | "least"
Recovered(V, i, v) ==
  IF \A w \in Values : Count(V, i, w) = 0
  THEN v = Noop
  ELSE IF Rule = "most" THEN MostHeld(V, i, v) ELSE LeastHeld(V, i, v)

\* A voter of its own configuration campaigns in the next term, whatever
\* it was: one that led has heard of a later term, or lost its members, and
\* leads no longer; it keeps its log.  The members of Q give it their
\* votes, each saying what it holds by itself, and it leads if it counts a
\* quorum V of its configuration's voters among them and itself; a vote of
\* Q that is not of V came after the count.  V is empty for a campaign
\* that counts no quorum, whose voters are left in its term.
\*
\* The votes are one step.  Taken one by one, with other steps between,
\* they reach nothing more: a voter that has voted takes nothing from an
\* older leader, what it comes to hold after its vote the candidate does
\* not hear of with the vote and hears of when the voter says it, and a
\* step of a member that has not yet voted is the same step taken before
\* the campaign.
Campaigns(c) == term[c] < MaxTerm /\ c \in Voters(ConfigurationOf(c))
Asked(c, Q) ==
  /\ c \notin Q
  /\ \A m \in Q : /\ term[m] < term[c] + 1 \/ (term[m] = term[c] + 1 /\ vote[m] = Nobody)
                  /\ Current(c, m)
Quorums(c, Q) ==
  LET counted == ConfigurationOf(c) IN
  {{}} \cup {V \in SUBSET ((Q \cup {c}) \cap Voters(counted)) :
               c \in V /\ ClassicOf(counted, V)}

Elect(c, Q, V) ==
  LET t == term[c] + 1
      voted == Q \cup {c}
      counted == ConfigurationOf(c)
  IN
  /\ Campaigns(c)
  /\ Asked(c, Q)
  /\ V \in Quorums(c, Q)
  /\ term'   = [m \in Servers |-> IF m \in voted THEN t ELSE term[m]]
  /\ vote'   = [m \in Servers |-> IF m \in voted THEN c ELSE vote[m]]
  /\ says'   = [m \in Servers |-> [i \in Indexes |->
                 IF m \in voted /\ held[m][i] # Nothing
                 THEN [value |-> held[m][i], term |-> t]
                 ELSE says[m][i]]]
  /\ IF V = {}
     THEN /\ role'  = [m \in Servers |-> IF m \in voted THEN "follower" ELSE role[m]]
          /\ under' = [m \in Servers |-> IF m \in voted THEN NoConfiguration ELSE under[m]]
          /\ UNCHANGED <<log, held>>
     ELSE LET length == Len(log[c])
              reported == {i \in Indexes : /\ i > length
                                           /\ \E v \in Values : Count(V, i, v) > 0}
              top == IF reported = {} THEN length
                     ELSE CHOOSE i \in reported : \A j \in reported : j <= i
          IN /\ \E taken \in [(length + 1)..top -> Stated] :
                  /\ \A i \in (length + 1)..top : Recovered(V, i, taken[i])
                  /\ log' = [log EXCEPT ![c] =
                               @ \o [i \in 1..(top - length) |->
                                      [term |-> t, value |-> taken[length + i]]]]
             /\ held' = [held EXCEPT ![c] = Release(@, top)]
             /\ role' = [m \in Servers |-> IF m = c THEN "leader"
                                           ELSE IF m \in voted THEN "follower"
                                           ELSE role[m]]
             /\ under' = [m \in Servers |-> IF m = c THEN counted
                                            ELSE IF m \in voted THEN NoConfiguration
                                            ELSE under[m]]
  /\ UNCHANGED <<commit, acks, chosen>>

\* Each step's guards that do not depend on its later arguments come
\* first, so that the checker does not try every argument of a step that
\* is not enabled; the steps are the same.
Next ==
  \/ \E m \in Servers, i \in HeldAt, v \in Values : Hold(m, i, v)
  \/ \E m \in Servers, i \in HeldAt : Say(m, i)
  \/ \E l \in Servers : /\ role[l] = "leader"
                       /\ \/ \E v \in Stated : Take(l, v)
                          \/ Reconfigure(l)
                          \/ FastCommit(l)
                          \/ \E i \in Indexes : ClassicCommit(l, i)
                          \/ \E m \in Voters(ConfigurationOf(l)) \ {l} :
                               /\ term[m] <= term[l]
                               /\ \E p \in 0..Len(log[l]) : Replicate(l, m, p)
  \/ \E c \in Servers : /\ Campaigns(c)
                       /\ \E Q \in SUBSET (Servers \ {c}) :
                            /\ Asked(c, Q)
                            /\ \E V \in Quorums(c, Q) : Elect(c, Q, V)

Spec == Init /\ [][Next]_vars

----------------------------------------------------------------------------
Agreement ==
  \A m, n \in Servers : \A i \in 1..Min(commit[m], commit[n]) :
    log[m][i].value = log[n][i].value

Committed ==
  \A m \in Servers : \A i \in 1..commit[m] :
    /\ chosen[i] # NotChosen
    /\ log[m][i].value = chosen[i].value

LeaderHolds ==
  \A l \in Servers : role[l] = "leader" =>
    \A i \in Indexes : (chosen[i] # NotChosen /\ chosen[i].term < term[l]) =>
      /\ i <= Len(log[l])
      /\ log[l][i].value = chosen[i].value

OneLeader ==
  \A l, m \in Servers :
    (role[l] = "leader" /\ role[m] = "leader" /\ term[l] = term[m]) => l = m

LogMatching ==
  \A m, n \in Servers : \A i \in 1..Min(Len(log[m]), Len(log[n])) :
    log[m][i].term = log[n][i].term =>
      \A j \in 1..i : log[m][j] = log[n][j]

\* A leader committed an index of its term that no classic quorum holds
\* from it: it counted what members hold by themselves.  A configuration
\* that checks the fast track must reach this, or it checks nothing of it
\* (FastTrackReached.cfg, which the checker must refuse): with the first
\* rule and one index it is never reached, for a member whose log is of the
\* leader's term then holds the index from the leader.
FastByHeld ==
  \E l \in Servers, i \in Indexes :
    LET c == ConfigurationOf(l) IN
    /\ role[l] = "leader"
    /\ commit[l] >= i
    /\ chosen[i].term = term[l]
    /\ ~ClassicOf(c, {m \in Voters(c) : HoldsFromLeader(l, m, i)})
NoFastByHeld == ~FastByHeld

\* The same after a change: the leader applied, in its term, a change it
\* wrote below the index.  A configuration that checks the second rule
\* must reach it (FastTrackGrowReached.cfg, refused), or the rule leaves no
\* fast commit after a change to check.
FastByHeldAfterChange ==
  \E l \in Servers, i \in Indexes :
    LET c == ConfigurationOf(l) IN
    /\ role[l] = "leader"
    /\ commit[l] >= i
    /\ chosen[i].term = term[l]
    /\ ~ClassicOf(c, {m \in Voters(c) : HoldsFromLeader(l, m, i)})
    /\ under[l] # c
    /\ \E j \in 1..(i - 1) : log[l][j].value \in Changes /\ log[l][j].term = term[l]
NoFastByHeldAfterChange == ~FastByHeldAfterChange

\* For the checker: it has found no more states than the configuration
\* states it has.
WithinBudget == TLCGet("distinct") <= StateBudget

\* For the checker: the voters a change keeps are alike, as are the values.
Alike == Permutations(Initial \cap Target) \cup Permutations(Values)
=============================================================================
