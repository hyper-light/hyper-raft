------------------------------- MODULE Suspicion -------------------------------
(***************************************************************************)
(* hyper-raft's elections by suspicion (docs/timing.md §2.9,              *)
(* crates/hyper-raft/src/watch.rs): what a member does with its owner's   *)
(* detectors' word, which may be wrong at any time, while some voters are *)
(* stopped for good, or one stops in order.  It is the rules alone: no   *)
(* log (every log is as current as every other, so every vote the log   *)
(* comparison would grant is granted, which only adds elections), one    *)
(* configuration of all the Servers, and messages as the members'        *)
(* states: a request is out while its asker asks in its term, and a      *)
(* leader's heartbeats of a term it led may arrive at any later time     *)
(* (leaderOf), a stopped member's as well.  What the core keeps safe     *)
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
(*                   SuspicionAsking);                                   *)
(*   "nodrop"        as "core", but a member that took a voter's         *)
(*                   announced stop takes what that voter sends as any   *)
(*                   other's (refused, SuspicionStopNoDrop).             *)
(*                                                                         *)
(* A voter in Leaving stops in order during the run (Stop), acting as    *)
(* any member until then; what it sent may still arrive after.  With     *)
(* Announced its stop is announced (hyper-liveness's stopping heartbeat, *)
(* raft.rs's Raft::stopped): each other member takes the announcement at *)
(* some time, before or after the voter stops (Announce), and from then  *)
(* suspects it, trusting it never again (the voter's next run is not     *)
(* modelled), orders it never, and drops its requests for votes and what *)
(* it sends of a later term than its own (Drops).  Without Announced     *)
(* its stop is detected as any silence is, by Suspect at some later time *)
(* (refused, SuspicionStopUnannounced).                                  *)
(*                                                                         *)
(* Stable: where the voters out leave no quorum without the leader, a    *)
(* group whose remaining voters follow one leader in its term keeps that *)
(* leader and that term, whatever the detectors then say of the          *)
(* remaining voters (the history variable settled holds the first such   *)
(* leader and term, CheckStable).  The voters out, at each remaining     *)
(* voter: those stopped for good, which it suspects, and the leaving     *)
(* one, whose announcement it took (Announced), or which stopped (not    *)
(* Announced).                                                           *)
(*                                                                         *)
(* Nothing grows without a bound: terms to MaxTerm, and StateBudget, as  *)
(* in FastTrack.tla.                                                     *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets, TLC

CONSTANTS Servers, Nobody, MaxTerm, Stopped, Leaving, Announced, Rule,
          CheckStable, StateBudget

Terms == 1..MaxTerm
Quorum(S) == 2 * Cardinality(S \cap Servers) > Cardinality(Servers)
\* The voters Stable is about: all but those stopped for good and the
\* leaving one.
Remaining == Servers \ (Stopped \cup Leaving)
Roles == {"follower", "precandidate", "candidate", "leader"}
\* How a candidate's campaign began: asked for, or ordered by a leader,
\* with or without the condition.
Kinds == {"asked", "order", "condition"}
Orders == [from : Servers, to : Servers, term : Terms, cond : BOOLEAN]
NotSettled == [l |-> Nobody, t |-> 0]

VARIABLES term, vote, role, leader, left, suspects, granted, kind, orders,
          leaderOf, down, gone, settled
vars == <<term, vote, role, leader, left, suspects, granted, kind, orders,
          leaderOf, down, gone, settled>>

\* The members that act: all but those stopped.
Up == Servers \ down
TQ(s) == Quorum({s} \cup (Servers \ suspects[s]))
ElectedWithout(h, o) == Quorum(({h} \cup (Servers \ suspects[h])) \ {o})
TrustsLeader(s) == leader[s] # Nobody /\ leader[s] \notin suspects[s]
\* s took x's announced stop, and keeps x from taking the group's leader:
\* it orders x never, and drops x's requests for votes and what x sends of
\* a later term than s's own.
Drops(s, x) == Rule # "nodrop" /\ x \in gone[s]

\* The remaining voter s holds the voters out as out: those stopped for
\* good suspected, and the leaving one's announcement taken or, where it is
\* not announced, the leaving one stopped.
Out(s) ==
  /\ Stopped \subseteq suspects[s]
  /\ IF Announced THEN Leaving \subseteq gone[s] ELSE Leaving \subseteq down
OutNext(s) ==
  /\ Stopped \subseteq suspects'[s]
  /\ IF Announced THEN Leaving \subseteq gone'[s] ELSE Leaving \subseteq down'

\* l leads, and every remaining voter follows it in its term, holding the
\* voters out as out.
Settled(l) ==
  /\ l \in Remaining
  /\ role[l] = "leader"
  /\ \A s \in Remaining :
       /\ Out(s)
       /\ s # l => term[s] = term[l] /\ role[s] = "follower" /\ leader[s] = l
SettledNext(l) ==
  /\ l \in Remaining
  /\ role'[l] = "leader"
  /\ \A s \in Remaining :
       /\ OutNext(s)
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
  /\ down \subseteq Servers
  /\ gone \in [Servers -> SUBSET Servers]

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
  /\ down = Stopped
  /\ gone = [s \in Servers |-> {}]
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

\* The detectors of s come to suspect x, which they did not: a leader left
\* no quorum steps down under "stepdown"; a pre-candidate left no quorum
\* stops asking under every rule but "asking".
SuspectsNext(s, x) ==
  LET sus == suspects[s] \cup {x}
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

\* The detectors of s suspect x.
Suspect(s, x) ==
  /\ s \in Up
  /\ x \in Servers \ ({s} \cup suspects[s])
  /\ SuspectsNext(s, x)
  /\ UNCHANGED <<term, vote, orders, leaderOf, down, gone>>

\* The detectors of s trust x again.  A stopped voter's heartbeats never
\* come, nor those of a run whose stop s took.
Trust(s, x) ==
  /\ s \in Up
  /\ x \in (suspects[s] \cap Up) \ gone[s]
  /\ suspects' = [suspects EXCEPT ![s] = @ \ {x}]
  /\ UNCHANGED <<term, vote, role, leader, left, granted, kind, orders, leaderOf,
                 down, gone>>

\* s, trusting no leader and with a quorum it trusts, asks for pre-votes for
\* its next term, anew if it asked before.
PreVote(s) ==
  /\ s \in Up
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
  /\ UNCHANGED <<term, vote, suspects, orders, leaderOf, down, gone>>

\* The lease of m against a request of c for term t (Raft::step_newer_term):
\* m leads, or trusts a leader other than c and c's term is not past m's
\* next.  A request from m's own leader ends m's trust first.
Lease(m, c, t) ==
  LET ldr == IF role[m] # "leader" /\ leader[m] = c THEN Nobody ELSE leader[m]
  IN \/ role[m] = "leader"
     \/ ldr # Nobody /\ ldr # c /\ t <= term[m] + 1 /\ ldr \notin suspects[m]

\* m grants pre-candidate c's request for c's next term.  The grant names
\* that term, later than c's own.
GrantPre(m, c) ==
  /\ role[c] = "precandidate"
  /\ m \in Up \ ({c} \cup granted[c])
  /\ ~Drops(m, c) /\ ~Drops(c, m)
  /\ LET t == term[c] + 1 IN
     \/ /\ t > term[m]
        /\ ~Lease(m, c, t)
        /\ leader' = IF role[m] # "leader" /\ leader[m] = c
                     THEN [leader EXCEPT ![m] = Nobody] ELSE leader
     \/ /\ t = term[m]
        /\ vote[m] = c \/ (vote[m] = Nobody /\ leader[m] = Nobody)
        /\ UNCHANGED leader
  /\ granted' = [granted EXCEPT ![c] = @ \cup {m}]
  /\ UNCHANGED <<term, vote, role, left, suspects, kind, orders, leaderOf, down,
                 gone>>

\* A pre-candidate granted by a quorum campaigns in its next term.
ElectPre(c) ==
  /\ c \in Up /\ role[c] = "precandidate"
  /\ Quorum(granted[c])
  /\ term' = [term EXCEPT ![c] = @ + 1]
  /\ vote' = [vote EXCEPT ![c] = c]
  /\ role' = [role EXCEPT ![c] = "candidate"]
  /\ leader' = [leader EXCEPT ![c] = Nobody]
  /\ left' = [left EXCEPT ![c] = Nobody]
  /\ granted' = [granted EXCEPT ![c] = {c}]
  /\ kind' = [kind EXCEPT ![c] = "asked"]
  /\ UNCHANGED <<suspects, orders, leaderOf, down, gone>>

\* m votes for candidate c in c's term: a later term moves m there as a
\* follower knowing no leader, unless the lease refuses an unforced request.
GrantVote(m, c) ==
  /\ role[c] = "candidate"
  /\ m \in Up \ ({c} \cup granted[c])
  /\ ~Drops(m, c)
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
  /\ UNCHANGED <<suspects, orders, leaderOf, down, gone>>

\* A candidate a quorum voted for leads its term.
Win(c) ==
  /\ c \in Up /\ role[c] = "candidate"
  /\ Quorum(granted[c])
  /\ role' = [role EXCEPT ![c] = "leader"]
  /\ leader' = [leader EXCEPT ![c] = c]
  /\ granted' = [granted EXCEPT ![c] = {}]
  /\ kind' = [kind EXCEPT ![c] = "asked"]
  /\ leaderOf' = IF leaderOf[term[c]] = Nobody
                 THEN [leaderOf EXCEPT ![term[c]] = c] ELSE leaderOf
  /\ UNCHANGED <<term, vote, left, suspects, orders, down, gone>>

\* A heartbeat or an append of term t's leader reaches m: m follows it in t,
\* unless m asks in t and suspects it, when the message is dropped.
Hear(m, t) ==
  /\ t \in Terms /\ leaderOf[t] \in Servers
  /\ m \in Up \ {leaderOf[t]}
  /\ t >= term[m]
  /\ ~(t > term[m] /\ Drops(m, leaderOf[t]))
  /\ ~(role[m] = "precandidate" /\ t = term[m] /\ leaderOf[t] \in suspects[m])
  /\ ~(role[m] = "follower" /\ t = term[m] /\ leader[m] = leaderOf[t])
  /\ Follow(m, t, leaderOf[t])
  /\ UNCHANGED <<suspects, orders, leaderOf, down, gone>>

\* s, which sends (a leader, or a member that asks), hears an answer of a
\* later term from m.
Learn(s, m) ==
  /\ s \in Up /\ m \in Servers \ {s}
  /\ role[s] # "follower"
  /\ term[m] > term[s]
  /\ ~Drops(s, m)
  /\ Follow(s, term[m], Nobody)
  /\ UNCHANGED <<suspects, orders, leaderOf, down, gone>>

\* l orders heir h to campaign: under "core" and "unconditional" a leader
\* its detectors leave no quorum; under "stepdown" a member that led its
\* term and has stepped down, left no quorum (it campaigns itself where it
\* has one).  The order carries the condition under "core", "asking" and
\* "nodrop".  A member whose stop l took is no heir.
Order(l, h) ==
  /\ l \in Up /\ h \in Servers \ {l}
  /\ ~Drops(l, h)
  /\ ~TQ(l)
  /\ IF Rule = "stepdown"
     THEN role[l] = "follower" /\ term[l] \in Terms /\ leaderOf[term[l]] = l
     ELSE role[l] = "leader"
  /\ LET o == [from |-> l, to |-> h, term |-> term[l],
               cond |-> Rule \in {"core", "asking", "nodrop"}]
     IN /\ o \notin orders
        /\ orders' = orders \cup {o}
  /\ UNCHANGED <<term, vote, role, leader, left, suspects, granted, kind, leaderOf,
                 down, gone>>

\* h takes order o: a later term moves it there as a follower knowing no
\* leader first, whatever it was; it campaigns, forced, unless the order
\* carries the condition and it and those it trusts, the orderer apart, are
\* no quorum.  One of its own term only a follower or a pre-candidate takes.
TakeOrder(h, o) ==
  /\ o \in orders /\ o.to = h /\ h \in Up
  /\ \/ o.term > term[h]
     \/ o.term = term[h] /\ role[h] \in {"follower", "precandidate"}
  /\ ~(o.term > term[h] /\ Drops(h, o.from))
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
  /\ UNCHANGED <<suspects, orders, leaderOf, down, gone>>

\* s takes the leaving voter x's announced stop: it suspects x, if it did
\* not, and from now on keeps it out (Trust, Drops).
Announce(s, x) ==
  /\ Announced
  /\ x \in Leaving
  /\ s \in Up \ {x}
  /\ x \notin gone[s]
  /\ gone' = [gone EXCEPT ![s] = @ \cup {x}]
  /\ IF x \in suspects[s]
     THEN UNCHANGED <<suspects, role, leader, left, granted, kind>>
     ELSE SuspectsNext(s, x)
  /\ UNCHANGED <<term, vote, orders, leaderOf, down>>

\* The leaving voter x stops: it acts no more, and what it sent may still
\* arrive.
Stop(x) ==
  /\ x \in Leaving \cap Up
  /\ down' = down \cup {x}
  /\ UNCHANGED <<term, vote, role, leader, left, suspects, granted, kind, orders,
                 leaderOf, gone>>

Step ==
  \/ \E s, x \in Servers : Suspect(s, x) \/ Trust(s, x)
  \/ \E s \in Servers : PreVote(s) \/ ElectPre(s) \/ Win(s)
  \/ \E m, c \in Servers : GrantPre(m, c) \/ GrantVote(m, c)
  \/ \E m \in Servers, t \in Terms : Hear(m, t)
  \/ \E s, m \in Servers : Learn(s, m)
  \/ \E l, h \in Servers : Order(l, h)
  \/ \E h \in Servers, o \in orders : TakeOrder(h, o)
  \/ \E s, x \in Servers : Announce(s, x)
  \/ \E x \in Servers : Stop(x)

\* The history: the first leader and term the group settled on.
Next ==
  /\ Step
  /\ settled' = IF CheckStable /\ settled = NotSettled /\ \E x \in Remaining : SettledNext(x)
                THEN LET w == CHOOSE x \in Remaining : SettledNext(x)
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
AskingCanWin == \A s \in Up : role[s] = "precandidate" => TQ(s)

\* The scope reaches what Stable is about: a settled leader its detectors
\* leave no quorum.  A configuration that checks Stable must reach it
\* (SuspicionReached.cfg, refused), or it checks nothing of the rule.
NoLeaderLeftNoQuorum == ~\E l \in Remaining : Settled(l) /\ ~TQ(l)

\* Where heirs can be elected without the leader, the conditional order
\* still elects one: a candidate it made is granted a quorum
\* (SuspicionHandOver.cfg, refused).
NoHandOver ==
  ~\E c \in Up : role[c] = "candidate" /\ kind[c] = "condition" /\ Quorum(granted[c])

\* The scope of a stop reaches what Stable is about there: a settled group
\* one of whose remaining voters suspects the leader, wrongly
\* (SuspicionStopReached.cfg, refused).
NoWrongSuspicionSettled ==
  ~\E l \in Remaining : Settled(l) /\ \E s \in Remaining \ {l} : l \in suspects[s]

WithinBudget == TLCGet("distinct") <= StateBudget
=============================================================================
