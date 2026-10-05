# Research: deterministic simulation and checking

Source notes for `docs/sim.md`, the design of `hyper-sim` and `hyper-check`. Each entry says what
the source establishes and what it leaves open. Papers were read as PDFs (text extracted with
`pdftotext`) on 2026-10-02; section numbers are the papers' own. Code and project documents were
read at the revision named. Where an entry rests on another note's reading rather than a reading
made for this note, it says so, and it names the note.

Tags: **[PR]** peer-reviewed; **[PS]** primary source (a project's own code or documentation);
**[VD]** vendor documentation; **[via]** read for another note, not re-read here.

## 1. Deterministic simulation of whole systems

**[PR] Zhou et al., "FoundationDB: A Distributed Unbundled Transactional Key Value Store",
SIGMOD 2021, doi:10.1145/3448016.3457559** (<https://www.foundationdb.org/files/fdb-paper.pdf>).

- **§4, the simulator.** "The real database software is run, together with randomized synthetic
  workloads and fault injection, in a deterministic discrete-event simulation." "All database code
  is deterministic; accordingly multithreaded concurrency is avoided (instead, one database node is
  deployed per core)." "All sources of nondeterminism and communication are abstracted, including
  network, disk, time, and pseudo random number generator." "The production implementation is a
  simple shim to the relevant system calls."
- **§4, oracles.** Workload assertions "checking invariants in their data that can only be
  maintained through transaction atomicity and isolation"; local assertions throughout; and
  recoverability: "returning the modeled hardware environment ... to a state in which recovery
  should be possible and verifying that the cluster eventually recovers."
- **§4, faults.** "Machine, rack, and data-center level fail-stop failures and reboots, a variety of
  network faults, partitions, and latency problems, disk behavior (e.g. the corruption of
  unsynchronized writes when machines reboot), and randomizes event times." "Fault injection
  distributions are carefully tuned to avoid driving the system into a small state-space caused by
  an excessive fault rate."
- **§4, buggification.** The code gives the simulation "the opportunity to inject some unusual (but
  not contract-breaking) behavior such as unnecessarily returning an error from an operation that
  usually succeeds, injecting a delay in an operation that is usually fast, choosing an unusual value
  for a tuning parameter". "Randomization of tuning parameters also ensures that specific
  performance tuning values do not accidentally become necessary for correctness."
- **§4, swarm testing.** "Each run uses a random cluster size and configuration, random workloads,
  random fault injection parameters, random tuning parameters, and enables and disables a different
  random subset of buggification points."
- **§4, coverage.** "Conditional coverage macros": `TEST( buffer.is_full() );` reports how many
  distinct runs reached the condition; "if the number is too low, or zero, they can add
  buggification, workload, or fault injection functionality".
- **§4, speed.** The simulator "can fast-forward clock to the next event"; runs are "embarrassingly
  parallel".
- **§4, limitations.** "Simulation is not able to reliably detect performance issues ... It is also
  unable to test third-party libraries or dependencies"; "several bugs have resulted from the true
  operating system contract being weaker than it was believed to be."
- **§6.2.** Logging "generally does not affect the deterministic ordering of events, so an exact
  reproduction is guaranteed"; a bug found in the wild is first reproduced by improving the
  simulator; CloudKit ran "more than 0.5M disk years without a single data corruption event".
- **Left open.** No scale figures (runs, CPU-hours), no statement of how fault rates are tuned, no
  guarantee on what a number of runs covers.

**[PS] TigerBeetle, the VOPR**, at `tigerbeetle/tigerbeetle` `6f8e6b5`: `docs/internals/vopr.md`,
`src/vopr.zig`, `src/testing/storage.zig`, `src/testing/packet_simulator.zig`.

- **`vopr.md`.** "All non-deterministic parts of the system are stubbed out. This includes the
  clock, network, and disk operations." A failure replays from "the seed and Git commit hash".
  Assertions stay on in production; checkers verify that "replicas' data files are designed to be
  byte-for-byte identical across caught-up nodes".
- **`vopr.zig`, swarm and two phases.** `options_swarm` draws the replica count, standbys, clients,
  batch limits, storage size and cache sizes per seed. A run is a safety phase with faults, then
  `transition_to_liveness_mode(core)`: "a core set of replicas is up and fully connected. The rest of
  the replicas might be crashed or partitioned permanently. The core should converge to the same
  state", within `ticks_max_convergence`. When the safety phase ran out of ticks the core is the
  whole cluster, "to repair all faulty grid blocks, headers, and prepares that can be repaired".
- **`packet_simulator.zig`.** Partition modes `uniform_size`, `uniform_partition`,
  `isolate_single`, symmetric or asymmetric, each with a probability of starting and of healing and
  a stability time; packet loss and replay probabilities; one-way delay with a minimum and a mean.
- **`storage.zig`, the fault model.** Read and write latency (minimum and mean); read corruption,
  write corruption, **misdirected writes** ("each misdirected write creates two overlays"; at most
  one a storage, within one zone, the whole write) and crash corruption of a pending write's target.
  Faults are placed by a `ClusterFaultAtlas` "to ensure that at least one replica will have a valid
  copy to help others repair"; with one replica its log "can only be corrupted by a crash".
- **Left open.** No published account of what the VOPR's runs cover, beyond the claim that a minute
  of VOPR time "is equivalent to days of real-world testing".

**[VD] Antithesis, documentation** (<https://antithesis.com/docs/>, pages "How Antithesis works",
"Asserting correctness", "Controlling faults", read 2026-10-02).

- Faults: network (latency, partitions, clogs), node (pause, kill, throttling), clock
  ("forward/backward clock skips"), thread pausing, CPU modulation. Faults can be paused for a quiet
  period, and "eventually or finally" commands give "a terminal pause at the end of a timeline,
  giving the system under test time to recover before final validation checks".
- The "multiverse": "each event potentially starts a new timeline"; a run is a tree of branching
  timelines, and a guidance component "drives the platform to seek out new and interesting states".
- Assertions: `always`, `alwaysOrUnreachable`, `reachable`/`unreachable`, and `sometimes` ("a
  condition should be true at least once per test run ... useful for checking whether tests
  exercise meaningful scenarios"). Assertions "provide hints to our platform about which states
  should be explored".
- **Left open.** The guidance algorithm is not published; the deterministic hypervisor is described
  in marketing material, not in a paper. Vendor claims about Raft bugs are UNVERIFIED (mantle note
  06 A10.2 [via]).

**[PR] Deligiannis et al., "Uncovering Bugs in Distributed Storage Systems during Testing (not in
Production!)", FAST 2016.**

- **§1–§2.** P# runs the real system's components as machines with "all sources of nondeterminism"
  declared, and "systematically exercises" interleavings of their events; specifications are
  safety monitors and liveness monitors.
- **§2.5, liveness.** A liveness monitor has hot and cold states; "an infinite execution is
  erroneous if the liveness monitor is in the hot state for an infinitely long period of time", and
  the tool approximates it: "we consider an execution longer than a large user-supplied bound as an
  'infinite' execution".
- **§6, Table 2.** 100,000 executions per bug. Of eleven MigratingTable bugs the default harness
  caught seven and custom test cases the other four; "the random scheduler only managed to trigger
  seven of the MigratingTable bugs; we had to use the priority-based scheduler to trigger the
  remaining four bugs". A liveness bug in Azure Storage vNext had appeared "intermittently ... during stress
  testing for months".
- **Left open.** How to choose the bound that stands for an infinite execution.

## 2. Schedules with guarantees, and how deep real bugs are

**[PR] Burckhardt, Kothari, Musuvathi, Nagarakatte, "A Randomized Scheduler with Probabilistic
Guarantees of Finding Bugs", ASPLOS 2010** (PCT).

- **§2.** "The depth of a concurrency bug" is "the minimum number of ordering constraints" that
  reliably reveal it; ordering bugs have depth 1, atomicity violations and two-lock deadlocks
  "in general, have depth 2".
- **§2, the algorithm.** "Assign the n priority values d, d+1, ..., d+n randomly to the n threads";
  "pick d − 1 random priority change points k1, ..., kd−1 in the range [1, k]"; "schedule the threads
  by honoring their priorities", lowering a thread to priority i at change point ki.
- **§3, Theorem 9 (the coverage theorem).** "Given a program that creates at most n threads and
  executes at most k instructions, PCT finds a bug of depth d with probability at least
  1/(n·k^(d−1))." The bound "is also tight", and in practice is exceeded because a bug can usually
  be found several ways.
- **Left open.** Threads with shared memory, not message passing; `k` is every step.

**[PR] Ozkan, Majumdar, Niksic, Befrouei, Weissenbacher, "Randomized Testing of Distributed
Systems with Probabilistic Guarantees", OOPSLA 2018 (PACMPL 2(OOPSLA):160).** *Abstract only; the
full text was not reachable (ACM's PDF refused the fetch).* The abstract states a randomized
scheduler for "arbitrary partially ordered sets of events revealed online": for partial orders of
width at most `w` and size at most `n` it "discovers a bug of depth d with probability at least
1/(w²·n^(d−1))", implemented for message-passing programs and run on ZooKeeper and Cassandra. The
design uses this bound; reading the paper's construction (chain partitioning) is an open item.

**[PR] Leesatapornwongsa et al., "TaxDC: A Taxonomy of Non-Deterministic Concurrency Bugs in
Datacenter Distributed Systems", ASPLOS 2016.** 104 bugs from Cassandra, HBase, Hadoop MapReduce
and ZooKeeper.

- **Finding #1.** Triggered "mostly by untimely messages (64%) and sometimes by untimely
  faults/reboots (32%), and occasionally by a combination of both (4%)".
- **Finding #3.** "The timing conditions of most DC bugs only involve one to three messages, nodes,
  and protocols (>90%)"; "most DC bugs are mostly triggered by only one untimely event (92%)".
- **§1.** "63% of DC bugs surface in the presence of hardware faults such as machine crashes (and
  reboots), network delay and partition (timeouts), and disk errors"; "47% of DC bugs lead to silent
  failures".

**[PR] Yuan et al., "Simple Testing Can Prevent Most Critical Failures", OSDI 2014.** 198 failures
from Cassandra, HBase, HDFS, MapReduce and Redis. **Finding 3:** "almost all (98%) of the failures
are guaranteed to manifest on no more than 3 nodes. 84% will manifest on no more than 2 nodes."
**§1:** "77% of the failures can be reproduced by a unit test"; 92% of catastrophic failures come
from incorrect handling of non-fatal errors.

**[PR] Lukman et al., "FlyMC: Highly Scalable Testing of Complex Interleavings in Distributed
Systems", EuroSys 2019**, read as chapter 4 of Lukman's dissertation (University of Chicago, 2020,
<https://knowledge.uchicago.edu/record/2603>), which reprints it.

- **§4.1.** Three algorithms: communication and state symmetry ("the state transitions of such
  symmetrical nodes usually depend solely on the order and content of messages, irrespective of the
  node IDs"), event independence ("concurrent messages that update disjoint sets of variables" and
  crash events), and parallel flips ("simultaneous reorderings of concurrent messages across
  different nodes").
- **Result.** "On average 16× (up to 78×) faster than other state-of-the-art systematic and
  random-based approaches"; 12 known bugs reproduced and 10 new bugs found "without random walks".
  Figure 4.8: "Many choices make random techniques ineffective."
- **Left open.** Symmetry and independence are declared from static analysis of Java systems; the
  reduction's soundness depends on that analysis.

**[PR] Groce, Zhang, Eide, Chen, Regehr, "Swarm Testing", ISSTA 2012.**

- **Abstract, §1–§2.** "The usual practice of potentially including all features in every test case
  is abandoned. Rather, a large 'swarm' of randomly generated configurations, each of which omits
  some features, is used". Two mechanisms: "some features actively prevent the system from executing
  interesting behaviors" (pops keep a stack from overflowing), and "test features compete for space
  in each test". A week of swarm testing "found 42% more distinct ways to crash a collection of C
  compilers" than the hand-tuned default configuration.

**[PR] Bano, Sonnino, Chursin, Perelman, Li, Ching, Malkhi, "Twins: BFT Systems Made Robust",
arXiv:2004.10617v2** (LIPIcs format; the published version was not checked).

- **§2–§3.** A faulty node is two (or `k`) instances with one identity running correct code;
  equivocation and **amnesia** ("vote for a proposal and then 'forget' that it has voted") arise
  from partitions between the twins, not from coded misbehaviour.
- **§4.2.** Scenarios are generated per round: the partitions of the nodes (Stirling numbers of the
  second kind), a leader for each, arranged over `R` rounds; "a small number of rounds (< 10)
  suffices to expose logical bugs"; duplicates under symmetry are pruned, and "it suffices to play
  with two or three partitions per round". For liveness, "the scenario generator must guarantee that
  eventually such a quorum exists".
- **§6.1.** Injected bugs (mutation testing) validate the generator: with 4 nodes and 2 twins over 7
  rounds, 62 scenarios found 8 safety violations in 86 s.
- **For a crash-fault protocol** (inference, not the paper's): amnesia is a member whose durable
  state was lost or rolled back, which Raft forbids (thesis §3.8, `docs/research/durable.md` §1) and
  which repair (R-5, R-7) must detect; a twin is the way to produce one on purpose.

## 3. Guided exploration

**[PR] Gulcan, Ozkan, Majumdar, Nagendra, "Model-Guided Fuzzing of Distributed Systems",
arXiv:2410.02307v3** (OOPSLA 2025, PACMPL 9(OOPSLA2):274).

- **§3.2, Algorithm 1.** A coverage-guided fuzzer over *schedules*: each executed schedule's events
  are mapped to actions of an abstract model (a developer-written map), the model is run on them,
  and the abstract states reached are the coverage; schedules reaching new states get energy and
  are mutated.
- **§3.2, feasible mutation.** A schedule is a sequence of `⟨buffer : action⟩`, actions deliver(n),
  crash, restart, naming *which buffer* rather than which message, so a mutated schedule stays
  feasible; mutations are SwapBuffers, SwapCrashProcesses, SwapMaxMessages.
- **§3.3.** Line coverage "can ignore the orderings of message interactions", and message traces
  "may provide too many coverage goals"; abstract model states sit between.
- **Abstract.** On etcd-raft and RedisRaft, consistently higher coverage than random and than
  scheduler-coverage guidance; "12 previously unknown bugs ..., four of which could only be detected
  by model-guided fuzzing" (v3; mantle note 06 A10.2 [via] reports 13 from the published version).

**[PR] Meng, Pîrlea, Roychoudhury, Sergey, "Greybox Fuzzing of Distributed Systems" (Mallory),
arXiv:2305.02601v3** (CCS 2023). Abstract and §1: Lamport timelines abstracted into
"happens-before summaries" are the feedback; a Q-learning policy chooses faults; against Jepsen,
"54.27% more distinct states within 24 hours", bugs found 1.87× faster, 22 previously unknown bugs
in Braft, Dqlite, Redis and others.

**[PR, via] Mocket (Wang et al., EuroSys 2023) and SandTable (Tang et al., EuroSys 2024)**, as
mantle note 06 A10.2 records them: TLC's state space drives tests of the implementation; Mocket
found bugs in the official Raft TLA+ specification itself, SandTable 23 bugs (18 new) in eight
Raft- and Zab-based systems. Not re-read here.

## 4. Crash consistency and storage faults

**[PR] Pillai et al., "All File Systems Are Not Created Equal: On the Complexity of Crafting
Crash-Consistent Applications", OSDI 2014** (ALICE).

- **§2.2.** Persistence properties are of atomicity (single-sector, single-block, multi-block
  appends and writes, rename) and ordering ("Append → Append (same file)", "[Append, rename] → any
  op"), and file systems differ on every one (Table 1).
- **§3.2.2, abstract persistence models.** An APM "specifies all constraints on the atomicity and
  ordering of logical operations", splitting each into micro-operations (block writes, size changes,
  directory-entry creation and deletion). The default APM orders every operation after an `fsync` of
  its file, and an append may land with its size before its data ("blocks are filled with random
  data").
- **§3.3, the states explored.** For each prefix of the system calls (atomicity across calls); for
  each call, its intermediate states (atomicity of a call, appends and writes split into blocks and
  into three parts); for each pair, every call before B except A (ordering).
- **§4.** 60 vulnerabilities across eleven applications, among them ZooKeeper's and LevelDB's logs.

**[PR] Mohan, Martinez, Ponnapalli, Raju, Chidambaram, "Finding Crash-Consistency Bugs with Bounded
Black-Box Crash Testing", OSDI 2018** (CrashMonkey, ACE).

- **§3, the study.** "24 out of the 26 reported bugs require three or fewer core file-system
  operations to reproduce on an empty file system"; "all reported bugs involved a crash right after
  a persistence point: a call to fsync(), fdatasync(), or the global sync".
- **§4, B3.** Exhaustive within bounds, crashing "only after each persistence point": "if a
  file-system operation translates to n block IO requests, there could be 2^n different on-disk
  crash states if we crashed anywhere during the operation. Restricting crashes to occur after
  persistence points bounds this space linearly". Only what was explicitly persisted is checked.
- 10 new bugs in mature Linux file systems, seven present since 2014.

**[PR] Ganesan, Alagappan, Arpaci-Dusseau, Arpaci-Dusseau, "Redundancy Does Not Imply Fault
Tolerance", FAST 2017.** **§3.1, the fault model:** "exactly a single fault to a single file-system
block in a single node at a time", in application-level structures only. **Table 1:** corruption to
zeros or junk on read (from "misdirected and lost writes"), EIO on read (latent sector errors) and
on write, ENOSPC and EDQUOT. A block marked corrupt that is rewritten reads back the new data.
`docs/research/durable.md` §5 records what this paper found of Raft-based systems.

**[PR] Rebello, Patel, Alagappan, Arpaci-Dusseau, Arpaci-Dusseau, "Can Applications Recover from
fsync Failures?", ATC 2020.** After a failed `fsync`, ext4, XFS and Btrfs mark the pages clean;
ext4 and XFS keep the new contents in memory while Btrfs reverts; applications "can only assume
that the underlying file system experienced a fault and that data may have either been persisted
partially, completely, or not at all". hyper-block's simulated file models this already
(`crates/hyper-block/src/sim.rs`, `Fault::SyncError`).

## 5. Linearizability and histories

Re-read for S-4 on 2026-10-04 (the PDFs fetched from the URLs mantle note 06 A6 names, SHA-256 in
`docs/sim.md` §14.9; Wing and Gong's is a scan, read from page images). What each establishes for
`hyper-check`:

**[PR] Herlihy, Wing, "Linearizability", TOPLAS 12(3), 1990** (`cs.brown.edu/~mph/HerlihyW90/p463-herlihy.pdf`).
§2.2: "A history H is linearizable if it can be extended (by appending zero or more response events)
to some history H′ such that: L1: complete(H′) is equivalent to some legal sequential history S,
and L2: <H ⊆ <S." "Extending H to H′ captures the notion that some pending invocations may have
taken effect even though their responses have not yet been returned to the caller. Restricting
attention to complete(H′) captures the notion that the remaining pending invocations have not yet
had an effect." This is the definition `history::verify` applies to every order a checker
exhibits, and the treatment of an operation that never returned (in the order or not). §3.1,
Theorem 1: "H is linearizable if and only if, for each object x, H|x is linearizable": the
partitions of `search_partitions` and the witness checker's per-object orders.

**[PR] Wing, Gong, "Testing and Verifying Concurrent Objects", JPDC 17, 1993**
(`cs.cmu.edu/~wing/publications/WingGong93.pdf`, pp. 170–173 read from page images, no longer
[via]). §4: "we try every possible sequential order of H's concurrent operations while preserving
its real-time order relation <H". §4.1: the history "is stored in a doubly linked list of events"
with a `match` pointer from an invocation to its response and a sentinel at the end (Fig. 3);
`search` keeps a stack of the operations linearized; `lift` "temporarily removes" an operation's
events and `unlift` puts them back "if we later backtrack" (Figs. 4–6). §4.2 argues correctness;
§4.3: "There exists simple data types and histories for which testing linearizability is
NP-complete", "analyzing many short (100 operations) histories is tractable", and "a better chance
of finding a nonlinearizable history by testing many short histories rather than testing one long
one".

**[PR] Lowe, "Testing for linearizability", CCPE 29(4), 2017** (author's preprint). Verified:
- **§3**: the tree search (Figure 1), an operation "minimal in a given history if no return event of
  another operation is before the call of op"; its debugging extension "prints the maximum
  linearizable prefix, and the following event, necessarily a return event, and the alternative
  values that could have been returned at this point" — `search::Counterexample`.
- **§3.1**: the memo of configurations, "the same operations have been linearized and the sequential
  object is in the same state", needing an immutable specification object.
- **§4**: the specification automaton of configurations `(s, calls, rets)`; Lemma 4 (a lin event
  commutes with a call or a return of another thread) and Lemma 6 ("If a recorded history has a
  linearization, then it has a just-in-time linearization"); the bound `(N+1)·2^p·(p+1)` for a
  register, "linear in the length of the history; it is exponential in the number of threads"; a
  queue exponential in the worst case.
- **§6**: on a map "the two [graph search] algorithms have similar behaviour ... with the JIT
  Algorithm performing better on longer runs. We believe that this is because of the just-in-time
  linearization reducing the number of configurations"; WG graph search is "particularly heavy on
  memory for long runs, because it records the set of operations linearized in each stored
  configuration"; competition parallel, tree searches "fast but erratic".
- **§1, §8**: bugs are "normally ... discovered within 20 seconds, often less than a second".
  **§7.1**: per-thread logs merged afterwards, with a call stamped strictly before any return that
  observes it. **§9**: wall-clock stamps do not work across machines.

What follows from these for the design, as inference (`docs/sim.md` §14.3): WGL's search is over
orders, not over the placement of linearization points, so Lemma 6 shrinks no space WGL searches;
it gives the order of trying (the operation whose return comes first, before the others) and the
compact form of a configuration (a position and the operations linearized ahead of it, where WGL
keeps a bit for every operation). A WGL configuration's linearized set is every operation returned
before the first return left in the list and some of those pending there, so the memo's
configurations are Lowe's and his register bound holds for them; a test holds it
(`a_registers_configurations_are_within_lowes_bound`).

**[PR] Horn, Kroening, "Faster Linearizability Checking via P-Compositionality", FORTE 2015,
arXiv:1504.00204v1.** Verified: Definition 5 considers complete histories; Definition 6
(P-compositionality) and Example 7 (a set or map by key); Algorithm 1 (WGL: at a call entry,
`apply`, and if legal and ⟨linearized′, s′⟩ is new push, set the bit, lift and restart from the
head; at a return entry with an empty stack, "return false"; else pop, clear, unlift and continue
from the popped entry's next), Algorithm 2 (LIFT unlinks the call then its match; UNLIFT relinks
the match then the call), Algorithm 3 and Theorem 1 (partitions checked separately); §5.1, the
constant-time bitset hash as XOR "forms an abelian group", and LRU eviction as an option; Table 1
(TBB: WGL 101 s, 9,792 MiB; WGL+P 6 s, 672 MiB; WGL+LRU 11 s, 670 MiB with 14 % and 9 % of the
LSL and OPTIMIST runs timing out).

**[PR] Knuth, The Art of Computer Programming, Vol. 3, 2nd ed., 1998, §6.4** [via the standard
statement; the book was not re-read here]: linear probing at load α takes about `½(1 + 1/(1−α))`
probes for a successful search and `½(1 + 1/(1−α)²)` for an unsuccessful one, which an insertion
of a new fingerprint is; the memo's load of ¾ (8.5 probes at its fullest, against 32.5 at ⅞) rests
on it (`docs/sim.md` §14.3).

**[PR, via] Gibbons, Korach, "Testing Shared Memories", SIAM J. Comput. 1997:** NP-completeness in
general, as Lowe §1, Horn and Kroening §1 and Kingsbury and Alvaro §1 report it; not accessed.

**[PS, via] Porcupine and Knossos**, as mantle note 06 A6.6 records them: Porcupine implements
Horn and Kroening's partitioned WGL, treats `[call, return]` as closed, gives a crashed operation a
synthetic return at the end that accepts any result; Knossos marks a timed-out process "crashed",
never to operate again.

**[PR] Kingsbury, Alvaro, "Elle: Inferring Isolation Anomalies from Experimental Observations",
PVLDB 14(3), 2020.** §1: strict serializability is linearizability over a map, so a linearizability
checker applies, limited "by the NP-complete nature" of the problem. §2.1, §3: traceable objects
(list-append with unique values) let the checker recover the version order and build a dependency
graph; cycle detection is linear. §6.1: "detecting cycles are all linear-time operations"; Knossos
on 5,000 transactions with 40+ processes was "(generally) uncheckable in reasonable time".

**[PS] focal's witness checker**, `crates/focal-sim/src/history.rs` at focal `9549f4f`: "not a
black-box search for an arbitrary valid linearization: the simulator supplies ordered publication
events from the actual state owner". One pass over invoke, publish and complete events refuses a
publication that skips a sequence, a second commit of a request, success before its publication, a
response of another request, and a read that breaks its consistency level or its prefix's state
hash. It is linear in the events and bounded by a declared event budget.

## 6. Model-based and scripted interaction tests

**[PS] etcd-io/raft's interaction tests**, at `etcd-io/raft` `1c0011d`: `rafttest/interaction_env.go`,
`interaction_test.go`, `testdata/async_storage_writes.txt`. A test is a script of commands
(`add-nodes`, `campaign`, `deliver-msgs`, `process-ready`, `process-append-thread`,
`process-apply-thread`, `stabilize`, `tick-election`, `propose-conf-change`, ...) with the expected
log after each; `go test -rewrite` regenerates the expectations, and a diff is reviewed: "only commit
the changes if you understand what caused them". Asynchronous storage writes are explicit steps,
as hyper-raft's `Step::Take`, `Durable`, `Notify` are.

**[PS] The core's TLA+ model**, `docs/models/README.md` here: each action is mapped to the
functions of `crates/hyper-raft` that perform it (`Hold`, `Take`, `FastCommit`, `ClassicCommit`,
`Replicate`, `Elect`, ...). That map is the `map` Gulcan et al.'s Algorithm 1 asks the developer to
write.

## 6a. The generator

**[PR] Steele, Lea, Flood, "Fast Splittable Pseudorandom Number Generators", OOPSLA 2014,
doi:10.1145/2714064.2660195.** SplitMix: a state advanced by an odd gamma and each output mixed.
Read through its two primary-source implementations on 2026-10-02:
- **[PS] OpenJDK `java.util.SplittableRandom`** (`openjdk/jdk` master). `GOLDEN_GAMMA` =
  `0x9e3779b97f4a7c15`, "the golden ratio scaled to 64bits". `mix64` "computes Stafford variant 13
  of 64bit mix function": shifts 30, 27 and 31, multipliers `0xbf58476d1ce4e5b9` and
  `0x94d049bb133111eb`.
- **[PS] Vigna, `splitmix64.c`** (<https://prng.di.unimi.it/splitmix64.c>, 2015). "A fixed-increment
  version of Java 8's SplittableRandom generator", with the same constants and a citation of the
  paper's DOI.

These are the constants of focal's `Seeded`, of hyper-raft's and hyper-durable's harnesses and of
`hyper-sim`. The finalizer is a bijection of `u64`, which `hyper-sim` uses to derive each stream's
first state and to fold the digest. Two streams of one gamma overlap only if their first states
differ by a small multiple of the gamma, so two streams of `L` draws each overlap with probability
about `2L / 2⁶⁴` (inference from the construction, not a statement of the sources).

## 7. The projects' own evidence about their harnesses

- **slates `docs/bugs/2026-09-25-simulated-parks-read-the-host-clock.md`** (slates `ec5e0df`): a
  simulated shard read the host clock on every park; Miri (no `CLOCK_BOOTTIME`) and callgrind
  (a vDSO fault) caught it, not the simulation. The rule it broke: "the simulation makes no OS call".
- **slates `docs/bugs/2026-09-30-a-simulation-freed-its-clock-under-a-protected-reference.md`**: the
  simulation's clock was held as a `&'static` field and freed by its owner's `Drop`; Miri reported
  undefined behaviour. The clock is now lent for a borrow.
- **slates `crates/cluster/tests/support/exhaustive.rs`**: a search holding whole keys took 18 GB
  after 300 s without finishing; fingerprints of 128 bits (collision below `n²/2¹²⁹`) and a memory
  ceiling derived from measured bytes per state replaced it; breadth-first and depth-first visited
  the same 463,715 classes in 1.13 s and 1.16 s, and the parallel search in 0.16 s (2026-09-28).
- **slates `crates/cluster/tests/explore.rs`**: without calm stretches "a five-voter history
  committed almost nothing (108 entries over 400 seeds), which would leave the checks vacuous".
- **slates `crates/cluster/tests/prefix_model.rs`**: two rejected rules for dropping slots were
  found by the explorer at full scale (18 steps) and by the exhaustive model (12 steps), each kept
  as a `Variant` the model must refuse.
- **hyper-raft `docs/raft.md` §3**: the fast track's two election defects were found by random
  schedules at 40,000 seeds (seeds 9843 and 54104), not at the default 96, and the TLA+ model missed
  both because it lacked a step and a change of configuration.
- **mantle `docs/research/06-consensus-and-metadata.md` B6 [via]**: madsim's `power_fail` and
  `sync_all` are no-ops; turmoil's durable-versus-pending file model is behind an unstable feature;
  stateright's linearizability tester has no memo. No outside crate gives what §4 above asks.
- **slates `crates/cluster/tests/prefix_model.rs`, measured again here (S-5, 2026-10-04)**: its
  representative of a class is not one key an orbit. An append that cuts a log leaves the cut
  entries in the array past the log's length, and the signature that sorts members reads every
  place of the array; a state with such a stale tail can sort its members apart from the same
  state without one, and its orbit then takes a second key. Built again in hyper-check with the
  tail cleared, the design reaches 21,771,580 classes at (3, 3, 1, 3) against slates' 21,776,022,
  12,551,719 at (4, 2, 2, 3) against 12,559,351, and 3,400,472 at (3, 2, 2, 4) against 3,401,082;
  with the tail kept as slates keeps it, every count is slates' exactly, and so is every path's
  count where both run (the 49,654 recoveries from a log of the rejected variant that reports logs
  too). The transitions are the same; only the keys differ. No verdict changes: the extra keys
  re-visit states already visited. At (3, 3, 1, 2) no stale tail splits a class (1,951,672 both
  ways). The slot model has no such field and reaches slates' counts exactly (463,715 and the rest).

## 8. Sources S-5 adds

Cited for the algorithms as their authors state them; not re-read for S-5.

- **[PR] Zeller, Hildebrandt, "Simplifying and Isolating Failure-Inducing Input", IEEE TSE 28(2),
  2002** (delta debugging, `ddmin`): a failing input reduced by removing subsets of a granularity
  that is refined while no removal fails, ending in a 1-minimal input (no single element can be
  removed with the test still failing). `hyper_check::strategy::tape::shrink` removes chunks of
  steps, halving them, then single steps to a fixed point: the complement steps of `ddmin`.
- **[PR] Knuth, *The Art of Computer Programming* Vol. 2, §3.4.2, Algorithm P** (the shuffle of
  Fisher and Yates, as Durstenfeld gave it): each of the `n!` orders with equal probability from
  `n − 1` uniform draws. `hyper_check::strategy::pct` draws PCT's first priorities by it.
- **[PR] Lemire, "Fast Random Integer Generation in an Interval", ACM TOMACS 29(1), 2019**: the
  high half of the product of a 64-bit word and a bound `b` maps a uniform word to `[0, b)` with
  a bias below `b/2⁶⁴` without the rejection step. hyper-raft's schedule harness has always drawn
  so (`support::Seeded::below`), and the decision tape reads its words the same way, so a tape
  recorded from a seed replays the seed's run.

