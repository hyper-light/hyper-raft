------------------------------- MODULE Suspicion -------------------------------
(***************************************************************************)
(* hyper-raft's elections by suspicion (docs/timing.md §2.9,              *)
(* crates/hyper-raft/src/watch.rs): what a member does with its owner's   *)
(* detectors' word, which may be wrong at any time, while some voters are *)
(* stopped for good.  It is the rules alone: no log (every log is as      *)
(* current as every other, so every vote the log comparison would grant  *)
(* is granted, which only adds elections), one configuration of all the  *)
(* Servers, and messages as the members' states: a request is out while  *)
(* its asker asks in its term, and a leader's heartbeats of a term it led *)
(* may arrive at any later time (leaderOf).  What the core keeps safe    *)
(* with its logs is FastTrack.tla's and Reconfig.tla's, which take a     *)
(* campaign at any time; this module checks the rules that decide when   *)
(* one is made.                                                          *)
(*                                                                         *)
(* A member's detectors suspect or trust any other member at any time    *)
(* (Suspect, Trust), a stopped one never trusted again: its heartbeats   *)
(* never come.  TQ(s): s and those it trusts are a quorum.               *)
(*   A member that trusts no leader and has TQ asks for pre-votes        *)
(*   (PreVote); one that trusts its leader, or a leader, grants none     *)
(*   (the lease); a request from the member's own leader ends its trust. *)
(*   Granted by a quorum it campaigns (ElectPre); a vote of a later term *)
(*   moves a voter's term unless the lease refuses it, and a forced one  *)
(*   (an order's) is refused by no lease (GrantVote).                    *)
(*   A pre-candidate drops what a leader it suspects sends in its term   *)
(*   (Hear).                                                             *)
(* Rule, what a member does when its detectors leave it no quorum:       *)
(*   "core"          a leader leads on, and orders its heirs to campaign *)
(*                   only where each and those it trusts, the leader     *)
(*                   apart, are a quorum (Order, TakeOrder); a pre-      *)
(*                   candidate stops asking and follows the leader it    *)
(*                   left (Suspect);                                     *)
(*   "stepdown"      the core before 2026-10-24: a leader steps down,    *)
(*                   and having led its term orders heirs to campaign    *)
(*                   whatever they trust (refused, SuspicionStepDown);   *)
(*   "unconditional" a leader leads on but orders unconditionally        *)
(*                   (refused, SuspicionUnconditional);                  *)
(*   "asking"        as "core", but a pre-candidate asks on (refused,    *)
(*                   SuspicionAsking).                                   *)
(*                                                                         *)
(* Stable: where the stopped voters leave no quorum without the leader   *)
(* and every live voter suspects them, a group whose live voters follow  *)
(* one leader in its term keeps that leader and that term, whatever the  *)
(* detectors then say of the live voters (the history variable settled   *)
(* holds the first such leader and term, CheckStable).                   *)
(*                                                                         *)
(* Nothing grows without a bound: terms to MaxTerm, and StateBudget, as  *)
(* in FastTrack.tla.                                                     *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets, TLC

CONSTANTS Servers, Nobody, MaxTerm, Stopped, Rule, CheckStable, StateBudget

Live == Servers \ Stopped
Terms == 1..MaxTerm
Quorum(S) == 2 * Cardinality(S \cap Servers) > Cardinality(Servers)
Roles == {"follower", "precandidate", "candidate", "leader"}
\* How a candidate's campaign began: asked for, or ordered by a leader,
\* with or without the condition.
Kinds == {"asked", "order", "condition"}
Orders == [from : Servers, to : Servers, term : Terms, cond : BOOLEAN]
NotSettled == [l |-> Nobody, t |-> 0]

VARIABLES term, vote, role, leader, left, suspects, granted, kind, orders,
          leaderOf, settled
vars == <<term, vote, role, leader, left, suspects, granted, kind, orders,
          leaderOf, settled>>

TQ(s) == Quorum({s} \cup (Servers \ suspects[s]))
ElectedWithout(h, o) == Quorum(({h} \cup (Servers \ suspects[h])) \ {o})
TrustsLeader(s) == leader[s] # Nobody /\ leader[s] \notin suspects[s]

\* l leads, and every live voter follows it in its term, suspecting every
\* stopped voter.
Settled(l) ==
  /\ l \in Live
  /\ role[l] = "leader"
  /\ \A s \in Live :
       /\ Stopped \subseteq suspects[s]
       /\ s # l => term[s] = term[l] /\ role[s] = "follower" /\ leader[s] = l
SettledNext(l) ==
  /\ l \in Live
  /\ role'[l] = "leader"
  /\ \A s \in Live :
       /\ Stopped \subseteq suspects'[s]
       /\ s # l => term'[s] = term'[l] /\ role'[s] = "follower" /\ leader'[s] = l

TypeOK ==
  /\ term \in [Servers -> 0..MaxTerm]
  /\ vote \in [Servers -> Servers \cup {Nobody}]
  /\ role \in [Servers -> Roles]
  /\ leader \in [Servers -> Servers \cup {Nobody}]
  /\ left \in [Servers -> Servers \cup {Nobody}]
  /\ suspects \in [Servers -> SUBSET Servers]
  /\ \A s \in Servers : s \notin suspects[s]
  /\ granted \in [Servers -> SUBSET Servers]
  /\ kind \in [Servers -> Kinds]
  /\ orders \subseteq Orders
  /\ leaderOf \in [Terms -> Servers \cup {Nobody}]

Init ==
  /\ term = [s \in Servers |-> 0]
  /\ vote = [s \in Servers |-> Nobody]
  /\ role = [s \in Servers |-> "follower"]
  /\ leader = [s \in Servers |-> Nobody]
  /\ left = [s \in Servers |-> Nobody]
  /\ suspects = [s \in Servers |-> {}]
  /\ granted = [s \in Servers |-> {}]
  /\ kind = [s \in Servers |-> "asked"]
  /\ orders = {}
  /\ leaderOf = [t \in Terms |-> Nobody]
  /\ settled = NotSettled

\* s becomes a follower of l (Nobody for none) in term t: what it asked or
\* counted goes.
Follow(s, t, l) ==
  /\ term' = [term EXCEPT ![s] = t]
  /\ vote' = IF t > term[s] THEN [vote EXCEPT ![s] = Nobody] ELSE vote
  /\ role' = [role EXCEPT ![s] = "follower"]
  /\ leader' = [leader EXCEPT ![s] = l]
  /\ left' = [left EXCEPT ![s] = Nobody]
  /\ granted' = [granted EXCEPT ![s] = {}]
  /\ kind' = [kind EXCEPT ![s] = "asked"]

\* The detectors of s suspect x.  A leader left no quorum steps down under
\* "stepdown"; a pre-candidate left no quorum stops asking under "core" and
\* "unconditional".
Suspect(s, x) ==
  /\ s \in Live
  /\ x \in Servers \ ({s} \cup suspects[s])
  /\ LET sus == suspects[s] \cup {x}
         tq == Quorum({s} \cup (Servers \ sus))
     IN /\ suspects' = [suspects EXCEPT ![s] = sus]
        /\ IF role[s] = "leader" /\ ~tq /\ Rule = "stepdown"
           THEN /\ role' = [role EXCEPT ![s] = "follower"]
                /\ leader' = [leader EXCEPT ![s] = Nobody]
                /\ UNCHANGED <<left, granted, kind>>
           ELSE IF role[s] = "precandidate" /\ ~tq /\ Rule # "asking"
           THEN /\ role' = [role EXCEPT ![s] = "follower"]
                /\ leader' = [leader EXCEPT ![s] = left[s]]
                /\ left' = [left EXCEPT ![s] = Nobody]
                /\ granted' = [granted EXCEPT ![s] = {}]
                /\ UNCHANGED kind
           ELSE UNCHANGED <<role, leader, left, granted, kind>>
  /\ UNCHANGED <<term, vote, orders, leaderOf>>

\* The detectors of s trust x again.  A stopped voter's heartbeats never
\* come.
Trust(s, x) ==
  /\ s \in Live
  /\ x \in suspects[s] \cap Live
  /\ suspects' = [suspects EXCEPT ![s] = @ \ {x}]
  /\ UNCHANGED <<term, vote, role, leader, left, granted, kind, orders, leaderOf>>

\* s, trusting no leader and with a quorum it trusts, asks for pre-votes for
\* its next term, anew if it asked before.
PreVote(s) ==
  /\ s \in Live
  /\ role[s] # "leader"
  /\ ~TrustsLeader(s)
  /\ TQ(s)
  /\ term[s] < MaxTerm
  /\ role' = [role EXCEPT ![s] = "precandidate"]
  /\ left' = [left EXCEPT ![s] = CASE role[s] = "follower" -> leader[s]
                                     [] role[s] = "precandidate" -> left[s]
                                     [] OTHER -> Nobody]
  /\ leader' = [leader EXCEPT ![s] = Nobody]
  /\ granted' = [granted EXCEPT ![s] = {s}]
  /\ kind' = [kind EXCEPT ![s] = "asked"]
  /\ UNCHANGED <<term, vote, suspects, orders, leaderOf>>

\* The lease of m against a request of c for term t (Raft::step_newer_term):
\* m leads, or trusts a leader other than c and c's term is not past m's
\* next.  A request from m's own leader ends m's trust first.
Lease(m, c, t) ==
  LET ldr == IF role[m] # "leader" /\ leader[m] = c THEN Nobody ELSE leader[m]
  IN \/ role[m] = "leader"
     \/ ldr # Nobody /\ ldr # c /\ t <= term[m] + 1 /\ ldr \notin suspects[m]

\* m grants pre-candidate c's request for c's next term.
GrantPre(m, c) ==
  /\ c \in Live /\ role[c] = "precandidate"
  /\ m \in Live \ ({c} \cup granted[c])
  /\ LET t == term[c] + 1 IN
     \/ /\ t > term[m]
        /\ ~Lease(m, c, t)
        /\ leader' = IF role[m] # "leader" /\ leader[m] = c
                     THEN [leader EXCEPT ![m] = Nobody] ELSE leader
     \/ /\ t = term[m]
        /\ vote[m] = c \/ (vote[m] = Nobody /\ leader[m] = Nobody)
        /\ UNCHANGED leader
  /\ granted' = [granted EXCEPT ![c] = @ \cup {m}]
  /\ UNCHANGED <<term, vote, role, left, suspects, kind, orders, leaderOf>>

\* A pre-candidate granted by a quorum campaigns in its next term.
ElectPre(c) ==
  /\ c \in Live /\ role[c] = "precandidate"
  /\ Quorum(granted[c])
  /\ term' = [term EXCEPT ![c] = @ + 1]
  /\ vote' = [vote EXCEPT ![c] = c]
  /\ role' = [role EXCEPT ![c] = "candidate"]
  /\ leader' = [leader EXCEPT ![c] = Nobody]
  /\ left' = [left EXCEPT ![c] = Nobody]
  /\ granted' = [granted EXCEPT ![c] = {c}]
  /\ kind' = [kind EXCEPT ![c] = "asked"]
  /\ UNCHANGED <<suspects, orders, leaderOf>>

\* m votes for candidate c in c's term: a later term moves m there as a
\* follower knowing no leader, unless the lease refuses an unforced request.
GrantVote(m, c) ==
  /\ c \in Live /\ role[c] = "candidate"
  /\ m \in Live \ ({c} \cup granted[c])
  /\ LET t == term[c] IN
     \/ /\ t > term[m]
        /\ kind[c] # "asked" \/ ~Lease(m, c, t)
        /\ term' = [term EXCEPT ![m] = t]
        /\ vote' = [vote EXCEPT ![m] = c]
        /\ role' = [role EXCEPT ![m] = "follower"]
        /\ leader' = [leader EXCEPT ![m] = Nobody]
        /\ left' = [left EXCEPT ![m] = Nobody]
        /\ granted' = [granted EXCEPT ![c] = @ \cup {m}, ![m] = {}]
        /\ kind' = [kind EXCEPT ![m] = "asked"]
     \/ /\ t = term[m]
        /\ vote[m] = c \/ (vote[m] = Nobody /\ leader[m] = Nobody)
        /\ vote' = [vote EXCEPT ![m] = c]
        /\ granted' = [granted EXCEPT ![c] = @ \cup {m}]
        /\ UNCHANGED <<term, role, leader, left, kind>>
  /\ UNCHANGED <<suspects, orders, leaderOf>>

\* A candidate a quorum voted for leads its term.
Win(c) ==
  /\ c \in Live /\ role[c] = "candidate"
  /\ Quorum(granted[c])
  /\ role' = [role EXCEPT ![c] = "leader"]
  /\ leader' = [leader EXCEPT ![c] = c]
  /\ granted' = [granted EXCEPT ![c] = {}]
  /\ kind' = [kind EXCEPT ![c] = "asked"]
  /\ leaderOf' = IF leaderOf[term[c]] = Nobody
                 THEN [leaderOf EXCEPT ![term[c]] = c] ELSE leaderOf
  /\ UNCHANGED <<term, vote, left, suspects, orders>>

\* A heartbeat or an append of term t's leader reaches m: m follows it in t,
\* unless m asks in t and suspects it, when the message is dropped.
Hear(m, t) ==
  /\ t \in Terms /\ leaderOf[t] \in Live
  /\ m \in Live \ {leaderOf[t]}
  /\ t >= term[m]
  /\ ~(role[m] = "precandidate" /\ t = term[m] /\ leaderOf[t] \in suspects[m])
  /\ ~(role[m] = "follower" /\ t = term[m] /\ leader[m] = leaderOf[t])
  /\ Follow(m, t, leaderOf[t])
  /\ UNCHANGED <<suspects, orders, leaderOf>>

\* s, which sends (a leader, or a member that asks), hears an answer of a
\* later term from m.
Learn(s, m) ==
  /\ s \in Live /\ m \in Live \ {s}
  /\ role[s] # "follower"
  /\ term[m] > term[s]
  /\ Follow(s, term[m], Nobody)
  /\ UNCHANGED <<suspects, orders, leaderOf>>

\* l orders heir h to campaign: under "core" and "unconditional" a leader
\* its detectors leave no quorum; under "stepdown" a member that led its
\* term and has stepped down, left no quorum (it campaigns itself where it
\* has one).  The order carries the condition under "core" and "asking".
Order(l, h) ==
  /\ l \in Live /\ h \in Servers \ {l}
  /\ ~TQ(l)
  /\ IF Rule = "stepdown"
     THEN role[l] = "follower" /\ term[l] \in Terms /\ leaderOf[term[l]] = l
     ELSE role[l] = "leader"
  /\ LET o == [from |-> l, to |-> h, term |-> term[l],
               cond |-> Rule \in {"core", "asking"}]
     IN /\ o \notin orders
        /\ orders' = orders \cup {o}
  /\ UNCHANGED <<term, vote, role, leader, left, suspects, granted, kind, leaderOf>>

\* h takes order o: a later term moves it there as a follower knowing no
\* leader first, whatever it was; it campaigns, forced, unless the order
\* carries the condition and it and those it trusts, the orderer apart, are
\* no quorum.  One of its own term only a follower or a pre-candidate takes.
TakeOrder(h, o) ==
  /\ o \in orders /\ o.to = h /\ h \in Live
  /\ \/ o.term > term[h]
     \/ o.term = term[h] /\ role[h] \in {"follower", "precandidate"}
  /\ LET takes == ~o.cond \/ ElectedWithout(h, o.from)
     IN IF takes /\ o.term < MaxTerm
        THEN /\ term' = [term EXCEPT ![h] = o.term + 1]
             /\ vote' = [vote EXCEPT ![h] = h]
             /\ role' = [role EXCEPT ![h] = "candidate"]
             /\ leader' = [leader EXCEPT ![h] = Nobody]
             /\ left' = [left EXCEPT ![h] = Nobody]
             /\ granted' = [granted EXCEPT ![h] = {h}]
             /\ kind' = [kind EXCEPT ![h] = IF o.cond THEN "condition" ELSE "order"]
        ELSE /\ o.term > term[h]
             /\ Follow(h, o.term, Nobody)
  /\ UNCHANGED <<suspects, orders, leaderOf>>

Step ==
  \/ \E s, x \in Servers : Suspect(s, x) \/ Trust(s, x)
  \/ \E s \in Servers : PreVote(s) \/ ElectPre(s) \/ Win(s)
  \/ \E m, c \in Servers : GrantPre(m, c) \/ GrantVote(m, c)
  \/ \E m \in Servers, t \in Terms : Hear(m, t)
  \/ \E s, m \in Servers : Learn(s, m)
  \/ \E l, h \in Servers : Order(l, h)
  \/ \E h \in Servers, o \in orders : TakeOrder(h, o)

\* The history: the first leader and term the group settled on.
Next ==
  /\ Step
  /\ settled' = IF CheckStable /\ settled = NotSettled /\ \E x \in Live : SettledNext(x)
                THEN LET w == CHOOSE x \in Live : SettledNext(x)
                     IN [l |-> w, t |-> term'[w]]
                ELSE settled

Spec == Init /\ [][Next]_vars

\* Election Safety (Ongaro, Figure 3.2).
OneLeader ==
  \A l, m \in Servers :
    (role[l] = "leader" /\ role[m] = "leader" /\ term[l] = term[m]) => l = m

\* Once settled, the group keeps its leader and its term.
Stable ==
  settled # NotSettled => Settled(settled.l) /\ term[settled.l] = settled.t

\* A member that asks for pre-votes and drops its leader's messages could be
\* elected without that leader: it and those it trusts are a quorum.
AskingCanWin == \A s \in Live : role[s] = "precandidate" => TQ(s)

\* The scope reaches what Stable is about: a settled leader its detectors
\* leave no quorum.  A configuration that checks Stable must reach it
\* (SuspicionReached.cfg, refused), or it checks nothing of the rule.
NoLeaderLeftNoQuorum == ~\E l \in Live : Settled(l) /\ ~TQ(l)

\* Where heirs can be elected without the leader, the conditional order
\* still elects one: a candidate it made is granted a quorum
\* (SuspicionHandOver.cfg, refused).
NoHandOver ==
  ~\E c \in Live : role[c] = "candidate" /\ kind[c] = "condition" /\ Quorum(granted[c])

WithinBudget == TLCGet("distinct") <= StateBudget
=============================================================================
