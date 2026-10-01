# hyper-raft — project rules for Claude Code

hyper-raft holds the crates slates, focal and mantle share:
- the transport: standard QUIC with the projects' refinements, their application layer, and the
  sealed UDP datagram plane;
- SWIM membership;
- the timing laws;
- the Raft core, its durable shell and the shared log;
- the simulation and checking harnesses.

Every consumer takes what lands here, so the floor is the union of the three projects' rules,
whichever is strictest on each point:
- `../slates/CLAUDE.md`
- `../focal/CLAUDE.md`
- `../mantle/CLAUDE.md`

The inventory, the enhancement ledger and the migration plan are in mantle's
`docs/research/32-shared-transport-and-raft.md`. That note's §6 records the owner's decisions.

## 1. Rules, each enforced by a lint, a test or a gate

- **No shared ownership and no locks.** `Arc`, `Rc`, `Mutex`, `RwLock` and `Condvar` are denied
  (`clippy.toml`). There is no exception for foreign APIs: quinn-proto and rustls are vendored and
  conformed, so their configuration is owned or borrowed too. Use single owners, moves over bounded
  channels, arenas with generational handles, and `std::task::Waker` for completions.
- **No panics in shipped code.**
  - Denied: `unwrap`, `expect`, `panic!`, `todo!`, `unimplemented!`, `unreachable!`, the `assert!`
    family, out-of-bounds indexing or slicing, and overflowing arithmetic.
  - Every failure is a typed error the caller handles.
  - A vendored panic site reachable from peer input is closed at its cause. Until it is closed, the
    call runs inside an unwind boundary that turns the unwind into a closed connection or a refused
    operation.
- **Sans-io.**
  - A crate is a state machine: it is fed `now`, bytes and events, and it returns bytes, timeouts and
    events.
  - It never spawns, never reads a clock, never opens a socket.
  - The one exception is `hyper-log`, which owns its device writer and the files it writes.
- **No foreign async runtime in the core crates.** `hyper-tokio` is a separate adapter crate. slates
  never depends on it.
- **No arbitrary numbers.**
  - A standard's constant is named and cites its section, e.g. `kInitialRtt` from RFC 9002
    Appendix A.2.
  - A tunable is derived from measurement or data by a stated formula, or exposed as configuration.
  - Every `const` carries a `///` with its derivation or citation.
- **Bounded growth.** Every queue, cache, table and retry loop has a derived bound and a typed refusal
  at it.
- **Standards first.** A wire behaviour follows its RFC unless a measurement says otherwise. The
  departure is recorded with its number.
- **Tests exercise behaviour.**
  - Oracle tests: raft-rs differential, upstream quinn-proto interop.
  - Deterministic simulation with injected faults.
  - Hostile-input and fuzz tests.
  - Recorded-seed equivalence when code moves.
  - A bug fix starts with a failing test.
- **Model checking.**
  - TLC runs in this repository's CI only, with a fixed worker count and a stated state and time
    budget.
  - It never runs on the owner's machine or in slates' CI.
  - The exhaustive Rust explorers in `hyper-check` run beside it.
- **Portable.** Linux, macOS and Windows on x86_64 and aarch64. `unsafe` is allowed only in the files
  `scripts/check-contracts.py` lists, each block with a `// SAFETY:` comment.

## 1a. Law: measured against what each crate replaces

The owner's law for every crate in this repository, with no exceptions:

- **Allocations, reallocations and page faults are measured and driven down on every hot path.**
  - Counts come from a counting allocator and the OS (`getrusage` minor and major faults, and
    `task_info` on macOS), per operation, recorded with each benchmark.
  - A change that raises any of them on a hot path does not land without a measured reason.
- **Every crate is benchmarked against each project's own implementation it replaces:** slates',
  focal's and mantle's.
  - The comparison uses the same workload, the same hardware and recorded commands.
  - Results go in `docs/benchmarks.md` with the hardware, the date and the exact command.
  - A crate replaces a project's implementation only where it is at least as fast and allocates no
    more. Any loss is fixed before the switch, not accepted.
- **End-to-end tests of real usage.**
  - Real processes on real UDP sockets and real disks, driven the way each consumer drives the
    crate: slates' runtime, focal's nodes, mantle's node and client.
  - These run beside the sans-io unit tests and the deterministic simulation, never instead of
    them.

## 2. Vendored sources

Upstream crates are staged under `vendor/` exactly as published, outside the workspace and the lint
wall. Each is conformed into a member crate under `crates/`.
- Every change from upstream is recorded in that crate's `VENDORED.md`.
- Upstream's own tests are kept as the oracle that conformance changed no behaviour.

## 3. Consumers

slates, focal and mantle each vendor a snapshot of these crates. Each snapshot records the
hyper-raft revision it came from. A change lands here only once every consumer's suite passes
against it. Each consumer then updates its snapshot in its own gated commit, under its own rules.

A consumer's full suite can be timing-sensitive; focal's runs about 80 minutes. Before running
one, check with whoever is working in that repository that no other gate run is in flight.

## 4. Gates

`bash scripts/gates.sh` runs the gates in order and stops at the first failure. CI runs them on all
six targets.

## 5. Documents

- `docs/transport.md`: the transport's design and plan.
- `docs/raft.md`: the consensus crates' design and plan.

A change in behaviour updates its document in the same commit.
