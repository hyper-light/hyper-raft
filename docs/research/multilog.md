# Research: several Raft logs merged into one application order

Source notes for `docs/multilog.md`. Each entry says what the source establishes, verified against
its text on 2026-10-04 (papers read as PDFs at the addresses named, web pages read where they are
published, code read at the revision named), and what it leaves open. Where a source is a project
of ours, the entry names the file and revision read.

## 1. MLRaft, the layer's namesake

**Zhang and Xu, "MLRaft: Improvement of Raft Based on Multi-log Synchronization Model",
*Proceedings of the 2022 6th International Conference on Electronic Information Technology and
Computer Engineering* (EITCE '22), ACM, pp. 1050–1055, published 2022-10-21,
doi:10.1145/3573428.3573617.** Menghao Zhang and Lizhen Xu, Southeast University. slates cites it as
"ICEITCE 2022"; the proceedings' own acronym is EITCE.

- **What was read.** The abstract, from two indexes that agree on it: Semantic Scholar's record
  (`api.semanticscholar.org/graph/v1/paper/DOI:10.1145/3573428.3573617`, verbatim) and OpenAlex's
  (`api.openalex.org/works/doi:10.1145/3573428.3573617`, which also gives the pages and the
  affiliation). Semantic Scholar's record gives the reference list, eleven items: Raft-PLUS,
  Weighted RAFT, ECRaft, "Accurate and efficient follower log repair for Raft-replicated database
  systems", "Network-Assisted Raft Consensus Algorithm", "Leader or Majority", "Limitations of
  Highly-Available Eventually-Consistent Data Stores", the Raft paper, TiKV, LC-Raft and "Paxos
  Made Simple".
- **What was not.** The full text. Both indexes list it as gold open access at
  `dl.acm.org/doi/pdf/10.1145/3573428.3573617`; every fetch from this machine on 2026-10-04 (the
  PDF, `epdf`, `fullHtml`, with a browser's user agent and without) met a Cloudflare challenge, HTTP
  403, as slates' fetch did on 2026-09-28. slates flagged it for verification; what is verifiable is
  the abstract, the bibliographic record and the references, and nothing below rests on more.
- **The abstract establishes.** "MLRaft divides a single log file in Raft into n log files. For each
  log file, a leader can be elected to maintain the consistency of the log file. Multiple leaders
  jointly undertake the requests of the distributed system, which improves the performance of the
  distributed system and makes the entire cluster more balanced." It "also proposes the priority
  election method and the dynamic transfer mechanism of leader to ensure the uniform distribution
  of multiple leaders in distributed nodes", and measures "a 3-node KV storage system ...
  throughput, latency, and load balance", reporting "better performance than Raft".
- **Left open, and so the whole of what this layer decides.** How a command is assigned to a log;
  whether commands of different logs are ordered for application, and how; what a command that
  reads more than one log's state sees; recovery and compaction across logs. Neither the abstract
  nor the reference list names a protocol that orders several leaders' logs (no Mencius, EPaxos,
  Calvin or Generalized Paxos): a key-value store whose every operation touches one key needs no
  order across logs if each key lives in one log, which is Multi-Raft's arrangement (§2). The
  barrier and the merge are slates' design (§8), and the design re-derives them from §3–§6.

## 2. Multi-Raft: many independent logs between the same nodes

**TiKV, "Multi-raft" (tikv.org/deep-dive/scalability/multi-raft/, read 2026-10-04).**

- The keyspace is divided into Regions, each its own Raft group ("Raft group is divided into
  multiple Raft groups in terms of partitions, namely, Region"); Regions split as they grow and
  merge when small.
- Groups share a store's resources: "TiKV uses an event loop to drive all the processes in a batch
  manner"; one RocksDB `WriteBatch` persists many groups' appends; "TiKV reuses the connection
  between two nodes for multiple Raft groups".
- Nothing orders one Region's log against another's; a transaction that spans Regions goes through
  TiKV's Percolator-style transaction protocol, not through a merged log.

**Darnell, "Scaling Raft", Cockroach Labs blog, 2015-06-12.**

- MultiRaft is "a layer on top of Raft" that manages "an entire node's worth of ranges as a group";
  "Each pair of nodes only needs to exchange heartbeats once per tick, no matter how many ranges
  they have in common"; a small, constant number of goroutines (three) instead of one per range.
- Ranges are independent consensus groups; MultiRaft batches their communication per node pair and
  orders nothing across them.

**What §2 establishes, and leaves open.** Running many logs between the same nodes is routine, and
what they cost per node pair (heartbeats, connections, threads) is shared, as hyper-liveness's one
stream per node pair already is for every group a pair shares (`docs/timing.md` §2.8). Neither
system merges its groups' logs: a state that spans groups is reached by transactions. MLRaft's one
state machine over n logs is the case they leave open.

## 3. What must be ordered: interference and commutativity

**Lamport, "Generalized Consensus and Paxos", Microsoft Research MSR-TR-2005-33 (3 March 2004,
revised 15 March 2005, corrected 28 April 2005).**

- Engineer's abstract: "We generalize the state-machine approach to involve reaching agreement on a
  partially ordered set of commands ... concurrently issued commands can always be executed in two
  message delays if they are non-interfering, so it does not matter in which order those commands
  are executed."
- §4.4, "Command Histories": "defining an execution of a set of commands does not require totally
  ordering them. It suffices to determine the order in which every pair of non-commuting commands
  are executed." An interference relation ≍ is symmetric and must hold for every non-commuting
  pair; two sequences are equivalent "iff one can be transformed into the other by permuting
  elements in such a way that the order of all pairs of interfering commands is preserved"; a
  *command history* is such an equivalence class, isomorphic to a Mazurkiewicz trace and to the
  graph whose edges join interfering commands in sequence order; `[σ] ⊑ [τ]` iff `G(σ)` is a
  prefix of `G(τ)`. "If non-interfering commands commute, then ... executions of equivalent c-seqs
  yield the same result—that is, the same output for each command and the same final state. Hence,
  to use the state-machine approach for implementing a system, it suffices to solve the consensus
  problem for histories."
- **Establishes** the meaning `docs/multilog.md` gives "one application order every replica
  reaches alike": one command history, of which every replica's sequence is a member, and of whose
  prefixes every replica's state is one.

**Moraru, Andersen and Kaminsky, "There Is More Consensus in Egalitarian Parliaments", SOSP 2013
(EPaxos; `cs.cmu.edu/~dga/papers/epaxos-sosp2013.pdf`).**

- §4.1: "Two commands γ and δ interfere if there exists a sequence of commands Σ such that the serial
  execution Σ, γ, δ is not equivalent to Σ, δ, γ (i.e., they result in different machine states
  and/or different values returned by the reads within these sequences)." Each replica owns its own
  instances: "the state of each replica can be regarded as a two-dimensional array with N rows and
  an unbounded number of columns", a log per command leader.
- §4.2, the guarantees: **execution consistency**, "If two interfering commands γ and δ are
  successfully committed (by any replicas) they will be executed in the same order by every
  replica"; **execution linearizability**, "If two interfering commands γ and δ are serialized by
  clients (i.e., δ is proposed only after γ is committed by any replica), then every replica will
  execute γ before δ."
- §3, on Mencius: "every replica must hear from all other replicas before committing a command A
  ... (1) the replicated state machine runs at the speed of the slowest replica, and (2) Mencius
  can exhibit worse availability than Multi-Paxos, because if any replica fails to respond, no
  other replica can make progress until a failure is suspected and another replica commits no-ops
  on behalf of the possibly failed replica."
- **Establishes** the two safety properties this layer states (`docs/multilog.md` §4) in their
  standard form, and the availability cost of making every log take part in every order decision.
- **Left open for this layer.** EPaxos computes dependencies per command at commit time, with a
  Paxos round of its own; here the order across logs must come from n ordinary Raft logs, whose
  leaders know nothing of each other's entries.

**Cao et al., "PolarFS: An Ultra-low Latency and Failure Resilient Distributed File System for
Shared Storage Cloud Database", PVLDB 11(12):1849–1862, 2018 (ParallelRaft, §5;
`vldb.org/pvldb/vol11/p1849-cao.pdf`).**

- §5.2: "If the writing ranges of the log entries are not overlapped with each other, then these log
  entries are considered without conflict, and can be executed in any order. Otherwise, the
  conflicted entries will be executed in a strict sequence as they arrived." The *look behind
  buffer* of each entry holds "the LBA modified by the previous N log entries ... N is the span of
  this bridge, which is also the maximum size of a log hole permitted"; "N set to 2 is good enough
  for its I/O concurrency."
- §5.3: a new leader runs a *merge stage* to collect committed entries it lacks before serving;
  §5.5: "the conflicting logs can only be applied in a strict sequence, which means that the state
  machine ... on all nodes in the same quorum will be consistent with each other."
- **Establishes** the rule this layer applies across logs rather than across one log's holes:
  commands of disjoint state (here, keys of different logs) are applied in any order; commands of
  the same state in their log's order. ParallelRaft's out-of-order *commitment* is not taken: slates'
  prefix model found it loses a committed entry (note 32 R18, excluded; `docs/raft.md` §3.2, R17).

**Mencius (below, §4), §5, "Out-of-order commit"**: "x and y ... commutable, i.e., executing x
followed by y produces the same system state as executing y followed by x ... We implement
out-of-order commit in Mencius by tracking the dependencies between the requests and by committing a
request as soon as all requests it depends on have been committed."

## 4. Deterministic merges of several logs into one order

**Mao, Junqueira and Marzullo, "Mencius: Building Efficient Replicated State Machines for WANs",
OSDI 2008 (`usenix.org/legacy/event/osdi08/tech/full_papers/mao/mao.pdf`).**

- §1, §4.2: the instances are partitioned among the servers in turn ("instance cn + p to server
  p"); each server leads its own; the merged order is the instance order.
- §2: "a server commits an instance only when it has learned and committed all previous consensus
  instances."
- §4.3, Rule 1: "Servers cannot commit requests before all previous requests are committed, and so
  Rule 1 commits requests at the rate of the slowest server. In the extreme case that a server
  suggests no request for a long period of time, the state machine stalls, preventing a potentially
  unbounded number of requests from committing." Rule 2 makes an idle server skip its turns; Rule 3
  lets another server revoke a suspected server's instances, because "a crashed server does not
  broadcast SKIP messages, and such a server can prevent others from committing."
- §4.4: "Since all servers in Mencius act as a leader for an unbounded number of instances, Mencius
  has this reduced performance when *any* server fails. Thus, Mencius has higher performance than
  Paxos in the failure-free case at the cost of potentially higher latency upon failures."
- **Establishes** the price of a total order over several leaders' logs: every log takes part in
  every step of the order, idle logs must say so (skips), and a failed leader stalls everyone until
  it is revoked.

**Thomson et al., "Calvin: Fast Distributed Transactions for Partitioned Database Systems", SIGMOD
2012 (`cs.yale.edu/homes/thomson/publications/calvin-sigmod12.pdf`).**

- §3.1: "Calvin divides time into 10-millisecond epochs during which every machine's sequencer
  component collects transaction requests from clients. At the end of each epoch, all requests that
  have arrived at a sequencer node are compiled into a batch." Once replicated, each batch goes to
  every scheduler with the sequencer's id and the epoch, which "allows every scheduler to piece
  together its own view of a global transaction order by interleaving (in a deterministic,
  round-robin manner) all sequencers' batches for that epoch."
- §3.2: a deterministic lock manager grants locks "strictly in the order in which those
  transactions requested the lock", so transactions that do not conflict execute concurrently while
  the outcome equals the serial order.
- **Establishes** a deterministic merge by epoch: every log contributes a batch (empty or not) to
  every epoch, and an epoch's order is complete only when every log's batch for it is in.

**Marandi, Primi and Pedone, "Multi-Ring Paxos", DSN 2012 (technical report version,
`inf.usi.ch/pedone/Paper/2012/2012TR-MRP.pdf`).**

- §II-C: a partitioned service multicasts single-partition requests to their partition's group and
  requests over several partitions to a group of all, `g_all`.
- §IV-A: learners merge rings "in round-robin fashion, delivering a fixed number of messages from
  each group they subscribe to in a pre-defined order ... M messages from `g_l1`, then M messages
  from `g_l2`, and so on"; with rates `λ1 < λ2` the learner delivers at `2λ1` and its "buffer will
  grow at rate `λ2 − λ1` and will eventually overflow". The remedy: each ring's coordinator compares
  its rate `μ` with the maximum expected rate `λ` and "proposes enough 'skip messages' to reach λ".
- **Establishes** that a deterministic round-robin merge is paced by its slowest log and needs
  skips to stay live, and that a merge's buffer grows without bound unless the merge's inputs are
  rate-matched: the bound `docs/multilog.md` §6 states on what a log may hold beyond its merge.

**Ding et al., "Scalog: Seamless Reconfiguration and Total Order in a Scalable Shared Log", NSDI
2020 (`usenix.org/system/files/nsdi20-paper-ding.pdf`).**

- §2.3: "Periodically, each storage server reports the lengths of the log segments it stores to an
  ordering layer. The ordering layer, also periodically, determines which records have been fully
  replicated ... Using the globally ordered sequence of reports from the ordering layer, a storage
  server can interleave its log segments into a global order consistent with the original partial
  order."
- §3: "The ordering layer periodically summarizes the fully replicated prefix of the primary log
  segment of each storage server in a cut ... The storage servers use these cuts to deterministically
  assign a unique global sequence number to each durable record"; the ordering layer is a Paxos
  group.
- **Establishes** a total order by agreed cuts: every record waits for the next cut, and the
  ordering group's availability is every record's.

**What §4 establishes together.** A total order of several logs needs every log to take part at
every step (Mencius' turns and skips, Calvin's per-epoch batches, Multi-Ring Paxos's rate-matched
skips) or a sequencer of cuts (Scalog); either way the slowest log paces all, and a log without a
leader stalls all until it is revoked or replaced. A history (§3) needs order only between
interfering commands, so a merge that orders only those stalls only what interferes with a stalled
log. MLRaft's global commands interfere with everything; for them the barrier is a Mencius turn
every log must take, and slates measured that cost (§8).

## 5. Linearizability, across objects

**Herlihy and Wing, "Linearizability: A Correctness Condition for Concurrent Objects", ACM TOPLAS
12(3):463–492, July 1990 (`cs.brown.edu/~mph/HerlihyW90/p463-herlihy.pdf`).**

- §2.2: a history `H` is linearizable if it can be extended to `H'` such that "L1: complete(H') is
  equivalent to some legal sequential history S, and L2: `<_H ⊆ <_S`", `<_H` the real-time
  precedence of operations.
- §3.1, Theorem 1 (locality): "H is linearizable if and only if, for each object x, H | x is
  linearizable." The proof takes the transitive closure of each object's linearization order and
  `<_H` and shows it has no cycle.
- **Establishes** that keyed commands, each on one key, are linearizable together once each key's
  are. Global commands touch every object, which locality does not cover; `docs/multilog.md` §4.3
  proves the whole by the same construction, the merge's order and the real-time order having no
  cycle.

## 6. Compaction and snapshots

**Ongaro and Ousterhout, extended Raft paper, §7** (quoted in `docs/research/durable.md` §1): each
server snapshots independently, covering committed entries; a snapshot carries its last index, term
and the configuration as of that index, and a follower too far behind is sent the leader's.

**slates `docs/wip/GAPS.md`, 2026-09-29 (MLRaft entry, read at `c7a2b75`):** "Owed only if a group
ever runs `n > 1`: compaction across logs. A log's snapshot must not pass entries the merge has not
applied, so log 0's snapshot waits until every other log has reached a barrier at or past it, and
the others' snapshots carry an epoch floor. Today a restored multi-log node replays from each log's
snapshot, which holds only while nothing is compacted."

**What §6 leaves open, and `docs/multilog.md` §5 closes.** Which positions of n logs an image may be
taken at so that any member can install any other member's image: an image must be a prefix of the
one history (§3) that every member's state is comparable with, or a member could be handed an image
that is ahead in one log and behind in another.

## 7. hyper-raft's own pieces the layer stands on (read at `2e9fd39`)

- `RawNode::given_to_apply`: through which index the core has given entries to apply, each
  committed and durable here (`node.rs`, I4).
- `Storage::any_entry`: a walk of `[low, high)` in order "without copying any" (`storage.rs`).
- A follower forwards a proposal to its leader (`raft.rs`, `step_follower`, `MsgPropose`), and a
  candidate drops it (`ProposalDropped`).
- Priority with `Precedence::Log`, `transfer_leader`, `read_index`, `apply_conf_change`
  (`docs/raft.md` §1, §3.3).
- A campaign waits for a committed change of configuration to be applied (`docs/raft.md` §3.3,
  "One told to campaign before it applied a change"; R4 in §3.2), which is why the layer applies
  changes as they are given and not as the merge reaches them (`docs/multilog.md` §3.4).

## 8. slates' MLRaft (read at `5cce86a`, the revision `hyper-raft-compare` pins, and at `c7a2b75`; the three files are identical at both)

`crates/cluster/src/multilog.rs`, `tests/multilog.rs`, `tests/multilog_timed.rs`; the record in
`docs/wip/research/consensus-enhancements.md` §3.6 and slice 14, `docs/wip/BENCHMARKS.md`
("MLRaft: one to five logs across five regions"), `docs/wip/GAPS.md`.

- **Routing.** A command is keyed (it reads and writes one key's state and reads the group's global
  state) or global (it may read and write anything). A keyed command goes to the log its key's
  SplitMix64 hash modulo `n` names; a global one to log 0.
- **Barriers.** "each other log's leader, once its replica of log 0 has committed a global command,
  appends a barrier naming it." A leader appends afresh in each term ("a barrier naming a global
  command already applied is passed over, so a repeat costs one entry").
- **The merge.** A keyed entry in its log's order, in the epoch its log's last barrier opened; a
  barrier once the global it names has been applied (`global <= epoch`); a global only when every
  other log's next entry is a committed barrier naming it or a later one. Entries a log holds out of
  place (a global outside log 0, a barrier in log 0) are passed over; a keyed entry is applied in
  whatever log holds it, whatever its key routes to.
- **Leaders spread.** Log `k` prefers the voter ranked `k`, which advertises the least measurable
  quorum round trip there; priority elections and transfers move each log to it.
- **n = 1** "is today's single log: no barriers, and the merge is log 0 in order".
- **Explorer** (`tests/multilog.rs`): 200 seeds × 3,000 steps at full scale (16 in the workspace
  run), three voters × three logs and five × two, adversarial and calm stretches of 200 steps, a bag
  of 512 messages, 96 proposals over 12 keys, one in six global; Raft's invariants per log and the
  merge's history across nodes and restarts. Measured: 99,499 and 62,858 keyed commands applied,
  18,483 and 13,180 global, 3,398 and 1,142 barriers, 9,714 and 9,597 crash-restarts, 46,551 and
  20,077 replays matched; a global applied without waiting for barriers is caught at seed 0, step
  1,072.
- **Timed** (`tests/multilog_timed.rs`): five Azure regions, a keyed stream of 20 a second over 64
  keys and a global stream of two a second; each log's preferred voter crashed in turn. Measured
  (20 seeds; 1 / 2 / 3 / 5 logs): steady keyed medians 174, 298, 301, 302 ms; global 199, 785, 786,
  839 ms; messages 107,633 to 538,144; log 0's leader's crash pauses the keyed stream 4,484 ms with
  one log and 428–498 ms with more; another log's leader's crash stalls every log's keyed commands
  3,713–6,354 ms; a keyed command's expectation as a region is lost, 1,036 ms with one log and
  1,301–1,399 ms with more. Decided: both of slates' groups keep one log.
- **Owed:** compaction across logs (§6).

**What the design here changes, each for a reason `docs/multilog.md` states:** the merge reads each
log where its storage holds it, copying nothing; a barrier is passed once log 0's merge has consumed
the index it names, which equals slates' rule for a barrier naming a global and cannot hold its log
forever when the entry there is no global; an entry out of place, a keyed entry whose key routes to
another log among them, is refused alike on every member and reported, never applied (applied, a
misrouted key's commands would not commute); a leader refuses a forwarded proposal that only the
layer may make (a barrier) or that is out of place; a leader counts the barriers its merge has seen
before it appends another; changes of configuration are applied as given, not as merged;
compaction is possible at the cuts §6 asks for; and what a log may hold beyond its merge is
bounded, with a typed refusal at its leader.
