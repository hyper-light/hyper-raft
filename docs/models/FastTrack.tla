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
(* A member of Losers may lose the tail of its log at rest and reopen     *)
(* knowing what it acknowledged (Lose): hyper-log's uncertainty mark,      *)
(* PAR's faulty entry (Alagappan et al., FAST 2018), docs/durable.md §5.   *)
(* It keeps its term and vote; its mark names the last entry it held, the  *)
(* greater index and term of two marks where one was not yet ended, and    *)
(* ends once its log reaches the mark's index or holds an entry of a later *)
(* term (Lost::resolved_by).  A leader still counts what it was told the   *)
(* member holds (acks): the core's leader takes the member's word that it  *)
(* lost them (R-5) and counts less, so the model has every run of the core *)
(* and more.  With Marks = "core", what the core does since R-7: a marked  *)
(* voter grants a vote only to a log at least as current as its mark; a    *)
(* marked candidate campaigns on its log, and its own vote is not counted  *)
(* (a quorum of the others must find its log at least as current as what  *)
(* each answers for); elected, its mark ends.  Marks = "self" counts the   *)
(* marked candidate's own vote, and "whole" lets a marked voter judge by   *)
(* its log: each is refused (MarkedSelf.cfg, MarkedWhole.cfg).  With       *)
(* Losers = {} no step of these is enabled and every configuration has the *)
(* states it had.                                                          *)
(*                                                                         *)
(* What a member holds by itself it holds until it knows the index        *)
(* committed by a classic quorum (Releases = "classic"): only then is the *)
(* entry in every later leader's log (Raft's election rule), and a fast   *)
(* commit puts it in no majority's logs.  A member learns the classic     *)
(* commit with what follows from a leader (classic), a leader by its own  *)
(* count.  Releases = "log", what the core did before, lets a member drop *)
(* what it holds once its log reaches the index, and a later leader's     *)
(* append can cut that log back: refused (FastTrackCovered.cfg).  A fast  *)
(* quorum counts what members hold by themselves only (Votes = "held"),   *)
(* the leader's own held entry among them; Votes = "logs" counts what     *)
(* they hold from the leader too, which no election reads: refused        *)
(* (FastTrackLogs.cfg).  slates' search refuses both (its prefix model's  *)
(* DropCovered and PruneAtFastCommit; hyper-check's tests/exhaustive.rs). *)
(*                                                                         *)
(* A leader sends what follows a point through any index of its log, not *)
(* only through its end: the core's appends carry at most what its        *)
(* message bound admits.  An append that conflicts below where it stops   *)
(* cuts the member's log short.                                            *)
(*                                                                         *)
(* A scenario may name the member elected in each term (Leads) and the    *)
(* values proposed in each (Proposed): a run of it is a run of the model, *)
(* so what it refuses the model refuses (FastTrackScenario.tla).          *)
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
          Losers,    \* the members whose log may lose its tail at rest
          Leads,     \* [1..MaxTerm -> SUBSET Servers]: who may be elected in a term
          Proposed,  \* [0..MaxTerm -> SUBSET Values]: what is proposed in a term
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
  chosen,   \* [Indexes -> [value, term]]: what was first committed, and by a leader of which term
  mark,     \* [Servers -> [index, term]]: what a member's log may lack of
            \* what it acknowledged; NoMark for none (Raft::lost)
  markedLed, \* whether a member was elected while marked (NoMarkedLeader)
  classic   \* [Servers -> 0..MaxLen]: the index a member knows committed by a
            \* classic quorum, and everything below it

vars == <<term, vote, role, log, held, commit, says, acks, under, chosen, mark, markedLed,
          classic>>

\* Any member may be elected in any term, and any value proposed.
AnyLeads == [t \in 1..MaxTerm |-> Servers]
AnyProposals == [t \in 0..MaxTerm |-> Values]

NotChosen == [value |-> Nothing, term |-> 0]
NoMark == [index |-> 0, term |-> 0]

Min(a, b) == IF a < b THEN a ELSE b
Max(a, b) == IF a > b THEN a ELSE b
LastTerm(l) == IF Len(l) = 0 THEN 0 ELSE l[Len(l)].term

\* Whether a log holds again what a mark says it may lack (Lost::resolved_by).
Resolves(k, l) == Len(l) >= k.index \/ LastTerm(l) > k.term
Marked(s) == mark[s] # NoMark
\* The mark a member keeps once its log is l.
Settled(s, l) == IF Marked(s) /\ Resolves(mark[s], l) THEN NoMark ELSE mark[s]

\* The configuration a member counts by: the one its committed log states.
States(s, x) == \E i \in 1..commit[s] : log[s][i].value = x
ConfigurationOf(s) ==
  IF States(s, Change) \/ States(s, Leave) THEN Configuration(Target, {})
  ELSE IF States(s, Enter) THEN Configuration(Target, Initial)
  ELSE Configuration(Initial, {})
\* A change written and not committed (Raft::has_pending_conf).
Pending(s) == \E i \in (commit[s] + 1)..Len(log[s]) : log[s][i].value \in Changes

\* What a member holds by itself at or below `upto` it holds no more.
Release(h, upto) == [i \in Indexes |-> IF i <= upto THEN Nothing ELSE h[i]]

CONSTANT Releases \* "classic" | "log"
\* What member s holds once its log is `length` long and it knows `known`
\* committed by a classic quorum.  "classic" is what the core does.
ReleaseBy(s, length, known) ==
  IF Releases = "log" THEN Release(held[s], length) ELSE Release(held[s], known)

TypeOK ==
  /\ term \in [Servers -> 0..MaxTerm]
  /\ vote \in [Servers -> Servers \cup {Nobody}]
  /\ role \in [Servers -> {"follower", "leader"}]
  /\ \A s \in Servers : /\ Len(log[s]) <= MaxLen
                        /\ \A i \in 1..Len(log[s]) : log[s][i] \in Entries
                        /\ commit[s] <= Len(log[s])
  /\ held \in [Servers -> [Indexes -> Values \cup {Nothing}]]
  /\ Releases = "log" =>
       \A s \in Servers : \A i \in Indexes : i <= Len(log[s]) => held[s][i] = Nothing
  /\ \A s \in Servers : classic[s] <= commit[s]
  /\ \A s \in Servers : role[s] = "follower" => under[s] = NoConfiguration
  /\ mark \in [Servers -> [index : 0..MaxLen, term : 0..MaxTerm]]
  /\ markedLed \in BOOLEAN

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
  /\ mark   = [s \in Servers |-> NoMark]
  /\ markedLed = FALSE
  /\ classic = [s \in Servers |-> 0]

----------------------------------------------------------------------------
\* A proposal reaches a voter, which holds it if it holds nothing there,
\* and says so as of its term.  What it says a leader may act on at any
\* later time or never: one that holds and has not said is one whose word
\* no leader has acted on.
Hold(m, i, v) ==
  /\ m \in Voters(ConfigurationOf(m))
  /\ v \in Proposed[term[m]]
  /\ i > Len(log[m])
  /\ held[m][i] = Nothing
  /\ held' = [held EXCEPT ![m][i] = v]
  /\ says' = [says EXCEPT ![m][i] = [value |-> v, term |-> term[m]]]
  /\ UNCHANGED <<term, vote, role, log, commit, acks, under, chosen, mark, markedLed, classic>>

\* A member says again what it holds, as of a term it has come to since.
Say(m, i) ==
  /\ held[m][i] # Nothing
  /\ says[m][i].term # term[m]
  /\ says' = [says EXCEPT ![m][i] = [value |-> held[m][i], term |-> term[m]]]
  /\ UNCHANGED <<term, vote, role, log, held, commit, acks, under, chosen, mark, markedLed,
                 classic>>

Write(l, v) ==
  /\ log' = [log EXCEPT ![l] = Append(@, [term |-> term[l], value |-> v])]
  /\ held' = [held EXCEPT ![l] = ReleaseBy(l, Len(log[l]) + 1, classic[l])]

\* A leader takes an entry for the next index of its log.
Take(l, v) ==
  /\ role[l] = "leader"
  /\ v \in {Noop} \cup Proposed[term[l]]
  /\ Len(log[l]) < MaxLen
  /\ Write(l, v)
  /\ UNCHANGED <<term, vote, role, commit, says, acks, under, chosen, mark, markedLed, classic>>

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
  /\ UNCHANGED <<term, vote, role, commit, says, acks, under, chosen, mark, markedLed, classic>>

CONSTANT Counts   \* "round" | "any"
\* A member's log is of the leader's round: it said it holds the leader's
\* log through an entry of the leader's term.
OfTheRound(l, m) ==
  LET a == acks[m][term[l]]
  IN a >= 1 /\ a <= Len(log[l]) /\ log[l][a].term = term[l]
\* Who holds the entry a leader has at an index, as the leader was told.
\* "round" is what the core does; a leader's own log is of its round.
HoldsByItself(l, m, i) ==
  /\ Counts = "any" \/ m = l \/ OfTheRound(l, m)
  /\ says[m][i] = [value |-> log[l][i].value, term |-> term[l]]
HoldsFromLeader(l, m, i) ==
  \/ m = l
  \/ acks[m][term[l]] >= i

CONSTANT Votes  \* "held" | "logs"
\* What a fast quorum counts.  "held" is what the core does.
Holds(l, m, i) ==
  \/ HoldsByItself(l, m, i)
  \/ Votes = "logs" /\ HoldsFromLeader(l, m, i)

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
  /\ FastOfTheTerm(l, {m \in c.in : Holds(l, m, i)})
  /\ commit' = [commit EXCEPT ![l] = i]
  /\ chosen' = Choose(i, l)
  /\ UNCHANGED <<term, vote, role, log, held, says, acks, under, mark, markedLed, classic>>

\* A classic quorum holds the leader's entry of its term at i: the leader
\* knows i committed by it, and commits it if it had not (a fast quorum may
\* have committed it first).
ClassicCommit(l, i) ==
  LET c == ConfigurationOf(l) IN
  /\ role[l] = "leader"
  /\ i > classic[l]
  /\ i <= Len(log[l])
  /\ log[l][i].term = term[l]
  /\ ClassicOf(c, {m \in Voters(c) : HoldsFromLeader(l, m, i)})
  /\ commit' = [commit EXCEPT ![l] = Max(@, i)]
  /\ chosen' = [j \in Indexes |->
                 IF j > commit[l] /\ j <= i /\ chosen[j] = NotChosen
                 THEN [value |-> log[l][j].value, term |-> term[l]]
                 ELSE chosen[j]]
  /\ classic' = [classic EXCEPT ![l] = i]
  /\ held' = [held EXCEPT ![l] = IF Releases = "classic" THEN Release(@, i) ELSE @]
  /\ UNCHANGED <<term, vote, role, log, says, acks, under, mark, markedLed>>

\* A member of the leader's configuration takes from it what follows the
\* point p, through k: the leader's append carries at most what its bound
\* admits, so k is any index of the leader's log from p on.
Replicate(l, m, p, k) ==
  /\ l # m
  /\ role[l] = "leader"
  /\ m \in Voters(ConfigurationOf(l))
  /\ term[m] <= term[l]
  /\ p <= k
  /\ k <= Len(log[l])
  /\ \/ p = 0
     \/ p <= commit[m]
     \/ p >= 1 /\ p <= Len(log[m]) /\ log[m][p].term = log[l][p].term
  /\ LET from     == Max(p, commit[m]) + 1
         differs  == {i \in from..k : i > Len(log[m]) \/ log[m][i].term # log[l][i].term}
         taken    == IF differs = {}
                     THEN log[m]
                     ELSE LET c == CHOOSE i \in differs : \A j \in differs : i <= j
                          IN SubSeq(log[m], 1, c - 1) \o SubSeq(log[l], c, k)
         known    == Max(classic[m], Min(classic[l], k))
     IN /\ log' = [log EXCEPT ![m] = taken]
        /\ classic' = [classic EXCEPT ![m] = known]
        /\ held' = [held EXCEPT ![m] = ReleaseBy(m, Len(taken), known)]
        /\ commit' = [commit EXCEPT ![m] = Max(@, Min(commit[l], k))]
        /\ mark' = [mark EXCEPT ![m] = Settled(m, taken)]
  /\ term' = [term EXCEPT ![m] = term[l]]
  /\ vote' = [vote EXCEPT ![m] = IF term[m] = term[l] THEN @ ELSE Nobody]
  /\ role' = [role EXCEPT ![m] = "follower"]
  /\ under' = [under EXCEPT ![m] = NoConfiguration]
  /\ acks' = [acks EXCEPT ![m][term[l]] = Max(@, k)]
  /\ UNCHANGED <<says, chosen, markedLed>>

\* A member of Losers loses the entries of its log after k at rest, and
\* reopens: it keeps its term and vote (their two copies, PAR §3.3.1), and
\* marks through the last entry it held, with any mark not yet ended
\* (hyper-log's merge).  What its log no longer holds it no longer states
\* committed; it leads no more.
Lose(m, k) ==
  LET old == [index |-> Len(log[m]), term |-> LastTerm(log[m])]
      merged == IF Marked(m)
                THEN [index |-> Max(mark[m].index, old.index),
                      term |-> Max(mark[m].term, old.term)]
                ELSE old
  IN
  /\ m \in Losers
  /\ k < Len(log[m])
  /\ log' = [log EXCEPT ![m] = SubSeq(@, 1, k)]
  /\ commit' = [commit EXCEPT ![m] = Min(@, k)]
  /\ classic' = [classic EXCEPT ![m] = Min(@, k)]
  /\ mark' = [mark EXCEPT ![m] = merged]
  /\ role' = [role EXCEPT ![m] = "follower"]
  /\ under' = [under EXCEPT ![m] = NoConfiguration]
  /\ UNCHANGED <<term, vote, held, says, acks, chosen, markedLed>>

CONSTANT Marks   \* "core" | "self" | "whole"
\* What a voter answers for in an election (Raft::claim): its log's last
\* entry, or its mark while its log may lack what it acknowledged.  "whole"
\* judges by the log alone, which the core does not.
Claim(m) ==
  IF Marked(m) /\ Marks # "whole"
  THEN mark[m]
  ELSE [index |-> Len(log[m]), term |-> LastTerm(log[m])]
Current(c, m) ==
  \/ LastTerm(log[c]) > Claim(m).term
  \/ LastTerm(log[c]) = Claim(m).term /\ Len(log[c]) >= Claim(m).index

CONSTANT Reports  \* "held" | "acknowledged"
\* What voter m says it holds at i with its vote.  "held" is what the core
\* does: what it holds by itself.  "acknowledged" is design B (docs/raft.md
\* §3.4): every entry it acknowledged above what it knows committed by a
\* classic quorum, its log's where its log reaches i (slates' ReportLogsToo,
\* here over the most-held rule).
Report(m, i) ==
  IF Reports = "acknowledged" /\ i <= Len(log[m]) /\ i > classic[m]
  THEN log[m][i].value
  ELSE held[m][i]

\* How many of the voters V report v at i.
Count(V, i, v) ==
  Cardinality({m \in V : Report(m, i) = v})
\* MostHeld is what the core does.  LeastHeld is what it does not, kept to
\* show that the properties fail without the rule.
MostHeld(V, i, v) ==
  /\ Count(V, i, v) > 0
  /\ \A w \in Values : Count(V, i, w) <= Count(V, i, v)
LeastHeld(V, i, v) ==
  /\ Count(V, i, v) > 0
  /\ \A w \in Values : Count(V, i, w) > 0 => Count(V, i, v) <= Count(V, i, w)

\* slates' ballot rule (its slot model, after Fast Paxos): of the reports
\* said in the latest term, the value at least |Q| + |F| - n of them hold,
\* with F the fast quorum of the n voters counted by; else the index is free.
FastSize(n) == CHOOSE f \in 0..n : 4 * f >= 3 * n /\ \A g \in 0..(f - 1) : 4 * g < 3 * n
Ballot(V, i, v, C) ==
  LET reports == {m \in V : held[m][i] # Nothing}
      top     == CHOOSE t \in {says[m][i].term : m \in reports} :
                   \A m \in reports : says[m][i].term <= t
      latest  == {m \in reports : says[m][i].term = top}
      enough  == Cardinality(V \cap C) + FastSize(Cardinality(C)) - Cardinality(C)
      forced  == {w \in Values : Cardinality({m \in latest : held[m][i] = w}) >= enough}
  IN IF forced = {} THEN v = Noop ELSE v \in forced

CONSTANT Rule   \* "most" | "least" | "ballot"
Recovered(V, i, v, C) ==
  IF \A w \in Values : Count(V, i, w) = 0
  THEN v = Noop
  ELSE CASE Rule = "most"  -> MostHeld(V, i, v)
         [] Rule = "least" -> LeastHeld(V, i, v)
         [] Rule = "ballot" -> Ballot(V, i, v, C)

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
Campaigns(c) ==
  /\ term[c] < MaxTerm
  /\ c \in Voters(ConfigurationOf(c))
  /\ c \in Leads[term[c] + 1]
Asked(c, Q) ==
  /\ c \notin Q
  /\ \A m \in Q : /\ term[m] < term[c] + 1 \/ (term[m] = term[c] + 1 /\ vote[m] = Nobody)
                  /\ Current(c, m)
\* Whose vote a candidate counts besides those it was given: its own, but
\* for a marked candidate's (R-7), whose log may lack what it acknowledged;
\* "self" counts that too, which the core does not.
Own(c) == IF Marked(c) /\ Marks # "self" THEN {} ELSE {c}
Quorums(c, Q) ==
  LET counted == ConfigurationOf(c) IN
  {{}} \cup {V \in SUBSET ((Q \cup Own(c)) \cap Voters(counted)) :
               Own(c) \subseteq V /\ ClassicOf(counted, V)}

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
          /\ UNCHANGED <<log, held, mark, markedLed, classic>>
     ELSE LET length == Len(log[c])
              reported == {i \in Indexes : /\ i > length
                                           /\ \E v \in Values : Count(V, i, v) > 0}
              top == IF reported = {} THEN length
                     ELSE CHOOSE i \in reported : \A j \in reported : j <= i
          IN /\ \E taken \in [(length + 1)..top -> Stated] :
                  /\ \A i \in (length + 1)..top : Recovered(V, i, taken[i], counted.in)
                  /\ log' = [log EXCEPT ![c] =
                               @ \o [i \in 1..(top - length) |->
                                      [term |-> t, value |-> taken[length + i]]]]
             /\ held' = [held EXCEPT ![c] = ReleaseBy(c, top, classic[c])]
             /\ role' = [m \in Servers |-> IF m = c THEN "leader"
                                           ELSE IF m \in voted THEN "follower"
                                           ELSE role[m]]
             /\ under' = [m \in Servers |-> IF m = c THEN counted
                                            ELSE IF m \in voted THEN NoConfiguration
                                            ELSE under[m]]
             \* Elected, what it lost was committed by no one: its mark ends
             \* (Raft::become_leader).
             /\ mark' = [mark EXCEPT ![c] = NoMark]
             /\ markedLed' = (markedLed \/ Marked(c))
  /\ UNCHANGED <<commit, acks, chosen, classic>>

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
                               /\ \E p \in 0..Len(log[l]), k \in 0..Len(log[l]) :
                                    Replicate(l, m, p, k)
  \/ \E c \in Servers : /\ Campaigns(c)
                       /\ \E Q \in SUBSET (Servers \ {c}) :
                            /\ Asked(c, Q)
                            /\ \E V \in Quorums(c, Q) : Elect(c, Q, V)
  \/ \E m \in Losers : \E k \in 0..MaxLen : Lose(m, k)

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

\* Two logs that hold an entry of one term at one index hold the same
\* values through it, and the same terms wherever neither member has
\* committed: an entry a fast quorum committed keeps, at a member that
\* committed it, the term of the leader that took it, and a later leader
\* that took it again at its election holds it under its own.
LogMatching ==
  \A m, n \in Servers : \A i \in 1..Min(Len(log[m]), Len(log[n])) :
    log[m][i].term = log[n][i].term =>
      \A j \in 1..i : /\ log[m][j].value = log[n][j].value
                     /\ (j > commit[m] /\ j > commit[n]) => log[m][j].term = log[n][j].term

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

\* No member was elected while marked.  A configuration that checks R-7 must
\* reach it (MarkedReached.cfg, which the checker must refuse), or it checks
\* nothing of a marked member's election.
NoMarkedLeader == ~markedLed

\* For the checker: it has found no more states than the configuration
\* states it has.
WithinBudget == TLCGet("distinct") <= StateBudget

\* For the checker: the voters a change keeps are alike, as are the values.
Alike == Permutations(Initial \cap Target) \cup Permutations(Values)
=============================================================================
