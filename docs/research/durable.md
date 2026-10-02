# Research: the durable shell around a Raft core

Source notes for `docs/durable.md`. Each entry says what the source establishes, verified against
its text on 2026-10-01 (papers read as PDFs, code read at the revision named), and what it leaves
open. Where a source is a project of ours, the entry names the file and revision read.

## 1. Raft: what must be durable, and when

**Ongaro, *Consensus: Bridging Theory and Practice*, PhD dissertation, Stanford, 2014.**

- **§3.3, RPCs.** "Raft assumes RPC requests and responses may be lost in the network; it is the
  requester's responsibility to retry", and it "does not assume the network preserves ordering
  between RPCs". A shell may therefore refuse an input as the network may drop it.
- **§3.8, persisted state.** "Each server persists its current term and vote; this is necessary to
  prevent the server from voting twice in the same term or replacing log entries from a newer
  leader with those from a deposed leader. Each server also persists new log entries before they
  are counted towards the entries' commitment." "Other state variables are safe to lose on a
  restart ... The most interesting example is the commit index, which can safely be reinitialized
  to zero on a restart." "A persistent state machine ... has already applied most entries after a
  restart; to avoid reapplying them, its last applied index must also be persistent." "If a server
  loses any of its persistent state, it cannot safely rejoin the cluster with its prior identity
  ... it can usually be added back into the cluster with a new identity by invoking a cluster
  membership change."
- **§4.1, configuration.** "The new configuration takes effect on each server as soon as it is
  added to that server's log ... each server always uses the latest configuration found in its
  log." The thesis's configuration is therefore durable with the entry; a configuration that
  takes effect only when applied (etcd's, and hyper-raft's) is not, which is what §4 of this note
  turns on.
- **§10.1, LogCabin's architecture.** One monitor with one lock, a thread per peer, service
  threads, a state machine thread, timer threads and a "log sync thread" that writes "without
  holding the lock on the consensus state, so replication to followers can proceed in parallel".
  Followers write "directly to disk from their service threads while holding the consensus lock".
- **§10.2.1, writing to the leader's disk in parallel.** "The leader can write to its disk in
  parallel with replicating to the followers and them writing to their disks ... the leader uses
  its own match index to indicate the latest entry to have been durably written to its disk. Once
  an entry in the leader's current term is covered by a majority of match indexes, the leader can
  advance its commit index." "The leader may even commit an entry before it has been written to
  its own disk, if a majority of followers have written it to their disks; this is still safe."
- **§10.2.2, batching and pipelining.** "Pipelining ... allowing one entry to start to be
  processed when another is in progress ... while a follower is writing the previous entry to
  disk, pipelining allows the leader to replicate the next entry over the network to that
  follower." "The AppendEntries consistency check guarantees that pipelining is safe; in fact, the
  leader can safely send entries in any order." The batching policy is left open ("we are still
  investigating the best policy"); LogCabin's 1 MB batch limit "is arbitrary".
- **Left open.** How a single-threaded owner of many groups overlaps writes with the protocol;
  what a member may act on before its commit is durable; storage faults (the model is crash-stop
  with durable storage).

## 2. Group commit

**DeWitt, Katz, Olken, Shapiro, Stonebraker, Wood, "Implementation Techniques for Main Memory
Database Systems", SIGMOD 1984.**

- **§5.2, pre-commit and the commit group.** A transaction's commit record goes into the log
  buffer and it "releases all locks without waiting for the commit record to be written to disk.
  The transaction is delayed from committing until its commit record actually appears on disk. The
  'user' is not notified that the transaction has committed until this event has occurred."
  Dependent transactions may read pre-committed data, safely "as long as the pre-committed
  transaction actually commits before its dependent transactions", which sequential log writes
  guarantee. "The transactions with commit records on the same log page are committed as a group
  ... A single log I/O is incurred to commit all transactions within the group": with ten
  transactions a commit group, 100 → 1,000 transactions a second on a 10 ms log write.
- **What it establishes here.** Applying (making visible) before durability and answering only
  after it are separable, provided order is kept: the design's apply-before-local-durability rule
  (§3 of `docs/durable.md`) is a pre-commit whose "disk" is a quorum's. One flush for every group
  that submitted before it is the commit group (mantle `docs/research/03` §13 holds the same
  quotes and Helland et al.'s HPTS 1987 citation, whose text is paywalled and was not read).

## 3. Asynchronous storage writes in deployed Raft libraries

**etcd-io/raft, `doc.go`, `rawnode.go`, `log.go` (main, fetched 2026-10-01).**

- **Synchronous usage, steps 1–4.** Write hard state, entries and snapshot; "no messages be sent
  until the latest HardState has been persisted to disk, and all Entries written by any previous
  Ready batch (Messages may be sent while entries from the same batch are being persisted)"; the
  leader may write in parallel (thesis §10.2.1); apply; `Advance`. In this mode committed entries
  may be applied before they are locally stable: `maxAppliableIndex(allowUnstable)` returns
  `committed`, and `applyUnstableEntries()` is `!asyncStorageWrites` (`log.go`, `rawnode.go`).
- **"Usage with Asynchronous Storage Writes".** Storage work becomes messages: `MsgStorageAppend`
  to a `LocalAppendThread` (entries, hard state, snapshot) and `MsgStorageApply` to a
  `LocalApplyThread` (committed entries). "Messages to the same target must be reliably processed
  in order. Messages to different targets can be processed in any order." "Each local storage
  message carries a slice of response messages that must delivered after the corresponding
  storage write has been completed." In async mode only locally stable committed entries are given
  to apply (`maxAppliableIndex(false)` = `min(committed, unstable.offset − 1)`).
- **The ABA problem** (`newStorageAppendRespMsg`'s comment). Five members; B takes A's entries and
  begins writing them; C is elected and B takes C's entries at the same indexes; A is elected again
  and B takes A's original entries again; the first write completes. The `(index, term)` matches,
  but a later write still in the pipeline will overwrite storage, so the unstable log must not be
  truncated. etcd attaches the term the write was issued in and ignores a response whose term has
  changed, then truncates on any later response to keep liveness.
- **Implementation notes.** Membership "takes effect when its entry is applied, not when it is
  added to the log", which "introduces a problem when you try to remove a member from a two-member
  cluster: If one of the members dies before the other one receives the commit of the confchange
  entry, then the member cannot be removed any more since the cluster cannot make progress." This
  is the same liveness hole focal's F17 met (§4).

**etcd-io/raft PR #8, "raft: support asynchronous storage writes" (Nathan VanBenschoten; first
etcd-io/etcd#14627).**
- Design: storage work as messages; the unstable log "should remain true to its name. It should
  hold entries until they are stable and should not rely on an intermediate reliable cache"; leader
  and followers symmetric; a follower's append thread sends its `MsgAppResp` straight to the leader.
- Measured with rafttoy on three AWS m5.4xlarge, gp3 EBS, open loop: at 16 MB/s the basic pipeline
  averaged 13.2 ms, CockroachDB's then pipeline (parallel append and early acknowledgement) 7.3 ms,
  the async pipeline 5.2 ms (−29 %); saturation 30 MB/s against 52 MB/s (+73 %), the average append
  batch growing from 928 to 1,542 entries.

**CockroachDB PR #94165, "kv: integrate raft async storage writes" (merged 2023-02-03).**
- Log writes are initiated in the Raft loop; waiting on their durability is offloaded (Pebble's
  `ApplyNoSyncWait`), and a separate goroutine delivers each `MsgStorageAppend`'s responses when
  the fsync completes. kv0 on three n2-standard-32 across zones: average latency −7 % to −38 % and
  p99 −9 % to −44 % from 1,000 to 32,000 qps; +6 % average and +9 % p99 at 64,000 qps, "over-saturated
  and CPU bound, presumably because of the extra goroutine handoff".
- Prototype PR #87050: concurrency of log writers hurt, since concurrent syncs of Pebble's WAL
  coalesce on one thread: one syncing writer per log is the better shape.
- Issue #125266 and PR #129083: the commit index delivered by `MsgApp` is not reliable without
  feedback from the follower; CockroachDB now treats a commit change as `MustSync`, piggybacked on
  the entries' write.

**tikv/raft-rs (master, fetched 2026-10-01).**
- `RawNode::advance_append_async(rd)` then `on_persist_ready(number)`: readies are persisted in
  order, so a persist notice for one covers all before it. "It's still required that the updates
  can be read by raft from the `Storage` trait before calling `advance_append_async`": raft-rs
  moves entries out of the unstable log when the write is issued, not when it is durable, the
  opposite of etcd's choice.
- `RaftLog::maybe_persist(index, term)` guards ABA differently: the persisted index advances only
  if `index < first_update_index` (the unstable snapshot's index or offset) and storage's term at
  `index` matches. hyper-raft's `Log::maybe_persist` is this rule (`crates/hyper-raft/src/log.rs`).
- PR #537 (2024-03-28), "allow raft apply committed logs before they are persisted", with
  `max_apply_unpersisted_log_limit`: "As `committed` means more than quorum node are persisted ...
  it is safe to apply this log even if it is still not persisted." Consequences stated: applied may
  exceed persisted, and after a restart applied may exceed committed. PR #561 (2025-02-28) resets the
  limit to zero on demotion from leader: "we only enable `max_apply_unpersisted_log_limit` on raft
  leader ... to prevent potential corner cases."

**TiKV RFC 0112, "Apply Raft Log Before Persistence" (tikv/rfcs#112).**
- Motivation: one instance's slow disk dominates distributed transactions' latency; "waiting for
  the persistence of the current instance is unnecessary".
- Conditions: leader only; the applied entry's term equals the latest persisted term; a bounded gap
  between applied and persisted. `PrepareMerge` and `CompactLog` still wait for persistence, and
  the leader never compacts past its persisted index.
- Correctness after restart: "if applied raft log are not persisted, the initialized last index
  will fall behind the applied index, while the raft group's maximum last index must be bigger than
  the applied index. So this peer is impossible to become the leader", and it catches up by entries
  or snapshot. Unsafe recovery must account for an applied index past the last index.
- **Left open by all three.** A shared per-device log under many groups on one owner thread;
  storage faults; which applied effects a member acts on at its next start.

**TiKV thread pools (docs.pingcap.com, "Tune TiKV Thread Pool Performance").** With
`store-io-pool-size` non-zero "the Raftstore thread sends the logs to the StoreWriter thread",
which writes them with `fsync`; committed logs go to the apply pool. A bounded pool of writer
threads per store, not a thread per region.

## 4. Applying only on a commit the log holds

**focal F17 (focal `aa1f162`, `crates/focal-consensus/src/persistence.rs`; focal
`docs/archictecutre/27-consensus-roadmap-and-slates-port.md` §9).**
- The commit is volatile and rides the group's next record; a member that alone decides writes
  `commit = last` in the append that holds the entries (`sole_commit`); a commit no record carried
  for a whole owner period is written then and at `Drop`, one such write in flight, waited for by no
  one (`settle_commit`, `write_commit_behind`).
- Two exceptions found by SIGKILL inside the window: a configuration change is applied only once a
  write states the commit covering it (`fenced`, `after_advance`, `commit_durable`) — `cli_network`:
  "the founder removed its peer, both were stopped, and the founder did not come back"; and a
  control group applies nothing past the logged commit (`apply_on_written_commit`) — `cli_upgrade`:
  a host "killed and started below [the fence] published that it was ready before its group told it
  of the fence again".
- Measured (`cargo bench -p focal-consensus --bench commits`, APFS, three `F_FULLFSYNC` a group
  commit, three members on one disk): one voter 25.5 → 12.8 ms an entry and 2 → 1 flushes a commit;
  three voters 70.1 → 36.2 ms median, 18,092 → 28,880 entries a second pipelined.
- **What it establishes.** The rule is a consequence of §1's two facts together: the commit index
  may be lost at restart (thesis §3.8), and in a core whose configuration takes effect at apply
  (etcd's doc, above) the configuration then reverts with it. The thesis's own design (configuration
  effective on append, §4.1) does not have the hole; etcd documents it for two-member removal.

## 5. Storage faults and protocol-aware recovery

**Ganesan, Alagappan, Arpaci-Dusseau, Arpaci-Dusseau, "Redundancy Does Not Imply Fault Tolerance:
Analysis of Distributed Storage Reactions to Single Errors and Corruptions", FAST 2017.**
- §4.1.7 LogCabin: crashes on read, write and space errors and on a corrupted closed segment; in an
  open segment it "discard[s] ... the corrupted entry and all subsequent entries"; election keeps
  the node from leading, and it refetches from the leader.
- Observations (§4.2): #2 faults are often locally undetected, and "crashing is the most common local
  reaction"; failed operations are rarely retried. #3 "Redundancy is underutilized: A single fault
  can have disastrous cluster-wide effects". #4 "Crash and corruption handling are entangled ...
  On detecting a checksum mismatch due to corruption, all systems invariably run the crash recovery
  code", losing acknowledged data (Kafka truncates; RethinkDB rolls back a committed transaction);
  LogCabin and MongoDB "fetch inordinate amount of data" by discarding every entry after the
  corrupted one. #5 protocol subtleties spread loss: a Kafka node that truncated its log stays in
  the in-sync set and can lead.

**Alagappan, Ganesan, Luo, Alquraan, Al-Kiswany, Arpaci-Dusseau, Arpaci-Dusseau,
"Protocol-Aware Recovery for Consensus-Based Storage", FAST 2018 (CTRL).**
- §2 and Table 1: Crash, Truncate, DeleteRebuild, MarkNonVoting, Reconfigure and the naive leader
  restriction are each unsafe or unavailable in some of Figure 1's scenarios; Truncate loses
  committed data (Figure 2: a truncating node forms a majority with lagging nodes and is elected).
- §3.2: "if there exists at least one correct copy of a committed data item, it will be recovered
  or the system will wait for that item to be fixed"; an uncommitted faulty item is decided "as
  early as possible".
- §3.3.1: the metainfo (term, vote) "cannot be recovered from other nodes ... recovering metainfo
  obliviously from other nodes could violate safety", so CLSTORE keeps two local copies.
- §3.3.3: a persist record `p_i` per entry, appended as `write(e_i), write(p_i), fsync()`. A
  mismatch of `e_i` without `p_i` is a crash; with `p_i` and a later entry or record, a corruption;
  the last entry with its `p_i` present cannot be disentangled ("a fundamental limitation in
  log-based systems") and is marked faulty for distributed recovery to fix or discard.
- §3.3.4: identifiers (`⟨epoch, index⟩`, offset, checksum) are stored physically apart from the
  entries, since a misdirected write can corrupt both if adjacent.
- §3.4: the naive restriction (no faulty node leads) is unavailable in Figure 4(b), where only a
  faulty node's log is up to date. CTRL lets it lead; the leader must fix its faulty entries before
  accepting commands, since out-of-order application is unsafe. Followers' faulty entries the
  leader knows are resent; ones it does not know are uncommitted and truncated (§3.4).
- §3.4.2–3.4.3: the leader asks followers about each faulty `⟨epoch, index⟩`: `have` (fix from it),
  `dontHave` from a majority (uncommitted: discard it and every later entry), `haveFaulty` (wait).
  Either decision may come first; a leader that fails mid-repair leaves only safe partial repairs.
- §3.5: snapshot chunks recovered from identical, leader-initiated snapshots.
- §5.2: overhead 8–10 % on HDD at 32 clients, at most 4 % on SSD; fixing one corrupted entry of
  30,000 took 1.2 ms and 7 KB in CTRL against 1.24 s and 32 MB in stock LogCabin.

**Rebello, Patel, Alagappan, Arpaci-Dusseau, Arpaci-Dusseau, "Can Applications Recover from fsync
Failures?", USENIX ATC 2020.** §3.3.4: ext4, XFS and Btrfs "mark the page clean even after fsync
fails" and none retries data or journal blocks; ext4 and XFS keep the new contents in memory, so a
read after the failure returns data the device may not hold. A failed flush says nothing about what
is durable; retrying it is meaningless.

## 6. Our three shells, as read

**mantle `crates/range` (origin/dev `1275bf8`), `docs/design/replica.md`, note 32 §2.3, §3.8.**
`Replica` drives focal-raft's core over hyper-log's `GroupLog`: `begin` gives out a leader's
messages and the confirmed reads, submits the update and applies what is committed, returning with
`persisting`; `drive` finishes. While a `Ready` flushes it holds messages (one flow-control window a
member) and ticks (at most `2·election_tick`); a `Ready` refused for room waits whole and the
replica refuses calls `Stalled`; a failed write fences it. Restart writes the engine's applied
index into the log's commit and finishes an install the log never recorded. Uncertainty marks with
vote judgment and a requested snapshot are its protocol-aware repair. Reads are confirmed one round
out at a time. Measured: an answered update waits for its frame's flush and the confirming record's
(4.33 → 8.65 ms p50, `docs/measurements/2026-09-29-log-confirmation.md`); on hyper-log, 72
allocations and 0.2 reallocations a committed entry, 69 µs at load 34 (hyper-raft
`docs/STATUS.md`). Read against §4: committed changes are applied on a volatile commit
(`Replica::apply`, `changed`), and `configuration_known` counts followers' volatile commit.

**focal `crates/focal-consensus` (focal `slates-port`, `7a6170e`).** `DurableNode` over a shared
WAL: one Ready out a group; every mutation refused `PersistencePending` while it is out; a drain
state machine (`Phase::{Start, Ready, Light}`); staging reserved from a memory budget before any
transition (`guarded_in`, `memory.rs`); the whole retained log in RAM (`RamLog`); `catch_unwind`
at the owner boundary; decoder floors; checkpoints; `sendable` and `wait_persisted` for early
leader sends; `notify_persisted` wakes a shared owner. Open in its own record: a group commit is
three device flushes (data, fence file, directory entry).

**slates `crates/cluster` and `crates/server` (`ec5e0df`).** The retained state (`SavedRaft`: term,
vote, the log above the snapshot, commit, the snapshot, the fast window, the synced term) is
re-encoded, BLAKE3-hashed and published to one of two alternating slots of the anchor process's
shared memory before any reply (`retention.rs`). Cost linear in the record, about 0.8 ns a byte:
70 µs at 1,000 entries, 3.1 ms at 50,000 (`docs/wip/BENCHMARKS.md`, 2026-10-01); bounded by the
thesis's compaction rule; delta publication measured and rejected at its sizes. Durable across a
daemon crash, not a power loss.
</content>
</invoke>
