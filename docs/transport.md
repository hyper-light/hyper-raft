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
- **Shared objects live in the endpoint's `Configs` slab.** A slot holds a server or a client
  configuration, and with it the TLS config, the handshake-token key, the time source and the
  initial-CID provider. The stateless-reset key stays in the endpoint's own `EndpointConfig`,
  which no connection reads.
  - A connection, and a pending incoming attempt, holds a generation-checked handle.
  - Each call that needs configuration is lent it. Handling a datagram takes `&mut Configs`,
    because a TLS read can change the config's stores (session cache, ticketer, key log); polling
    a transmit takes `&Configs`, for the NEW_TOKEN key and clock; handling a timeout takes none.
  - The few plain values a connection reads outside a TLS step (whether the peer may migrate,
    whether a preferred address was sent, how many NEW_TOKEN frames to send) are copied into it
    at accept.
  - The endpoint counts the connections and incoming attempts on each slot, as bookkeeping, not
    ownership: up at creation, down at the drain events quinn-proto already sends and when an
    attempt is accepted, refused, retried or ignored.
  - A slot that has been superseded and has no users left is reclaimed. A full slab is a typed
    refusal (`ConfigsFull`) of the new configuration. The bound is
    `EndpointConfig::config_slots`, default 4: one current and one draining configuration per
    side, since rotations are days apart.
  - Rotation therefore costs memory only while old connections live.
  - Connections made with one client handle share its TLS session cache, as connections made
    from clones of one `Arc`'d config did.
- **Shared mutable state moves to its single owner.**
  - The address-validation token log is consulted only by the endpoint, so the endpoint owns it
    without a lock.
  - The client's token store moves to the caller: a connection reports a received NEW_TOKEN as an
    event instead of writing a shared store under a mutex.
  - A qlog writer is owned by its connection.
- **rustls follows the same rule.** Its handshake states take the config as a call argument
  instead of holding `Arc<ServerConfig>`. Certificate resolvers, session stores, ticketers and the
  crypto provider are borrowed from that config for the call.
- Measured on `tests/handshake.rs` (crates/hyper-quic/VENDORED.md §2.11): the same datagrams,
  size for size, as quinn-proto with `Arc`, and 11 and 10 fewer allocations per full and resumed
  handshake.

### 3.2 rustls conformed (`hyper-tls`)

rustls 0.23.45 holds configuration by `Arc` in about 230 places. The largest are `Arc<ClientConfig>`
and `Arc<ServerConfig>` (51), then `Arc<RootCertStore>` and `Arc<CryptoProvider>` (17 each), then
the certificate keys and signing keys (25), the verifiers and resolvers (23), the session stores,
ticketer and key log (10), and the certificate-compression cache (6). Measured with
`grep -rn "Arc<" src`. Each moves as follows:

- **A connection holds no configuration.**
  - `ClientConnection`, `ServerConnection` and the QUIC connections take their config by
    reference on the calls that can advance a handshake: `process_new_packets`, `complete_io`,
    the unbuffered `process_tls_records`, and QUIC's `read_hs`. QUIC's `write_hs` takes none: it
    drains the flight a read produced and consults no configuration.
  - The reference travels in the state machine's `Context`, so no handshake state keeps a
    `config` field.
  - The caller owns the config: in hyper-quic, the endpoint's `Configs` slab.
- **The crypto provider is `&'static CryptoProvider`.** AWS-LC's provider is static tables of
  algorithm references.
- **Certificate keys are borrowed for the call that uses them.**
  - In TLS 1.3 a server selects its certificate and signs CertificateVerify in the call that
    handles ClientHello. TLS 1.2's ServerKeyExchange is the same.
  - A client does not: it receives CertificateRequest in one call and signs CertificateVerify in
    a later one (TLS 1.3: on the server's Finished; TLS 1.2: on ServerHelloDone). It keeps the
    request and resolves its credentials in the signing call. The messages are unchanged; the
    resolver now runs after the server's certificate is verified.
  - A resolver therefore returns `&CertifiedKey` borrowed from itself, and no key outlives the
    call that signs with it.
- **Mutable shared state is reached through `&mut` configuration.**
  - The session stores, the ticketer's key rotation, the key log and the compression cache are
    owned (`Box`) by the config.
  - Each call that may change them takes `&mut ServerConfig` or `&mut ClientConfig`.
  - Their internal `Mutex`es go. One endpoint drives its connections from one thread, so the
    calls never overlap.
  - The config is split into read-only settings and these stores, so one call can hold a
    borrowed certificate key and a mutable store together.
  - A TLS 1.2 session stays in the cache for reuse, so resuming copies its ticket and
    certificate chain out, where `Arc` shared them: +2 allocations per resumed TLS 1.2
    handshake, measured (crates/hyper-tls/VENDORED.md §2.10). Every other handshake allocates
    less than before.
- **Verifiers, resolvers and root stores are owned by their config**, as `Box<dyn …>` or by
  value. Configs are not `Clone`: each endpoint builds its own.
  - Upstream refused to resume a session under another verifier or client resolver by comparing
    `Arc` pointers. Each installation now draws a process-unique identity from an atomic
    counter, and a session records the identities it was made under.
- **Oracle.** rustls's own suite from its repository at the crate's source commit `2976d90`:
  `tests/` and the `rustls-test` crate, which the published archive omits. It is kept passing
  throughout. Interop runs against unmodified upstream rustls as a dev-only dependency.

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

## 4a. The application layer: `hyper-transport` (T-1, 2026-10-01)

Built: `crates/hyper-transport`, mantle note 32 §3.4's application layer, a sans-io state machine
over hyper-quic. Its generic core is ported from focal-wire (`ORIGIN.md` lists each module's source,
each ported test's origin, and what was left out).

**The API as it stands** (`src/lib.rs`, `src/endpoint.rs`):
- A project supplies `Classes` (its kinds; the class each sender role gives each kind, or none; each
  class's rank and frame bound; each kind's wire code), `Budget<Class>` (reserve and release over its
  own accountant, on the lane `Window` or `Class(c)`) and `Directory` (the peer and role a
  certificate names, and the name a peer's certificate is checked against when dialed).
- `Endpoint<C, B, D>` is driven by `handle_datagram`, `poll_transmit`, `poll_timeout`,
  `handle_timeout` and `poll_event`, every one taking the caller's `now`; its owner calls `connect`,
  `disconnect`, `open`, `head`, `write_body`, `read_body`, `body_complete`, `reply`, `end`,
  `reserve`, `release`, `send_frame`, `export_keying_material`, `path`, `credit`, `exchange_tail`
  (over the caller's measured timer granularity) and `stats`.
- Events: `Connected { peer, role, epoch }`, `Request`, `Reply`, `BodyReady`, `Writable`,
  `Frame { peer, lane, kind, frame }`, `Refused { exchange, refusal, by_peer }`,
  `Closed { peer, epoch }`, `Unreachable`.
- Departures from §3.4's sketch, none of substance: `Directory` is the third type parameter (§3.4's
  `PeerId` presumed one); `Classes` gains `RANKS`, `rank`, `kind_code`, `kind_of` (the reserve counts
  the classes above, and the kind crosses the wire); `send_frame` takes the frame's kind (its class
  bounds it and orders it); `write_body`, `reply` and `read_body` take the endpoint's last `now`
  rather than one of their own; `handle_datagram` takes `&[u8]`, since hyper-quic takes its own
  buffer.

**What it does.** One connection per peer under mutual TLS 1.3 with 0-RTT off. An exchange is a
bidirectional stream: each message a 20-byte prefix (kind, head and body lengths, CRC-32C of prefix
and head), the head, and an optional body ended by its CRC-32C. The receiver checks the prefix
against its own class bound and head bound before reserving the head from the budget and reading
it; bodies are read only into the owner's reservations, so QUIC's flow control holds the sender.
The class comes from the kind and the sender's role. A class leaves one packet's stream bytes of the
peer's connection credit for each class above it (slates' reserve), and takes only the credit left
after every more urgent class on the connection has what it declared and not yet sent, offered by
its owner or not (strict priority at the point credit is taken, which the end-to-end run showed
quinn's send-order priority alone does not give). The receive window
starts at RFC 9002's initial window plus the reserve and doubles when consumed within two round
trips (Chromium's rule; a round trip under the 1 ms timer granularity counts as 1 ms), each growth
reserved from the budget; the stream window is quinn's assembler limit made explicit (patch Q5).
Exchanges are judged by progress-charged deadlines (T39); a period's sent bytes count only if the
peer was heard in it, so probes sent to a dead peer do not keep its exchanges alive; an answer is
charged with the stream bytes the connection delivered of its class and the less urgent ones,
against what the peer declared of them, and ends a period after all of that arrived without it, so
a body its owner writes slowly, or that waits behind the peer's other replies, is not refused while
it moves (focal's residency, priced by the path's round trip, refused an 8 MiB reply at 5.5 MB read
on a loaded runner; `src/progress.rs`); and a period
that moved nothing through this side's own doing (an owner not reading a body, or bytes the peer's
credit would take but this side has not sent) is no evidence against the peer and is not judged. Replication frames travel on lanes, unidirectional streams as wide as the core's
window, always read; a frame its class or the budget cannot take is skipped and counted. Admission
bounds pending handshakes (Retry under load), identities, connections per identity (the one used
longest ago replaced) and connections in all; every exchange and lane has a table bound. Every
refusal is typed, and one that ends an exchange crosses the wire as the stream reset's code.

**Measured** (`docs/benchmarks.md`, "hyper-transport against focal-wire's core"): an exchange adds
0.27 allocations to the bare hyper-quic stream without a body and about 5 with one (three QUIC
writes a side, each copied by hyper-quic); against focal-wire's core on loopback, both on tokio
and parked the same way (hyper-transport under hyper-tokio, §4b), its rounds are 8 to 65 % shorter
at every size and it makes half the allocations of a small exchange and two fifths to a half of
the bytes at every size, at load 36 to 50. Its reallocations, 1.9 to 4 a round with a body against
focal-wire's 0 to 0.13, were each the receive buffer grown a datagram at a time; datagrams are now
cut from a bounded pool of chunks charged to the budget and never grown (past the pool, a datagram
is copied into a buffer of its own size, so the chunk memory a peer can pin is at most
`receive_chunks × 65,527` bytes an endpoint), and a round makes no reallocation at any size, with an
eighth of focal-wire's allocations at 64 KiB and a twelfth at 512 KiB (load 53 to 68). End to end, between real processes over
UDP: exchanges in every class with megabyte bodies, the reserve keeping a vote moving past held
bulk, typed refusals at every bound, a peer killed mid-upload, and frames on lanes in order.

**Owed.** focal-wire's domain layer over this crate, with focal's suites as the gate; the TCP
fallback (T53); slates' class-latency grid on this layer; charging hyper-quic's assembler
over-allocation (at most `max(32 KiB, 1.5 × buffered)` a stream) to the budget; a frame bound a lane's frames are checked against
apart from the message bound.

## 4b. The tokio adapter: `hyper-tokio` (T-1's second half, 2026-10-01)

Built: `crates/hyper-tokio`, the only crate here that names a runtime (note 32 §6 item 6); slates
never depends on it.

**The API.**
- `Driver<C, B, D>`: one hyper-transport `Endpoint`, its UDP socket and one timer.
  - `Driver::bind(endpoint, address, io)` or `Driver::new(endpoint, std_socket, io)`, within a
    tokio runtime with its I/O and time drivers; otherwise `Error::Runtime`.
  - `event().await` (or `poll_event`) returns the endpoint's next event. Until one is ready it
    drains the socket, fires the timers due, sends what the endpoint transmits, and parks on the
    socket and the timer. An event already queued is handed out with no system call.
  - `endpoint()` lends the endpoint for the owner's calls between events; `flush()` sends what
    those calls queued without waiting; `stats()` counts datagrams, system calls and drops.
- `PlaneSocket`: a hyper-datagram `Plane`'s own socket. `flush(plane, route, refused)` seals and
  sends; `receive(plane, fence, deliver).await` opens a batch.
- `Io { batch }`: the datagrams one system call carries either way, 1 to `UIO_MAXIOV` (1,024).

**Shape.** The owner's task holds the driver and awaits it. tokio wakes that task through the
socket's and the timer's wakers. Inside there is no task, thread, channel, lock or `Arc`, and no
thread per connection or exchange. `event` and `receive` take and process a batch with no await in
between, so dropping them loses nothing: an owner selects over them and its own work. tokio holds
its scheduler and driver handles by `Arc` inside the runtime; that is tokio's code, not a site in
this crate.

**Bounds.**
- The outbox holds at most `batch` datagrams. A datagram past it, with the socket full, is dropped
  and counted; QUIC recovers it, and the plane retransmits nothing by design.
- A turn drains at most 128 batches, and one poll takes at most 128 turns before it yields. 128 is
  tokio's cooperative budget a task a poll.
- Receive buffers are 64 KiB each (`GRO_LEGACY_MAX_SIZE`): `batch` of them on Linux, one
  elsewhere.

**Errors.**
- A receive error that reports an earlier datagram gone astray is counted and read past: Windows'
  reset after a send to a closed port, and refused or unreachable reports.
- Any other receive error is `Error::Io`.
- A send the kernel refuses is a lost datagram, dropped and counted. An `EIO` from a segmented
  send turns segmentation off, since the device cannot segment.
- tokio panics when no runtime, or no I/O or time driver, is current. Registration runs behind an
  unwind boundary that returns `Error::Runtime`.

**Sockets.** The owner's decisions: epoll on Linux, kqueue on macOS and IOCP on Windows (tokio's
reactor); no io_uring; no AF_XDP.
- Linux sends with `sendmmsg(2)` and receives with `recvmmsg(2)`. Consecutive equal-sized
  datagrams to one destination go as one segmented message (`UDP_SEGMENT`, at most 64 segments and
  65,507 bytes). Coalesced receives (`UDP_GRO`) are split by the segment size the kernel reports.
  Each offload is used only where the kernel accepts the socket option (`src/sys/linux.rs`, the
  one file with `unsafe`).
- macOS and Windows send and receive one datagram a system call through tokio.
- Owed: Windows' `UDP_SEND_MSG_SIZE` and `UDP_RECV_MAX_COALESCED_SIZE`. ECN marks are not set or
  read, so the endpoint is handed none.

**Measured** (`docs/benchmarks.md`, "Against focal-wire" and "hyper-tokio end to end").
- In steady state the adapter allocates nothing a round: a 64 B round is 12.4 allocations under it,
  as under the busy-polled driver it replaced in the comparison.
- Real processes on macOS and on Linux pass every scenario: exchanges in every class with every
  byte checked while the owner's tick drops the driver's future mid-wait, lane frames in order,
  plane messages keyed from the connection's exporter, and a peer killed mid-upload.
- On Linux a `sendmmsg` call carries 7.5 to 8 datagrams and a `recvmmsg` call 13 to 19.
- Draining the socket before surfacing an event, rather than one batch a turn, cut a 64 KiB round
  from 762 to 648 µs and its datagrams from 108.6 to 97.0.

## 5. Consumers

slates, focal and mantle each vendor a snapshot of the conformed crates, recording the hyper-raft revision
it came from, and drive them with their own runtimes. A refinement lands here once, after every consumer's
suite passes against it, and reaches all three.
