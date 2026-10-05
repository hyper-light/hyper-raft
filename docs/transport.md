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
  (`Cubic`, `NewReno`, `Bbr`, `Copa`; §4d). That replaces the shared
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
  - A TLS 1.2 session stays in the cache for reuse, so the cache lends it: the call that sends
    the ClientHello reads its session ID and ticket where it is, and the connection keeps only
    the session's stamp. The call that takes the server's answer finds it again by the stamp
    and copies the chain into `peer_certificates`, as upstream did; a resumed session is renewed
    in place. A ClientHello sent after a HelloRetryRequest repeats the first's ticket, kept
    from the first (RFC 8446 §4.1.2). The cache keeps a session it displaced while lent, up to
    its bound on servers, and a resumption whose session it pushed out fails typed. Every
    handshake shape allocates no more than upstream's, held exactly by a test against upstream
    rustls in the same process (crates/hyper-tls/VENDORED.md §5); before, a resumed TLS 1.2
    handshake copied the session out and made 2 allocations more (§2.10).
- **Verifiers, resolvers and root stores are owned by their config**, as `Box<dyn …>` or by
  value. Configs are not `Clone`: each endpoint builds its own.
  - Upstream refused to resume a session under another verifier or client resolver by comparing
    `Arc` pointers. Each installation now draws a process-unique identity from an atomic
    counter, and a session records the identities it was made under.
- **Oracle.** rustls's own suite from its repository at the crate's source commit `2976d90`:
  `tests/` and the `rustls-test` crate, which the published archive omits. It is kept passing
  throughout. Interop runs against unmodified upstream rustls as a dev-only dependency: the
  allocation counts are held to it in one process, and the end-to-end test runs it in the other
  process, as client and as server (crates/hyper-tls/VENDORED.md §5, §6).

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

   Interop and migration are proved at the crate: `crates/hyper-quic/tests/e2e.rs` runs every
   scenario between processes three ways, hyper-quic against itself and against unmodified upstream
   quinn-proto as client and as server, an active migration and an unannounced rebinding among them,
   each validated by the server (crates/hyper-quic/VENDORED.md §8). The defect regressions and qlog
   remain stage 5's in slates' fleet suite.

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
  (over the caller's measured timer granularity), `set_granularity` (that granularity, for tuning
  receive windows) and `stats`.
- Events: `Connected { peer, role, epoch }`, `Request`, `Reply`, `BodyReady`, `Writable`,
  `Frame { peer, lane, kind, frame }`, `Refused { exchange, refusal, by_peer }`,
  `Closed { peer, epoch }`, `Unreachable`; at most `Limits::event_bound` wait for the owner (below).
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
trips (Chromium's rule; a round trip under the owner's measured timer granularity `G` counts as
`G`, RFC 9002's 1 ms `kGranularity` until the owner reports one through `set_granularity`), each growth
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

**The events the owner has not polled** are bounded too (`Limits::event_bound`): at most
`4·exchanges + peers·(1 + lanes_per_peer·lane_window)`, `peers` the peer table's bound (the larger
of the identity and connection bounds). Before it the queue grew without one: an owner that did not
poll kept every `Connected`, `Closed` and `Unreachable` a reconnecting peer caused, a `BodyReady` or
`Writable` each time it read or wrote without polling, a request and a refusal of every exchange its
peer opened and gave up on (a refused exchange's slot was reused at once), and every frame its peers'
lanes carried. Each event now waits in a seat of a bounded table, given back when the owner polls it:
- an exchange's events hold its slot in the exchange table, removed or not, so the slot is reused only
  once its last event is polled; each kind waits at most once, its head (`Request` or `Reply`), a
  `BodyReady` and a `Writable` (one arriving while one waits says nothing it does not) and its
  `Refused`: four;
- a lane's frames are counted against the lane, at most the core's window (T37's bound, enforced on
  receipt), and against its peer, at most its lanes' windows, whatever connection they came on; a lane
  at either bound is not read, so its bytes wait in QUIC, whose flow control holds the sender (RFC 9000
  §4.1), and it is read again as the owner polls its peer's frames;
- a peer's lifecycle waits as one event, its latest state with its epoch (its newest connection up;
  that connection closed; a dial failed with no connection up since): a change while it waits
  replaces it and moves it to the back, after the refusals of the exchanges a closed connection
  carried, and a state the owner was already told waits as nothing; a state that came and went
  between its polls is not reported, and a `Connected` of a later epoch says the earlier are not the
  ones exchanges use. The peer's entry is held while the event waits, and while a connection or a
  dial of the peer needs it.

A table at its bound refuses, typed, the stream (`Refusal::Exchanges`, as before), the connection or
the dial (`Refusal::Events`) that would need one more seat. No event is dropped, and no datagram is
refused: a refused datagram would refuse the acknowledgements in it, which every connection's loss
detection needs (RFC 9002 §6), so a backlog of one owner's events would stall every connection,
where the seats stop only the entity whose events wait. The bound is tested at its edge in
`tests/exchange.rs`: a lane's window of frames and no more, a peer back on a new connection read only
as the old one's frames are polled, the exchange table's slots each holding its request and refusal
with the peer's exchanges past it refused, one `BodyReady` for an owner reading without polling, a
reconnecting peer waiting as its latest epoch, and a peer table held by unpolled events refusing a new
peer until they are polled.

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
  sends; `receive(plane, fence, deliver).await` opens a batch; `receive_ready(plane, fence,
  deliver)` opens what is queued without waiting, for an owner to feed its detectors before it
  judges a deadline: on a kernel-stamped socket it asks the kernel, not the reactor, whose
  readiness is as of its last turn (a datagram that came through a stop of the process was queued
  with a stamp before the owner's time while the reactor said the socket was empty, and the owner
  suspected its sender; `docs/benchmarks.md`, "The detector model, at its causes"). Each datagram is delivered with its `Arrival`: the
  address and when it arrived on the socket's `Clock` (`clock()`), the kernel's receive stamp where
  the platform gives one (`docs/timing.md` §2.4, §2.8).
- `Stamped`: a standard UDP socket's receive with the same arrivals, for an owner that waits on its
  socket itself and runs no tokio (the E2E harnesses' members): `receive(socket, buffer)` takes
  the next datagram queued without waiting and gives its length and `Arrival`.
- `Clock`: the host's monotonic clock in nanoseconds, which every process on the host reads alike:
  `CLOCK_MONOTONIC` on Linux, `mach_absolute_time` on macOS, `QueryPerformanceCounter` on Windows.
- `Io { batch }`: the datagrams one system call carries either way, 1 to `UIO_MAXIOV` (1,024).
- The driver folds how late its timer fires (`hyper_timing::Lateness`) and gives the endpoint that
  `G` (`Endpoint::set_granularity`) each time it fires.

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
- macOS and Windows send and receive one datagram a system call through tokio (macOS's plane socket
  through `recvmsg(2)`, for the stamp: `src/sys/macos.rs`).
- Owed: Windows' `UDP_SEND_MSG_SIZE` and `UDP_RECV_MAX_COALESCED_SIZE`. ECN marks are not set or
  read, so the endpoint is handed none.

**Receive stamps** (the plane socket; the endpoint's is not stamped, as QUIC's own clock reads are
its own). A heartbeat is judged by when the kernel received it, not when its owner read it, so an
owner that wakes late does not blame its peer (`docs/timing.md` §2.4).
- Linux: `SO_TIMESTAMPNS`, its `SCM_TIMESTAMPNS` read from each `recvmmsg` message's control
  buffer beside `UDP_GRO`'s (seven words, `CMSG_SPACE(4) + CMSG_SPACE(16)`). The stamp is
  `CLOCK_REALTIME`; it is carried to `CLOCK_MONOTONIC` by its age, both clocks read once a batch
  after the receive, the realtime one first so that a preemption between the reads makes a stamp
  late and never early (read the other way, a container throttled to two CPUs put a stamp before
  its datagram was sent, one run in fifty), and held within the read and no earlier than the stamp before it (a socket's
  queue is first in, first out), so a step of the realtime clock moves one stamp within those
  limits. Linux turns stamping on through a static key flipped from a work queue
  (`net_enable_timestamp`), so the first datagrams after the option is set may be stamped when read,
  late and never early.
- macOS: `SO_TIMESTAMP_MONOTONIC`, `mach_absolute_time` taken as UDP input queues the datagram,
  read through `recvmsg(2)` (`src/sys/macos.rs`; `SCM_TIMESTAMP_MONOTONIC`, 0x04, declared there as
  hyper-timing-trace declares it, since libc does not).
- Windows: none taken; a datagram is stamped when it is read. Winsock's receive timestamps
  (`SIO_TIMESTAMPING`, build 20348 and later) are attached by a NIC miniport driver that reports
  timestamping capabilities, with system configuration, and on no loopback path or virtual NIC
  (`docs/research/timing.md`, "Winsock timestamping"). The cost is the read's delay counted in the
  delays the detector measures, and so in its margin.
- Tested on macOS and Linux (`tests/stamps.rs`): a datagram held 20 ms in the socket before it is
  read is stamped within the send-to-read span and before the read, on the kernel's stamp; the
  stamps never go back; what is queued is taken without a wait.

**Measured** (`docs/benchmarks.md`, "Against focal-wire" and "hyper-tokio end to end").
- In steady state the adapter allocates nothing a round: a 64 B round is 12.4 allocations under it,
  as under the busy-polled driver it replaced in the comparison.
- Real processes on macOS and on Linux pass every scenario: exchanges in every class with every
  byte checked while the owner's tick drops the driver's future mid-wait, lane frames in order,
  plane messages keyed from the connection's exporter, and a peer killed mid-upload.
- On Linux a `sendmmsg` call carries 7.5 to 8 datagrams and a `recvmmsg` call 13 to 19.
- Draining the socket before surfacing an event, rather than one batch a turn, cut a 64 KiB round
  from 762 to 648 µs and its datagrams from 108.6 to 97.0.

## 4c. Node-pair liveness on the plane: `hyper-liveness` (timing step L-3, 2026-10-02)

Built: `crates/hyper-liveness`, sans-io (`docs/timing.md` §2.8 holds the design, the API and the
tests). One heartbeat stream per pair of nodes that share a consensus group, shared by every group
they share; a pair that shares none sends nothing, and a group sends no heartbeat of its own. Each
heartbeat is one plane message (first byte `KIND`, `'L'`, so an owner multiplexing the plane tells
it apart; 75 bytes, 99 with its echo), sealed and checksummed by the plane, and leaves only once
the sender's log made a write durable after the previous heartbeat was due. The plane carries it
because QUIC's datagrams share the connection's congestion window (RFC 9221 §5): a heartbeat must
not wait behind bulk. The receiver judges it by the kernel's receive stamp (§4b, `PlaneSocket`'s
`Arrival`), reading its clock, then feeding what `receive_ready` gives, then polling at that time.

Measured (`docs/benchmarks.md`, "hyper-liveness"): no allocation a heartbeat once configured, about
0.7 to 1.0 µs of the crate's work a heartbeat; per node, two plane messages a pair an interval whatever the
groups, against per-group heartbeats that grow with the groups.

## 4d. Copa in the congestion enum (stage 4's first patch, 2026-10-04)

Built: `crates/hyper-quic/src/congestion/copa.rs`, a fourth controller of the closed enum,
`Congestion::Copa(CopaConfig)`. Since the cut (below) Copa meets focal's harm rule and its own leave
rule over focal's grids; CUBIC stays the default, which is a choice of its own. The source notes are
in `docs/research/congestion.md`.

The law has three layers:

- **Copa (NSDI 2018).** The paper as slates fixed its details from the paper, genericCC and mvfst:
  - integer arithmetic throughout, with Nichols' filters for the four windows (constant space);
  - RFC 9002 §7.8 bounds only the window's growth, so a window the sender does not fill still
    shrinks. slates' guard once skipped every update, and a window slow start had overshot stayed
    at 1.5 MB on a 250 kB product.
  - the mode's windows span the five smoothed round trips of Copa's cycle (§2.2, §3; focal's A1),
    where genericCC, slates and focal took four.
- **focal's measured changes (focal's record F39):**
  - Slow start doubles once what was sent after the last doubling is heard of. Judged by what was
    sent before it, the window reached 3.07 MB where the path and queue hold 2.5 MB at 100 Mbit/s,
    100 ms.
  - A round trip moves the window by half of itself at most (`CopaConfig::stride`, default 2):
    95% of the path at 100 Mbit/s, 100 ms, where the paper's whole-window bound carried 67%.
  - A mark (ECN-CE) is answered as a classic sender answers congestion, halving the window
    (`CopaConfig::mark_backoff`). The gentler RFC backoffs left NewReno and CUBIC under CoDel below
    nine tenths of their bar.
  - For ten seconds after a mark past slow start, the window grows a datagram a round trip.
- **The cut and the competing law (below)**, which replaced the paper's AIMD on `1/δ`.

**Pacing.** A controller states its pacing rate (`Controller::pacing_rate`, bytes a second), and the
path's pacer refills at it. `None` keeps the connection's rule, 5/4 of the window per smoothed round
trip (RFC 9002 §7.7). Copa states `2·cwnd/RTTstanding` (§2.1); NewReno, CUBIC and BBR keep the
connection's rule. Measured on the harness below, Copa alone emptied its queue at the same intervals
paced either way on three of focal's five paths. It carried the same share at 10 Mbit/s and 20 ms
beside NewReno and CUBIC. Its rate held within 1.6% of its median at 100 Mbit/s and 20 ms, so pacing
on the standing round trip drives no oscillation.

**The harness.** `crates/hyper-quic/tests/congestion.rs` runs real hyper-quic endpoints over
hyper-sim's network on focal's F39 grids. Each seed's run is exact and replays from its seed and its
trace; focal's harness judged harm by deviations over seeds. The tests that run in the gate use one
path, 10 Mbit/s and 20 ms for 10 s, and check that:

- every law carries its transfer alone, with Copa's queue the shortest;
- Copa shares a bottleneck with NewReno and CUBIC with no stall, ECN kept, CoDel marking it, and
  each incumbent at nine tenths of its bar under CoDel at least.

The grids run by hand (`--ignored`) and are recorded in `docs/benchmarks.md`, "Copa's competing mode
over focal's grids".

**focal's derivations for the competing mode, measured (2026-10-04).** focal's b18 proposed three
derivations:

- A1: the mode judged over five round trips, the cycle's period;
- A2: a delay sample judged by the window its packet was sent under;
- B: competing, `1/δ` raised by `d_q/RTTstanding` a round trip, so Copa's rate grows as a classic
  sender's.

Three further variants were measured beside them:

- G: a queue of a datagram or less is nearly empty;
- H: the mode ends only after the queue stays nearly empty longer than its window;
- E: competing, the window is NewReno's.

The rule (`docs/benchmarks.md`) was fixed before the full grid ran. None of those five laws met it.
b18 lowered Copa's share beside the incumbents from 41.7% to 25.0%, and every variant left Copa
competing after its competitor left in 30 or more of 64 runs.

**The cut: whose queue is it (2026-10-04).** Every failure of rules 5 and 6 was Copa alone judged
competing; GH-E's competing law met rules 1 to 4 in every run. A passive identity cannot tell alone
from sharing: by Little's law Copa's bytes in the queue are `r·d_q` with or without a competitor, so
the excess in flight always explains Copa's own part, and what separates the two is the bottleneck's
capacity, which the sender's own rate does not show while it shares. So Copa acts on the identity:

- where the paper would switch to competing, Copa cuts its window to `r·RTTmin` less two datagrams,
  never under three, withdrawing its own bytes in the queue;
- the packets sent under the cut over half a smoothed round trip judge it: within two datagrams' time
  on the link of `RTTmin` (plus what the floor holds beyond Copa's share), the queue was Copa's, and
  Copa keeps the default mode with its excess dropped; above it, another sender's bytes stood, and
  Copa competes;
- competing, the window grows as NewReno's and halves on a loss or a mark once a recovery period
  (RFC 9002 §7.3.2, §B.5), and Copa cuts again every 40 smoothed round trips (`CUT_INTERVAL_SRTTS`,
  measured: 10 failed rule 3, 80 failed rule 6, 40 carried more than 20); the mode ends only on a
  cut that finds the queue empty, never on one empty moment;
- with A1 (the mode over five round trips), A2 (a sample judged by the window its packet was sent
  under) and G (a queue of a datagram is nearly empty).

The floor and the allowance were set by what the alone grid showed before any shared run: cut to two
datagrams, one packet was in flight and every sample waited out the peer's 25 ms `max_ack_delay`
(RFC 9000 §13.2.2), and an allowance at Copa's own rate rather than the path's let NewReno's backlog
pass for empty at 1 Mbit/s and 100 ms. Measured over focal's grids, the law meets all six rules
(`docs/benchmarks.md`, "The cut"): Copa alone never competes, its queue is the shortest on every
path, every incumbent carries 1.04 of its bar or more with a manager or without, and Copa never
competes after its competitor leaves. Its share beside the incumbents is 35.5% (geomean of 128 runs),
against 41.7% for focal's law, which took it from the incumbents.

Closed: **finding 1** (without a manager the incumbents now carry 1.16 of their bar or more) and
**finding 3** (`copa_stops_competing_once_its_competitor_leaves` runs in the gate again).

Open:

- **Finding 4, as a behaviour.** Under CoDel at 100 ms the cut finds no backlog beyond its allowance
  and Copa does not compete; the incumbents still carry 1.04 of their bar or more, the marks answered
  as a classic sender answers them.
- **Still to come.** slates' bake-off on the harness, and slates' 1 ms pacing quantum and
  two-datagram floor.

## 4e. The handshake at a geographic distance (2026-10-04)

The owner's condition is 500 ms one way. slates reported that a two-datagram hybrid ClientHello
(X25519MLKEM768, a 1,184-byte key share) failed there. `crates/hyper-quic/tests/geo.rs` runs real
endpoints on hyper-sim's network and records every datagram with its time and its packets' types.
It checks four guarantees exactly, on a clean path and over 32 seeds of 5% loss with ±100 ms of
reordering:

- **(a)** The client retransmits its first flight on RFC 9002's PTO from kInitialRtt, 333 ms
  (§6.2.2): flights go at 0, 999 ms and then double. Exactly one retransmission precedes the first
  reply at 1,000 ms, and no lossy seed has more than two.
- **(b)** A retransmitted two-datagram flight is one PTO expiry, so the backoff doubles once a
  flight. It is one attempt against `max_incoming`, even when its first datagram was lost and its
  second held.
- **(c)** Duplicate Initials are acknowledged at once and surface no second attempt. Their bytes
  grow the server's allowance, and the server never sends past three times what it received
  (RFC 9000 §8.1).
- **(d)** The hybrid ClientHello with a server flight larger than three times it completes at two
  round trips on the clean path, the floor the amplification limit sets. Every lossy seed
  completes and answers its first request. A seed that lost nothing completes within two of the
  slowest round trips.

Six defects were fixed at their causes to meet these (`crates/hyper-quic/VENDORED.md` §9): the
attempt bound counting one attempt twice, the amplification limit overshot by a datagram, a PTO
probing only one space while the lost ServerHello waited, no packet on entering recovery, packets
dropped when they overtook their keys, and the PTO backoff kept past a key discard.

TLS between nodes is restricted to the hybrid post-quantum groups (X25519MLKEM768, then
SecP256r1MLKEM768) and the 256-bit TLS 1.3 suites (AES-256-GCM, then ChaCha20-Poly1305). Initial
packets keep AES-128-GCM (RFC 9001 §5.2), and a classical-only peer is refused
(`crates/hyper-quic/VENDORED.md` §10).

## 4f. What the stack adds above the round trips at 500 ms one way (2026-10-04)

The owner's goal is a tenfold cut in what the stack adds above the physical floor of round trips at
500 ms one way, with no correctness given up. The research is `docs/research/quic-overhead.md`;
the numbers are `docs/benchmarks.md`, "Probe timeouts, tickets and Careful Resume".

- **Loss recovery.** A probe carries the oldest in-flight packet's STREAM frames, as Chromium,
  msquic and quiche do; the probe timer weighs the RTT variation by 2 (Chromium's constant;
  RACK-TLP's 2·SRTT at the first sample), while the periods RFC 9002 and RFC 9000 define from the
  PTO keep RFC 9002's 4; a PTO probes the Data space too once an RTT sample exists
  (`crates/hyper-quic/VENDORED.md` §11, items 1 to 3).
- **Two defects that cost a round trip or the connection.** A server armed its probe timer while
  still handshaking and never again, so a full window with dropped acknowledgements idled out; and
  an ACK in the Initial space reset the Data space's max_ack_delay timer and bundling deadline, so
  0-RTT packets were acknowledged a round trip late (§11, items 4 and 5).
- **Tickets.** The server sends its session tickets with its Finished on a handshake that does not
  authenticate the client (RFC 8446 §4.6.1), so a resumed dial that closes at its first reply still
  receives one (`crates/hyper-tls/VENDORED.md` §7).
- **Careful Resume** (RFC 9959). A connection to a remote IP address that an earlier connection
  measured jumps, once its first round trip confirms the path, to half of what that connection
  delivered a round trip, and retreats on the first congestion (`crates/hyper-quic/VENDORED.md`
  §12). A connection with no earlier measurement of its path still starts at the initial window
  (RFC 9002 §7.2): that start is the network's safety rule, not overhead.
- **The handshake's flights twice** (2026-10-05). Each packet of the handshake's flights is copied
  once, in a packet of its own number, under the window, pacing and the anti-amplification limit,
  so a lost one costs no probe timeout; a lost original is still answered as congestion (RFC 9265).
  With it, three defects closed: acknowledged stream data no longer goes again, a server holds 0-RTT
  packets that come before the whole ClientHello (RFC 9001 §4.1.4), and the window counts as used
  for the round trip after it blocked (RFC 9002 §7.8), so a burst past the initial window stalls
  once, not each round trip. The initial window follows the datagram size (RFC 9002 §7.2)
  (`crates/hyper-quic/VENDORED.md` §13). What is left: a burst's first round trip on a path no
  connection has measured waits on the initial window, which no standards-track mechanism lets a
  sender exceed (`docs/research/quic-overhead.md` §4.2).
- **Copies spaced past a burst** (2026-10-05). Losses come in bursts on every measured path, and a
  copy sent with its original died with it in hyper-sim's burst model; a copy now waits
  `τ·ln(PTO/τ)` behind its original (117 ms at the first probe timeout, none where the probe
  timeout is shorter than a burst), and a server holds 1-RTT packets that come before the client's
  Finished (RFC 9001 §5.7). Under bursts the fresh first reply's p90 fell from 2,749 to 394 ms; under
  independent loss it rose from 136 to 317 ms (`crates/hyper-quic/VENDORED.md` §14,
  `docs/research/burst-loss.md`).
- **The path measured before its first burst** (2026-10-05). An idle connection to a remote with
  no measurement warms its path up, PING and PADDING under the window and the pacer, until four
  initial windows are acknowledged in a round trip (RFC 9959 §3.1's floor), so a later burst starts
  from Careful Resume's jump rather than the initial window; the jump is taken only where it beats
  slow start (`crates/hyper-quic/VENDORED.md` §15, `docs/research/quic-overhead.md` §5).

## 5. Consumers

slates, focal and mantle each vendor a snapshot of the conformed crates, recording the hyper-raft revision
it came from, and drive them with their own runtimes. A refinement lands here once, after every consumer's
suite passes against it, and reaches all three.
