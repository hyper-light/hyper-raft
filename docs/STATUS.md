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
| hyper-raft | focal-raft `a8e95f7`, with history; the owner's decision (2026-10-01): core changes start here | yes | yes: 32 → 11 allocations per entry at batch 1, 0 reallocations (`docs/benchmarks.md`) | yes: against focal `a8e95f7` and `1395e22`, slates `5cce86a` and raft-rs; behind slates on catch-up and snapshot, traced to raft-proto's message size (fixed by step R-2) | yes: 3- and 5-member processes over UDP with fsynced logs; kill -9 of the leader, of a follower, and of all; partition | yes |
| hyper-timing | focal-timing `a8e95f7` with history, and slates' election law `5cce86a` | yes | owed | owed | through hyper-swim and hyper-raft | yes |
| hyper-datagram | new, after slates' seal and node.md §3.4 | yes | owed | owed against slates' `Sealer`/`Opener` | yes: two processes, UDP; replay and forgery from a third socket | yes |
| hyper-swim | slates' detector `5cce86a` | yes | owed | owed | yes: four processes over hyper-datagram, SIGKILL detection | yes |
| hyper-quic | quinn-proto 0.11.18 | no: about 1,080 sites; no `Arc` or `Mutex` left in shipped code (endpoint `Configs` slab) | per handshake: QUIC full 505→494, resumed 511→501 | owed against slates' and focal's transports | an in-process handshake through the public API; real processes owed | no (`hyper-quic`) |
| hyper-tls | rustls 0.23.45 | no; no `Arc` or `Mutex` left in shipped code (configs lent per call, `&'static` provider) | per handshake: TLS 1.3 full 296→286, resumed 219→213, 1.2 full 144→140; owed: TLS 1.2 resumed 99→101 (+1,388 B) from copying the cached ticket and chain, to be lent by the store instead | owed | owed | no (`hyper-quic`) |
| hyper-log, hyper-block | mantle's log and block layer (L-1) | | | | | not started |
| hyper-durable | mantle's replica shell (D-1) | | | | | not started |
| hyper-transport | focal-wire's core (T-1) | | | | | not started |
| hyper-multilog, hyper-sim, hyper-check, hyper-tokio | note 32 §3.2 | | | | | not started |

## Consumers

| Consumer | Takes | State |
|---|---|---|
| mantle | hyper-raft as `vendor/hyper-raft` (Cargo rename `focal-raft`), snapshot `cec55a6` | on mantle `dev` (`590f475`). The next snapshot takes the in-place `Ready` and the core ports |
| focal | hyper-raft (R-1, F-1) | blocked: changes in `~/Projects/focal` need the owner's permission rule. focal's F43 (one ReadIndex heartbeat round per ready) is pending in focal-raft; it is ported here once focal gates it, so the cores do not diverge |
| slates | hyper-quic, hyper-datagram, hyper-swim, hyper-timing | slates' session owns the integration (slates A-52 §5) |
