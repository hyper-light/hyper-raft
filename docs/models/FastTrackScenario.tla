--------------------------- MODULE FastTrackScenario ---------------------------
(***************************************************************************)
(* FastTrack.tla held to a scenario: who may be elected in each term and   *)
(* what is proposed in each, searched from a state a run of FastTrack      *)
(* reaches.  Every run of it is a run of FastTrack, so what it refuses     *)
(* FastTrack refuses.                                                      *)
(*                                                                         *)
(* The scenario is swarm fast seed 41,345's (docs/sim.md §15.9), at the    *)
(* fewest members, terms, indexes and values its shape takes: four members *)
(* (a fast quorum of three, and D, outside it, whose log is short), three  *)
(* terms (the fast commit's, the one whose recovery takes the entry again  *)
(* under its own term, and the one that loses it), three indexes (the      *)
(* no-op every vote of the round rests on, and two indexes committed by    *)
(* the fast quorum: the first restamped by term 2 and reached by D's log,  *)
(* the second lost) and two values (the one committed and the one the      *)
(* recovery takes in its place).  Each of the five is needed: three        *)
(* members are a fast quorum all, which leaves no short log to elect; with *)
(* two terms no log can be more current than a fast holder's and short of  *)
(* the index; with two indexes the index below the lost one is the no-op,  *)
(* committed by the classic quorum, which no later term restamps; with one *)
(* value an election can take nothing else.                                *)
(*                                                                         *)
(* The whole model at that scope is past any search: hyper-check's, held   *)
(* to this order of leaders and these proposals and counting states by     *)
(* ScenarioView, passed 402 million classes without ending (tests/         *)
(* fasttrack.rs).  So the search starts where term 1 has ended as the run  *)
(* the swarm found began: A leads term 1, its no-op committed by the       *)
(* classic quorum and both indexes after it by the fast quorum of A, B and *)
(* C, which held Early at both (TermOneHeld; under Votes = "logs" A holds  *)
(* nothing and counts by its log, TermOneLogs).  Both are reached by a run *)
(* of FastTrack from Init that hyper-check replays step by step            *)
(* (tests/fasttrack.rs, term_one).  From there every run is searched: B    *)
(* may be elected in term 2, D in term 3, Late proposed in term 2.         *)
(***************************************************************************)
EXTENDS FastTrack

CONSTANTS A, B, C, D, Early, Late

ScenarioLeads ==
  [t \in 1..MaxTerm |-> CASE t = 1 -> {A} [] t = 2 -> {B} [] OTHER -> {D}]
ScenarioProposals ==
  [t \in 0..MaxTerm |-> CASE t = 1 -> {Early} [] t = 2 -> {Late} [] OTHER -> {}]

Entry(t, v) == [term |-> t, value |-> v]
Said(v, t) == [value |-> v, term |-> t]
Holding == [i \in Indexes |-> IF i = 1 THEN Nothing ELSE Early]
SaidHeld == [i \in Indexes |-> IF i = 1 THEN NotChosen ELSE Said(Early, 1)]
NoneHeld == [i \in Indexes |-> Nothing]
NoneSaid == [i \in Indexes |-> NotChosen]
Acked == [t \in 0..MaxTerm |-> IF t = 1 THEN 1 ELSE 0]
NoAcks == [t \in 0..MaxTerm |-> 0]

\* Term 1 has ended: A leads it, its log the no-op and Early at 2 and 3,
\* committed through 3 and known committed by a classic quorum through 1;
\* B and C hold the no-op from it and Early by themselves at 2 and 3, as A
\* does when `aHolds`.
TermOne(aHolds) ==
  /\ term = (A :> 1 @@ B :> 1 @@ C :> 1 @@ D :> 0)
  /\ vote = (A :> A @@ B :> A @@ C :> A @@ D :> Nobody)
  /\ role = (A :> "leader" @@ B :> "follower" @@ C :> "follower" @@ D :> "follower")
  /\ log = (A :> <<Entry(1, Noop), Entry(1, Early), Entry(1, Early)>>
            @@ B :> <<Entry(1, Noop)>> @@ C :> <<Entry(1, Noop)>> @@ D :> << >>)
  /\ held = (A :> (IF aHolds THEN Holding ELSE NoneHeld)
             @@ B :> Holding @@ C :> Holding @@ D :> NoneHeld)
  /\ commit = (A :> 3 @@ B :> 0 @@ C :> 0 @@ D :> 0)
  /\ says = (A :> (IF aHolds THEN SaidHeld ELSE NoneSaid)
             @@ B :> SaidHeld @@ C :> SaidHeld @@ D :> NoneSaid)
  /\ acks = (A :> NoAcks @@ B :> Acked @@ C :> Acked @@ D :> NoAcks)
  /\ under = (A :> Configuration(Initial, {}) @@ B :> NoConfiguration
              @@ C :> NoConfiguration @@ D :> NoConfiguration)
  /\ chosen = [i \in Indexes |-> IF i = 1 THEN Said(Noop, 1) ELSE Said(Early, 1)]
  /\ mark = [s \in Servers |-> NoMark]
  /\ markedLed = FALSE
  /\ classic = (A :> 1 @@ B :> 0 @@ C :> 0 @@ D :> 0)

TermOneHeld == TermOne(TRUE)
TermOneLogs == TermOne(FALSE)

\* What a state is, for the checker: what any step or invariant can still
\* read of it.  Its terms have one leader each at most (the voters never
\* change here), so what was said as of a term that has no leader now, of
\* an index nothing is held at, and what was acknowledged to such a term's
\* leader, no step reads again; nor for whom a member voted, only whether
\* it did.  Two states alike in these have the same runs from them.
Led(t) == \E l \in Servers : role[l] = "leader" /\ term[l] = t
ScenarioView ==
  <<term,
    [s \in Servers |-> vote[s] # Nobody],
    role, log, held, commit,
    [s \in Servers |-> [i \in Indexes |->
       IF held[s][i] = Nothing /\ ~Led(says[s][i].term) THEN NotChosen ELSE says[s][i]]],
    [s \in Servers |-> [t \in 0..MaxTerm |-> IF Led(t) THEN acks[s][t] ELSE 0]],
    under, chosen, mark, markedLed, classic>>
=============================================================================
