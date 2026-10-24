# Status

What each crate is, what it has met of the law in CLAUDE.md §1 and §1a, and what each consumer
runs. Updated with every change to a crate's standing.

The law's four columns:
- **Wall**: the union lint wall passes.
- **Allocs**: allocations, reallocations and page faults measured and driven down on hot paths.
- **Bench**: benchmarked against each project's implementation it replaces.
- **E2E**: real processes on real sockets and disks.

| Crate | Source | Wall | Allocs | Bench | E2E | On `main` |
|---|---|---|---|---|---|---|
| hyper-raft | focal-raft `a8e95f7`, with history; the owner's decision (2026-10-01): core changes start here | yes | yes: 32 → 11 → 5 allocations per entry at batch 1, 0 reallocations, the owner giving each queue back as hyper-durable's replica does (`docs/benchmarks.md`, "The bytes an entry asks, traced") | yes: against focal `a8e95f7` and `1395e22`, slates `5cce86a` and raft-rs; behind slates on catch-up and snapshot, traced to raft-proto's message size; R-2 (its own types and wire format, `docs/raft.md` §3.1) is in, and the message is 160 bytes, not 280: 344 bytes asked per entry against slates' 3,368, every byte traced (was 7,064, then 3,800 and 4,184 with each ready's queue dropped; `docs/benchmarks.md`, "The message's layout") | yes: 3- and 5-member processes over UDP with fsynced logs; kill -9 of the leader, of a follower, and of all; partition. Since L-4 the members elect by suspicion on their own node-pair liveness streams, the partition is seen by every detector, a group whose devices all stall goes on once they do (`stalled-devices`), a member silent past what the members' own measures excuse fails a wait on every member's own facts, naming it, while a write the others can commit is waited for as they move (`member-stopped`), every report is held to the member's bookkeeping, and the test waits while the group moves, counting as quiet only time in which it saw the group (`docs/benchmarks.md`, "End to end"; the stall this closed, a shared disk holding every member's flush 1.8 s, in `docs/timing.md` §2.9). R-4 (readies ahead of their persistence) is in: allocation counts identical on every workload of the comparison, time per op within the noise of the synchronous path; the E2E members keep the synchronous path, each `Ready` written and flushed before the next is taken. R-6 (the durable commit in answers, the apply pause, a leader applying before its write is durable) is in: allocation counts identical on every workload, the raft-rs differential unchanged (`docs/benchmarks.md`, "The durable commit and the apply pause (R-6)"). Timing step L-2 (elections by suspicion: no ticks, idle groups woken for nothing, check-quorum from the detectors) is in beside the tick path, which is unchanged (`docs/timing.md` §2.9): allocations identical on all 22 cells, time and cycles within `main`'s intervals, an idle group 542.5 ns and two messages a tick on ticks against none by suspicion (`docs/benchmarks.md`, "Elections by suspicion (L-2)"). R-5 (a member whose log lost what it acknowledged repaired by its leader resending the lost entries; the mark is the core's) is in: one lost entry of 30,000 repaired in 32 µs and 1.8 KB against a snapshot's 8.9 ms and 30.7 MB in process (`docs/benchmarks.md`, "Repair by entries (R-5)"); the schedules with faults at rest lose nothing acknowledged. R-7 (a marked member elected on its log, without its own vote, by a quorum of the others that answer for no more than it holds) is in, with the TLA+ model's marked member: allocations identical on all 32 cells (`docs/benchmarks.md`, "A marked member's election (R-7)"). Found by mantle's D-1 (branch `mantle-d1`, `ORIGIN.md`): a campaign supersedes its own vote requests still waiting (234 requests after a hundred timeouts with three writes out, before), and a member keeps a spare message queue for each ready in flight (a follower's queue grown from four slots 176 times in 12,000 entries, before). R-3 (slates' enhancements, `docs/raft.md` §3.2) is done: R6, R7, R4, R5, R20 and R21 pass as slates' tests state them, with three defects at the end of what is counted and a lease its member's own timer kept fixed; the window a member is sent ahead of its answers is one rule from focal's and slates' (R16), slates' pipelining measurement reproduced on this core: at 2,000 proposals a second across five regions the rule commits every one at 125 / 127 ms where one batch out at a time falls behind (`docs/benchmarks.md`, "The window a member is sent ahead of its answers (R16)"); a member keeps what arrives ahead of a hole and its leader sends only the holes again (R17): on paths that reorder the group commits every proposal at 123 / 126 ms up to 4,000 a second, where it fell behind at 4,000 (`docs/benchmarks.md`, "What arrives ahead of a hole (R17)"); a learner is caught up in rounds before it is promoted (R13): slates' replay of the thesis's Figure 4.4(a), 45 rounds without a commit with the newcomer added directly against one round trip staged; a log is due for compaction by the thesis's rule, the shell's policy (R22): slates' finding replayed, three images to a voter a round behind at three compactions made the moment the majority held each round, none by the rule (`docs/benchmarks.md`, "When a log is compacted (R22)"); the simulation at 1,000 seeds a shape found a write made again that started the log past the state machine, in `main`'s shell, now fixed; and every bound is derived from what the member's owner states, its transport's largest message, its group's members and the bytes a queue may hold, where focal's literals and `MAX_MEMBERS` stood (`Limits::derive`). A read asked of a new leader waits for its term's first commit, as the thesis's §6.4 has it, where the core, as raft-rs, dropped it and its owner waited out a deadline; a read with no leader to ask is refused (focal's finding; `docs/raft.md` §3.3) | yes |
| hyper-timing | focal-timing `a8e95f7` with history, and slates' election law `5cce86a`; the detector's configurator and estimator, and the election law from them (L-1, new); a log-linear histogram of nanoseconds for owners' metrics (`docs/timing.md` §2.10, new) | yes | 0 allocations in every operation of a period and of the election law (was 1 a priority); a path read is a load (was three sorts); the detector's estimator 0 allocations, reallocations and faults a heartbeat and a configuration, 80–126 ns a heartbeat (`docs/benchmarks.md`, "The detector's estimator") | yes: against its own law before L-1, focal-timing and slates' timing, operation by operation; the per-period timing 3.8 ns against 8.4 and 8.7, even or ahead in each but a sample (28 ns against 2.3 and 3.4, the median window), a tail read (1.1 against 0.7, unexplained, under two cycles) and the span's search (306 ns, run once a detector configuration) | through hyper-swim and hyper-raft | yes |
| hyper-datagram | new, after slates' seal and node.md §3.4 | yes | 0 allocations, reallocations and faults a datagram, reordered ones too (`docs/benchmarks.md`) | yes: against slates' seal `5cce86a`, a tenth to a half of its cost a message with the same hardware crypto, 24 to 150 times less as slates builds on aarch64 | yes: two processes, UDP; replay and forgery from a third socket | yes |
| hyper-swim | slates' detector `5cce86a`; its timing measured, each pair an NFD-E detector (`docs/timing.md` §2.7); its view bounded by the owner's placement, a refusal typed past it, and a dead member forgotten once SWIM's dissemination window has passed; its Vivaldi engine Dabek's Fig. 3 in two dimensions and a height, every constant cited or derived (`docs/research/swim.md`); a probe states its prober's own state and an answer the answerer's suspicion or death of the prober; anti-entropy once a dissemination window, digests first and views pushed where they differ (Demers et al. §1.3, §1.5); no detection bound while a probe would go unjudged; unanswered measurement periods back off (RFC 6298 §5.5); a probe states its deadline, and a poll what it found, with the evidence | yes | 0 allocations, reallocations and faults a member a period, quiet or churning, 4 to 256 members, estimators, verdicts and the anti-entropy exchanges included (slates' was 6 to 14.5 and 8 to 13) | yes: against slates' detector `5cce86a`, a lower median a period at every point, 17 to 28 % less with the split closed (load 20.6–27.1); the directed gossip and the exchanges cost 5.4 to 20.7 % over `main` quiet and 9.3 to 40.9 % where a member refutes a suspicion every period, the exchanges themselves nothing measurable quiet (`docs/benchmarks.md`, "The split closed: the period"); the bounded view and the corrected engine had left the period within `main`'s spread or under it, and the findings leave it within the spread of the head before them | yes: five processes over hyper-datagram, SIGKILL detection under the configured detectors, one kill while the pools judge and one once every surviving pair is judged by its own estimator, every survivor within its stated bound, and every suspicion and condemnation traced, from the members' own records, to the detector's rule: its probe, the deadline stated as it was sent, and the answer that missed it or was lost (Theorem 7's allowance a report beside the counts, no test deciding by a statistical level); every wait ends on the members' stated detection bound, failing with their last lines; 2,000 runs of the one-kill form on macOS and on Linux at one, two and four CPUs with competing load, and 40 of the two-phase form on macOS at load 45–58, all passing; then 300 of the two-phase form with bounded waits at load 22–41, 298 passing: in the two, a live member's refutation missed one survivor, which forgot it. With the split closed (a probe states its prober, an answer what it holds against the prober, anti-entropy behind the rumors), the soak: 2,000 runs on macOS at load 27.6–95.2 and 500, 655 and 638 on Linux at one, two and four CPUs with busy loops, every one passing, no split and no overshoot, every survivor within 0.55 of its stated bound. With every finding traced: 2,000 runs on macOS and 454, 475 and 468 on Linux at one, two and four CPUs under ambient load, every one passing, 33,490 findings traced and every answer a live member's probe missed late, none lost (`docs/timing.md` §2.7; `docs/benchmarks.md`, "The cluster test") | yes |
| hyper-liveness | new (timing step L-3, `docs/timing.md` §2.8): one heartbeat stream per node pair on the datagram plane, shared by every group the pair shares, each heartbeat proving a durable flush, each pair an NFD-E detector configured from measurement, its mistakes bounded per heartbeat taken (a slot the sender skipped is the next one's lateness, not a loss; no configuration whose unavailability is one or more; `docs/timing.md` §2.2); shares hyper-timing's estimator, configurator, timer fold and `Exposure` with hyper-swim, its `G` from the waits its owner began before its wakes and that reached them, whatever ended them (`Liveness::on_wait`), each suspicion's bound from the run's best echo, stated once any heartbeat carried one, and a heartbeat sent no earlier than the one it echoes came (sent at an owner's `now` read before its socket, the echo's hold was clamped at zero and the peers' bounds fell short of the time measured, by 51.6 to 245 µs on Linux CI). Every link configures or is judged by what its node measured, the widest of its pool's and its configured links' (a peer dead in its links' first heartbeats suspected once a live link has its own evidence, not minutes later at its lengthened interval): its correlation measured online at its interval, a link refused for it waiting there for a longer level, the floor followed up at once and down once the flushes it is the mean of have doubled (a new log's first flushes no longer hold the interval), a moved interval expected, a peer never heard from judged from the attach, a restart reported (`Change::Restarted`) once: runs are ordered, a count the owner keeps durably and raises at every start, so a superseded run's heartbeat is stale; the owner told every trust that differs from what it was told; renewals derived, renewed once a configuration is as stale as its estimates are uncertain, the `β` rule gone (`docs/timing.md` §2.8–§2.9; a link that changes measured, the history kept against a sliding window, §3 item 11) | yes | 0 allocations, reallocations and faults a heartbeat sent or taken once configured, 2 to 8 nodes and a thousand groups a pair; 71 to 121 ns of its calls a heartbeat at load 8–14 against main's 76 to 150 before the per-arrival model (`docs/benchmarks.md`, "The detector model, at its causes"); 102 to 169 ns at load 30–48 before it, 2.3–2.7 % fewer instructions than `4769074` and 2.9–4.9 % fewer than `7a8812c`, the bootstrap a heartbeat 0.60–0.91 of L-3's with the owner's wake asks counted (`docs/benchmarks.md`, "The node's evidence, kept") | yes: against per-group heartbeats on the Raft core mantle and focal run and slates' detector, per node per second at one interval: flat in the groups (12 µs a second at two peers, 60 at eight, at 50 ms), ahead of per-group heartbeats from about 3 groups a node at two peers and 13 at eight (at one group a node they cost less: 3.5 µs), 18 times cheaper at 1,024; even with slates' detector; an idle node's one liveness flush an interval besides | yes: four processes over UDP on the sealed plane with kernel receive stamps, real files flushed in the block their system reports; a stalled disk and a SIGKILLed node each suspected by every other within its stated bound, every suspicion traced from the members' records to the detector's rule and every count a member reports held to its record (no test decides by a confidence level); every wait ends on the members' stated law, failing with their last states, quiet only while the supervisor hears every member (the E2E harnesses' rule in its own terms: a member past its due is unheard and decides nothing, the members' writes extend the wait, a member silent past their longest write and the quiet period is named, its silence counted over the time the supervisor listened), a member stopped named by the wait that needs it; 100 runs on macOS and 300 on Linux at one, two and four CPUs with busy loops, every one passing after the three defects they found, and the CI hang (a stall the device thread never took) reproduced on ubuntu-24.04-arm and fixed (`docs/benchmarks.md`). Its simulation draws its worlds from measured traces, the hosts' freezes replayed, every test at ten and a hundred times its seeds passing (`docs/benchmarks.md`, "The simulation's worlds") | yes |
| hyper-quic | quinn-proto 0.11.18; Copa from slates' and focal's laws, focal's b18 derivations measured and not taken; its cut meets focal's harm and leave rules over focal's grids, closing focal's findings 1 and 3, finding 4 standing as a behaviour and not a harm (`docs/transport.md` §4d); CUBIC the default. At 500 ms one way: the handshake's flights copied, Careful Resume, and an idle connection's path measured before its first burst (`docs/transport.md` §4f) | yes: no panic in shipped code, every site typed or proven (VENDORED.md §4); no `Arc` or `Mutex` (endpoint `Configs` slab) | per handshake: QUIC full 505→492, resumed 511→497 | yes, against upstream quinn-proto 0.11.18, which focal's transport runs: a handshake 0.80 (full) and 0.74 (resumed) of its instructions, an exchange or a MiB of bulk 1.06–1.13 of them, allocations the same (`docs/benchmarks.md`, "hyper-quic against upstream quinn-proto"); against slates' session plane owed, its class grid on hyper-transport once slates' adapter drives it. Copa's harness built on hyper-sim's network, each seed exact, and run over focal's F39 grids (`docs/benchmarks.md`, "Copa's competing mode over focal's grids") and slates' bake-off, where Copa is the one law slates' rule admits (`docs/benchmarks.md`, "slates' congestion bake-off on hyper-quic") | yes: client, server and relay processes over UDP, every scenario three ways (hyper-quic on both sides, and against unmodified upstream quinn-proto 0.11.18 in the other process, both directions): full and resumed handshakes with 0-RTT data accepted, 2, 3 and 5 MiB on concurrent streams, 2 MiB through a relay dropping 2 % and reordering, an active migration and an unannounced rebinding validated by the server, a killed server and a killed client each timed out by the survivor no sooner than its idle timeout, the restarted server rejecting the 0-RTT data the killed one's ticket allowed and the client opening its stream again; every byte checked; waits on facts under the quiet rule at the idle timeout (`tests/e2e.rs`, VENDORED.md §8) | yes |
| hyper-tls | rustls 0.23.45 | yes: no panic in shipped code (VENDORED.md §3); no `Arc` or `Mutex` (configs lent per call, `&'static` provider) | per handshake, against upstream rustls 0.23.45 in one process with one counter, eleven shapes, none above upstream (`tests/alloc_per_handshake.rs`, exact): TLS 1.3 full 286 against 296, resumed 209 against 219, 1.2 full 140 against 144, 1.2 resumed 91 against 99 (was 101: the store lends its TLS 1.2 session and keeps one it displaced while lent, VENDORED.md §5), by ticket 106 against 115; with Ed25519 credentials reallocations and bytes held to upstream's too | yes: against upstream rustls 0.23.45, which slates' and focal's transports run, a handshake 0.1–0.2% more instructions, its CPU and wall time within the noise of a loaded host (0.96–1.07, every median's interval overlapping upstream's), building a configuration and a bulk transfer the same (`docs/benchmarks.md`, "hyper-tls against upstream rustls") | yes: a client process and server processes over TCP, every scenario three ways (hyper-tls on both sides, and against unmodified upstream rustls 0.23.45 in the other process, both directions): full and resumed handshakes under TLS 1.3 and 1.2 (session ID and ticket), a client certificate verified and reported by the server, refused certificates (an untrusted server or client chain, a missing client certificate, a wrong name) with each side's typed error and alert, a mebibyte each way checked byte by byte (`tests/e2e.rs`, VENDORED.md §6) | yes |
| hyper-log | mantle-log `147f035`, with history (L-1); one owner and tickets (L-2); a group's handle (`log-reads`); the owner on its own thread, a blocking caller flushing its own frame (`log-switch`); a waking submitter not waiting for its admission, and a handle that splits its own updates (`log-batch`); its statistics, asked of its owner (`Log::stats`, `docs/durable.md` §13.1, new) | yes | appends and fetches into a reservation: 0 allocations once warm (was 23 an append at one replica, 8.4 at sixteen); through a group's handle, 0 too; on mantle's replica path 72 allocations and 0.2 reallocations a committed entry, against mantle-log's 213 and 21 | against mantle-log (its own `mantle bench log`) and focal-log `4bf7b64`; mantle's replica path at or below mantle-log in every same-run comparison (69 µs a committed entry against 81, load 34; 197 against 224, load 14–16; was 697), with mantle-log's 12.0 context switches an entry (was 19.5) (`docs/benchmarks.md`, "The replica's path"); mantle's bench point of 256 replicas appending 128 B at 7% more appends a second than mantle-log in every paired round at load 26–36 (29,891 against 27,912), and 12% more at 16 KiB ("Many small appends") | yes: a writer process killed with SIGKILL mid-append 24 times, every acknowledged append recovered, the log sealed (`docs/seal.md` §11) | yes |
| hyper-block | mantle-disk `147f035`, with history: block, buf, commit, issuer and what they need; mantle's residency epochs and segmented files since (`55d923e`, `ORIGIN.md` changes 13 and 14) | yes | `Pool` takes and gives with no allocation once grown | through hyper-log | through hyper-log | yes |
| hyper-durable | new, designed from mantle's (`1c179e8`), focal's and slates' shells and the literature (`docs/durable.md`, `crates/hyper-durable/ORIGIN.md`) | yes | measured on mantle's workload: 18–36% fewer allocations an entry than mantle's shell, and mantle's range replica on it 30–43%; the same reallocations once a group has warmed, with the core's spare queues (`docs/benchmarks.md`, "The durable shell against mantle's", "mantle's range replica on the shell (D-1)") | against mantle's `Replica` at `1c179e8` (`crates/hyper-durable-compare`), load 10–40: with three and five members p50 23–27% lower and 27–40% more entries a second on a real disk, fewer flushes an entry; mantle's range replica on it, load 3.7–20.5: p50 39–44% lower, 35–67% more entries a second. The losses closed: reallocations equal; context switches the overlap's, within a few percent an entry, CPU even; the one-member tail the device's, an entry's latency its write's on both shells. mantle switched (D-1). focal and slates later | yes: members as processes over UDP on sealed hyper-log files, `SIGKILL` at named durability points and at random, F17's two cases, a failed flush, devices held while the survivors of a stalled leader elect, a member stopped and named (`crates/hyper-durable-e2e`); its waits keep hyper-raft-e2e's rule, quiet only while the test hears every member (`hyper_raft_e2e::quiet`) | D-1's core built (`docs/durable.md` §15): `Replica` over `LogStore` (hyper-log's `GroupStore`, the depth-one `RamStore`) and `StateMachine`, the commit fence on R-6, readies to the store's depth, the owner's arena and turns; its simulation (crashes after every event, 128 seeds × 5 shapes by default, 1,000 soaked), mantle's four and focal's two F17 cases cut at every device operation, every bound at its edge. Elects by suspicion (L-2): no ticks, its owner wiring the node-pair liveness stream (L-3) to its replicas, every pair charged its groups' election, restarts taken to the core (`docs/timing.md` §2.9); or, for an owner not yet on the stream (focal until its timing step, mantle until D-1's liveness), on its owner's ticks, fixed at open with no mixed mode (`abae01a`, `docs/durable.md` §8). A store may hold a write for its owner (`Fault::Held`, §2.4), focal's decoder floor. A drive applies one page at most, or one larger entry, which is what an owner reserves before it (§2.2), and a snapshot carries the configuration held at its image's point (§2.3). The shell reports a fast-track proposal another entry displaced (`Output::displaced`, focal-consensus's `NodeEvents::displaced`), and its E2E runs the fast track's kill scenarios on hyper-log (`fast-kill-*`, `docs/sim.md` §15.13). No owner enables the fast track yet, and focal's shell backend refuses a fast group, typed, as its conversion refuses a data directory holding one; the E2E runs on its members' own detectors, a stalled disk among its scenarios, and derives nothing. A marked member is repaired by entries since R-5 (the core's; the shell's snapshot request is gone) and campaigns on its log since R-7 |
| hyper-seal | new (`docs/seal.md`), designed from mantle's `crates/s3/src/seal.rs`, focal's and slates' requirements and the literature | yes | none on the seal and open paths after setup; a sealed file's writer and reader make one aligned buffer and no allocator call after it (`cargo bench -p hyper-seal --bench files`) | §11 on Apple silicon: a 4 KiB open 500 ns warm and 1.21 µs cold, sealing 9.25–10.49 GB/s a core, a 4 KiB overwrite 500 ns version-keyed against 9.88–31.0 µs for a key per file, group commit sealed 0.964–1.043 of plain at the median (`docs/benchmarks.md`, "Sealing at rest"); slates' stated targets (a 4 KiB open ≤ about 1 µs p99, a key made in single-digit µs) met; not yet run against mantle's `seal.rs` itself; energy a byte on x86 and the Linux runs owed | yes: hyper-log's and hyper-durable's E2E suites run sealed, a writer killed with SIGKILL mid-append 24 times and every acknowledged append recovered | yes |
| hyper-transport | focal-wire's core `99191da` (T-1), slates' credit law; `ORIGIN.md`. The events an owner has not polled are bounded by its limits, `4·exchanges + peers·(1 + lanes_per_peer·lane_window)`: each waits in a seat of a bounded table (an exchange's slot, a lane's and its peer's window, a peer's entry), a peer's lifecycle as its latest state, and a table at its bound refuses the stream, connection or dial that would need one more, typed (`docs/transport.md` §4a) | yes | an exchange makes 8.35 allocations without a body and 12.4 with 4 KiB, 26.4 with 64 KiB, and no reallocation (datagrams cut from a bounded pool of chunks charged to the budget, never grown: 2 to 4 reallocations a round before); a lane frame 1.07 and 0.09, upstream's send-buffer shrink (`docs/benchmarks.md`); the events' bound changed no count, the exchanges' time within the comparison's noise and a lane frame's 15 ns more, its seats (`docs/benchmarks.md`, "The events' bound") | yes: against focal-wire's core at `99191da` on loopback, both on tokio and parked alike (under hyper-tokio): rounds 8–65 % shorter at every size, half its allocations for a small exchange, two fifths to a half of its bytes (load 36–50); since the receive pool, no reallocation at any size against its 0–0.12, an eighth to a twelfth of its allocations and a fifth of its bytes at 64 and 512 KiB (load 53–68); slates' session plane not comparable until its adapter drives this layer | yes: processes over UDP; every class with megabyte bodies, the class reserve under held bulk, typed refusals at every bound, a peer killed mid-upload, lane frames in order; passing on CPU-starved hosts (a fifth of a CPU on Linux, background QoS on macOS) since an answer is charged with what arrives, not priced by the path's round trip | yes |
| hyper-tokio | new (T-1's second half; note 32 §6 item 6); the plane socket's kernel receive stamps on the host's clock (L-3) | yes; `unsafe` only in `src/sys/linux.rs`, `src/sys/macos.rs` and `src/sys/windows.rs` | 0 allocations a round of its own: a 64 B round is 12.4 under it as under a busy-polled driver; a fixed outbox and receive buffers | through hyper-transport's comparison against focal-wire, both on tokio (`docs/benchmarks.md`) | yes: processes over UDP on macOS and Linux (sendmmsg, recvmmsg, UDP_SEGMENT, UDP_GRO); exchanges with every byte checked while the owner drops the driver's future mid-wait, lanes, the plane keyed from the TLS exporter, a peer killed mid-upload | no (`tokio`) |
| hyper-rt | slates' runtime `6b9ce5c` with its `mem` and `machine` (`crates/hyper-rt/ORIGIN.md`), designed onward for the three consumers (`docs/runtime.md`); mantle's runtime work merged at `17920a7` | yes; `unsafe` only in the OS-interface files `scripts/check-contracts.py` lists | a thread's round trip to a task 0 allocations (`benches/echo.rs`); timers filled, renewed and expired with none (`tests/timer_allocations.rs`); no hot round of a shard losing its driver allocates (`tests/service_resources.rs`); a wake is a slot in a pre-sized ring or a bit in the bitmap | yes: against slates' runtime `8a18d90` and tokio 1.53.1 on mantle's cases (`crates/hyper-rt-compare`; `docs/benchmarks.md`, "hyper-rt: the loop under load, against slates' runtime and tokio"): ahead on spawn, a request and reply across and within a shard, eight clients onto a shard and one client, level on a yield; behind tokio's median beside a yielding task (53 µs against 12, the same p99, the task holding 0.81 of the shard against 0.36–0.43) and in CPU a request for a client 200 µs apart (18.7 µs against 12.0–13.7) | in one process on the OS's own sockets and signals: TCP, UDP and local stream sockets through kqueue, epoll and IOCP, signals and stdio (`tests/tcp.rs`, `udp.rs`, `signals.rs`, `stdio.rs`); hyper-rt-transport's exchanges over loopback; no multi-process suite of its own | yes |
| hyper-rt-transport | new (`d209d69`, `docs/runtime.md` §14): hyper-transport's endpoint and hyper-datagram's plane driven on a hyper-rt shard, with hyper-tokio's driver API, so a consumer moves from one to the other by type | yes | 23.5–24.1 allocations an exchange of a 600 B request and its reply, flat from 1 to 512 connections held (`benches/connections.rs`) | an exchange 36–46 µs at every count of connections held to 512, where visiting every connection each turn cost 232–243 µs at 512 (`docs/benchmarks.md`, "An exchange by the server's connections"); against hyper-tokio's driver on tokio, which it replaces, owed | in one process over loopback UDP (`tests/exchanges.rs`); focal's wire runs on it (its transport switch, step 4) | yes |
| hyper-multilog | designed from slates' MLRaft `5cce86a`, the core and the literature (`docs/multilog.md`, `ORIGIN.md`); a member cut from a lower log's leader yields what it leads (§7.1, found by the hostile-network measurement); each log on the fast track, commands only, a leader's fast takes capped by the bound past its merge (§9.1); the count of logs changed while the group runs, at one point of the merged order, each log's generation stamped on its messages (§3.5) | yes | yes: the merge applies a command with no allocation; a proposal, a barrier and a handed-over entry allocate exactly what the core's do (`tests/allocs.rs`, exact) | yes: against slates' MLRaft `5cce86a` on its workload, tails with intervals, CPU, instructions, cycles, energy, allocations and wire bytes (load 96–116): even at one log; ahead at batches of 64 on one log (p50 16 against 21 µs, p99.9 225 against 538, 19% less CPU); behind at one command a round on three logs (p99 10 against 6 µs, 37% more CPU, 26% more wire bytes); traced 2026-10-08 to slates driven one follower at a time, the harness driving members no message reached, and log 0's leader relaying barriers it need not (`docs/multilog.md` §3.1, now none): under one message discipline even or ahead at every shape (CPU 2,513 against 2,537 ns at three logs, 12–26% fewer instructions), against slates' serial order still 16% more CPU at a command a round, the core's commit notices and their answers (`docs/benchmarks.md`, "under one message discipline"); slates with its publication 50–60 times the CPU. Under hyper-sim's hostile networks (loss to 10%, 50 ms jitter, duplication, complete, partial and simplex partitions, 20 seeds): more logs never shorten a tail (`docs/benchmarks.md`, "Hostile networks") | yes: three members as processes over UDP, a fsynced file a log; commits at one and three logs, log 0's leader killed and restarted, cut off and healed, 3,688-write tails runs; three logs' write p99 409 ms against one log's 143 at load 130, and 2.4 times the CPU, 2.1 times the energy, 1.9 times the device bytes per write; idle 2.5 wakeups and 0.4 ms CPU a second a member whatever its logs | yes, since `line` was promoted to `main` (2026-10-06); no consumer enables it until an owner shows a gain |
| hyper-sim | designed from the simulation harnesses of all four repositories and the literature (`docs/sim.md`, sources in `docs/research/sim.md`); S-1 built (`docs/sim.md` §12): focal's SplitMix64 and exact `below`, named streams, node clocks, the world under the ordered and free disciplines, the decision trace, the digest and `twice`; S-3's block device built (§16): hyper-block's simulated file moved here, with lost and misdirected writes, blocks read as zeros or junk and the crash states within a persistence point, each new fault reported by hyper-log's checks; the typed write queue, its completions events of the world, with S-6's first half (§17): hyper-durable's schedules in the world | yes; Miri-clean (CI's `miri` job) | 0 allocations, reallocations and faults a step under both disciplines; the trace reserved when a world is made | against the harnesses it replaces (`docs/benchmarks.md`, "hyper-sim's world against the harnesses"), load 22.6: flat in messages in flight, 23–26 ns a step against hyper-raft's support at 29–2,677; even with hyper-liveness's harness at 8 nodes and 3× faster at 64; behind it at 3 nodes, 22.0 ns against 14.8 (the trace and the exact draws, open for S-2); even on hyper-liveness itself, 538 ns a heartbeat against 553 | — (a test crate) | S-1 on `main` (`f083b2f`); its lints (§3.9) in the workspace: of 193 sites after L-2, 27 moved to simulated time and 166 allowed at the boundary with the reason stated (`docs/sim.md` §12.6) |
| hyper-check | designed from the four repositories' checks and the literature (`docs/sim.md` §4, sources in `docs/research/sim.md` §5); S-4 built (`docs/sim.md` §14): nine oracles, the quiet period and hot/cold monitors, floors, the WGL search with Lowe's memo and just-in-time order, fingerprints confirmed on whole keys, and focal's witness checker over a `Model`, the two held to agree | yes; no `unsafe`; Miri on its oracle, liveness and witness tests (CI `miri` job; the search tests are past its budget, `docs/sim.md` §14.8) | per history checked, p50 / p99 / max: 94–223 / 180–391 / 312–495 allocations and 8–29 / 18–61 / 47–106 KB held at most over 25,000 hyper-raft seeds, 2.5·10⁻⁵ of the 4 GiB ceiling at most (`docs/benchmarks.md`, "hyper-check's checkers") | the search and witness per configuration and per event, load 48.7: 103–133 ns and 1,610–1,658 instructions a configuration passing, 316–324 ns refusing with its confirmation, 77–80 ns an event; no checker it replaces yet runs on the same histories (S-6) | — (a test crate); it judges hyper-raft's group, fast and pipelined schedules and lossy networks (`tests/check.rs`) and mantle's range simulation's 48 recorded histories | S-4 on `main` since `line` was promoted (2026-10-06): mantle's `linear.rs` and focal's `history.rs` cases pass; the checkers agree on every history judged (25,000 campaign seeds and the defaults); the planted mutants caught (`docs/sim.md` §14.6); two hyper-raft defects it found fixed (§14.5) |
| hyper-check S-5 | the strategies of `docs/sim.md` §4.5, built at S-5 (§15): slates' exhaustive search and its slot and prefix models built again (not copied) under the production lints, the swarm, PCT over members, coverage over `FastTrack.tla`'s abstraction with a conformance check of every step, a round search of the real core, crash enumeration by fork (hyper-raft's member types derive `Clone`) and shrinking by tape | yes; no `unsafe`; the searches' memory counted exactly and held to the 4 GiB ceiling, `Unknown` past it | per run, p50 / p99, group harness, load 17–20: random walk 403k / 790k allocations, swarm 257k / 3,029k, PCT 423k / 819k, coverage-guided 510k / 623k and 774 / 895 KB held at most; a round of the round search 712 allocations; crash enumeration by fork 58 % of the replay's allocations (`docs/benchmarks.md`, "hyper-check's strategies and searches (S-5)") | slates' search: the slot model reaches every count slates recorded (463,715 at 4, 1, 2, 4; 23,552,907 at 5, 1, 2, 4), the prefix model every count with slates' stale log tail and the true orbit counts without it; full scopes 14.7 min, 2.48 GB resident; each strategy's runs to catch each planted defect against the random walk's (§15.8): swarm 172 / 16 / 1 / 706 / — (run 41,346 found the core's open defect first), PCT 2,096+ / 3 / 1 / 21,181–25,324 / 103,278, coverage 2,096+ / 99 / 1 / 4,787 / not in 121,041, the round search 33,621 rounds for the older-term commit (107,313 at its 2026-10-08 scope, which reaches reads and lagging writes: the read before the first commit in two rounds, 13,917 played, and a vote sent before it is durable in one lagged round, 11, `docs/sim.md` §15.14) | — (a test crate); runs hyper-raft's real core: 244,088 default steps and 15,000 swarm seeds held to the model | S-5 on `main` since `line` was promoted (2026-10-06): slates' counts reached and every `Variant` refused; every forked crash point (3,980) ends as its replay; two defects of the core found: one fixed (without check-quorum or pre-vote a leader of an older term was never told of the newer one by a member whose configuration named it no voter, and its group never converged), one fixed in its own series (a fast-committed entry lost once a member released its vote on its log covering the index and a later leader cut that log short of it, slates' `DropCovered`: a member now holds what it holds until a classic commit covers it, `docs/raft.md` §3.5, `docs/sim.md` §15.13), two of the harness (a snapshot the network dropped never reported; an unbounded liveness phase) and three of the judges (the durability oracle's I1 on a request whose log its term's leader cut; the abstraction's leader whose term was not yet durable; Log Matching on a term falling after a fast member's committed prefix), each with a directed test that fails without it (§15.9) |
| hyper-measure | new (`f3a4187`): a counting allocator and the OS's page-fault counts, then each process's own account (`usage`: CPU, instructions, cycles, energy, footprint), the open-loop schedule and quantile intervals of T-1 (`docs/tails.md` §3) | yes; `unsafe` in the five files the contract script lists | measurement only, no shipped crate depends on it; its Linux probe reads `/proc` with no allocation (`service_driverless_receipts` had counted one there as the runtime's) | — (the measurement kit itself; its readings are checked against the OS's, `tests/counts.rs`, `probes.rs`, `process.rs`) | every benchmark and end-to-end suite here reports through it | yes |

Timing (`docs/timing.md`, sources in `docs/research/timing.md`): elections and failure detection
move from picked constants to measurement — one NFD-E detector per node pair configured to
minimize unavailability, the election span from Ongaro's split-vote probability, the timer
granularity measured. hyper-swim's probes are already such detectors, one per pair, with the
period, the time-outs, the suspicion and the dissemination budget derived (§2.7). Steps L-1 (`hyper-timing`) to L-5 (the consumers); L-1 done (`qos.rs`: the bound, the configurator, the span; `link.rs` and `folds.rs`: the per-link NFD-E estimator, its window `min(n_G, n_A)` under a derived drift bound, the unseen-delay term for the first variance, and the folds for `G`, the sender's flush and the MTBF, path estimates fresh and of one endpoint (each sample stamped with its time and its endpoint's generation, a new generation starting the window again, a sample past the window's span dropped, the age reported: focal's audit F44), `G` never below the resolution of the owner's clock, so an owner whose waits all read on time still configures, where a mean of zero had floored nothing, configured no detector and suspected no one; `election.rs`: the election law from the detector and the span, the priority's window from `T_c`, the measured `G` under every tail, with `ELECTION_MARGIN`, `PATH_WINDOW` and `GRANULARITY_NS` gone). L-2 done (`docs/timing.md` §2.9): the core elects by its owner's detectors' suspicion after the law's draw over the measured span, steps down on a suspected majority, beats only while its group has work in flight, and takes no ticks; item 10 is closed (the first election needs only the handshake's round trip; a link younger than its evidence is judged by what its node measured, built in hyper-liveness). L-3 done (`crates/hyper-liveness`, §2.8): the node-pair stream with its flush proof, kernel receive stamps from hyper-tokio's plane socket (Linux `SO_TIMESTAMPNS`, macOS `SO_TIMESTAMP_MONOTONIC`; Windows stamps at the read, item 5), each suspicion with its stated bound; the core takes its `Suspected`, `Trusted` and `Restarted` changes as `suspect`/`trust`/`restarted` (L-2). Its follow-ups done (§2.9): every link configures or is judged by what its node measured, the widest of its pool's and its configured links' (item 10's detector half), the interval a link's evidence needs measured online, restarts reported. Its review done (§2.8): runs ordered and kept durably by the owner, the owner told every trust that differs from what it was told, the node's evidence kept and read only where a pair has no configuration of its own, the doubling renewal derived, the simulation on measured worlds. The detector model at its causes (§2.2, §2.8, §3 item 11): mistakes bounded per heartbeat taken, a sender's skipped slots its lateness, no margin cap, no configuration whose `U` is one or more (the node's evidence's margin for a young link held to the same rule), the evidence interval held only until a link configures, the `β` rule gone and the history kept against a sliding window as measured, `G` from every wait its owner began before a wake and that reached it (counted only where the timer ended them, members asked for reports back to back on Linux's 1 ms tick never measured it and refused every heartbeat, §2.9); every test exact, every suspicion traced from the nodes' records and every reported count held to them, the allowance reported; the account end to end against main on macOS and in Linux at one, two and four CPUs (`docs/benchmarks.md`). L-4 done: hyper-durable-e2e's and hyper-raft-e2e's members run on their own detectors, and neither test computes a tick, a tolerance bound or a count of elections (§2.9, "On real detectors").

The core's TLA+ model (`docs/models/`, from focal `7ea6f63`): the classic track, a change of
configuration by one entry or through a joint configuration, the fast track with both of its
rules, and members marked at rest with a marked member's election (R-7). Seven configurations pass
at their stated states and five are refused as they must be, among them each fast-track rule taken
out; eight more for the marked members, three that pass and five refused, at their stated states
as CI counted them (`docs/models/README.md`). CI's `model` job runs
them on Linux.

Landed 2026-10-16, each in its document: in the end-to-end harnesses' waits
(`hyper_raft_e2e::quiet`, which all three keep), a member silent past its excuse ends at once only a
wait on every member's own facts, a wait for what the group answers going on while the members heard
move, and an operation asked again waits twice its ask before, to sixty seconds (RFC 6298 §5.5,
§2.5; `docs/timing.md` §2.9, "A member stopped while the others' writes are slow"); in hyper-durable's
depth bench, many groups on their members' logs, measured on Linux with one group and many and on
macOS with many (`docs/durable.md` §14 item 1); in hyper-liveness's simulation, a device slowed for
good while its node's links are young (the open defect above, measured).

Landed 2026-10-15, each in its crate's document: in hyper-raft, a candidate's vote for itself counted
once the hard state naming it is durable (`docs/durable.md` I9, `docs/raft.md` §3.3; the simulation's two
leaders of one term, below); in hyper-rt, a wake slower than its probe round's share kept, the round
waiting for its answer within the probe's span (`docs/runtime.md` §10.4; main's red windows-2025 gates,
below); in hyper-quic and hyper-transport, focal's admission credit (STREAMS_BLOCKED sent and kept, the
pooled stream credit, `Refusal::Unsent`, `AskQueue::Credit`, a finished reply's credit given back with it;
`docs/transport.md` §4g, `VENDORED.md` §28–§29) and a lane's `Event::LaneRoom`, which focal's replication
lanes wait on; in hyper-liveness, a pair's report states its own sending interval and next heartbeat
(`PairReport::sending`, `sending_due_ns`); in both E2E harnesses and hyper-multilog-e2e's, every member's
report states its stream's evidence (heartbeats sent, refused by kind, `G`, its sending interval, its
next heartbeat and wake), and hyper-durable-e2e's its reads (waiting, confirmed, held by its core,
refused) and the drives its replica refused, which the member had dropped silently; in hyper-durable's
simulation, every read a leader took held to an answer once its group settles.

Landed 2026-10-12, each in its crate's document: in hyper-raft, an append behind a follower's commit
taken past it where the entry it names there is the one held (`docs/raft.md` §3.3; focal's ordered
appends); in hyper-quic, BBR from RFC 9002's initial window, a run of PADDING decoded as one frame, and
the path's warm-up as background traffic, LEDBAT++'s slow start ending at its own queue or a plateau
(`VENDORED.md` §21–§24); in hyper-transport, the endpoint visiting only the connections an event or a
call touched (512 held: 232–243 µs an exchange before, 40–43 after; `docs/benchmarks.md`); in hyper-log
and hyper-durable, a group's handle keeping what its replica has yet to apply, bounded by the replica's
budget, with no allocation an append (`docs/durable.md` §8, `docs/benchmarks.md`); in hyper-rt, a wake
probe's round timed from its waiter's start and a `block_on` whose root unwinds ending refused (below);
in hyper-rt and hyper-block, mantle's runtime work, its seven commits merged with `main`
(`crates/hyper-rt/ORIGIN.md`, `crates/hyper-block/ORIGIN.md`, `docs/runtime.md` §3.9), with
`tests/service_resources.rs`, red in mantle, green at its two causes: a shard losing its driver
allocated as it closed a control queue that was std's channel, now a ring in its registry entry
(`docs/runtime.md` §8), and the test started an observer thread inside each driver-loss round's
measured window, now one for every round. With it, a receiver's drop closes its ring (a mark in the
tail, std's disconnect bit), so no send lands after the close, which mantle's channel let in between its
check and its push; one of `readiness_waits`' tests waits for a later step, not one yield, since a step
polls a task that yields again within its batch (PR #16); and hyper-measure's Linux probe reads `/proc`
with no allocation, which `service_driverless_receipts` counted as the runtime's (`crates/hyper-rt/ORIGIN.md`).
In hyper-durable, the keep mark's follower half: a leader's store keeps what a member in `Replicate` has yet
to acknowledge of what was sent it, while all of it is in flight, within its window, so a resend reads
from the handle and a stalled member pins nothing past what it was sent (`docs/durable.md` §8, focal's
question); the store's two keep methods have no defaults, so a wrapper passes the mark on.
In hyper-rt, a step's long polls judged at one reading of the thread's clock at its end, not two a poll
(`4cbea75`): on mantle's ranges bench the reads fell 8–9 times, and `__thread_selfusage` from 2.9 % to
0.5 % of a seek's shard samples (`docs/benchmarks.md`, "hyper-rt: long polls judged by step"); on
mantle's newest fill (2026-10-13), its fill 3.3 % faster at 10M against mantle's vendored `f358210`, ahead
in every one of six rounds. A poll's end and a task's live reading carried from the step's reading of the
clock by the counter, read without synchronization: `mach_continuous_time` from 2.5 % of the fill's shard
samples to 0.1 % (`docs/runtime.md` §3.9). A channel's empty receive reads only its own lines, its senders'
end on a line of its own rather than in the cell table: 1.5 ns where other threads' one-shots cost it
2.1–2.7 ns (`docs/runtime.md` §3.9, D4).

## Open defects

Found under load or by a fault a test injected. They are traced as far as the evidence goes; none
is worked around.

- **hyper-liveness: a node that never heard any peer judges none (found 2026-10-20, beside the deaf
  leader below).** A node deaf from its start has no link heard, so no pool to starve and no
  interval to time a starvation by (`Liveness::starves`), and a peer never heard is judged only by
  the node's evidence (`Pair::judge_unheard`), which such a node never has: it trusts every peer for
  ever. Its own stream is measured all the same (`Liveness::own`), and judging its never-heard peers
  by it would end this, but at every start: a node whose peers have not started yet would suspect
  them where it now trusts them, and by suspicion a member campaigns only with a quorum it trusts
  (`docs/timing.md` §2.9), so a cell's first election would wait on its slowest member's first
  heartbeat. That trade is the consumers' to measure in their start-up tests before it is made.

- **hyper-block: translated by Rosetta 2, a segmented file's direct handle reads its pieces as in
  memory (found 2026-10-20, running hyper-block's tests for x86_64-apple-darwin on the M5 Max).**
  `segmented::tests::a_segment_written_whole_holds_its_fill_and_takes_no_memory` fails translated,
  `piece 0 is in memory`, before and after the kernel's-ticks repair below, and passes natively: the
  resident read (`read_resident_at`, an `mmap` and `mincore` of the piece) answers that a piece
  written through a handle granted past the cache is held. Not traced yet: a translated process sees
  4 KiB pages where the kernel keeps 16 KiB, which `mincore`'s answer may straddle. Until it is, the
  CI job that runs the clocks' crates translated (`translated`) leaves hyper-block out.

- **hyper-rt-transport, hyper-tokio: a driver folds its timer's lateness into `G` only when it finds
  its timer fired in the turn that armed it (found 2026-10-20, by the simulation).** Each turn of
  `poll_event` hands a due timeout to the endpoint (`handle_timeout`) before it looks at its timer, and
  the timer for the next deadline then replaces the one that fired, unread; `fired`, which folds the
  wait (`Lateness::on_wait`) and tells the endpoint (`Endpoint::set_granularity`), runs only when a
  deadline passes inside a turn, between the turn's reading of the clock and its timer's poll, which a
  simulated clock never does. hyper-rt-transport's `tests/simulation.rs` world run for 5 s of virtual
  time with scratch prints (never landed): every due timeout its three drivers handled, 82 over three
  runs, keep-alives among them, had the driver's timer armed for it, and none was folded, so the
  endpoints kept RFC 9002's 1 ms (`K_GRANULARITY`). hyper-tokio's `poll_event` takes the same order.
  Open: fold a wait at the turn that finds its deadline passed (a wait that reached its deadline,
  whatever ended it: hyper-liveness's rule, `docs/timing.md` §2.4), which moves the windows' tuning
  (`Window::tune`) from 1 ms to the measured `G` on every host, so it lands measured with the
  consumers' suites. A simulated driver then floors `G` at the virtual clock's step, a nanosecond, not
  at the host clock's resolution `Driver::with_ring` reads (`hyper_rt::machine::clock::resolution_ns`:
  1 µs on macOS, 100 ns on Windows), or one seed's run would differ from host to host.
- **hyper-raft: on the swarm's quarter network one group's liveness phase does not converge (found
  2026-10-17).** With the swarm's losses and repeats narrowed to a quarter (`docs/sim.md` §11 item 4,
  the setting that reaches the most abstract states and catches every planted defect within the
  random walk's count), the swarm with no defect planted over 13,585 seeds a harness
  (`the_swarm_with_no_defect_keeps_every_oracle_and_the_model`, branch `swarm-quarter-network`) met
  group seed 6,135 (three voters, check-quorum on, pre-vote off, changes and partitions on, losses 24
  and repeats 3 in a hundred), whose liveness phase passed its bound with every oracle held: terms
  past 850 and no leader, member 1 holding the configuration {1, 3}, members 2 and 3 holding {2} with
  3 a learner, member 2 a candidate at term 845 with entries past its commit (85 against 79). On the
  hostile network the same seed's run settled (its losses 73 in a hundred, the campaign of 9,924 seeds
  clean). The fast and pipelined harnesses were not run past it. Traced so far (an operation log of the
  seed's run): member 1 stopped taking entries at step 371 (commit 8); member 2 led term 22 from step
  1,209 and, at step 1,462, met term 23 from a campaign and stepped down with entries 80–85 past its
  commit of 79, member 3 at 56; no member led again in the 38,695 operations after, the liveness phase's
  included. Member 2's log holds at index 80 (term 22, past its commit) the change that adds member 3 as
  a voter, so by the newest configuration in its log (`docs/sim.md` §15.12) its quorum is {2, 3} and it
  needs member 3's vote; member 3 holds the committed {2} with itself a learner, never received index
  80, and cast no vote in any of the 800 terms that followed (its hard state's vote 0 at steps 1,463,
  3,000 and 39,000), while member 1, holding {1, 3} with eight entries, campaigned throughout without
  pre-vote, its terms ahead of member 2's (824 against 820 at step 39,000), and each of its campaigns
  restarts the others' election timers (`Raft::become_follower` resets the elapsed ticks). Open: why
  member 3 grants member 2 no vote (`Raft::step_vote` does not refuse a learner), before the swarm's
  network is narrowed.
- **hyper-transport: on Linux's default receive buffer the shared socket holds two first windows
  (found 2026-10-17).** Docker's VM grants 425,984 bytes (`rmem_max` 212,992), 319,488 past the
  release lag. Charged at its costliest stream byte (896 bytes for a datagram carrying 120), the
  shared socket's room is about 31 KB once measured: two first windows and the room their peers'
  unprompted messages take (`docs/transport.md` §4s). focal-node fails 8 of 414 tests there (117
  first windows refused), against 13 on focal's `main` (2,285); focal's enrollment client gives up at
  its first refusal (`Transport(Unavailable)`). focal's Linux CI passes on `main`; what its runners
  grant is not known (their image sets no `rmem_max`). Open: whether a refused handshake's client
  should try again (TCP's client does when a listener's backlog is full), and what a socket of that
  buffer should admit at once.
- **hyper-rt-transport: a socket is asked for its windows' demand at a byte charged a byte (found
  2026-10-17, older than §4o).** A peer's socket asks `peer_demand` and a listener `receive_demand`,
  neither with the charge measured nor the room unprompted messages take. A socket whose windows'
  demand is 18,652 bytes (a 16 KiB ceiling and the reserve) is granted that much, and the 32 small
  datagrams its one connection's peer may send unprompted (73 KB) leave its room nothing; its window
  stays at the first. Seen 15 to 18 times in each Docker run above, on `main` as since. Open: an ask that adds
  what the listener has measured.
- **hyper-liveness: a link whose sender's floor keeps moving measures its evidence minutes late, and
  a node with only such links suspects no one meanwhile (found 2026-10-16).** A scratch fault in
  hyper-durable-e2e's `member-stopped` (branch `diag/slow-flush-repro`, never landed) holds every
  flush of the two members up 741 and 597 ms more from the stop on: their writes take 0.5–2.5 s and
  lengthen as their logs' writes queue behind one another. Stopped while their links were young, the
  two members had configured none of them 390 s on (216 heartbeats taken), and neither suspected the
  stopped member; with the leader stopped instead, the two members up suspected no one and elected
  no one in 291 s. A scratch trace of the estimators showed every pair refused
  `CorrelationUnmeasured`, its window one heartbeat at 88 received: an unconfigured link's interval
  is its sender's floor, followed up at once and down once the flushes it is the mean of have
  doubled, and it moved 35 ms → 765 ms → 955 ms → 1.17 s → 1.02 s, each move retiming the receiver's
  estimator, whose window and Allan levels start again (`LinkEstimator::retime`); the node's pool,
  fed every link's latenesses across the moves, refused the same. `E[flush]` is the mean of every
  flush since the log opened (`hyper_timing::Flushes`), so after the device slows it climbs toward
  the new flushes by a little at each one, reaching halfway only once as many slow flushes have come
  as fast ones came before, and the floor, followed up at once, moves the interval at every step.
  The simulation measures it (`links_young_when_every_device_slows_for_good_still_configure`: three
  nodes, every flush 741 ms more from the victim's first heartbeat on, 8 seeds of each of the macOS
  and Linux worlds): every pair configured 257–618 s after the slowdown, against 74–198 s for the
  same device slow from the start, and a peer killed once they had was suspected 4–19 s after its
  kill. The worst time without evidence the design measured is 13–35 s (`docs/timing.md` §2.8,
  "Judged before its own evidence"), and none of its worlds moves a sender's floor for good. Open:
  what keeps a link's evidence across its sender's moves, or bounds a node's time without evidence.
  Two candidates are built as prototypes on the unmerged branch `proto-liveness-floor`
  (`HYPER_LIVENESS_FLOOR_RULE`), each a rule for where an interval goes when its sender's floor rises
  past it, and measured on 2026-10-17 by the two simulations that state the trade (release, their
  default seeds): **doubling**, the interval doubled until it holds the floor (a link's restarts bounded
  by the logarithm of its floor's range; Lindley 1952 still holds, the interval above the floor), and
  **latest**, the larger of the floor and the latest flush plus `G` (one move for a change of the
  device's speed, where the mean of every flush creeps). The owner decides; neither is merged.

  | Rule | configured after every device slows, median (range), macOS / Linux | the victim suspected after its kill | a peer dead in its links' first heartbeats noticed, median / p90 / most: macOS, busy, Linux, Windows |
  |---|---|---|---|
  | as built | 370 s (257–618) / 430 s (280–493) | 4–19 s | 71/560/746, 119/1102/1933, 48/156/276, 313/1754/2494 ms |
  | doubling | 226 s (167–347) / 208 s (168–348) | 6–14 s | 91/607/1626, 129/984/2268, 58/278/608, 369/2703/5998 ms |
  | latest | 218 s (153–378) / 192 s (120–285) | 8–16 s | 77/589/1078, 117/894/1692, 90/788/2184, 622/2472/5725 ms |

  Each roughly halves the time to a link's own evidence, and each notices an early death later at the
  tail: doubling holds the interval up to twice its floor, and latest takes a young log's noisy last
  flush as the floor (Linux's most 2.2 s against 0.28 s). Neither dominates the rule as built.

### Closed (2026-10-24)

- **hyper-quic: right after its handshake a connection on a Windows loopback went silent for a probe
  timeout (found by mantle; main's patch Q25, `326ddd7`, taken to this line).** mantle's cell tests
  failed on Windows only, on silences of 0.87 to 1.48 s in which a lone node read nothing, each when a
  client of the same identity came after one that had closed its socket. Reproduced at `2b13b15` with
  a server and a client after client, each on a thread and runtime of its own
  (`hyper-rt-transport/tests/reset_e2e_diag.rs` on branch `hunt/udp-reset-windows`): 26 of 1,536
  exchanges stalled on the hosted Windows runners, 0.44 to 1.07 s on windows-11-arm and 0.5 to 1.56 s
  on windows-2025, and none of 360 on Linux 6.19 and Darwin 25.4. Right after its handshake the
  client sent its MTU search's first probe, as it did whenever it was idle, past its congestion
  window and its pacer: 33,353 bytes (the midpoint of 1,200 and a Windows loopback's 65,507-byte
  payload) against a first window of 12,000 (RFC 9002 §7.2's rule for 1,200-byte datagrams). It then
  sent nothing at all, its request nor an acknowledgement, until
  about a probe timeout later (the server sent its unacknowledged flight again a second after it),
  and counted the probe lost. With the probe held to the window and the pacer (RFC 8899 §3, item 7;
  `crates/hyper-quic/VENDORED.md` §36) the same runs stalled 0 of 576 against 91 of 576 without it
  (runs 40088388382 against 40087563225, each exchange's datagrams logged, which made stalls more
  frequent), and main's line 0 of 288 (run 40088250956); main's pacer repair alone (Q31, `c8c3cb6`)
  left 61 of 576 (run 40088804422). The ICMP reports mantle suspected came from the same closed
  sockets and caused none of it: the stalls came as often with Windows' reports switched off
  (`SIO_UDP_CONNRESET` and `SIO_UDP_NETRESET` FALSE), and hyper-rt reads past a report (the next
  entry). Test: `tests::an_mtu_probe_waits_for_the_window_on_a_slow_path` (12 probes at `2b13b15`,
  none with Q25).

- **hyper-quic: with Q25, a busy connection's MTU search starved (main's patch Q26, `0427988`, taken
  to this line with Q25).** Q25 holds a probe to the room data leaves, and a connection that kept
  sending left it none: its search stayed at 1,200 bytes, which main's
  `peer_sockets::a_connections_mtu_search_ends_at_its_paths_mtu_as_the_kernel_holds_it` found (7 runs
  of 40 on arm64 Linux). A probe the window could carry with nothing in flight and the pacer's bucket
  full goes before new 1-RTT data (`crates/hyper-quic/VENDORED.md` §37). Test:
  `tests::an_mtu_search_runs_on_a_connection_that_keeps_sending`.

- **hyper-rt: whether a Windows listener stops at a UDP port-unreachable report (asked by mantle).**
  It does not. A send to a closed port comes back on a later receive as `WSAECONNRESET`, counted
  astray and read past: on both Windows runners the report makes the socket readable on its own,
  holds its place among the datagrams, and the receive future and the batched drain take the
  datagrams queued behind it with nothing more arriving (diag runs 40085130113 and 40085783959; in
  240 of 240 rounds a drain racing a fresh report took every datagram).
  `tests/udp_astray_reads_on.rs` keeps it so on Windows CI (`docs/runtime.md` §5.1, "Astray
  reports"); the silence mantle saw is the entry above.

- **hyper-liveness: a node whose peers never answered heartbeated each at its floor and flushed its
  log for every heartbeat, for as long as they said nothing (found by mantle).** Before the receiver
  has chosen a stream sends at its floor, `E[flush] + G`, and a receiver that never answers never
  chooses: a founder of mantle's three-node cell started alone flushed about ten thousand times a
  second on fedora's NVMe at 132% of a core, and a node restarted into a cell before it learned where
  its peers listened wrote 82 MB/s for thirty minutes. A stream to a peer from which nothing has come
  is a sender with no return traffic, which RFC 8085 §3.1.3 (BCP 145) holds to one datagram every
  3 s: its interval doubles from the floor at each heartbeat up to 3 s (never below the floor), its
  liveness writes with it, and the peer's first heartbeat taken is answered at once, the stream at
  its floor from then on (`docs/timing.md` §2.8, "A peer that has said nothing"). Tests
  (`tests/sim.rs`): `a_node_whose_peers_never_answer_backs_off_to_a_heartbeat_every_three_seconds`
  (red at `54d7106`: 6,879 heartbeats to a peer in a minute of macOS's world, against at most 31) and
  `a_peer_heard_at_last_is_answered_at_once_and_its_stream_backs_off_no_more` (red without the answer
  at once: 2,354 ms against a poll's and a flush's 44 ms), and
  `pair::tests::a_stream_never_heard_backs_off_to_three_seconds_and_answers_its_peer_at_once` (the
  schedule exactly; the answer numbered next, no slot skipped, where an answer stated as its spacing
  from the heartbeat before skipped 50).

- **hyper-rt, hyper-rt-transport: a second listener could bind a node's named port and split its
  datagrams (found by mantle; main's `0802a05` taken to this line).** `Driver::bind` binds its
  listener with `Batched::bind_shared_lent`, which set `SO_REUSEPORT` before binding a named port, so
  a second process of the same user bound the same port without error on Linux and macOS, and the
  two took each other's datagrams by hash. A listener now binds first and shares after, whatever
  the port, so a held port is refused `AddrInUse`; the sockets a driver connects to its peers join
  the port with their flag set before their bind (`UdpSocket::join_shared`,
  `Batched::join_shared_lent`). Windows is refused before any socket is made. Test:
  `udp_shared::a_shared_port_is_refused_to_a_second_listener`, red at `2b13b15` on Linux 6.19.10
  and Darwin 25.4 (the second bind succeeded), green after with hyper-rt's UDP suites and
  hyper-rt-transport's on both; `peer_sockets::a_second_driver_on_a_held_port_is_refused`, the
  driver's own listener asked twice, red at `628111e` on Darwin 25.4 (the second driver bound)
  (`docs/transport.md` §4w, `docs/runtime.md` §5.1).

### Closed (2026-10-21)

- **hyper-liveness: on a device the nodes share, a stall of it made every judge suspect every peer
  (found by mantle).** The flush proof stops every stream on the device at once, and each judge, its
  own stream stopped too, suspected by a proof it could not produce itself: mantle's cluster suite on
  one ext4 volume, every all-pairs burst lined up with every node's flushes stalled at once, the
  cause of its liveness stalls and leader changes on `1b85eaf` as on this branch. A judge whose own
  proof is outstanding now waits for it, a long stall giving the peer its margin again from its end,
  and a judge with nothing durable since the point waits for its next completion (Lifeguard's local
  health, applied to the device: `docs/timing.md` §2.8, "A judge whose own proof stalls"). A point
  is held once: restated at every wait, a judge whose every write was slower than the margin never
  suspected a dead peer (mantle's pinned suite at `1ccd38d`). Costs: a crash is detected late by up
  to one of the judge's own stalls and the margin, or one sending interval and a flush; a node whose
  disk stalls for good
  suspects no one, its peers suspecting it. Tests in tests/sim.rs, the shared stall and the
  stall for good red on `79eb22b`.

### Closed (2026-10-20)

- **hyper-raft: a leader that suspected every other voter handed over to none (found by mantle).**
  `Raft::hand_over` took heirs only among the voters the leader trusted, and a leader steps down
  exactly when it and those it trusts are no quorum, so one that suspected every other voter sent
  no order while they heard it and trusted it; it could not campaign itself (no quorum it trusts),
  and the range stayed leaderless as long as its suspicions lasted: for ever under one-way loss
  (mantle's node simulation, the leader's plane deafened), and on mantle's Windows CI, with no fault
  injected, for the test's 4.63 s (run 39503516282: term 2's leader on a 2-vCPU runner running
  seven process tests suspected both followers, stepped down with no heir, both followers trusted
  it). The heirs are every other voter, trusted before suspected, then by what each holds and by id,
  each hand-over naming the next over all of them, the turn starting again at each term led
  (`docs/timing.md` §2.9, "Who the heir is"). A leader that trusts no voter at its step-down orders
  none at once, and decides a round and a draw later: ordered at once, every short stall of a
  leader's host moved its group's leadership (mantle's cluster tests, run 39525134119). It orders no
  heir, nor campaigns, within the election the law expects of its last order: re-ordered a round and
  a draw apart, stepped-down members forced heirs milliseconds apart and split votes (mantle's suite
  pinned on Linux at `eb85154`). Tests in `tests/suspicion.rs`, the first two red on
  `1b85eaf`, the third with the turn counted from the member's start.
- **hyper-liveness: a node all of whose links fell silent before it had evidence judged no peer it
  had heard (found by mantle).** Its pool and its links' estimators were fed nothing more, and only
  their measure judged a heard pair with no configuration (`pool_margins` ran only with the node's
  evidence): mantle's leader deafened at its election, 36 heartbeats taken and none judged, led for
  ever. Once the pool starves, the node judges by its pool's latenesses at the most correlation they
  can carry (an unseen share of a half) and by its own stream's, which it keeps measuring while deaf
  (`docs/timing.md` §2.8, "Judged with no evidence of its own"); a node any of whose links feeds it
  is unchanged. `a_node_deaf_before_it_has_evidence_suspects_every_peer`, red on `1b85eaf`.
- **hyper-liveness: a stream whose next slot was due as it sent stopped until a peer's heartbeat
  polled it (found 2026-10-20, by a node deafened in the simulation).** An interval that fell while the stream was
  behind left its next slot past at the send, and the poll asked no wake for a past slot: a node that
  heard none sent nothing more (Linux seed 175 of 256: sixteen heartbeats a peer, then none), and one
  that heard its peers sent the slot late by the wait for their next heartbeat, which its peers took
  as lateness. The poll now asks the flush that proves the next slot (`docs/timing.md` §2.8, "A
  stream keeps its schedule without hearing its peer"). `a_node_that_hears_no_peer_keeps_its_streams_on_schedule`,
  red on `1b85eaf`; over 256 seeds of the live-peers world the suspicions of live peers fell from 436
  to 425 at the same allowance.
- **Not a defect, measured: a deaf node's peers suspect it more often.** It cannot hear the interval
  they ask (it rides their heartbeats), so it keeps its floor, 6–7 times shorter than the asked
  (6.7 ms against 45 ms on macOS), and each heartbeat they judge is one more chance of a mistake
  (`docs/timing.md` §2.2). Over 32 seeds and 2 s from a deafening at 300 ms, against the same seed
  undeafened: macOS 4 against 2 false suspicions, Linux 18 against 8, the busy log and Windows 0 and
  0 (a scratch test, not landed). Its interval moved less than the undeafened node's (Linux 10
  moves against 47), and no peer lost a configuration.

- **hyper-rt, hyper-tokio, hyper-timing-trace, hyper-measure, hyper-block: a value the kernel takes in
  its own ticks was read at the process's timebase, which a process Rosetta 2 translates does not
  share (found by mantle).** XNU stamps a received datagram (`SCM_TIMESTAMP_MONOTONIC`) with its own
  `mach_absolute_time()`, in the kernel's ticks, `hw.tbfrequency` a second; a native process's
  timebase is 10⁹ over that frequency, so the two are one counter, but a translated process is shown a
  timebase of 1/1 and its own `mach_absolute_time` in nanoseconds while the stamp stays in Apple
  silicon's 24 MHz ticks (measured on the M5 Max: the stamp of a datagram a translated process sent
  itself was 1/41.667 of the clock read around it; docs/research/timing.md, "Rosetta 2 and the
  kernel's ticks"). hyper-rt's receive took the age as `now_ticks − stamp` at the process's timebase,
  122/125 of the host's uptime, so every arrival fell to the shard clock's zero and hyper-liveness never
  judged a pair: on mantle's macos-x86_64 runners (x86_64 binaries on Apple-silicon macos-15) a cell of
  `mantle node` processes stayed at term 0 and its process tests hung to the six-hour limit (mantle runs
  39394443852, 39410946746, 39424048894); hyper-raft's own x86_64 runner is Intel hardware, where the
  two are one. The same reading was in hyper-tokio's stamps, hyper-timing-trace's, hyper-measure's
  `proc_pid_rusage` times (3/125 of `getrusage`'s translated) and libc's
  `clock_gettime(CLOCK_THREAD_CPUTIME_ID)`, which is the kernel's ticks at the process's timebase
  (gen/clock_gettime.c), read by hyper-rt's long-poll attribution and park cost and by hyper-block's
  `thread_cpu`: translated, a thread was told 3/125 of its run time. A kernel tick value is now
  converted at the kernel's rate, `hw.tbfrequency` read once: hyper-rt carries a stamp into the
  process's ticks before taking its age (`machine::clock::MachTicks`), the identity for a native
  process, whose arrivals are what they were to the bit; hyper-tokio, hyper-timing-trace and
  hyper-measure convert at × 10⁹ / `hw.tbfrequency`, the native timebase's own fraction; the thread
  clock is restated at the kernel's rate. The process's own clocks keep its timebase: its continuous
  time, the shard's clock, is translated whole (measured: the same 1,587.18 s of sleep past
  `mach_absolute_time` natively and translated). hyper-tokio's `Clock::resolution` is the kernel's
  tick where the timebase claims less (a translated clock steps 41 or 42 ns, not 1). Tests, red
  translated before (`cargo test --target x86_64-apple-darwin` on the M5 Max) and green after, native
  green both: hyper-rt `tests/udp_kernel_stamps.rs` (a stamped socket and a lent one each send
  themselves a datagram; before, `arrived at 0 ns, outside the send at 1102708 ns to the receive at
  2175833 ns`), `attribution`'s and hyper-block's `a_threads_cpu_time_is_the_kernels_account`
  (`thread_info accounted 1002000 ns, the CPU clock read 24137 ns after it`), and the existing
  `udp_batched.rs` (three), hyper-tokio `stamps.rs` (two), hyper-timing-trace's
  `a_datagram_carries_its_kernel_receive_stamp` and hyper-measure's `user_time_is_in_nanoseconds`;
  unit tests with injected rates (`machine::clock`, hyper-tokio's `sys::macos`): a translated
  process and a native one read one age and one stamp, and a native one's are the old ones to the
  bit, over some 4,000 instants each. Three tests' own thread clocks (`long_poll_held.rs`, `attribution_by_step.rs`,
  `wake_estimate.rs`) read libc's alone and held 41.67 times their turn translated, which the
  runtime's old reading hid; they read the runtime's now. CI runs these crates translated from now
  on (`translated`, macos-15). The native receive is unchanged (docs/benchmarks.md, "hyper-rt: a
  stamp in the kernel's ticks, the receive's cost"). docs/runtime.md §3.9, §5.1; docs/transport.md;
  docs/timing.md §2.4; docs/tails.md.

- **hyper-transport: a dial that failed as the last one did was told nothing (found by mantle).**
  `Endpoint::connect` promises each dial the lifecycle event it ends in, but a state the owner was
  already told waited as nothing, so a dial that failed in the state the owner had polled last was
  not told. On mantle's Windows runners a cut of about 18 s outlasted two failed dials: the node was
  told `Closed`, `Unreachable` for its first redial and nothing for its second, and since it redials
  on those events it never dialed again; the peer stayed unreachable after the path healed (its link
  at `dials 3, ended 2`, the endpoint's entry `conn None dialing false unreachable Some(None)
  reported true`). A dial during which the peer connected in and went, the owner polling neither,
  was lost the same way. A dial that fails with no connection up now forgets what the owner was told
  (`Core::dial_failed`), so its `Unreachable` is told; one waiting unpolled stands for it, and a dial
  failing beside a connection the peer opened tells nothing again (`docs/transport.md` §4u). Tests:
  `a_dial_failing_as_the_last_one_did_is_told_all_the_same` (before: `dial 2: []`) and
  `a_dial_ending_after_its_peer_came_and_went_unpolled_is_told` (before: `[]`), red before the fix;
  `a_dial_ending_while_its_peer_is_connected_tells_nothing_new` holds the rest. In mantle's Windows
  CI, the same change in its vendored copy, its relayed cut tests (3 and 5 voters, three rounds
  each) passed 12 of 12 runs on windows-x86_64 and windows-aarch64; without it every round failed
  on both.
- **hyper-rt `a_request_drained_during_shutdown_is_terminated_on_its_receipt`, windows-11-arm (CI
  run 39410706279, dial-outcome's): the request refused `ShardGone` in 0.00 s.** The test holds its
  shard inside a spinning task and queues a shutdown and then a request behind it, and its hold
  returned once the shard had admitted the task. A shard answers an admission inside its pass over
  the control channel and takes what is queued behind it in the same pass before it polls what it
  admitted (`drain_control`, a batch at a time), so a shutdown sent as the admission was answered
  was taken in that pass, the hold cancelled unpolled, and the request after it refused as gone.
  Forced in a scratch test (never landed), a hold and a shutdown queued behind a first hold and
  drained in one pass: admitted, never polled, the next request `ShardGone`, every run. The hold now
  returns once its task's poll has begun (the task sets a flag first; the wait bounded by the test's
  `WAIT`). The runtime behaves as documented; the test's precondition was wrong. 500 runs of the
  file under 24 busy loops on the 18-core Mac passed with the fix, and with the old hold as well:
  the window is the shard's few instructions between answering the admission and taking its next
  message, open only while the shard is preempted there and the test thread runs.
- **hyper-rt: a shared UDP socket given port 0 could take a port another process held (found by
  mantle's process tests).** On adacachy (Linux, 32 cores) two `mantle node` processes held
  127.0.0.1:59152, each listener bound through hyper-rt-transport's driver to port 0; the node that
  announced the port was never reached on it, the other refusing its peers' dials for identity (169
  each), and the test hung. `UdpSocket::bind_shared` set `SO_REUSEPORT` before its bind, and Linux's
  search for a free port leaves out of the ports in use those held by a socket sharing with the
  binder's user (`udp_lib_lport_inuse`, net/ipv4/udp.c), in any process. A socket given port 0 now
  binds before it shares, so it takes a port no socket holds; one that names its port shares first,
  and still joins (`docs/runtime.md` §5.1). `shared_sockets_given_port_zero_each_take_a_port_no_other_holds`
  failed on Linux 6.12 in every run before the fix (3, 4 and 3 of 500 sockets) and passes after on
  Linux and macOS; `a_socket_joining_a_shared_port_takes_its_connected_peers_datagrams` and
  hyper-rt-transport's `peer_sockets.rs` hold the join on both. TCP's search skips a held port
  whatever its flags; macOS's skips one for UDP as well.

### Closed (2026-10-19)

- **hyper-durable: a page read from `GroupStore` ran past the core's bound on a message (found by
  mantle).** The store gave the core's `max_bytes` to hyper-log's fetch, which counted each entry
  at the bytes the log stores of it (`encode_entry`: its kind, its context's length, its context
  and its data), 20 fewer than its body in a message (`hyper_raft::proto::encoded_bytes`), and the
  core takes a page its storage cut as whole (`Log::slice_page`). In mantle a range with
  `max_size_per_msg` 1,024 and entries of about 50 bytes sent a follower a snapshot had caught up
  an append of 1,553 bytes, 1,449 of them entries, where the core promises
  `MESSAGE_RECORD_FIXED_BYTES` and the larger of the bound and one entry; its transport bounds the
  replication class's frame by that promise and refused the append on all 1,158 retries, and the
  follower never caught up. Appends of the live tail, which the core pages itself, were within the
  bound. hyper-log's fetch now counts each entry at its payload and a part the caller states for
  each (`hyper_log::Page`), the store states the 20 bytes, and the log chooses the core's page
  before anything is copied; a walk's pages (`LogStore::visit`) are counted the same way. Tests:
  hyper-durable's `a_page_read_from_the_store_is_the_cores_by_what_its_entries_take_in_a_message`
  (every bound a page turns at, from the handle's memory and from the log's file),
  `a_walk_of_the_store_reads_pages_of_the_cores_rule` and
  `a_learner_caught_up_from_the_log_is_sent_appends_within_the_bound` (before: an append of 5
  entries, 325 bytes, against a bound of 256), each red before the fix; hyper-log's
  `a_fetch_counts_each_entry_at_its_payload_and_the_pages_part_for_each`, and the handle's reads
  held to the log's with a part for each entry. hyper-raft-e2e's `Wal` counted an entry at its
  whole record, 6 bytes past its body, and cut pages shorter than the core's; it counts the body
  now. Its pages full to the byte showed the harness's room beside an append's entries 8 bytes
  short (`node::MESSAGE_ROOM` left out the classic commit every leader's append states): a full
  page was dropped as too long for its datagram at every send, and in `leader-killed` a member the
  new leader caught up from its log never caught up (ubuntu-24.04 and macos-15, CI run
  39366050595). The room is now the datagram's header, the sender and the core's
  `MESSAGE_RECORD_FIXED_BYTES`, and hyper-durable-e2e, which frames a member's datagram the same
  way and kept a copy of the short room, takes it
  (`a_leaders_append_filled_to_the_page_fills_its_datagram`: 65,515 bytes for a datagram of
  65,507 before). The stores that give the whole range asked (hyper-multilog's test store, the
  liveness bench's) are cut by the core's own rule.

### Closed (2026-10-17)

- **hyper-transport, hyper-quic: on Linux's default buffer a measured shared socket refused every
  new connection, and once MTU discovery reached the path a peer's own socket held less than one
  first window.** The shared socket reserved for every admission place, taken or not: focal's 32
  places of 30 probes took 860 KB of 319 KB. Now only the places taken count, and a first window is
  taken only with room for what its peer may send unprompted, held until the driver's next capacity
  counts it (`Room::first`, `Endpoint::set_receive_capacity(bytes, handshake)`). Each established
  connection's 31 probes were charged at up to the path's MTU, 262 KB to 2 MB on loopback. But
  every probe datagram is clamped to the smallest datagram, and now a probe timeout that follows an
  unanswered one repeats no stream data (hyper-quic patch Q23). So a peer that has stopped reading
  is sent at most one packet again, and the charge counts the probes as small datagrams and two of
  up to the path's MTU. focal-node on Docker's default buffer: 8 of 414 fail against 13 on focal's
  `main`, 117 first windows refused against 2,285, and no peer's socket below one first window
  against 17. On macOS: 414 of 414 (`docs/transport.md` §4s). Tests:
  `tests::a_probe_timeout_after_an_unanswered_one_repeats_no_stream_data`,
  `credit::tests::a_first_window_is_taken_with_its_peers_unprompted_room`,
  `exchange::a_connection_is_taken_only_with_room_for_its_peers_unprompted_messages`.

- **hyper-transport: once MTU discovery reached the path, every unprompted datagram was charged as
  an MTU probe, and every handshake waiting in a place was charged twice.** focal's shared socket on
  macOS then reserved 9,451,456 bytes of 8 MiB for 1,020 probes and 2 acknowledgements, each charged
  as a 9,216-byte probe: the room was nothing and every dial was refused (9 of focal-node's 414
  tests failed). A sample now carries its payload, and acknowledgements and handshakes' probes,
  each of which fits the smallest datagram, are charged as the costliest datagram alone of that
  size. A handshake is counted in its place and not again as a connection. One MTU probe is counted
  for each peer that is or may be established. focal-node passes on macOS, 414 of 414
  (`docs/transport.md` §4r). Tests: `charge::tests::acknowledgements_and_probes_are_charged_as_small_datagrams`,
  `a_small_datagram_fits_the_smallest_a_path_carries`,
  `exchange::a_handshake_is_reserved_in_its_place_and_an_established_peer_as_a_connection`.

- **hyper-transport: a run-twice check of handshakes was exact only while the handshakes' sizes
  happened to agree.** A three-node check on the in-memory network ran to two digests from one seed on
  windows-11-arm (CI run 39024340172), and here 55 times in 300 (the two-node check of `exchange.rs`
  none in 300). The world's digest folds every arrival, and how many datagrams a handshake takes moved
  with draws the world does not make: QUIC's reserved transport parameter (a random identifier and up to
  15 bytes of random length, hyper-quic `ReservedTransportParameter::random`, from the endpoint's RNG,
  which hyper-transport seeded from the system) and ECDSA signatures' DER length (70 to 72 bytes). The
  endpoint now takes a seed for its QUIC randomness (`Endpoint::new_seeded`), the tests give each node
  its own, and the tests' certificates sign with Ed25519, whose signatures are of one length (RFC 8032):
  the three-node check (`room::the_nodes_connect_the_same_twice`) and the two-node one each ran 300
  times to one digest.

- **hyper-tokio `stamps`: a kernel stamp converted before the send.** Seen once in Docker on a loaded
  host (`a_datagram_is_stamped_when_it_arrived_not_when_it_was_read` at `tests/stamps.rs:103`, its
  standard-socket twin at 189). Reproduced: Docker Desktop's VM steps `CLOCK_REALTIME` every 30 s
  (0.01–4.5 ms, forward and back), and the adapter carried a Linux stamp (`SO_TIMESTAMPNS`, realtime)
  to the monotonic clock by the offset read after the receive, so a forward step between a datagram's
  arrival and its read put its stamp the step early. Before: 16 of 1,095 runs failed, 15 of them at a
  step, one at each step but the run's last. A stamp is now carried by the least offset read since
  the read before the last one that emptied the socket, and to no earlier than that read
  (hyper-tokio `clock::Carry`, hyper-rt `udp/carry.rs`, whose shard clock had the same carry): late
  by a step at most, never early. After: 0 of 1,554 runs across 20 steps. The other failure seen, a
  stamp 6.5 µs past the standard-socket test's peek, was the test bounding the carry's stated
  lateness (a preemption between its clock reads) by the peek; it is bounded by the read, as the
  plane socket's test is (`docs/benchmarks.md`, "hyper-tokio: kernel stamps across a realtime step").

### Closed (2026-10-16)

- **hyper-quic: every packet of new stream data walked the stream's unacknowledged pieces.** A
  read found its bytes by walking the pieces from the front, past every one in flight (some two
  hundred at 100,000 frames a second, acknowledgements decimated). Pieces now carry their offsets and
  a read binary-searches them; an in-order acknowledgement skips the B-tree of ranges acknowledged
  ahead. Busy core: 3,978 → 3,926 ns a message at 100,000 a second, 1,196 → 1,177 at a million
  (`docs/transport.md` §4p). Tests: `send_buffer::reads_find_their_piece_among_many`,
  `send_buffer::in_order_acknowledgements_leave_the_set_alone`.

- **hyper-transport: MTU discovery stopped at Ethernet's 1,452 bytes on every path.** quinn's
  defaults bounded the search there and told peers 1,472, so a jumbo-frame path carried a datagram a
  message as Ethernet did. Each connection's search is now bounded by the path's MTU as its kernel
  holds it, read before its first probe (`Driver::tell_ceilings`, `UdpSocket::path_payload_to`:
  `IP_MTU` on Linux and Windows, the interface's MTU under the UDP cap on macOS), peers are told the
  receive chunks' 65,527 bytes, and a window or pacer short of a whole segment sends a datagram cut
  to what it takes (`Connection::congestion_room`). On a 9,000-byte bridge a busy-core bus node found
  8,972 and took 2.2 times less time a message: 878,956 delivered a second against 419,117
  (`docs/transport.md` §4q). Tests: `udp`'s, `peer_sockets`'s, `mtud`'s and `pacing`'s.

- **hyper-transport: every lane frame was copied twice before QUIC.** `send_frame` copied a frame into
  a buffer of its own and the lane's flush copied it into the stream (1.43 % and 1.36 % of a
  saturated bus node). A frame on a lane that holds none is now written straight from the sender's
  bytes, and only what QUIC leaves waits in a buffer (`docs/transport.md` §4o;
  `Stats::frames_direct`). Test: `exchange::a_frame_on_an_idle_lane_is_written_straight_into_its_stream`.

- **hyper-transport: a frame's CRC-32C took the slow path in every release build.** The crc32c crate
  reaches its hardware path only in a build for the machine's own CPU; built for the generic
  target, it summed a 1,230-byte message in 135.5 ns and a 1 MiB batch in 98.4 µs on the
  bare-metal bench, where crc-fast, which chooses its widest carry-less multiply when it runs,
  takes 18.1 ns and 12.3 µs (`docs/transport.md` §4n). A bus node sums every message it takes.
  hyper-transport now sums with crc-fast; the values are the same. Test:
  `crc::tests::the_checksum_is_crc32c_and_goes_on_from_any_division`.

- **hyper-rt: an idle turn asked the driver twice.** A step that found the shard idle with
  readiness waits registered asked its driver at once, a zero-timeout wait, and the park that
  followed asked again in its own wait: two system calls for one answer at every idle moment, one
  of each a message for a bus node at 10,000 one-datagram messages a second. An idle turn now
  leaves the ask to the park, whose wait returns at once with what is ready (`harvest_owed`). The
  spin and `run_until_idle`, which may not park, ask first, and a caller's own `step` loop finds the
  driver asked once a quantum, as a busy turn does. Test: `idle_spin::an_idle_shard_parks_without_asking_its_driver_first`
  (41 parks, 41 asks before; none now).

- **hyper-transport: a held lane's frame was counted as waiting for memory.** A lossless frame the
  budget could not hold stayed in `Stats::frames_reserving` after its owner held its lane, though no
  memory could move it. Focal's bus node gives such frames the room of its window, so at
  1,000,000 messages a second on a busy core (the bare-metal bench) it gave the window away at
  every turn until none was left. It held 16 MB of its 1.5 GiB at the time, and told a subscriber
  a gap of 1,817 offsets (`docs/transport.md` §4m). Holding a lane now takes its waiting frame out
  of the count, and letting the lane go puts the frame back and reads it on. A retry visits only
  the connections with a frame waiting, where it took every connection out and back.
  `Endpoint::frames_reserving` reads the count without the visit `stats` makes. Test:
  `exchange::a_held_lanes_frame_waits_for_its_owner_not_the_budget`.

- **hyper-rt, hyper-transport, hyper-rt-transport: a socket's account was read twice a drain.** The
  measured room read each socket's account (`SO_MEMINFO`, a `getsockopt`) before every drain, and
  again after a drain that took one message alone. On the bare-metal bench a bus node at 10,000
  messages a second made 8.3 system calls a message, 2.4 of them these reads. A socket now reads
  its account at an epoch's drains 0, 2, 6, 14, … (`SocketCharge::wants_before`). A drop is told
  by the traffic, not by the reads: `SO_RXQ_OVFL`, attached to each datagram once the kernel has
  dropped one. Measured (interleaved A/B, `docs/transport.md` §4l): CPU a message 2–6 % less at
  10,000 and 100,000 a second and on a busy core, with p99 and p99.9 no worse. Tests:
  `charge::tests::the_account_is_read_at_doublings_of_an_epochs_drains`,
  `a_drop_the_traffic_tells_forgets_the_samples`, `udp_batched::a_drop_is_told_by_the_next_datagram`.

- **hyper-quic, hyper-transport: a third of a bus node's datagrams were acknowledgements.** Every
  connection acknowledged every other ack-eliciting packet (RFC 9000 §13.2.2's default), so at
  100,000 one-packet messages a second a node handled 3.4 datagrams a message, 1.0 of them
  acknowledgements (the bare-metal bench, `docs/transport.md` §4k). Every connection now asks its
  peer, through the acknowledgement frequency extension, for Google QUIC's decimation (quiche's
  `kMaxRetransmittablePacketsBeforeAck` 10, `kPeerAckDecimationDelay` a quarter of the round trip,
  from `kMinReceivedBeforeAckDecimation`'s 100th packet), the smoothed round trip per the draft
  (§8.1) and the reordering threshold the loss detector's (§9): hyper-quic patch Q22,
  `TransportConfig::decimate_acks`. Measured: 2.7 datagrams a message, the node's CPU a message 6 %
  less. A path's probe timeout now covers the delay asked, 1 ms on a short path, not the default's
  25 ms. Test: `tests::decimated_acks_come_from_the_hundredth_packet_every_tenth_or_a_quarter_round_trip_on`.

- **hyper-rt: a parked shard's timed wait ended at the next millisecond.** The epoll driver handed
  rustix a timespec, which rustix (without its `linux_5_11` feature) rounds up to whole milliseconds
  for `epoll_pwait`: a timer the wheel had due in 200 µs fired after a millisecond or more on every
  Linux host, whatever the kernel's timers keep. The driver now asks the kernel once for
  `epoll_pwait2` (Linux 5.11, a timespec) and, where it is refused, arms a timerfd at each timed
  wait's timeout, the wait untimed (libevent's precise timer). Test:
  `epoll::tests::a_timed_wait_ends_at_its_timeout_not_the_next_millisecond` (its ends asked only of
  a kernel with high-resolution timers; Docker Desktop's has none). The OS's coalescing is held to
  the shard's tick: the shard thread's timer slack on Linux, restored when the driver goes. macOS
  coalesced a `kevent` timeout too (a 5 ms wait ended at 6.3 ms at the median); a timed wait there
  is now a one-shot `NOTE_CRITICAL` timer in the wait's own call (5.03 ms). Tests beside it, in
  `epoll` and `kqueue`. Windows rounded to milliseconds and then to its 15.6 ms timer interrupt; a
  timed wait there now arms a high-resolution waitable timer whose signal ntdll's wait completion
  packet posts to the port, the wait's own timeout backing it (Go's netpoller since 1.23), and an
  untimed wait cancels what a timed one left. Tests: `iocp::tests::a_timed_wait_ends_at_its_timeout_not_the_clock_s_interrupt`,
  `iocp::tests::an_untimed_wait_disarms_the_timer_a_timed_one_left`; clippy-checked here, their run
  owed to the Windows CI lanes.
- **hyper-block on overlayfs: AIO reads spun for good, and a handle said it read from memory where it
  could not.** overlayfs (Linux 6.12, a container's own file system) refuses `RWF_NOWAIT` with
  `EOPNOTSUPP`, where ext4 takes it (measured: `preadv2` on the overlay and on a volume, 2026-10-16).
  An AIO submission of reads asked the flag, the kernel refused it, and the context took the refusal
  for an error rather than the flag's (it knew only `EINVAL`, a kernel before 4.14); `pump` then
  returned before putting the submission's transfers back, so `fail_waiting` found none and their
  batches were never answered: five of `tests/aio.rs` ran 14 minutes at 100% CPU each (traced: two
  `io_submit` answered `EOPNOTSUPP`, then no system call at all). The context now sends reads again
  without the flag on `EOPNOTSUPP`, and a refused submission's transfers go back to the queue first,
  so each fails typed (`aio::tests::a_submission_the_kernel_refuses_fails_its_batch`). And a
  buffered handle said it read from memory until a read found otherwise (`reads_resident` read
  neither the refusal nor, for a segmented file, any segment but the first); it now asks the kernel
  as it opens, one byte at the file's end, and says so (`segmented`'s
  `a_segment_written_whole_holds_its_fill_and_takes_no_memory`, which failed on overlayfs).

- **hyper-transport: a socket's room rested on an assumed charge.** The windows a socket's peers
  were granted were held to its buffer over a fixed four times a datagram's payload (Linux) or
  twice (elsewhere): three loopback measurements on one kernel, rounded up, and wrong both ways
  (GRO trains about 1.1 times, a full datagram 1.84, a 100-byte one about 9). The room is now told
  from the kernel's own account:
  - Linux `SO_MEMINFO`;
  - macOS `FIONREAD` (`sb_cc`), with XNU's mbuf limit as a floor;
  - the fabric's own account.

  Each socket learns from exact samples: a message read as the only one queued, whose charge
  before the read is the account and nothing after it. The worst charge per stream byte is held
  over two window epochs. Acknowledgements and probes the peers may send unprompted are reserved
  first, the release lag Linux keeps is taken off, and a drop forgets the samples.
  `SOCKET_OVERHEAD` and `socket_room` are gone (`docs/transport.md` §4i; hyper-quic patch Q20).
  Tests: `charge::tests`, `a_socket_tells_what_its_queue_is_charged`,
  `a_socket_holds_the_room_told_from_its_measured_charge`, end to end on Linux and macOS. Open:
  Windows, which exposes no account. Its room stays at the first windows until AFD's capacity is
  measured.

- **hyper-durable-e2e `kill` `member-stopped`, ubuntu-24.04 CI (`36e01cd`, run 37152765895): the
  harness's rules, not the group.** Member 1 stopped at term 2; members 2 and 3 went through three
  elections to term 5 (3 leading, commit 7) with writes of 597 and 741 ms against detections of 54
  and 107 ms, and the test's write was given up when member 1's silence reached 3.08 s against 2.88
  s excused. Two rules of the harness decided it. A member silent past its excuse ended every wait
  at once, whatever else moved, though the write needed only the two members up, which were moving.
  And every ask waited one second, the operation then asked again with a new ask, so a commit slower
  than a second, as one through writes of that length is, was never answered: each answer came after
  its ask had been given up. A scratch fault (not committed) holding every flush of the two members
  up 741 and 597 ms more from the stop reproduced it: on `main` 3 of 3 failed with CI's message on
  macOS, no election, the write in both logs and not committed when the wait gave up; with only the
  first rule changed the write was never answered, the leader committing it 29 times more in 85 s.
  Now a member silent past its excuse ends at once only a wait on every member's own facts
  (`Waits::OnEvery`: what each applied, a line one prints); a wait for what the group answers
  through whichever members lead (`Waits::OnGroup`: a write, a read, a leader) waits on it no
  longer, goes on while the members heard move, and fails naming it once they are quiet; an
  operation asked again waits twice as long as its ask before, to sixty seconds (RFC 6298 §5.5,
  §2.5); and a member's silence runs between the sendings of its asks (`3363c72`; `docs/timing.md`
  §2.9, "A member stopped while the others' writes are slow";
  `a_silent_member_ends_a_wait_for_the_group_only_once_the_members_heard_are_quiet` fails on the old
  rule). Under the same fault the scenario passed 4 of 4: the write answered in 11 and 16 s with the
  links young at the stop, and in 26 and 91 s with every pair judged before it. That answers the
  question this entry left open: with every pair judged, a device whose every write is that slow
  makes the pair re-elect, from term 4 to 6 and to 11 in the two runs, each new leader's writes
  suspected in turn until their detectors had the device's evidence (detection from 0.4–0.7 s to
  7–17 s); every election moved the group, within the members' law. With the links young, the fault
  found the open defect above. Both scenarios soaked on 2026-10-16 (`docs/benchmarks.md`, "The two
  E2E sightings soaked, and a member stopped while the others' writes are slow"): on `f116ac2`, 100
  runs of each in Docker's Linux at each of one, two and four CPUs with twice as many busy loops
  (load 5.8–32.4); on `3363c72`, 50 of each at each count there (load 6.3–22.8) after
  hyper-raft-e2e's, hyper-durable-e2e's and hyper-multilog-e2e's binaries whole in each container,
  50 of each on macOS (load 9.7–52.2), and on CI's six targets for 90 minutes, 58 to 388 of each a
  target, 1,451 in all (diagnostic run 38850875933); every run passed.
- **hyper-raft-e2e `stalled-devices`, macOS (2026-10-03, the tree of 9893679): the stream's `G`.**
  After the 7 s stall the old leader had stepped down in its term, its followers still trusted it,
  and no member campaigned through the quiet period (2.6 s): no member took a heartbeat in that
  time, every count the same in two looks, while every member answered the test. The core elects in
  every order of suspicion and trust a simulation tried (1,200 orders, and 200 seeds × 16 orders
  with an entry in flight), so the cause was outside its rules, and the fixes that landed hours
  after it change exactly that path: with no `G` the stream refuses every heartbeat as unmeasured,
  so nothing is taken or judged, and a wait an arrival ended now counts toward it (9588f45); and an
  owner's own stall no longer inflates `G` (f498c63, which measured this scenario taking it to
  seconds). Since then a suspicion a member took after a drive is driven before it waits
  (`6d76f3c`), and the waits' law covers the interval each member's own heartbeats go at
  (`8978db4`), which a stall of every device raises. Not seen since: 120 runs on `line` and 40 of
  9893679's own tree on macOS (2026-10-05), 30 in Docker's Linux at 2 CPUs, 28 there on 2026-10-15;
  and both scenarios soaked on 2026-10-16 (`docs/benchmarks.md`, "The two E2E sightings soaked, and
  a member stopped while the others' writes are slow"): on `f116ac2`, 100 runs of each in Docker's
  Linux at each of one, two and four CPUs with twice as many busy loops (load 5.8–32.4); on
  `3363c72`, 50 of each at each count there (load 6.3–22.8) after hyper-raft-e2e's,
  hyper-durable-e2e's and hyper-multilog-e2e's binaries whole in each container, 50 of each on macOS
  (load 9.7–52.2), and on CI's six targets for 90 minutes, 58 to 388 of each a target, 1,451 in all
  (diagnostic run 38850875933); every run passed.

### Closed (2026-10-15)

- **hyper-rt-transport: a peer's socket carried two datagrams a system call.** Found by focal's
  event bus, which carried 45,000–56,000 messages a second at every rate offered past 100,000, where
  NATS carried 250,000. Two bounds caused it:
  - the endpoint asked hyper-quic for one datagram at a time;
  - each peer's socket batched two datagrams, because it held its own 64 KiB receive buffers.

  Now, as quinn's driver does:
  - the endpoint makes up to 64 of a connection's datagrams at once
    (`Endpoint::poll_transmit_batch`);
  - the socket sends them from the driver's buffer, segmented where the kernel segments
    (`Batched::send_segments`);
  - every socket receives into one ring the driver lends it (`Ring`, `Batched::receive_with`);
  - a transmit a socket took only part of is held until the socket can write, nothing sent past it.

  On Linux the bulk test's peer socket sent 1,459 datagrams in 92 calls (`docs/transport.md` §4h).
- **hyper-rt-transport: a drain read whatever its peers kept sending, the driver's sends held
  behind it.** A drain read each socket until it emptied or made `TURNS` calls (up to 1,280 messages
  at focal's batch of ten). It sends nothing while it reads, so a peer that kept sending kept its
  own credit waiting. With batching, focal's publisher was held for up to 12 ms at a time, and its
  p99 at 10k/s rose from 0.21 to 1.00 ms. A drain now takes what was queued when it began
  (`Batched::receive_queued`): by the kernel's receive stamps where the socket has them, else by the
  bytes it held (`FIONREAD`, which Winsock and the BSDs count over every queued datagram).
  Tests: `a_queued_receive_takes_what_was_queued_when_it_began`,
  `a_socket_without_stamps_takes_the_bytes_it_held` (`docs/transport.md` §4h). hyper-tokio's driver
  had the same drain to empty, and now takes the same rule. It binds its socket with stamps and
  counts `FIONREAD` on Windows; its test holds Linux, XNU and Winsock each to the count their
  sources state.
- **hyper-rt-transport: a peer socket left holding datagrams waited for the driver's next timer.**
  Readiness is one-shot and fires on arrival, and a socket marked for the next turn was not counted
  as work, so a driver with only that to do parked. A marked socket now keeps the turn loop going.
  Test: `a_peer_socket_left_holding_datagrams_is_drained_without_another_wake`. A poll that ends
  pending without waking itself (`Driver::yields`) leaves no socket holding datagrams
  (`Driver::backlogged`). Under the old rule it parked with 128 of 257 read; 40 of 40 runs pass
  with every core busy.
- **hyper-rt: a socket whose segmented sends the kernel refused with EINVAL dropped every one.** The
  batched send turned segmentation off on EIO alone. EINVAL, which the kernel returns to a socket
  sending without checksums and which quinn-udp has met from drivers, was counted as a refusal each
  time, so the connection lost every batch until it timed out. A segmented message refused either
  way now turns segmentation off and goes again a datagram a call. Test:
  `a_socket_the_kernel_will_not_segment_for_sends_each_datagram_alone` (`SO_NO_CHECK`), in which ten
  datagrams were all refused before the fix.

- **hyper-transport: a stalled reader's socket dropped what its windows let peers send.** Found by
  focal's event-bus comparison (a node on one CPU, Linux 6.x in Docker on a loaded host): at 50,000
  messages a second the node's socket counted 1,202 `RcvbufErrors` in 25 s (69 at 10,000), its
  clients' 583, each loss a retransmission and a congestion event, the run's tail. The cause: the
  windows an endpoint grants are what peers may send before its owner reads, and they wait in the
  socket's queue while the owner does not, but nothing tied the windows to the queue: the bus
  granted windows to 8 MiB on a socket Linux had left at its default 208 KiB, and a reader the host
  stopped for 3.5 ms overflowed it (TCP's receive window is its kernel's queue; UDP has none). Now
  the windows granted, summed, never pass what the queue holds (`credit`'s socket room): each driver
  (hyper-rt-transport's, hyper-tokio's) asks its socket for the endpoint's `receive_demand`
  (`SO_RCVBUFFORCE` where the process may, else `SO_RCVBUF`, a refused size halved) and tells the
  endpoint the room the kernel granted (`socket_room`: the buffer reported less the kernel's
  overhead, measured on Linux 6.x at 895, 1,275 and 2,290 bytes a datagram by size class, at most
  four times the payload from 320 bytes up); a window grows only while what is left holds the first
  windows of the handshakes admission lets wait, and a connection whose first window does not fit is
  refused `Refusal::ReceiveBuffer` (wire code 15). Tests: hyper-transport's
  `a_socket_that_holds_no_first_window_refuses_the_connection` and
  `the_windows_granted_never_pass_what_the_socket_holds`, hyper-rt-transport's
  `a_socket_holds_the_room_the_transport_counts_in_it` (full packets sent unread up to the room,
  every one read back, on macOS and Linux). One shared socket of Linux's default 208 KiB holds about
  three connections' first windows, so where a port can be shared (Linux, the BSDs) each connection
  the endpoint identifies now receives on a socket of its own, bound to the listener's port and
  connected to its peer (`UdpSocket::bind_shared`, `connect`; measured on both: the connected socket
  takes its peer's datagrams, the listener everyone else's), its windows funded from that socket's
  room alone (`Endpoint::set_peer_room`): a node's receive capacity grows with its connections at the
  kernel's default each, nothing tuned, no privilege asked, as TCP's sockets do. The driver matches its
  sockets to the endpoint's connections whenever they change (`connection_generation`,
  `established`); Windows and the simulation fabric keep one socket. Test: hyper-rt-transport's
  `each_connection_receives_on_a_socket_of_its_own_and_lets_it_go_with_the_connection`.
- **hyper-transport: a lane frame the budget could not hold was skipped, even where it was its
  sender's only copy.** A lane skips (and counts) a frame its class bound or budget cannot take, so one
  full range never stalls a shard's stream: right for consensus messages, which Raft sends again,
  and a silent loss for focal's event bus, whose messages a lane carries once. A class may now be
  lossless (`Classes::lossless`): its frame waits for the budget (`Reading::Reserving`), its lane
  unread so QUIC's flow control holds the sender, and asks again whenever bytes come back
  (`Endpoint::release`, a timeout, or `retry_reserving` from an owner whose budget freed elsewhere);
  a frame past its class's bound is still refused. `frame_grant` states what an outgoing frame asks
  of the budget (the frame and its prefix), from which an owner sizes its buffers: focal's bus had
  sized its largest at its batch bound, and a batch at the bound, plus the prefix, was refused
  `Budget`. Tests: `a_lossless_frame_waits_for_the_budget_and_is_never_skipped`,
  `a_lossy_frame_the_budget_cannot_hold_is_skipped_and_counted`.
- **hyper-transport: a lossless lane held seconds of frames behind QUIC.** A lane bounds its queue
  in frames, the core's window, and a frame may be a class's bound: for focal's bus 256 frames of
  up to 256 KiB, 64 MiB behind a path that holds about 100 kB, so a publisher past what the path
  carried met 2.6 s of delay at the median before its sender was held. A lossless class's lane
  first refused `LaneFull` past the connection's congestion window (TCP Small Queues' rule), but a
  loss-based law's window on a path that loses nothing has no bound: the bus runs CUBIC, whose
  window on loopback reached 1.55 GB, and a publisher the node held queued 470 ms of messages in
  its own lane. The lane now holds one frame beyond what QUIC took: QUIC's stream holds what the
  peer's credit allows, a socket's send buffer whose writer waits once it is full
  (`docs/transport.md` §4). Tests: `a_lossless_lane_holds_one_frame_past_what_quic_took_and_its_sender_waits`
  (what QUIC took and one frame, where a lossy lane takes its 128-frame window and three);
  `a_lossless_frame_waits_for_the_budget_and_is_never_skipped`, the sender held while its receiver
  polls nothing, every frame arriving once it does.
- **hyper-quic: BBR's model diverged from Chromium's in four places.** Found while measuring focal's
  event bus, whose publisher's window reached 409 MB on a 72 µs path; that window was CUBIC's, the
  bus's law, whose lane bound is now one frame (above), but BBR, focal's law elsewhere, had the
  same unbounded growth in an app-limited startup. The port's BBR diverged from Chromium's in four places
  (`crates/hyper-quic/VENDORED.md` §30):
  - its startup compared the gain with the target;
  - its bandwidth filter took no app-limited sample;
  - its minimum round trip expired before it was ever measured, so every connection began in
    ProbeRTT at four datagrams;
  - and its bandwidth estimate divided bytes by the gaps between unrelated acknowledgements and
    sends: 357 MB/s on a 12.5 MB/s pair, a window 65 times the path's product.

  Each path now keeps delivery-rate samples (draft-cheng-iccrg-delivery-rate-estimation; Linux's
  `tcp_rate.c`): a 16-byte snapshot a packet, and the sent packet's record stays within its 128
  bytes. BBR takes them through `Controller::on_delivered`, in Chromium's order. NewReno, CUBIC and
  Copa wrote the same 495 rows of slates' bake-off byte for byte. BBR's goodput shortfall went from
  1.075 to 1.020.

  Open: BBR states no pacing rate. A sender its window holds therefore keeps about a product of queue
  (ping p99 about twice the round trip); Chromium paces at its model's rate.
- **hyper-transport: a consumer that could not take more queued its producers' frames without
  bound.** A bus node whose subscriber fell behind took every publish into memory, its subscriber's
  lag growing for as long as the publisher outran it (2.5 s at the median). `Endpoint::hold_lane`
  reads a peer's lane no further past its opening, so the peer is held at QUIC's flow control and
  its own lane says `LaneFull`; `release_lane` reads on from where it stopped. A hold is the peer's,
  at most one a lane it may open, so a lane not yet opened is held from its opening. Test:
  `an_owner_that_holds_a_peers_lane_holds_the_peer_and_loses_nothing`.

- **hyper-durable-e2e `founder`/`founder-fenced`: a read a sole voter held for good.** CI run
  38523559088 (ubuntu-24.04-arm, `71992ae`: "founder: an answered write of before/3 did not read
  back"), and twice in a soak (runs 11 and 719, the second's report: "reads 1 waiting, 0 confirmed, 1
  held by its core, 0 refused, its term committed true", no write out, no deadline). The cause was
  the member's loop, in all three harnesses: after a drive it reads its socket for what came while
  it drove, then waits until its replica's deadline or its stream's wake, and drives only after the
  wait. A read that read took was answered by the core into its `Ready` and waited for the wait to
  end, and a sole voter has nothing timed: it waited for the next datagram, and the test's ask had
  given up. A member that took anything since it drove, or handed its replica a word of its stream,
  now drives before it waits (`docs/durable.md` §2.2). Each harness's `taken-while-driving` holds its
  one member where a drive ends (`stream::put_pause`), sends it a read and lets it go: on the old
  loop the read was never answered (4 of 4 in hyper-durable-e2e, its report the sighting's, "reads 1
  waiting, 0 confirmed, 1 held by its core"; 2 of 2 in each of the others), and now it is at once.
- **A refused write's acknowledgement released over a log cut back since (I2), hyper-durable's
  simulation at 10,000 seeds (CI run 38774873412's explore, on `6d76f3c`).** Seed 3828 of its
  second shape: "member 1's MsgAppendResponse: Durability { member: 1, rule: I2, at: 46, held: 41
  }". Traced step by step: member 1 took a `Ready` whose write held entries 42 to 46 of term 1 and
  its acknowledgement of 46; the log refused that write; before the refusal was taken, member 2's
  append of term 2 cut member 1's log back to 41. The shell made the refused writes again as one
  write of what the core held, to 41, with the refused write's messages, and the acknowledgement of
  46 left over a log made durable to 41. A refused write's messages now leave with the write made
  again only while the core's log still holds the entry it ended at when that write was laid out
  (`Out::tip`), and are dropped otherwise, as the network may drop any message
  (`a_refused_writes_acknowledgement_of_entries_cut_since_never_leaves` fails on the old shell:
  "member 3's entries, cut away, were acknowledged to it: [(3, 3), (2, 1)]"; `docs/durable.md`
  §2.4). The seed's schedule reaches the window only on the sole voter's new campaign rule (60,000
  seeds of each shape passed on the old core); with the fix 60,000 of each pass.
- **A sole voter campaigning at every wake (since `14a014f`, I9, before it reached `main`).**
  `taken-while-driving`'s member, the first E2E group of one, reached term 1,232,434 before it led:
  its vote counted only once durable, and by suspicion a sole voter campaigned at once whenever it
  was woken, so each wake before its vote's write was durable moved its term past that write. It now
  campaigns once a term and waits for its own vote's write with nothing armed
  (`a_sole_voter_waits_for_its_own_vote_and_campaigns_once`, which fails on the old core at its
  first wake; `docs/durable.md` I9).
- **hyper-durable-e2e's founder at its start, no leader: the waits' law short of the members' own
  heartbeat interval.** A soak of the scenario in Docker's Linux failed at run 771: "a write was never
  answered; nothing moved for 1s". Both members were at term 0 with no heartbeat taken and no
  interval known, so both stated a law of nothing and the wait's floor, one retransmission timeout,
  was its quiet period. Member 1's one write had taken 2.17 s before the wait began; a member's
  heartbeats go at its flush floor, so its went 2.17 s apart, and a member takes no heartbeat until
  its own stream's wake has measured `G`. `quiet::law` now takes the interval each member's own
  heartbeats go at (its report's `sending_interval_ns`), the most its peers' evidence of it goes
  without moving, which no peer can state until a heartbeat at it has come
  (`a_members_law_covers_the_interval_its_own_heartbeats_go_at`).

- **Two leaders of one term in hyper-durable's simulation (a safety defect of the core under R-4).**
  The simulation's default 128 seeds never reached it; at 256, seed 206 of its first shape broke
  Election Safety ("member 1 led term 3: TwoLeaders { term: 3, first: 3, second: 1 }"), on `main`
  as on the branch. Traced step by step: member 3, the one voter of a configuration its log held
  and had not written, campaigned, counted its own vote at once and led term 3 with its hard state
  and that configuration in writes still out; it crashed with them unwritten, restarted at term 2
  under the configuration before, voted for member 1 in term 3, and member 1 led term 3 too. What a
  leader does already waited for its term and vote to be durable (I1, I3, reads at the term's first
  commit), so nothing had left member 3. A candidate's vote for itself now counts once the hard state
  naming it is durable, as etcd's core counts it (`docs/durable.md` I9, `docs/raft.md` §3.3):
  `a_sole_voter_leads_only_once_its_vote_is_durable` fails on the old core; 1 seed in 1,000 broke it
  before, none in 10,000 of each of the five shapes since. A sole voter leads after its first write:
  an owner that proposes to it before driving that write is refused `NoLeader`.
- **hyper-rt's record test, `MeasurementTimeout { probe: "wake" }`, windows-2025 (main's run
  38552220675 on `ccdbbd8`, and 38376252976).** The refusal 627c176 left for a probe that kept no
  wake: diag run 38756344126 traced every round of the machine tests' probes, run at once as the gates
  run them, over 120 runs of the library on windows-2025, and 349 of 4,256 rounds ended with their
  first wake answered 8 to 21 ms into an 8 ms share, kept nothing, since a round gave its wake up at
  its share's end (176 rounds so on windows-11-arm). A round now waits for the answer to a wake it
  began until the probe's span ends, and keeps it (`a_wake_slower_than_its_rounds_share_is_kept`
  fails with the share's bound): rounds that kept nothing fell to 5 and 21 of about 4,300 (diag run
  38757539787), each one whose waiter never parked or started, and the probes' time stayed within
  their span (p50 341 against 315 ms, p99 513 against 552 ms, windows-2025; `docs/runtime.md`
  §10.4).
- **hyper-rt `a_wait_stepped_past_its_deadline_reports_the_lateness`, macos-15-intel (CI run
  38445610795).** The test stepped its shard until idle and read the sleep's deadline from the last
  step; a host that held it 5 ms between two steps let the later step fire the timer, and the test
  lost its premise. It now takes the deadline from the step that armed it (10 ms held after a step:
  the old form fails every run, the new passes every run).
- **hyper-rt `a_poll_blocked_in_a_call_past_the_quantum_is_its_tasks_where_blocks_are_counted`,
  macos-15-intel (CI run 38336430915).** The test counted a run on the CPU only where a poll's own
  body ran past the quantum, while the shard judges a long poll over a window of its own readings,
  which a guest that counts a stolen virtual CPU as the thread's can fill outside the body. The test
  now holds each step's verdicts to the thread clock over the step and the one before it, every
  window it can have judged in (with 2.5 ms of CPU spent after each held poll, the old count fails 3
  of 3 and the new passes 3 of 3; with `attribute` planted to call every poll a run, the new fails).

### Closed (2026-10-13)

- **A hyper-rt task that woke itself held its shard.** The loop's bench against slates' runtime and tokio
  (`crates/hyper-rt-compare`) put eight clients' wait beside a task computing in 50 µs slices at 0.5–1.0
  ms at the median, where slates' waited 70 µs and tokio's 18 µs. Yielding or waking itself, the task
  was listed again at once and polled again until the step's count ran out, while the clients' wakes
  from other threads waited in the bitmap. Now such a task waits for the loop's next reading of its inputs, and a
  batch whose local handoffs have run the step's budget reads them again mid-step
  (`docs/runtime.md` §3.3). Beside a yielding task the clients wait 60 µs, and 56 µs beside one that wakes
  itself (tokio's own wait there is 11.8 ms). Every other case is level with before except a yield,
  469–475 instructions against 423–436. A shard that never parks no longer reads its thread's clock at
  every step after one long poll armed it (3.3% of a yielding shard's samples): at most two readings a
  long poll (`attribution::Tracker`, `tests/thread_account_reads.rs`).
- **hyper-rt's calibration took one burst for the whole run, and refused on a loaded host.** One of
  mantle's benchmark calibrations read a 227,774 ns mean wake where the host's others read 1,444–5,904 ns
  (`benchmark-results/mantle-contig-20261013`, `n10000000-r111-mcs`). That one reading set the quantum's
  prior, the tick, the batch and the control depth, and a window of 2¹⁴ wakes that left the shard's
  quantum at 228 µs for the run. Separately, a 40 ms probe on windows-2025 refused `MeasurementTimeout`
  short of the stopping rule's floor (CI run 38376252976, the record test). The run's 3.5-times slower
  fill was not these values' doing: with them frozen in the same binary, it filled at 2.26–2.29 M puts/s
  against 2.27–2.44 M (`benchmark-results/hyper-rt-calibration-20261013`); the host's merges took 5.27 s
  against 1.7–2.6 s. The probe now reports the median of its rounds' means, the shard's spread is the
  rounds' median coefficient of variation, and a shard tracks the median of its last five blocks' means,
  so its own wakes outvote the boot's after 36. A probe short of the floor reports what it measured,
  `quick` (`docs/runtime.md` §10.4; `machine::wake`'s tests).
- **hyper-seal §11: the log sealed by default.** Group commit in an open loop, a plain, a second
  plain and a sealed log at once on the device, at 100 B to 16 KiB entries and 25–75% of their
  capacity, load 22–40: at the median sealing costs nothing a run resolves (0.964–1.043 of the plain
  log, a second plain log 0.973–1.041), its tails within what the host makes of two identical logs;
  docs/seal.md §11's rule, fixed before each run, holds. hyper-log's and hyper-durable's end-to-end
  suites now run sealed logs, every kill and stall scenario passing (`docs/seal.md` §12).
- **hyper-quic closed a connection under its own sender's losses.** slates' congestion bake-off on
  the harness found NewReno and CUBIC carrying nothing at 100 Mbit/s and 100 or 300 ms with no random
  loss: a slow start's overshoot lost every other datagram of a round trip at the drop-tail queue, and
  the receiver refused its 1,025th out-of-order span, upstream quinn's fixed bound, with "too many
  gaps in stream buffer". The bound now follows the window the peer holds, the spans' own memory
  within it (patch Q15, `crates/hyper-quic/VENDORED.md` §26); those runs carry 96–112% of the link.
  The bench against upstream quinn-proto found the copies of the handshake's flights scanning every
  packet in flight at every transmission, a MiB at 1.58 times upstream's instructions; now 1.07
  (§27).

### Closed (2026-10-12)

- **hyper-block `a_segment_written_whole_holds_its_fill_and_takes_no_memory`, ubuntu-24.04-arm
  (CI run 38348043505; the test is on `main` since `55d923e`).** It read each piece of a segment
  written past the cache with a resident read to show the piece was not in memory, and on Linux that
  read is no probe: `preadv2(2)` with `RWF_NOWAIT` on a page not cached starts read-ahead of it and
  of the window after it before it answers, and reads what that brought in by the time it looks
  (Linux 6.8 `mm/filemap.c`, `filemap_get_pages` and `filemap_update_page`). In Docker's Linux 6.12
  under busy loops it failed 1 run in 300, and `mincore(2)` before and after each read showed
  both ways: a piece not in memory read by its own probe, and the next piece of the same segment in
  memory before its probe, brought in by the read-ahead of the one before. The test now asks the
  page cache itself on Linux (`resident/linux.rs`, `mincore(2)` over a mapping, tests only, `1738d0e`):
  0 failures in 1,500 runs under the same load (hyper-block `ORIGIN.md` change 15).
- **A link's evidence moved up by a stall's bursts (hyper-liveness; found with the windows-2025 tail).**
  A link refused for want of `τ_int` asked the interval its first qualified Allan level said its
  heartbeats would be independent at, and a stall's burst inside that level read as a correlation of
  the whole history: in round 6 of diag run 38317208076 one link went 874 ms → 4.8 s → 9.4 s, its next
  56 heartbeats 9 minutes away, and each move started its levels again. A refused link now waits at its
  interval for a longer level: `T_c` costs about `42·T_c` of time at any interval, so a longer one only
  sends fewer heartbeats. Measured on the simulation's worlds against the moves (4 worlds, 32 seeds,
  384 links each): every link configured either way, at the same median, the slowest sooner (macOS
  5.1 → 1.9 s, Linux 5.7 → 1.4 s, Windows 24.5 → 8.5 s), mistakes against the allowance as before; an
  AR(1) link of 199 ms correlation at 1 ms configures there within 1,201 heartbeats over 64 seeds. The
  unseen share keeps `τ_int`: under clustered lateness a record's chance depends on the clusters' shape
  (1/n for a max-autoregressive chain whatever its extremal index), so the extremal index is no
  better-founded count (`docs/timing.md` §2.8, "The interval the evidence needs").
- **hyper-liveness `processes` on windows-2025, its tail (CI run 38288571905: the gates job passed its
  64 minutes inside it).** Each pair's configure outcome in the members' state lines, the three tests at
  once as three processes, 10 rounds (diag run 38317208076): in nine every pair configured in 4 to 30 s;
  in round 6 the members' first flushes took 0.7 to 3.1 s, and the interval, which followed the floor up
  and never down, kept them after the floors fell to 33 to 118 ms: links at 0.65 to 2.2 s measured
  `τ_int` there for 38 to 193 s and moved up from there for their evidence, the slowest configured after
  357 and 561 s, and one not in 720 s. The floor is now followed down once the flushes it is the mean of
  have doubled (`d06a949`, `docs/timing.md` §2.8). With round 6's start made every round's (each member's
  first three flushes 2.0, 1.0 and 0.5 s slower), 12 rounds a runner: on main every pair configured in
  116 to 447 s on windows-2025 and 116 to 349 s on windows-11-arm (diag run 38322890505), with the change
  in 30 to 72 s and 25 to 26 s (38322886713). The Server quantum measured on the way (`Win32PrioritySeparation`
  0x2, 12 clock intervals; a sleeping thread woken 105 to 107 ms late at the median beside 16 spinners,
  diag runs 38305342723 and 38306001723) slows a member's wakes, and is not this tail's cause.
- **hyper-quic `tests::pad_to_mtu`, windows-2025 CI (run 38316593172, the client sent nothing,
  "blocked by congestion control"), and on `main`'s own runs: `e513477` on macos-15 (run 38314019362),
  `aa82ada` on ubuntu-24.04-arm and windows-11-arm (run 38316408813), each green on its branch's run.** With `pad_to_mtu`, a packet of acknowledgments alone was padded:
  in flight against the window (RFC 9002 §2, §3), never checked against it, and acknowledged by the peer
  only along with an ack-eliciting packet (RFC 9000 §13.2.1). The server's warm-up drew twelve of them,
  and the client's datagrams waited behind them; how many depended on the packet numbers the server
  skipped at random. Only ack-eliciting packets are padded now (hyper-quic `VENDORED.md` §25; the test
  `padding_to_the_mtu_leaves_acknowledgments_unpadded_and_out_of_flight` fails before it).
- **A shell group stopped and opened again at once failed `Compacted` (focal, on `aa82ada`; focal-node
  on Linux, 3 to 4 runs in 10 of one test).** hyper-log released a dropped handle's group at once, while
  writes it had sent could still be out; a claim between was given the group without them and never heard
  of them, so a compaction among them left the new handle reading what the log had let go. The release now
  waits for those writes' answers, and a claim meanwhile waits for it (hyper-log `ORIGIN.md`, "A group
  claimed again waits for its dropped handle's writes"; hyper-durable's
  `a_group_stopped_with_its_compaction_out_opens_again_at_once`, focal's case whole).
- **hyper-rt's calibration tests, `MeasurementTimeout { probe: "wake" }`, windows-2025 CI (`1029f76`,
  run 38021030068).** The machine tests' probes ran at once, their waiters pinned to the same cores, and
  a waiter took p50 1.2 ms, p90 20 ms and up to 149 ms to start where a round's share of the 40 ms budget
  was 8 ms; the start was charged to the share, so 607 of 2,067 rounds kept nothing, and once every round
  of three probes did, each refused (diag run 38266052185). A round now times wakes from its waiter's
  start, the start waited for within the probe's span: 41 of 2,145 rounds kept nothing on windows-2025 and
  53 of 2,138 on windows-11-arm, each a first wake slower than its share on a busy core, and no probe
  refused (diag run 38268800203; `crates/hyper-rt/src/machine/wake.rs`).
- **hyper-rt `an_astray_report_is_counted_not_an_error` took a datagram, windows-11-arm CI (`67f038d`, run
  38102324750).** windows-11-arm hands out ports one after another, but the next bind can get the port a
  socket just released: in 2 of 300 runs the socket under test was given the port the test had closed, and
  the datagram reached it (diag run 38275004157). The socket is bound first and the test is a binary of
  its own (`tests/udp_astray.rs`). Its failing assertion inside the root then left the binary waiting:
  `block_on` waited for good on a root future that unwound, since tasks' polls run behind the shard's
  boundary (0784bb2); it now ends refused, `RtError::RootUnwound` (`docs/runtime.md` §4).
- **hyper-log `a_waker_that_unwinds_costs_only_its_wake`, ubuntu-24.04 CI (run 38269518082, 4 wakes
  against 5).** A caller's wait returns once its answer is in its port and the answering thread wakes its
  waker after that, so the count read after the waits could miss the last wake; it is read once the log's
  threads have ended.

### Closed (2026-10-05)

- **hyper-durable-e2e `kill-durable-leader`** (Linux Docker, 2 CPUs, load 6.8): the survivors'
  writes out 2 s against a longest finished write of 58 ms, and the wait judged a member stuck past
  that write and the quiet period. The cause was the wait's rule, not the group: one flush does not
  bound another on a contended device. 8594bf0 (09067dd on its branch) judges a member with a write
  in progress only past the kernel's own bound on a flush (`device::FLUSH_BOUND`, 60 s), its unit
  test pinning both sides. Since: 30 runs in Docker's Linux at 2 CPUs and load 6.4 to 7.4, 30 runs
  on macOS under 28 CPU burners at load up to 43, every gate on every target.
- **hyper-raft-e2e `member-stopped`, windows-11-arm (`1967cd9`, run 37152466424).** Member 2 did
  not campaign because its detectors suspected both other members: member 1, stopped, and member 3, its
  leader, held 5.1 s inside one flush. A member campaigns only while it and those it trusts are a
  quorum (`watch.rs`), and member 3, inside its flush, could vote for no one, so no election could
  run until the flush returned; the core did what its rule says, and this depends on neither the
  configuration-safety change. The test failed because member 3's silence inside that flush
  (3.0 s) passed its excuse (1.04 s, the longest write reported and the quiet period). On `line`
  the wait judges a silent member only once the device answers the test's own flush faster
  (2ef0ab3), and a member in its write by the kernel's bound (8594bf0). Since 2ef0ab3: 40
  windows-11-arm gates passed, none failing in an E2E scenario.
- **The access violation `0xc0000005` of an E2E member on windows-11-arm.** Three accounts from the
  fault handler, all on 2026-10-04 before c3406db (runs 37204898614, 37208590794, 37209556561),
  fault in `hyper_durable_e2e::node::Node::drain` writing `0x10e` through a stack slot that held 14:
  an I/O status block (STATUS_SUCCESS, 14 bytes, the length of a report ask) written into drain's
  reused frame by a timed peek (`SO_RCVTIMEO`) that Windows cancelled as the call returned and that
  completed after it. The invariant broken: an operation the kernel may still complete must not
  have its buffers on a frame the call has left. c3406db waits by `WSAPoll`, which writes nothing
  past its return (`hyper_measure::wait::arrives`); its loop ran 361 iterations on windows-11-arm
  without a fault. Searched since: every failed or cancelled run from 2026-10-04 08:00 UTC to
  2026-10-05 (38 runs, `gh run view --log-failed`), and 40 windows-11-arm gates passed since
  2ef0ab3: no other account.

## Consumers

| Consumer | Takes | State |
|---|---|---|
| mantle | on its `main` (`9ccd216`, a fast-forward from `f3deb05`) all ten crates it takes vendored at `80c1f82` (`vendor/*/SNAPSHOT`), no local delta, every vendored file outside the self-contained manifests equal to `80c1f82`'s (338 files); its gates pass and its CI is green on its nine jobs, the six targets, linux-aarch64-1cpu and linux-x86_64-2cpu-busy (run 38844914795); its range suites pass, a sole voter leading after its vote's write through hyper-durable's replica with no mantle change | its range replica is hyper-durable's `Replica` with its engine and layer as the state machine (D-1), electing on ticks; elections by suspicion wait for a node owner that carries the liveness stream (mantle `docs/design/node.md` §2.4) |
| focal | every crate it takes vendored at `80c1f82` on its branch `hr-vendor-80c1f82` (`eb6ce081` on its main's `95294367`; focal's gate exit 0, 167 test binaries, 3469 passed, 31 ignored), three of its tests draining until a sole voter leads (I9), which its session lands under its own rules; its `main` vendors twelve at `5007f44` and hyper-transport at `81af6b3`, both on `main` since `3d86b42` | the core (step 1) and the transport (step 4: hyper-quic, hyper-transport and hyper-rt in place of quinn and tokio) switched; its storage on hyper-log and the shell (F-1, D-2) built behind `STORAGE_LEVEL`, its `CAPABILITY_LEVEL` still 2; timing and liveness (step 3) open, its shell electing on ticks |
| slates | hyper-datagram, hyper-seal, hyper-swim and hyper-timing vendored at `80c1f82` on its branch `hr-vendor-80c1f82` (`5f2a76d4` on its main's head `b85b37ea`; fmt, its structural and literal checks, clippy on slates-cluster, its library's 213 tests and the member plane's 10 pass), one commit touching only `vendor/hyper-raft`, which its session lands on the owner's word; its main vendors them at `aa82ada`, their sources the same but for hyper-timing's `independent_interval`, which `43aec4a` removed and slates calls nowhere | SWIM (S-1) and the datagram plane (P-1) switched; its consensus onto hyper-raft (X-1) and its session plane onto hyper-transport (X-2) open, slates' runtime onto hyper-rt slates' own step |

## What remains (2026-10-16)

In the owner's order: red CI first; then every consumer on one revision of `main`; then the shared
crates' open work; then each consumer's switch-over (mantle note 32 §5, each project's own order).

1. **Red CI.** The open defects above, each traced to its cause and fixed at it, and every target
   green on `main` (green since `3d86b42`, 2026-10-15).
2. **One revision for every consumer.** mantle is on `80c1f82` on its `main` (`9ccd216`); slates'
   branch takes `80c1f82`, which its session lands (the Consumers table above). Merged on
   2026-10-16 at the owner's word, `focal-bus-receive` brings the event bus's transport, which focal
   vendors at this merge: hyper-rt (each connection's own socket, batched receives that end on a
   short batch, precise timed waits on Linux, macOS and Windows, a shard's counters), hyper-rt-transport
   (a connection's batch in one call, one ring of receive buffers, a drain of what was queued),
   hyper-transport (the measured socket room, one CRC pass a frame, the lossless lane and its hold),
   hyper-quic (BBR's delivery-rate bandwidth, the sent-packet deque, decimated acknowledgements),
   hyper-tokio (its drain) and hyper-block (overlayfs reads). mantle and slates take it as landings
   of their own; their suites have not yet run against it. What `main` took besides since `80c1f82`
   changes the end-to-end harnesses, hyper-durable's depth bench and the documents.
3. **The shared crates' open work**, each in its own document. The socket room's (`credit.rs`) is
   done (2026-10-17, `docs/transport.md` §4t): a window that grew stayed granted while its peer was
   idle, since QUIC takes back no credit it gave. When a growth or a first window is refused for the
   room or the budget, an idle connection's grown window is set back to its first, what the growth
   held going back as the peer's reads pay off the credit already given; one that stays idle a quiet
   period after, still owing what is wanted, is closed (`Refusal::ReceiveBuffer`). On the
   in-memory network the active peer's window grew from 24,000 to 48,000 bytes where it had stood,
   and a silent peer's 50,268 bytes came back at its close. In their documents:
   - hyper-durable: §14's measurements. Items 1 to 8 measured on this machine (2026-10-12 and 13,
     `docs/benchmarks.md` from "A group's writes out, depth one to three on hyper-log"): depth earns
     no place for one group, nor makes a lone frame's confirmation rare; applying ahead halves a
     slowest leader's median answer and changes nothing on a shared disk, its slowest entries waiting
     to leave the leader for a write slot, which is what is left of its tail once it answers at the
     quorum's pace; a quiet group's commit costs one period and one write, and the bench found its
     deadline missing (fixed); applying mantle's metadata is linear in a drive's entries, so the
     owner's page bounds it; repair by an image on real disks is as fast from one lost entry and far
     faster past a hundred, the image's bytes costing only on a path, and the bench found a core fault
     (a member trusted again kept a window of what its cut lost; fixed, `b66dc9e`); an `Unbounded`
     budget costs nothing, a counting one 13–15% of an entry's instructions; slates' publication grows
     with its log where a transition through the shell does not. Item 1 on Linux and with many groups
     (2026-10-16, `docs/benchmarks.md`, "A group's writes out on Linux, and many groups on their
     members' logs"; the depth bench's `GROUPS`): on two CI runners' disks and Docker's virtual
     machine no depth beyond one raised a group's throughput, and depth 3 raised one group's p50 under
     open load (×1.03–1.25); with 16 groups on each log depth 2 or 3 raised it at 14 of 18 heavier
     open points (×1.05–1.50), and with 256 it changed nothing. On macOS with many groups, where a
     confirmation is a flush the next frame saves, depth 2 lowered the p50 at 4 of 8 open points
     (×0.67–0.89) and raised it at none, and depth 3 raised it under heavier load (×1.15–1.35). Open:
     the shell's depth on hyper-log, which §6 bounds at the log's three while three cost the median
     under load everywhere and the best below it is one on Linux and two on macOS, a change every
     consumer's suite would take; a leader sending its appends before its own write has a slot
     (item 2); the shell choosing an image by the measured crossover (item 5); item 6's question of
     PAR's identifiers.
   - hyper-sim and hyper-check: S-6 is built (`docs/sim.md` §17, §18): hyper-durable's schedules in
     the world, on its network and on the typed write queue, judged by hyper-check's oracles, their
     crashes forked, every floor at the same seed counts, 60–69% fewer allocations a step; hyper-raft's
     judged schedules under swarm configurations, `Lagged` on the write queue, its draws kept for S-5's
     tapes. The world's chooser plays a tape (2026-10-17, §18): `Source::Tape`, the tape and its
     mutations moved into hyper-sim, hyper-durable's schedules played, mutated and replayed from
     their tapes with every oracle held. Open: hyper-raft's harness onto the world's draws (§18),
     hyper-durable's `support/device.rs` with hyper-log under the world (§11 item 6), the rest of S-3
     (the block device in the world, its completions' latency and its handles; the namespace model;
     the fault envelope), and S-8 (the consumers' simulations), `docs/sim.md` §9. The network's lost
     messages take their arrivals with them (2026-10-17, §17): `World::cancel`, the event bound §7's,
     and hyper-durable's `C` the rule's 21 where it was 4,096. S-7 is built (§19): every end-to-end
     scenario's clients' history held linearizable by hyper-check's search, a planted stale read
     refused. §11 item 4 is measured (2026-10-17): over the same 200 seeds of each harness, the
     network's losses and repeats narrowed to 25 in 100 reach 22–35 % more abstract states, and more
     a CPU-second, than the swarm's ranges as drawn, and widened to all, the fewest; calm stretches
     alternating with the drawn mix reach 13–24 % more, their length from 125 to 1,000 steps moving
     little. The narrower network catches every planted defect within the random walk's count (the
     fast track's second rule at run 450, which the ranges as drawn miss in 8,868) and each at least as
     often over the same 1,000 seeds (the read before the first commit 166 times against 72). The step
     that narrows the swarm's network is built on the unmerged branch `swarm-quarter-network` (the
     gate's floors derived again on it, every one holding; the swarm's catches recorded) and held back:
     its swarm with no defect planted met a group that does not converge (the open defect above).
   - Tails, `docs/tails.md` §7: T-0 (the grid) is built, its sources read and its conditions' values
     derived (§2.4); T-1 (the measurement kit: its open-loop schedule and quantile intervals built in
     hyper-measure, the per-process accounts of §1a open), T-2 (the impairment relay), then each crate
     class against its industry comparators under the same conditions.
   - hyper-quic against slates' session plane (its class latency grid on hyper-transport, once
     slates' adapter drives it), and its per-packet cost against upstream quinn-proto's: 6–13% more
     instructions an exchange or a MiB, spread over its patches (`docs/benchmarks.md`, "hyper-quic
     against upstream quinn-proto").
   - From slates' congestion bake-off (`docs/transport.md` §4d): Copa's slow start at a large
     product (5.5–7.6% of 100 Mbit/s at 300 ms), tolerance of reordering (slates' adaptive
     tolerance), Copa's answer to a step down, and slates' 1 ms pacing quantum with its
     two-datagram floor.
   - hyper-transport's stream window: its ceiling, `2 · 1,024 · d`, kept upstream quinn's fixed span
     bound from closing a connection, which hyper-quic's bound no longer does (its VENDORED.md §26);
     what should bound one stream's window below its connection's is owed.
   - hyper-seal §11's remaining measurements: energy a byte on an x86 laptop, and the sealed log and
     files on Linux (ext4 and XFS on NVMe). The log is sealed by default since §11's group commit was
     measured (`docs/benchmarks.md`, "Group commit in an open loop, sealed against plain"); each
     consumer opens its log sealed with its own key source (`docs/seal.md` §3.2).
   - hyper-rt §15's open items. Items 1, 2, 5 and 6 are closed (2026-10-17, `docs/research/runtime-sources.md`):
     AFD reports accept and receive readiness on Windows `AF_UNIX` sockets, measured on both Windows
     runners, which took 16 MiB of sends unread without refusing a write (a writer held back by no kernel
     buffer there); `TCP_NOTSENT_LOWAT` read at XNU and Linux, where Linux holds a write to the mark
     and XNU only the wake; `sbitmap` read at `v6.12`, which keeps no summary level, so the wake
     bitmap's summary is hyper-rt's own and its citation is corrected; the wake flag's loom failure
     reduced to a false alarm of loom 0.7.2's fence modelling (a plain store is correct, and passes the
     current models). Item 10 is measured: a calibration costs a command 67 ms, and its error against the
     machine now is the host's load, not the record's age (two rounds 1 to 29 minutes apart differed alike),
     so a record is reused while its key matches; the control ring's depth it derives ranged 32 to 512
     entries across the rounds, one draw of a p99 deciding it. That depth's rule is read (2026-10-17,
     §15 item 10) and waits on the consumers: `ring_entries` bounds the control queue, clamps the batch
     and sizes the completion buffer at once; `control.rs` states the queue's bound as the admission
     limit, past which a spawn could not be admitted, where the drawn depth refuses a burst admission
     would take (32 queued and 68 refused of 100 against a limit of 256); and the consumers set it by
     hand, mantle's ranges bench calibrating at one task a shard, where the admission limit would clamp
     its batch to one poll. The queue's bound taken apart from the batch's clamp is the change, decided
     with each consumer and confirmed by mantle's run at 100M. Item 8's rules are measured on a prototype
     (branch `proto-spin-rule`): Karlin-or-park takes 24–28 % less CPU a request from paused clients and
     moves nothing else measured; it lands once mantle's run at 100M confirms it. Item 7's words are
     measured at their mechanism (`benches/wake_lines.rs`): eight wakers of different words past the head
     wake 1.86 times as often when the words sit on lines of their own (1.06–1.18 at one to four), the
     head's pending flag shared either way; the layout waits on a consumer's foreign-wake row. The
     registration's half is closed (2026-10-17, `benches/rearm.rs`): the re-arm a one-shot wait makes,
     once a UDP echo's round trip, exactly, is 1.2–2.4 % of the shard's CPU a round trip on Linux and
     2–5 % on macOS, the most an edge-triggered registration could save, against a handle's readiness
     held past its waiters, a deregistration at every close and a guard against reused descriptors;
     one-shot is kept. On macOS each arm can instead ride in the wait's own `kevent` (branch
     `proto-kqueue-batched-arm`): 2–6 % fewer instructions a round trip at the medians, every range
     overlapping, a candidate that lands with mantle's confirmation. Item 8 was first measured against slates' runtime and tokio: a client that
     pauses 200 µs costs hyper-rt 18.7 µs of CPU a request against their 12.0–13.7, the spin before a
     park paid in full every time (`docs/benchmarks.md`, "hyper-rt: the loop under load, against slates'
     runtime and tokio"). Item 8's second half is measured (2026-10-17, branch `proto-always-kick`):
     on hyper-rt's spinning ping-pong the kick runs 14–17 % more round trips a second than
     kick-if-parked on macOS, and not by closing a scheduling gap (a `getppid` in its place gains
     nothing, and no 10–13 µs gap shows): its time in the kernel lets the reply land before the
     receive asks, so the initiator's shard drains 6,527 wakes for 22,000 round trips where as built it
     drains 21,999. Kick-if-parked loses on that row; always-kick is the candidate for mantle's run
     at 100M, with Linux measured there.
   - Two benches the table's rows name as owed: hyper-rt-transport against hyper-tokio's driver on tokio,
     and hyper-seal against mantle's `seal.rs`.
   - The fast track: note 32 §3.8's tests decide it; no owner enables it until then.
   - focal's two suggestions for the core (2026-10-11): a batched proposal on `RawNode`, and commit
     broadcasts coalesced into the next append.
4. **Each consumer's switch-over.**
   - mantle: D-1 is in (its range replica is hyper-durable's, electing on ticks). Elections by
     suspicion (L-5) wait for mantle's node to carry the node-pair liveness stream (mantle
     `docs/design/node.md` §2.4); mantle's transport on hyper-transport (T-2) comes with its node.
   - focal: the core (step 1) and the transport (step 4: hyper-quic, hyper-transport, hyper-rt) are
     switched; its storage on hyper-log and the shell (F-1, D-2) is built behind `STORAGE_LEVEL`,
     its `CAPABILITY_LEVEL` still 2, with focal's own items before it opens; timing and liveness
     (step 3) are open: its shell elects on ticks.
   - slates: SWIM (S-1) and the datagram plane (P-1) are switched; its consensus onto hyper-raft
     (X-1, groups re-founded on a fresh start, owner's decision 9) and its session plane onto
     hyper-transport (X-2) are open, and hyper-rt in place of slates' runtime is slates' own step.
