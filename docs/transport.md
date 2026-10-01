# The shared transport: standard QUIC with the projects' refinements on top

> Status (2026-10-01): **stage 1 done — `quinn-proto` 0.11.18 vendored under `vendor/`, building and passing
> upstream's 296 unit tests and 3 doctests.** Ada's directive: the transport must be RFC 9000, 9001 and 9002
> compliant without losing slates' measured performance work ("your goal is to combine the two"); it is
> built here as a crate shared by slates, focal and mantle ("we're building shared crates"). Ada chose to
> vendor `quinn-proto` and conform it to slates' rules ("vendor and conform") over an exception or a rewrite.
> slates records the decision as its amendment A-52 (`../slates/docs/wip/transport-quic.md`).

## 1. Why

slates' fleet transport (`slates/crates/transport`, its §4.10a) speaks a QUIC dialect; focal and mantle took
their transports from it.
Mantle's review (its note 30) and Ada's audit name seven defects. Each departs from a standard or breaks a
property the design promises. They are reported, not yet reproduced here: each gets a red test in slates'
current stack before its fix is claimed (stage 5 keeps one regression per defect).

| # | Defect | Standard or rule | Effect |
|---|---|---|---|
| 1 | Probe-timeout backoff is capped. | RFC 9002 §6.2.1: the PTO doubles on each consecutive expiry, without limit. | A sender keeps probing a dead or saturated path at a steady rate: the retransmission storm the RFC rules out. |
| 2 | The first handshake retransmit fires at 1 ms. | RFC 9002 §6.2.2 and Appendix A.2: with no RTT sample the initial RTT is `kInitialRtt` = 333 ms, so through §6.2.1's PTO formula a handshake starts with a PTO of about one second. | On a thin link the early copies queue behind the first and slow the handshake. |
| 3 | Bytes in flight count payload only. | RFC 9002 §2 and Appendix B: bytes in flight are whole packets, headers and AEAD tag included. | The sender sends more than the controller believes: overload and unfairness to other flows. |
| 4 | The priority class is carried in the stream ID. | Audit §13.3; R4. | A peer chooses its own priority: a privilege escalation. |
| 5 | Whole exchanges are retained before admission. | Audit §11.8; §4.2 bounded admission. | An 8 KiB receive ceiling still accepted a 32 MiB request. |
| 6 | No connection migration or path validation. | RFC 9000 §8.2 and §9. | A laptop moving from Wi-Fi to cellular loses every connection. |
| 7 | Not interoperable. | RFC 9000/9001 wire image. | Standard tools (Wireshark, qlog) cannot decode it; no conformance evidence against another stack. |

## 2. The combined design

| Layer | Source | Notes |
|---|---|---|
| Wire: packet format, loss recovery, timers, connection IDs, migration, path validation, key update, stateless reset. | Standard QUIC (RFC 9000, 9001, 9002) through vendored `quinn-proto`, conformed to the union of the consumers' rules. | Correct, interoperable and decodable by standard tools. Fixes defects 1, 2, 3, 6 and 7 at their root. |
| Congestion behaviour: Copa, the 1 ms pacing quantum with a two-datagram floor, RACK-style adaptive reordering, the path-MTU refinements. | slates (and focal's Copa port), as patches to the vendored `quinn-proto` through its congestion and pacing seams. | Every measured result is re-measured after the port; a regression does not land. |
| Application protocol: one connection per peer, a request and reply per exchange, priority classes with a credit reserve for the classes above, absolute credits, typed refusals. | slates' model (`endpoint.rs`, `streams.rs`, `flow.rs`), on `quinn-proto` streams. | Fixes defect 4: the class is set by message kind and the sender's role, never read from the peer's stream ID. Fixes defect 5: a request streams through an admitted reservation, never retained whole first. |
| Removed. | slates' own packet codec, loss recovery, PTO, handshake sequencing and bytes-in-flight accounting. | Replaced, not layered (banned item 7). |

## 3. Conformance of the vendored crate

The vendored crate must meet slates' rules before anything builds against it. Measured in 0.11.18, test
modules included: 182 `Arc` sites, 18 `Mutex`/`RwLock`, about 570 `unwrap`/`expect` and about 1,100
`panic!`/`assert!`/`unreachable!` sites, in about 30,700 lines.

- **No `Arc`, `Mutex` or `RwLock` (R2, banned item 1).** Shared configuration becomes owned values or
  `&'static` process singletons. The connection's crypto session is owned by its connection. rustls is
  vendored and conformed the same way, so its configuration takes no `Arc` either; there is no
  foreign-API exception (owner's decision, mantle note 32 §6).
- **No panics (banned item 6).** Every `unwrap`, `expect`, `panic!`, `assert!`, `unreachable!`, indexing
  and unchecked arithmetic in non-test code becomes a typed error, `.get()`, or `checked_*`/`saturating_*`.
  Protocol violations become the connection's own transport errors (RFC 9000 §20). The crate sits under
  the workspace lint wall like any other.
- **No magic numbers (R3).** RFC constants stay, named and cited (`kInitialRtt`, `kPacketThreshold`,
  `kGranularity`). Tunables slates derives (pacing quantum, windows) come from slates' derivations.
- **Runtime.** `quinn-proto` is sans-IO: no tokio and no foreign runtime (banned item 2). slates' runtime
  drives it through the UDP socket and timer it already owns.
- **Dependencies.** Kept to what the conformed crate still needs, each listed in the stage that adds it.
  rustls with the AWS-LC provider (`aws-lc-rs`), the provider mantle and focal already vendor and slates has queued; ring is not carried.
- **Licence.** Upstream's MIT/Apache-2.0 licence files are kept with the vendored source, and the upstream
  version and commit are recorded.

### 3.1 Shared configuration without `Arc`

quinn-proto shares immutable configuration among an endpoint and its connections by `Arc`. That
covers the transport config, the TLS configs, the token and reset keys, the time source, the
congestion factory and the initial-CID provider. rustls holds its config by `Arc` inside every
handshake state. Two replacements were weighed and rejected:
- **Process-lifetime `&'static` values.** Every certificate rotation would leak its config. With
  certificates as short as 6 days (Let's Encrypt's short-lived certificates; CA/B Forum ballot
  SC-081 caps lifetimes at 47 days by 2029), that growth has no bound but the process's life.
- **A `Connection<'c>` borrowing from an append-only arena.** It has the same leak, and it pins
  every consumer to one scope for the life of the arena.

Chosen design, by kind of state:
- **Plain data is copied into its owner.** A connection takes its own `TransportConfig` value,
  a few hundred bytes. Congestion control is a closed enum of controllers carried by value
  (`Cubic`, `NewReno`, `Bbr`, and Copa when it lands). That replaces the shared
  `ControllerFactory`, and it makes the controller set one reviewed list, not a plug-in point.
- **Shared immutable objects live in the endpoint's `Configs` slab.** These are the TLS configs,
  the handshake-token and reset keys, the time source and the initial-CID provider.
  - A connection holds a generation-checked handle.
  - Each call that needs configuration takes `&Configs`: handling a datagram or a timeout,
    polling a transmit, and the handshake's TLS steps.
  - The endpoint counts the connections on each slot, as bookkeeping, not ownership, through the
    drain events quinn-proto already sends.
  - A slot that has been superseded and has no connections left is reclaimed. A full slab is a
    typed refusal of the new configuration.
  - Rotation therefore costs memory only while old connections live.
- **Shared mutable state moves to its single owner.**
  - The address-validation token log is consulted only by the endpoint, so the endpoint owns it
    without a lock.
  - The client's token store moves to the caller: a connection reports a received NEW_TOKEN as an
    event instead of writing a shared store under a mutex.
  - A qlog writer is owned by its connection.
- **rustls follows the same rule.** Its handshake states take the config as a call argument
  instead of holding `Arc<ServerConfig>`. Certificate resolvers, session stores, ticketers and the
  crypto provider are borrowed from that config for the call.

## 4. Stages

Each stage lands with its tests, its GAPS row and its design status in the same commit.

1. **Vendor.** `quinn-proto` 0.11.18 in-tree as a workspace crate, licence kept, building on its own.
2. **Conform.** Remove every `Arc`, `Mutex`, `RwLock` and panic; pass the lint wall and `cargo xtask
   check`. Upstream's tests keep passing throughout, as the oracle that conformance changed no behaviour.
3. **Integrate.** slates' runtime drives the connection; slates' application layer (exchanges, priority
   classes set by kind and role, credits, streaming reservations) runs on its streams. The fleet suite
   passes on the new stack.
4. **Port slates' refinements.** Copa, pacing, RACK-style reordering and path-MTU as patches through
   `quinn-proto`'s congestion and pacing seams, each re-measured against its recorded number in
   `docs/wip/BENCHMARKS.md`.
5. **Prove it.**
   - Interop: the conformed stack completes handshakes and exchanges with unmodified upstream `quinn-proto`
     (a test-only dependency, never shipped).
   - Migration and path validation: a client rebinds its address mid-session and the session continues.
   - Defect regressions: one test per defect in §1 (PTO doubling past any cap, the first handshake PTO at
     the RFC value, bytes in flight counting whole packets, a peer unable to choose its class, a request
     past its reservation refused before it is retained).
   - qlog output decodable by standard tooling.

## 5. Consumers

slates, focal and mantle each vendor a snapshot of the conformed crates, recording the hyper-raft revision
it came from, and drive them with their own runtimes. A refinement lands here once, after every consumer's
suite passes against it, and reaches all three.
