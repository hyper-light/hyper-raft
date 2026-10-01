# hyper-raft

The crates slates, focal and mantle share:
- standard QUIC with the projects' refinements (`hyper-quic`), its application layer
  (`hyper-transport`) and a sealed UDP datagram plane (`hyper-datagram`);
- SWIM membership (`hyper-swim`) and the timing laws (`hyper-timing`);
- the Raft core (`hyper-raft`), its durable shell (`hyper-durable`) and the shared log
  (`hyper-log`, `hyper-multilog`);
- deterministic simulation and checking (`hyper-sim`, `hyper-check`).

Every crate is sans-io. A consumer drives it with its own runtime; `hyper-tokio` is the adapter
for tokio. The rules are in `CLAUDE.md`, the transport plan in `docs/transport.md`, and the
inventory and migration plan in mantle's `docs/research/32-shared-transport-and-raft.md`.
