--------------------------------- MODULE Held ---------------------------------
(***************************************************************************)
(* Core step R-8: commits on entries held in memory, across fates          *)
(* (docs/raft.md §3.7).  The classic core of three voters, each its own    *)
(* fate, whose followers answer an append for what they hold in memory     *)
(* under a durable promise, and whose leader commits on the first of the   *)
(* classic quorum and a held quorum.  It is hyper-check's model of the     *)
(* step (crates/hyper-check/tests/models/held.rs) written as a             *)
(* specification, state for state: the same variables, the same steps and  *)
(* the same guards, so that TLC's distinct states under the symmetry of    *)
(* the members are the classes the Rust search counts, and each count      *)
(* checks the other (docs/models/README.md).                               *)
(*                                                                         *)
(* A member keeps its term and vote, whether it leads, the leader it       *)
(* follows and how far its log is known to match that leader's (through);  *)
(* its log in memory (mlog) and on disk (dlog), each entry the term of the *)
(* leader that took it; its durable promise; at most one write out (a log  *)
(* and a promise, Issue, made durable by Complete: the core's readies      *)
(* ahead of their persistence at a depth of one); the greatest entry it    *)
(* answered for in memory since it opened (answered); and its mark.  A     *)
(* leader keeps what each member said it holds in memory (held) and        *)
(* durably (matched).  The write out is kept once it is durable, as the    *)
(* Rust model keeps it: its log and promise are the disk's then.           *)
(*                                                                         *)
(* A follower answers for memory only while it follows a leader of its     *)
(* term, is not marked, and every promise out covers the entries: the      *)
(* durable promise and the write out's (Cover); the answer is at most what *)
(* that cover reaches.  A write may state no promise, what the member owes *)
(* (what it answered for, or its mark), or a promise of its term to any    *)
(* index while it follows a leader whose log it matches; it never states   *)
(* one below what it owes and its log does not reach, and a promise its    *)
(* log reaches it states as none.  A crash loses the log in memory and the *)
(* write out, and opens the member marked through its durable promise      *)
(* where its durable log does not reach it (R-5's Lost).  A mark ends once *)
(* the durable log reaches it in the log's order (Lost::reached_by), and a *)
(* marked voter answers for its mark in an election (R-5's claim); a       *)
(* marked candidate's own vote is not counted, and elected, its mark ends  *)
(* (R-7).  A leader of the mark's term narrows it to where its own log     *)
(* ends (Raft::narrow_held).                                               *)
(*                                                                         *)
(* A leader counts itself by its disk alone.  It commits through an index  *)
(* of its term when a majority holds the entry durably (classic), or when  *)
(* a majority holds it in memory or durably and either one of them holds   *)
(* it durably or their fates and the leader's own span Spanning (a held    *)
(* quorum).  Each member is its own fate, and the leader's counts whether  *)
(* or not it holds the entry: so with three members a held quorum without  *)
(* the leader's disk spans all three, and Spanning of two and of three is  *)
(* one search.                                                             *)
(*                                                                         *)
(* Messages are not modelled one by one: an answer acts when it is given,  *)
(* and what a leader was told stays told, so a commit after the answerer's *)
(* crash is a run the model has.  A candidate may campaign at any later    *)
(* term with any quorum that grants it.                                    *)
(*                                                                         *)
(* Mutant names a rule of the core taken out, or "none": "leadermemory"    *)
(* counts the leader's memory toward a held quorum; "pastpromise" answers  *)
(* with no promise covering the entries; "coveronly" answers under the     *)
(* durable promise alone, past a lower one out; "lowerpromise" lets a      *)
(* write state a promise below what the member owes; "ownlog" narrows a    *)
(* mark to the member's own durable log with no leader's word; "index"     *)
(* ends a mark by index alone, as storage's mark ends.  The checker must   *)
(* find each defect but the last (docs/models/README.md says why "index"   *)
(* is not refused).                                                        *)
(*                                                                         *)
(* Nothing here grows without a bound: a configuration states its distinct *)
(* states (StateBudget) and the checker stops at one more (WithinBudget).  *)
(*                                                                         *)
(* Checked:                                                                *)
(*   LeaderHolds   a leader of a term after an entry's commit holds it, in *)
(*                 memory, at its index (leader completeness)              *)
(*   Agreement     no index is committed to a second entry                 *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets, Sequences, TLC

CONSTANTS Servers,    \* the voters, each its own fate
          Nobody,     \* no vote, and no leader followed
          MaxTerm,
          MaxLen,     \* how long a log grows
          Spanning,   \* the fates a held quorum and its leader must span
          Mutant,     \* the rule taken out, or "none"
          StateBudget \* the distinct states the checker may find

Indexes == 1..MaxLen
Quorum == Cardinality(Servers) \div 2 + 1
\* A log's last entry, a promise or a mark: ordered as Raft orders logs
\* (thesis §3.6.1), term first; None for none.
Points == [term : 0..MaxTerm, index : 0..MaxLen]
None == [term |-> 0, index |-> 0]
IsNone(p) == p.term = 0
Below(p, q) == p.term < q.term \/ (p.term = q.term /\ p.index < q.index)
MaxPoint(p, q) == IF Below(p, q) THEN q ELSE p
MinPoint(p, q) == IF Below(q, p) THEN q ELSE p
Min(a, b) == IF a < b THEN a ELSE b
Max(a, b) == IF a > b THEN a ELSE b
Maximum(S) == CHOOSE x \in S : \A y \in S : y <= x
Zeros == [s \in Servers |-> 0]
NotChosen == [entry |-> 0, by |-> 0]

VARIABLES
  term,     \* [Servers -> 0..MaxTerm]
  vote,     \* [Servers -> Servers \cup {Nobody}]
  leads,    \* [Servers -> BOOLEAN]
  leader,   \* [Servers -> Servers \cup {Nobody}]: the leader it follows in its term
  through,  \* [Servers -> 0..MaxLen]: how far its log is known to match that leader's
  mlog,     \* [Servers -> Seq(1..MaxTerm)]: its log in memory
  dlog,     \* [Servers -> Seq(1..MaxTerm)]: its log on disk
  promise,  \* [Servers -> Points]: its durable promise
  issued,   \* [Servers -> BOOLEAN]: whether a write is out
  ilog,     \* [Servers -> Seq(1..MaxTerm)]: the log of the write it issued last
  ipromise, \* [Servers -> Points]: the promise of the write it issued last
  answered, \* [Servers -> Points]: the most it answered for in memory since it opened
  mark,     \* [Servers -> Points]: what its log may lack of what it answered for
  held,     \* [Servers -> [Servers -> 0..MaxLen]]: of a leader, what each member
            \* said it holds in memory
  matched,  \* [Servers -> [Servers -> 0..MaxLen]]: of a leader, what each member
            \* said it holds durably
  chosen,   \* [Indexes -> [entry, by]]: the entry first committed at an index (its
            \* term) and the term of the leader that committed it
  agreed    \* no index was committed to a second entry

vars == <<term, vote, leads, leader, through, mlog, dlog, promise, issued, ilog, ipromise,
          answered, mark, held, matched, chosen, agreed>>

Last(l) == IF Len(l) = 0 THEN None ELSE [term |-> l[Len(l)], index |-> Len(l)]
TermAt(l, i) == IF i = 0 \/ i > Len(l) THEN 0 ELSE l[i]
\* Whether a log whose last entry is last reaches m in the log's order
\* (Lost::reached_by).
Reached(last, m) == last.term > m.term \/ (last.term = m.term /\ last.index >= m.index)
\* Whether a mark m ends at the durable log dl.  "index" is storage's rule,
\* which the core does not apply to a promise's mark.
Ends(dl, m) ==
  LET last == Last(dl) IN
  IF Mutant = "index" THEN last.index >= m.index \/ last.term > m.term
  ELSE Reached(last, m)
\* What a voter answers for in an election (Raft::claim): its log's last
\* entry, or its mark, the later.
Claim(v) == MaxPoint(Last(mlog[v]), mark[v])
\* Whether two logs hold the same entries through k.
Matches(a, b, k) == k <= Len(a) /\ k <= Len(b) /\ \A i \in 1..k : a[i] = b[i]
\* How far a member's disk holds its log in memory.
DurableMatch(s) ==
  Maximum({k \in 0..Min(Len(mlog[s]), Len(dlog[s])) : Matches(dlog[s], mlog[s], k)})
\* What every promise out covers: the durable one's, and the write out's.
Cover(s) ==
  IF issued[s] /\ Mutant # "coveronly" THEN MinPoint(promise[s], ipromise[s]) ELSE promise[s]
\* A follower follows a leader that leads its term now.
Followed(f) ==
  /\ ~leads[f]
  /\ leader[f] # Nobody
  /\ leads[leader[f]]
  /\ term[leader[f]] = term[f]
\* What a member owes a promise: what it answered for, or its mark.
Owed(s) == MaxPoint(answered[s], mark[s])
\* The promises a write of s may state: none, what it owes, or one of its
\* term to any index while it follows a leader whose log it matches.
Promises(s) ==
  {None}
  \cup (IF IsNone(Owed(s)) THEN {} ELSE {Owed(s)})
  \cup (IF ~leads[s] /\ leader[s] # Nobody /\ through[s] # 0 /\ Last(mlog[s]).term = term[s]
        THEN {[term |-> term[s], index |-> i] : i \in Indexes}
        ELSE {})

IsLog(l) == Len(l) <= MaxLen /\ \A i \in 1..Len(l) : l[i] \in 1..MaxTerm

TypeOK ==
  /\ term \in [Servers -> 0..MaxTerm]
  /\ vote \in [Servers -> Servers \cup {Nobody}]
  /\ leads \in [Servers -> BOOLEAN]
  /\ leader \in [Servers -> Servers \cup {Nobody}]
  /\ through \in [Servers -> 0..MaxLen]
  /\ \A s \in Servers : IsLog(mlog[s]) /\ IsLog(dlog[s]) /\ IsLog(ilog[s])
  /\ promise \in [Servers -> Points]
  /\ issued \in [Servers -> BOOLEAN]
  /\ ipromise \in [Servers -> Points]
  /\ answered \in [Servers -> Points]
  /\ mark \in [Servers -> Points]
  /\ held \in [Servers -> [Servers -> 0..MaxLen]]
  /\ matched \in [Servers -> [Servers -> 0..MaxLen]]
  /\ chosen \in [Indexes -> [entry : 0..MaxTerm, by : 0..MaxTerm]]
  /\ agreed \in BOOLEAN

Init ==
  /\ term = [s \in Servers |-> 0]
  /\ vote = [s \in Servers |-> Nobody]
  /\ leads = [s \in Servers |-> FALSE]
  /\ leader = [s \in Servers |-> Nobody]
  /\ through = [s \in Servers |-> 0]
  /\ mlog = [s \in Servers |-> << >>]
  /\ dlog = [s \in Servers |-> << >>]
  /\ promise = [s \in Servers |-> None]
  /\ issued = [s \in Servers |-> FALSE]
  /\ ilog = [s \in Servers |-> << >>]
  /\ ipromise = [s \in Servers |-> None]
  /\ answered = [s \in Servers |-> None]
  /\ mark = [s \in Servers |-> None]
  /\ held = [s \in Servers |-> Zeros]
  /\ matched = [s \in Servers |-> Zeros]
  /\ chosen = [i \in Indexes |-> NotChosen]
  /\ agreed = TRUE

----------------------------------------------------------------------------
\* c is elected in term t by the voters of G: each grants a log at least as
\* current as what it answers for, in a term it has not voted in for
\* another; a marked candidate's own vote is not counted (R-7).  Elected,
\* its mark ends and it writes its first entry (Raft::become_leader).
Elect(c, t, G) ==
  LET voters == G \ {c} IN
  /\ t > term[c]
  /\ Len(mlog[c]) < MaxLen
  /\ Cardinality(G) >= Quorum
  /\ c \in G => IsNone(mark[c])
  /\ \A v \in voters :
       /\ term[v] < t \/ (term[v] = t /\ (vote[v] = Nobody \/ vote[v] = c))
       /\ ~Below(Last(mlog[c]), Claim(v))
  /\ term' = [s \in Servers |-> IF s = c \/ s \in voters THEN t ELSE term[s]]
  /\ vote' = [s \in Servers |-> IF s = c \/ s \in voters THEN c ELSE vote[s]]
  /\ leads' = [s \in Servers |-> IF s = c THEN TRUE
                                 ELSE IF s \in voters THEN FALSE ELSE leads[s]]
  /\ leader' = [s \in Servers |-> IF s = c THEN c
                                  ELSE IF s \in voters THEN Nobody ELSE leader[s]]
  /\ through' = [s \in Servers |-> IF s = c \/ s \in voters THEN 0 ELSE through[s]]
  /\ held' = [s \in Servers |-> IF s = c \/ s \in voters THEN Zeros ELSE held[s]]
  /\ matched' = [s \in Servers |-> IF s = c \/ s \in voters THEN Zeros ELSE matched[s]]
  /\ mlog' = [mlog EXCEPT ![c] = Append(@, t)]
  /\ mark' = [mark EXCEPT ![c] = None]
  /\ UNCHANGED <<dlog, promise, issued, ilog, ipromise, answered, chosen, agreed>>

\* The leader l takes an entry of its term.
Take(l) ==
  /\ leads[l]
  /\ Len(mlog[l]) < MaxLen
  /\ mlog' = [mlog EXCEPT ![l] = Append(@, term[l])]
  /\ UNCHANGED <<term, vote, leads, leader, through, dlog, promise, issued, ilog, ipromise,
                 answered, mark, held, matched, chosen, agreed>>

\* The follower f takes the leader l's log through k, as Raft's append does:
\* from the first entry that conflicts, or past its own end; a step that
\* changes none of its log, its leader, how far it matches or whether it
\* leads is no step.
Replicate(l, f, k) ==
  /\ leads[l]
  /\ l # f
  /\ k <= Len(mlog[l])
  /\ term[f] <= term[l]
  /\ LET new == SubSeq(mlog[l], 1, k)
         conflict == \E i \in 1..k : i <= Len(mlog[f]) /\ mlog[f][i] # new[i]
         taken == IF conflict \/ Len(mlog[f]) < k THEN new ELSE mlog[f]
         thr == IF term[f] = term[l] /\ leader[f] = l THEN Max(through[f], k) ELSE k
     IN /\ ~(mlog[f] = taken /\ leader[f] = l /\ through[f] = thr /\ ~leads[f])
        /\ mlog' = [mlog EXCEPT ![f] = taken]
        /\ through' = [through EXCEPT ![f] = thr]
  /\ term' = [term EXCEPT ![f] = term[l]]
  /\ vote' = [vote EXCEPT ![f] = IF term[f] < term[l] THEN Nobody ELSE @]
  /\ leads' = [leads EXCEPT ![f] = FALSE]
  /\ leader' = [leader EXCEPT ![f] = l]
  /\ held' = [held EXCEPT ![f] = Zeros]
  /\ matched' = [matched EXCEPT ![f] = Zeros]
  /\ UNCHANGED <<dlog, promise, issued, ilog, ipromise, answered, mark, chosen, agreed>>

\* The follower f answers its leader for memory, as far as its promises
\* cover (Raft::answer_held).
HeldAnswer(f) ==
  /\ Followed(f)
  /\ IsNone(mark[f])
  /\ through[f] # 0
  /\ Mutant = "pastpromise" \/ Cover(f).term = term[f]
  /\ LET l == leader[f]
         thr == IF Mutant = "pastpromise" THEN through[f] ELSE Min(through[f], Cover(f).index)
     IN /\ thr > held[l][f]
        /\ held' = [held EXCEPT ![l][f] = thr]
        /\ answered' = [answered EXCEPT ![f] = MaxPoint(@, [term |-> term[f], index |-> thr])]
  /\ UNCHANGED <<term, vote, leads, leader, through, mlog, dlog, promise, issued, ilog,
                 ipromise, mark, matched, chosen, agreed>>

\* The follower f answers its leader for what its disk holds of the
\* leader's log.
DurableAnswer(f) ==
  /\ Followed(f)
  /\ LET l == leader[f]
         j == Maximum({k \in 0..through[f] : Matches(dlog[f], mlog[l], k)})
     IN /\ j > matched[l][f]
        /\ matched' = [matched EXCEPT ![l][f] = j]
  /\ UNCHANGED <<term, vote, leads, leader, through, mlog, dlog, promise, issued, ilog,
                 ipromise, answered, mark, held, chosen, agreed>>

\* s issues a write of its log in memory stating the promise p
\* (Raft::refresh_held_promise, Raft::held_taken).
Issue(s, p) ==
  /\ ~issued[s]
  /\ LET owed == Owed(s)
         last == Last(mlog[s])
     IN /\ \/ Mutant = "lowerpromise"
           \/ IsNone(owed)
           \/ Reached(last, owed)
           \/ ~Below(p, owed)
        /\ ipromise' = [ipromise EXCEPT ![s] = IF ~IsNone(p) /\ Reached(last, p) THEN None ELSE p]
  /\ issued' = [issued EXCEPT ![s] = TRUE]
  /\ ilog' = [ilog EXCEPT ![s] = mlog[s]]
  /\ UNCHANGED <<term, vote, leads, leader, through, mlog, dlog, promise, answered, mark,
                 held, matched, chosen, agreed>>

\* The write s issued is durable (Raft::held_durable); a mark its log now
\* reaches ends.
Complete(s) ==
  /\ issued[s]
  /\ issued' = [issued EXCEPT ![s] = FALSE]
  /\ dlog' = [dlog EXCEPT ![s] = ilog[s]]
  /\ promise' = [promise EXCEPT ![s] = ipromise[s]]
  /\ mark' = [mark EXCEPT ![s] = IF ~IsNone(@) /\ Ends(ilog[s], @) THEN None ELSE @]
  /\ UNCHANGED <<term, vote, leads, leader, through, mlog, ilog, ipromise, answered, held,
                 matched, chosen, agreed>>

\* s crashes and opens on what is durable: marked through its promise where
\* its durable log does not reach it (Raft::open_held).
Crash(s) ==
  /\ mlog' = [mlog EXCEPT ![s] = dlog[s]]
  /\ issued' = [issued EXCEPT ![s] = FALSE]
  /\ ilog' = [ilog EXCEPT ![s] = << >>]
  /\ ipromise' = [ipromise EXCEPT ![s] = None]
  /\ leads' = [leads EXCEPT ![s] = FALSE]
  /\ leader' = [leader EXCEPT ![s] = Nobody]
  /\ through' = [through EXCEPT ![s] = 0]
  /\ answered' = [answered EXCEPT ![s] = None]
  /\ held' = [held EXCEPT ![s] = Zeros]
  /\ matched' = [matched EXCEPT ![s] = Zeros]
  /\ mark' = [mark EXCEPT ![s] = IF ~IsNone(promise[s]) /\ ~Reached(Last(dlog[s]), promise[s])
                                 THEN promise[s] ELSE None]
  /\ UNCHANGED <<term, vote, dlog, promise, chosen, agreed>>

\* The leader l commits through i, an index of its term: a majority holds
\* the entry durably, the leader by its disk; or a majority holds it in
\* memory or durably and one of them durably, or their fates and the
\* leader's span Spanning (Raft::held_quorum_index, Progress's held_half).
Commit(l, i) ==
  LET disk == DurableMatch(l)
      own == IF Mutant = "leadermemory" THEN Len(mlog[l]) ELSE disk
      At(v) == IF v = l THEN disk ELSE matched[l][v]
      HeldBy(v) == IF v = l THEN own ELSE Max(held[l][v], matched[l][v])
      classic == {v \in Servers : At(v) >= i}
      holders == {v \in Servers : HeldBy(v) >= i}
      span == Cardinality(holders) + (IF l \in holders THEN 0 ELSE 1)
      ondisk == \E v \in holders : At(v) >= i
      byheld == Cardinality(holders) >= Quorum /\ (ondisk \/ span >= Spanning)
      fresh == \E j \in 1..i : chosen[j].entry = 0
      differs == \E j \in 1..i : chosen[j].entry # 0 /\ chosen[j].entry # mlog[l][j]
  IN
  /\ leads[l]
  /\ i <= Len(mlog[l])
  /\ mlog[l][i] = term[l]
  /\ Cardinality(classic) >= Quorum \/ byheld
  /\ fresh \/ differs
  /\ chosen' = [j \in Indexes |-> IF j <= i /\ chosen[j].entry = 0
                                  THEN [entry |-> mlog[l][j], by |-> term[l]]
                                  ELSE chosen[j]]
  /\ agreed' = (agreed /\ ~differs)
  /\ UNCHANGED <<term, vote, leads, leader, through, mlog, dlog, promise, issued, ilog,
                 ipromise, answered, mark, held, matched>>

\* The leader l states where its log ends to the marked m, of its term;
\* m's mark narrows to it, and ends where its durable log reaches it
\* (Raft::hear_asking, Raft::narrow_held).  "ownlog" narrows to m's own
\* durable log, with no leader's word.
Narrow(l, m) ==
  /\ ~IsNone(mark[m])
  /\ IF Mutant = "ownlog" THEN l = m
     ELSE (leads[l] /\ l # m /\ mark[m].term = term[l] /\ term[m] = term[l])
  /\ LET end == IF Mutant = "ownlog" THEN Len(dlog[m]) ELSE Len(mlog[l])
         narrowed == [mark[m] EXCEPT !.index = end]
     IN /\ end < mark[m].index
        /\ mark' = [mark EXCEPT ![m] = IF Ends(dlog[m], narrowed) THEN None ELSE narrowed]
  /\ UNCHANGED <<term, vote, leads, leader, through, mlog, dlog, promise, issued, ilog,
                 ipromise, answered, held, matched, chosen, agreed>>

Next ==
  \/ \E c \in Servers, t \in 1..MaxTerm, G \in SUBSET Servers : Elect(c, t, G)
  \/ \E s \in Servers :
       \/ Take(s)
       \/ HeldAnswer(s)
       \/ DurableAnswer(s)
       \/ \E p \in Promises(s) : Issue(s, p)
       \/ Complete(s)
       \/ Crash(s)
       \/ \E i \in Indexes : Commit(s, i)
       \/ \E m \in Servers : Narrow(s, m)
       \/ \E f \in Servers, k \in Indexes : Replicate(s, f, k)

Spec == Init /\ [][Next]_vars

----------------------------------------------------------------------------
LeaderHolds ==
  \A s \in Servers : leads[s] =>
    \A i \in Indexes : (chosen[i].entry # 0 /\ term[s] > chosen[i].by) =>
      TermAt(mlog[s], i) = chosen[i].entry

Agreement == agreed

\* An entry is committed that fewer than a majority of disks hold: a leader
\* committed it on held answers.  The configuration of the core must reach
\* this (HeldReached.cfg, which the checker must refuse), or it checks
\* nothing of held commits.  A classic commit is never so: a majority's
\* disks held the entry when it was counted, and no later write takes a
\* committed entry from a disk (every later leader holds it).
HeldOnly ==
  \E i \in Indexes :
    /\ chosen[i].entry # 0
    /\ Cardinality({s \in Servers : TermAt(dlog[s], i) = chosen[i].entry}) < Quorum
NoHeldCommit == ~HeldOnly

\* For the checker: it has found no more states than the configuration
\* states it has.
WithinBudget == TLCGet("distinct") <= StateBudget

\* For the checker: the members play alike.
Symmetry == Permutations(Servers)
=============================================================================
