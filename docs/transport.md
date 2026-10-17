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
    ownership: up at creation; down when an attempt is accepted, refused, retried or ignored,
    when a server connection's handshake completes, and when a client connection drains.
  - A server connection holds its configuration only through its handshake: its tickets were
    made as it read the client's Finished, a QUIC client sends no TLS message after its own
    (RFC 9001 §4.4, §6; one that does is refused as unexpected), and a token it issues later
    (NEW_TOKEN) is sealed under the endpoint's current configuration, the one that validates the
    token when the client returns. A client connection holds its configuration until it drains,
    since its server may send tickets for its cache at any time after the handshake.
  - A slot that has been superseded and has no users left is reclaimed. A full slab is a typed
    refusal (`ConfigsFull`) of the new configuration. The bound is
    `EndpointConfig::config_slots`. Its default, 4, is one current configuration per side and its
    replacement while it is replaced. That suffices only where replacements are further apart than
    a handshake and a client's old connections are retired before the next.
  - Peers hold their connections across a node's certificate renewals. The first bound assumed a
    connection never outlived a rotation, and each replacement left one more superseded server
    configuration pinned: after two or three, `set_tls` was refused and the node could not present
    its renewed certificate.
  - hyper-transport counts the bound from its admission limits instead:
    `connections + pending + 4`. Every replaced configuration still held has a holder: an inbound
    handshake for a server's, a dial or a connection for a client's.
  - A rotation therefore costs memory only while the handshakes begun under the old server
    configuration, and the connections made with the old client configuration, live.
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
  `disconnect`, `refuse` (closes a peer's connections telling it why: a peer whose grant was
  withdrawn is refused `Identity`, which its side reports as it reports a certificate refused at
  the handshake), `open`, `head`, `write_body`, `read_body`, `body_complete`, `reply`, `end`,
  `refuse_exchange` (ends an exchange its owner refuses, the refusal's code in the stream reset so
  the asker reads the peer's refusal rather than a connection failing: focal's ingress share),
  `reserve`, `release`, `send_frame`, `export_keying_material`, `path`, `credit`, `exchange_tail`
  (over the caller's measured timer granularity), `set_granularity` (that granularity, for tuning
  receive windows) and `stats`.
- Events: `Connected { peer, role, epoch }`, `Request` (with the `epoch` of the connection it came on:
  one identity may hold several connections at once, and what an owner keeps per connection, such as
  focal's greeting, is found by it; focal's listener refused a node's request as ungreeted once its
  second connection closed, 2026-10-10), `Reply`, `BodyReady`, `Writable`,
  and a peer's lifecycle events tell its current connection alone, so `connection_live(peer, epoch)`
  says whether an older one is still up (what an owner keeps for each lets go by it: three clients
  of one identity at once had two refused as ungreeted when only the newest connection was told),
  `Frame { peer, lane, kind, frame }`, `LaneRoom { peer, epoch, lane }` (a lane that refused a
  frame for being full has room again: an owner whose lanes carry many groups' frames, focal's,
  waits for it instead of losing what found the lane full; said once a refusal), `Refused { exchange,
  refusal, by_peer }`,
  `Closed { peer, epoch, refusal }`, `Unreachable { peer, refusal }`; at most
  `Limits::event_bound` wait for the owner (below).
- The TLS is the owner's (below, "The owner's TLS"): `Config { tls, role, limits }` takes a
  `tls::Tls`, the server side (when the endpoint listens) and the client side as hyper-tls
  configurations, `Tls::mutual(credentials, listen)` building the mutual TLS described next;
  `set_tls` replaces it while the endpoint runs; `directory` and `directory_mut` lend the directory
  to its owner between driving calls. `Config::congestion` is the controller every connection's
  paths run, one of hyper-quic's reviewed set (`congestion`, re-exported; CUBIC by default, focal
  runs Copa, §4d), and `PathFacts` carries the path's probe timeout (RFC 9002 §6.2.1, what an
  asker waits beyond the time it gives its peer to answer, for an answer sent at that deadline to
  arrive: focal's asker judged a reply its server sent a millisecond before the deadline as late,
  2026-10-10) and what the controller states of itself: its pacing rate
  and its slow-start threshold, when it has them.
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
reserved from the budget; the stream window is upstream quinn's assembler limit made explicit (patch Q5),
which hyper-quic's own bound, following the window since its patch Q15, no longer needs.
Exchanges are judged by progress-charged deadlines (T39), and never sooner than the path's probe
timeout (hyper-quic's `probe_timeout`, patch Q7: the round trip is what the peer's acknowledgements and
answers take to come). While a request is being sent a period is charged what was sent or what the peer
acknowledged, whichever is more (a sender held by the peer's credit on a slow path sends nothing while
the peer drains the bottleneck); sent whole, it is in the network until the peer acknowledges it
(hyper-quic's `acked_bytes`, patch Q6), and is waited on while each period brings the peer's
acknowledgement of a datagram's worth; the peer's period to answer begins when QUIC reports the
exchange's stream acknowledged whole, never when it was sent (focal's audit F38: a request in a
bottleneck's queue was refused before the peer had it); a stream its peer does not read — out of
credit, a datagram of it and more sent and all of that acknowledged — ends a window after it is found
so, and the exchanges waiting only for the credit it holds are not judged until it comes back (F38's
other half: one unread stream had starved every exchange of its class on the connection, hyper-quic
patch Q8); a period's sent bytes count only if the
peer was heard in it, so probes sent to a dead peer do not keep its exchanges alive; an answer is
charged with the stream bytes the connection delivered of its class and the less urgent ones,
against what the peer declared of them, and ends a period after all of that arrived without it, so
a body its owner writes slowly, or that waits behind the peer's other replies, is not refused while
it moves (focal's residency, priced by the path's round trip, refused an 8 MiB reply at 5.5 MB read
on a loaded runner; `src/progress.rs`); and a period
that moved nothing through this side's own doing (an owner not reading a body, or bytes the peer's
credit would take but this side has not sent) is no evidence against the peer and is not judged. Replication frames travel on lanes, unidirectional streams as wide as the core's
window, always read; a frame its class or the budget cannot take is skipped and counted. A class
may instead be lossless (`Classes::lossless`, 2026-10-15), for frames that are their sender's only
copy (focal's event bus; Raft sends its messages again, a bus does not). Its frame waits for the
budget, its lane unread so QUIC's flow control holds the sender, and asks again whenever bytes come
back (`Reading::Reserving`, `Endpoint::retry_reserving`); a frame past its class bound is still
refused. Its lane holds one frame beyond what QUIC took and then refuses `LaneFull`: QUIC's stream
holds what the peer's credit allows, as a socket's send buffer does, whose writer waits once it is
full, and the sender waits at the sender. Counted in frames alone, focal's bus held 64 MiB behind
a path of about 100 kB, 2.6 s at the median; bounded by the congestion window (TCP Small Queues'
rule, Dumazet 2012) it held as much as CUBIC's window, which on loopback, where nothing is lost,
grew to 1.55 GB: a publisher the node held queued 470 ms in its own lane. And an owner that cannot take more holds the
peer's lane (`Endpoint::hold_lane`, `release_lane`): read no further past its opening, the peer is
held at QUIC's flow control and its own lane fills, where it would otherwise queue in the owner
without bound (a bus node took a stalled subscriber's every publish, 2.5 s at the median); a held
lane's frame that waited for the budget waits for its owner instead, and leaves the count of frames
waiting for memory until it is let go (§4m). Admission
bounds pending handshakes (Retry under load), identities, connections per identity (the idle one
used longest ago replaced, below) and connections in all; every exchange and lane has a table bound.
Every refusal is typed, and one that ends an exchange crosses the wire as the stream reset's code.

**Only an idle connection is replaced** (2026-10-10, focal's fix of 2026-10-09 ported). An identity
at its bound used to replace whichever of its connections it had used least, busy or not: in focal,
64 callers of one principal against a bound of 16 replaced 27,765 connections in 30 s and a third
of their writes ended unknown, each caller's dial closing another's connection under its requests.
A connection is now replaced only when idle: no exchange under way and nothing begun, ended or
carried on it (frames included) for the serve period. With none idle the newcomer is refused,
`Refusal::Busy`, which crosses as the close code so the dialer is told; the exchanges under way
complete. A full endpoint gives an identity under its fair share (the connections among the
identities holding them, the newcomer counted) an idle connection of the identity holding the most,
when that one holds more than the share: max-min fairness (Bertsekas and Gallager, *Data Networks*,
§6.5.2), so one identity alone may use what its bound allows and no identity's share grows at the
cost of one holding less; an identity at its share takes nothing, so two never trade a connection
back and forth (`src/admission.rs`, `tests/exchange.rs`
`a_connection_carrying_an_exchange_is_not_replaced_and_the_newcomer_is_told_it_is_busy`).

**The owner's TLS** (2026-10-10, for focal-wire's layer over this crate). focal's trust is more than
one fixed set of roots: the roots a node verifies clients under change as its issuer is succeeded,
its client verifier adopts a successor its trusted issuer endorsed, it renews its certificate
without rebinding, and one endpoint serves a second protocol, enrollment, whose joiners have no
certificate yet and are served the founder's enrollment certificate. So the endpoint takes the
owner's hyper-tls configurations (`tls::Tls`) and holds them to the transport's rules whatever built
them: early data off on both sides, and an application protocol named on each (RFC 9001 §8.1), so
that every connection negotiates one (`Refusal::Configuration` otherwise). `set_tls` swaps them
while the endpoint runs: the next handshake meets the new certificate and roots, and a connection
made under the last ones stays up until it closes (hyper-quic retires a configuration once the
last connection made under it drains). The directory names a peer from all the handshake
established (`Authenticated`): the certificate, or none when the server's client verifier let the
peer in without one, the protocol it negotiated, and the peer this side dialed, if it dialed; the
role it names is what holds such a peer to the kinds it may send (`Classes::class_of`), and a peer it
does not name is refused (`Refusal::Identity`) and charged to no identity. A dialer is told why a
connection ended or a dial failed, when either side said: a TLS alert from either side (RFC 9000
§20.1's crypto codes) or either directory refusing is `Refusal::Identity`; a peer full at its
admission says which bound in its close code (`Pending`, `Identities`, `Connections` and `Events`
now cross the wire, appended to the codes so that every earlier code keeps its meaning), and its
CONNECTION_REFUSED of an attempt is `Pending`. A TLS 1.3 client's handshake is complete before the
server has verified the client's certificate (RFC 8446 §4.4.4), so a dial whose certificate is
refused is up and then closed: `Closed` carries the cause as `Unreachable` does
(`tests/tls.rs`).

**The events the owner has not polled** are bounded too (`Limits::event_bound`): at most
`4·exchanges + peers·(1 + lanes_per_peer·(lane_window + 1))`, `peers` the peer table's bound (the larger
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
- a lane to a peer holds at most one `LaneRoom`: one arriving while one waits says nothing more;
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
  `CLOCK_REALTIME`; it is carried to `CLOCK_MONOTONIC` by the offset between the two, both clocks
  read once a batch after the receive, the realtime one first so that a preemption between the
  reads makes a stamp late and never early (read the other way, a container throttled to two CPUs
  put a stamp before its datagram was sent, one run in fifty). A slew moves both clocks alike; a
  step of the realtime clock between a datagram's arrival and its read moves the read's offset and
  not the stamp's, so a stamp is carried by the least offset read since the read before the last
  one that found the socket empty, and to no earlier than that read (`clock::Carry`): late by a
  step at most, never early. Carried by the read's offset alone, every forward step of Docker
  Desktop's VM clock (0.28–0.85 ms, every 30 s) put the stamps it fell behind before their sends
  (`docs/benchmarks.md`, "hyper-tokio: kernel stamps across a realtime step"). A stamp is held
  within the read and no earlier than the stamp before it (a socket's queue is first in, first
  out). Linux turns stamping on through a static key flipped from a work queue
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
- **slates' bake-off, run (2026-10-13).** Over slates' grid as the harness ports it (56 paths, the
  four laws, three seeds), Copa is the one law slates' rule admits: its ping p99 1.044 of the best's
  (geometric mean), its worst 3.12 at the step to 2 Mbit/s, its goodput 1.245 short of the best's.
  NewReno and CUBIC stall on the 100 Mbit/s paths with loss, and BBR misses the fairness floor at
  20 and 100 ms (`docs/benchmarks.md`, "slates' congestion bake-off on hyper-quic"). The run found
  hyper-quic's assembler closing a connection under its own sender's losses
  (`crates/hyper-quic/VENDORED.md` §26).
- **Still to come**, from the grid: Copa's slow start at 100 Mbit/s and 300 ms, which leaves it at
  5.5–7.6% of the link; tolerance of reordering (RFC 9002's fixed packet threshold reads ±10 ms of it
  as loss, slates' adaptive tolerance is not ported); Copa's answer to a step down; and slates' 1 ms
  pacing quantum with its two-datagram floor.

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
- **The endpoint visits what its events touch** (2026-10-11). Each turn of the driver offered
  every connection its turn to transmit, drove every connection on a timeout, and read every
  connection's timer and every exchange's wait for the next one. So each exchange cost about
  0.4 µs more for every connection the server held, and focal's leader spent 8% of its CPU on it.
  A connection an event or a call changed is now queued for its turn and placed by its deadline
  in an indexed heap: 512 connections held cost what one does, 40 µs an exchange
  (`docs/benchmarks.md`, "An exchange by the server's connections").
- **BBR starts at the initial window** (2026-10-11). Upstream's BBR started at 200 datagrams,
  240,000 bytes, where RFC 9002 §7.2 starts a sender at ten. The warm-up's target and budget, four
  and sixteen initial windows, scaled with it to 3.84 MB of padding a connection. BBR now starts
  at RFC 9002's window by the datagram size, as CUBIC, NewReno and Copa do
  (`crates/hyper-quic/VENDORED.md` §21).
- **BBR's model from delivery-rate samples** (2026-10-15). Each path keeps what it delivered, and
  each packet in flight what had been delivered when it went (draft-cheng-iccrg-delivery-rate-
  estimation, Linux's `tcp_rate.c`). An acknowledgement's newest packet gives the delivery rate over
  the longer of its flight's send and ack phases, and BBR's bandwidth is the windowed maximum of
  those (`Controller::on_delivered`). Three other divergences from Chromium went with the old
  estimator: startup compared the gain with its target, so an app-limited sender's window grew
  without end; the bandwidth filter took no app-limited sample; and the minimum round trip expired
  before it was ever measured, so every connection began in ProbeRTT
  (`crates/hyper-quic/VENDORED.md` §30).

## 4g. Admission credit: a stream pool (2026-10-13)

A server that refuses what it cannot take still pays for each refusal about what an answer costs:
the request's packets, their decryption, the read of its head, a reply and its packets. focal's
leader on one CPU spent about 19 µs of its CPU on each write it refused at 12,000 writes a second
offered, and committed a fifth fewer than at 1,000. That cost grows with the load offered, so at
50,000 to 100,000 a second the refusals alone take more than a core (focal's A/B #8, 2026-10-12).
Breakwater (Cho et al., "Overload Control for µs-scale RPCs with Breakwater", OSDI 2020) has the
server grant credits, and a client sends only what it holds a credit for. A request the server
could not take waits at its client, or is refused there, and costs the server nothing. QUIC has the
credit already: a peer may open only the bidirectional streams the other side allows (MAX_STREAMS,
RFC 9000 §4.6), and an exchange is a stream.

- **`StreamCredit`** (`Limits::stream_credit`). `Fixed` is what every endpoint did: each
  connection holds `streams_per_connection` from its handshake. `Pooled { initial }` grants
  `initial` at the handshake, since QUIC takes back no stream it granted, and from then on the
  connections of a pooled role (`Classes::pooled`, false unless a side says) share a pool the owner
  sizes with `Endpoint::set_stream_pool`: the exchanges its pooled peers may have open at once, all
  connections' together. A connection of a role outside the pool is raised to
  `streams_per_connection` once its role is known, so a node's replication is never held to its
  clients' pool.
- **Granting** (`src/pool.rs`). A connection that joins while the pool has credit spare is raised
  to its fair share at once (the pool over the connections in it, one stream at least), so a peer
  nobody contends with waits no round trip before its second exchange. Under contention it holds
  its handshake's grant. A peer blocked at its grant says so with STREAMS_BLOCKED (hyper-quic now
  sends it, `crates/hyper-quic/VENDORED.md` §28), and the connection waits its turn, once. At each
  poll, while the pool holds credit no connection is granted, the connection that has waited
  longest is raised to its fair share, or by one if it holds that already, and sends its new
  limit.
- **Giving back.** A grant lowered falls as the peer's streams end. When an exchange its peer opened
  ends, a connection gives one stream back if the pool is overspent: the owner shrank it, or the
  connections' handshake grants exceed it. While another connection waits, it gives one back if it
  holds more than a fair share (max-min fairness among the connections that want streams), and all
  it holds if none of its peer's exchanges is open: credit goes with demand, as Breakwater issues
  it only to clients with requests to send. A connection's own wait is no other's, so a lone one
  keeps its grant; giving it back to grant it to itself again would leave it short until QUIC freed
  the streams it held.
- **When it goes back.** A served exchange settles its stream with the pool as its reply is
  finished, before the finish (`Endpoint::settle_credit`): QUIC gives the stream back to its peer
  once the request is read and the reply finished, not once the reply is acknowledged
  (`crates/hyper-quic/VENDORED.md` §29), so the raised limit goes out in the reply's datagrams, as
  Breakwater issues credit in its responses. Settled after, QUIC would have given the stream back
  within the connection's old grant, past a pool that shrank. Through a pool of one, twenty
  exchanges asked in turn take 40 datagrams of each side where the stream that went back once the
  reply was acknowledged, alone, took 59 (`a_pooled_servers_stream_goes_back_with_the_reply`).
- **Taking back from the idle.** QUIC takes back no stream it announced, so a connection holding
  credit that opens nothing would keep it from every other: it gives back only as its exchanges
  end. A connection with none of its peer's exchanges open is queued as idle, once: as it joins,
  and as its last exchange ends with credit kept. While another waits and nothing is spare, the
  endpoint takes back what an idle connection holds, in the pool's account (`Stats::pool.reclaimed`
  counts it). Its peer may still open the streams it was told of, so the pool is overcommitted by
  them, as Breakwater overcommits credit to idle clients and keeps its queue by refusing what an
  overload sends past it: the owner's own admission.
- **Bounds.** A connection waits once and is queued as idle once at most, so each queue holds no
  more than the pooled connections. Exchanges open at the server at once are at most the pool,
  plus the handshake grants not yet given back, plus what idle connections were told of before it
  was taken back from them.
- **The asker's side.** An exchange waiting for a stream is judged on whether its connection's
  credit queue moves (policy C), so it waits while streams come back. If none comes for a period,
  it is refused `Unsent`, a refusal of its own, where it was refused `Stalled`, which an exchange
  that reached its peer is refused too. A request refused `Unsent` is known not to have been taken,
  so its asker may send it again, where after `Stalled` it cannot know.
- **The asker's own door** (`Limits::ask_queue`). Under `AskQueue::Table`, today's, only the
  exchange table bounds what waits for streams, so a pooled peer's overload would move its queue to
  its askers, where it would wait seconds rather than be refused. Under `AskQueue::Credit` an
  exchange asked while its connection already waits for a round of its credit (the streams it
  holds and those it may still open, hyper-quic's `Streams::remaining`, one at least) is refused
  `Unsent` at `open`, nothing sent. The wait an asked exchange meets is held to about a round of
  what its peer grants, as the peer's admission holds its own queue to what it answers in its
  bound, and a refusal there costs the peer nothing. The counts are the tally's
  (`Tally::asked`, checked against their fold under the oracle).
- **Tests.**
  - `pool::tests`: a lone connection raised to the pool and kept there, two connections converging
    to equal shares, a shrunken pool given back as exchanges end, an idle connection giving its
    grant to one waiting, a busy one at its fair share keeping it, a joining connection welcomed
    with what is spare, and one waiting once and passed once gone.
  - `tests/exchange.rs`: `a_pooled_peer_holds_back_what_the_pool_cannot_take` (ten exchanges asked
    at once against a pool of three: never more than three open at the server, every one answered)
    and `a_pool_sized_to_nothing_holds_its_peer_until_sized_again`.

## 4h. A connection's batch in one call, and one set of receive buffers (2026-10-15)

focal's event bus, measured against NATS (each node on one CPU, Linux 6.x in Docker), carried 45,000
to 56,000 messages a second whatever was offered from 100,000 to 100,000,000 a second, where NATS
carried about 250,000. Diagnostic builds of the bus at 1,000,000 a second offered (three runs each)
traced it to the driver's sends, which two bounds held to a datagram or two a system call:

- hyper-transport asked hyper-quic for one datagram at a time (`poll_transmit(now, 1, ..)`), so each
  datagram cost its own pass through the connection's packet assembly and a copy into an outbox
  slot; and
- a peer's socket (hyper-rt-transport's socket a peer, its driver's module doc) batched two datagrams a call, because each socket held its
  own receive buffers, 64 KiB each, for as long as its peer was connected. A peer's bulk therefore
  left two datagrams a `sendmmsg`, never enough to segment.

A peer batch of 64 in place of 2 took the bus from 118,000 to 144,000 a second. With the kernel's
charge counted at twice the payload rather than four times (the socket room, still open, below), it
reached 257,000, with nothing dropped in either.

The design is quinn's (its endpoint driver and `quinn-udp`):

- **A batch at a time.** The endpoint makes up to `SEGMENTS_A_TRANSMIT` (64, `UDP_MAX_SEGMENTS`) of
  one connection's datagrams at once (`Endpoint::poll_transmit_batch`). They go into the driver's one
  buffer, each but the last of the transmit's segment size; hyper-quic already built such batches for
  any caller that asked (`enable_segmentation_offload`, on by default).
- **Sent from the driver's buffer.** The socket sends a batch with no copy (`Batched::send_segments`).
  Where the kernel segments, it uses one `sendmmsg` of one message carrying `UDP_SEGMENT`, as many
  messages as the kernel's bounds need (64 segments, 65,507 bytes). Elsewhere it sends a datagram a
  call. What a socket's outbox already holds leaves first, so datagrams leave in the order they were
  made.
- **Nothing dropped for room.** When the socket takes only part of a transmit, the driver holds the
  transmit with its progress. It awaits that socket's writability and sends nothing the endpoint
  makes until the transmit is done.
- **One set of receive buffers.** Every socket receives into one `Ring` that the driver lends to
  whichever socket it reads (`Batched::receive_with`). The ring holds `Io::batch` buffers of 64 KiB on
  Linux and one elsewhere, where each peer's socket had held its own. A peer's socket
  (`Batched::lent`) holds only its descriptor and the headers of a one-message call; its own
  `receive` is refused `BadConfig`, since it has nothing to receive into.
- **When the path cannot segment.** A segmented message refused EIO (udp(7): a device without
  checksum offload) or EINVAL turns segmentation off for the socket, and the message goes again a
  datagram a call. The kernel returns EINVAL to a socket sending without checksums (`udp_send_skb`),
  and quinn-udp turns segmentation off on EINVAL because drivers have returned it. A one-datagram
  message was not segmented, so its error is its own. This applies to the outbox's batched sends
  too. Before, they turned segmentation off only on EIO: a socket refused EINVAL dropped every
  segmented message, counted as send errors, so its connection lost its bulk until it timed out.

**Bounds.**
- The ring: `Ring::held_bound`, a batch of 64 KiB buffers and their headers on Linux.
- One transmit buffer: 64 of the path's datagrams, 93 KB at 1,452 bytes.
- Per peer socket: the headers of a one-message call and an empty one-slot outbox. A thousand peers
  had held 128 MiB of receive buffers between them; they now hold none of their own.
- Each turn sends at most `MAX_BATCH` datagrams before its other work, as before.
- `send_segments`' loop ends within three passes a datagram, plus two. Each pass sends a message,
  returns, marks one astray report, or turns segmentation off once.

**Tests.**
- hyper-rt `tests/udp_batched.rs`:
  - `a_segmented_send_crosses_whole_and_in_order_from_the_callers_buffer`: 150 datagrams of
    100 bytes (64 a message) and 60 of 1,200 (54 a message), each last one shorter, over IPv4 and
    IPv6. Each arrives once, in order, at its own length. On Linux the batch takes exactly as many
    calls as the bounds need; elsewhere, one a datagram.
  - `what_the_outbox_holds_leaves_before_a_segmented_send`.
  - `a_segmented_send_the_socket_does_not_take_is_done_by_none_and_sent_again`: on the simulation
    fabric, whose sends can be blocked.
  - `one_ring_serves_every_lent_socket_and_none_holds_its_buffers`.
  - `a_socket_the_kernel_will_not_segment_for_sends_each_datagram_alone`: `SO_NO_CHECK`. It fails
    under the EIO-only rule, with all ten datagrams refused.
- hyper-rt-transport `each_connection_receives_on_a_socket_of_its_own_and_lets_it_go_with_the_connection`:
  on Linux, the client's peer socket sent 1,459 datagrams in 92 calls, where it had been held to two
  a call. macOS took 1,449 calls for 1,449 datagrams, since it has no segmentation.

**A drain takes what was queued when it began.** Batching made each of the driver's receive calls
carry up to its ring's batch: ten messages for focal's bus, each possibly a 64 KiB coalesced train.
A drain read a socket until it emptied, or until it had made `TURNS` calls (tokio's cooperative
budget). A peer that kept sending therefore kept the driver reading, up to 1,280 messages a turn
where a peer socket's two-slot batch had allowed 256. The driver sends nothing while it reads: not
the frames it forwards, and not the credit its peers wait for. In an interleaved A/B (three runs
each, node on one CPU) the batched build carried 14% more at 1M/s offered on 14% less CPU. But its
p99 rose from 0.21 to 1.00 ms at 10k/s and from 0.23 to 0.82 ms at 100k/s. Its publisher was held
for up to 12 ms at a time (base: 1.3 ms) with nothing lost: the room of 106 KB lasts 1.5 ms of
publishing at 100k/s. A drain now takes the backlog it found (`Batched::receive_queued`) and leaves
what arrives meanwhile for the next turn, as an event loop runs the callbacks queued when its phase
began. What was queued is told by:
- the kernel's receive stamps where the socket has them (Linux `SO_TIMESTAMPNS`, macOS
  `SO_TIMESTAMP_MONOTONIC`): a datagram stamped after the drain began arrived since. A stamp is
  late, never early, so a busy machine can end a drain sooner, never later;
- elsewhere, the bytes the socket held at the first call (`UdpSocket::queued_bytes`, `FIONREAD`).
  Winsock counts every queued datagram ("Winsock IOCTLs": "the total number of bytes available to
  read, not the size of the first datagram"); XNU counts them with their addresses and control
  (`soioctl`: `so_rcv.sb_cc`); the fabric counts its own.

The endpoint is still told when each datagram was read (`Arrival::read_ns`), as hyper-tokio's driver
tells it. hyper-tokio's driver drained to empty on purpose ("so that the socket's receive buffer is
drained before anything is surfaced"), a guard the socket room has made needless. It now takes the
same rule: its socket is bound with the kernel's stamps, and on Windows it counts `FIONREAD` bytes.
tokio's readiness stays set until a read would block, so a datagram left for the next turn is read
at once. `socket::tests::fionread_counts_the_queue_as_the_platform_does` holds each platform to the
count its source states:
- Linux: the next datagram's alone (100 of three datagrams' 600 bytes);
- XNU: every one's with its address (past 600);
- Winsock: every one's (600).

**A socket left holding datagrams is work now.** A drain that stops with datagrams waiting marks
its socket for the next turn. Readiness is one-shot (epoll's `EPOLLONESHOT`, kqueue's `EV_ONESHOT`)
and fires when a datagram arrives, not for one already waiting, and the driver did not count a
marked socket as fired. A driver whose only work was such a socket therefore parked until its next
timer or another arrival woke it. Now a marked socket, the listener's or a peer's, keeps the turn
loop going (up to `TURNS` turns before it yields to the shard and wakes itself, counted by
`Driver::yields`). `Driver::backlogged` counts the sockets left holding datagrams. Test:
`a_peer_socket_left_holding_datagrams_is_drained_without_another_wake`. More datagrams than two
drains take wait in a peer socket, and whenever the server's poll ends pending without having woken
itself, no socket may be left holding any. Under the old rule it parked after one drain, 128 of 257
read; under load it passed 40 of 40 runs, with every core held by busy loops. Its first version
asserted instead that no timer fired before every datagram was read. That failed under the gate's
parallel load, where a timer 65 ms away came due first: a test that rests on timing is not one a
busy machine passes.

The socket room's charge was then a fixed four times a datagram's payload. §4i replaces it with
the kernel's own account, measured.

## 4i. A socket's room, from what the kernel is measured to charge (2026-10-16)

The windows a socket's peers are granted, summed, never pass what its receive queue holds (§4a's
socket room, `credit.rs`), so a reader the machine stops drops nothing. What the queue holds
depends on what the kernel charges each queued message. The room used to assume four times its
payload on Linux and twice elsewhere. That rule was an arbitrary constant: three loopback
measurements on one kernel, rounded up. It was wrong in both directions. Linux charges a GRO train
little more than its payload, a 1,252-byte datagram 1.84 times, and a 100-byte one about nine
times. The rule is gone. The room is now learned from the kernel's own account, as Linux TCP
learns its `scaling_ratio` from each segment's length over its truesize (`tcp_measure_rcv_mss`,
Linux 6.6), and nothing in it is assumed.

- **The account** (hyper-rt `UdpSocket::queue_charge`, hyper-tokio's own):
  - Linux's `SO_MEMINFO` (`sk_get_meminfo`): what the queue is charged
    (`SK_MEMINFO_RMEM_ALLOC`), its buffer and its drops. A quarter of the buffer may stay charged
    for datagrams already read while others wait: `udp_rmem_release` releases in batches of
    `forward_threshold = sk_rcvbuf >> 2`.
  - macOS's `FIONREAD`, which is `sb_cc`: each datagram's payload, address and control, against
    `sb_hiwat` (`soioctl`, `sbappendaddr`). XNU also limits mbuf storage to `sb_efficiency`, 8,
    times the byte limit (`sbspace`, `sbreserve`), which nothing reads. So a message is charged at
    least its mbufs over 8: its address's, control's and data's, the data's at most XNU's largest
    (jumbo) cluster (`MSIZE` 256 and `M16KCLBYTES` 16,384, `bsd/arm/param.h`), 2,144 bytes.
  - The simulation fabric's own account: each datagram its payload, admitted only while it fits.
  - Windows exposes none. Its room stays at the first windows admission lets in until a
    measurement of AFD's capacity replaces it (open, below).
- **A sample** is a message read as the only one queued (the singleton rule; hyper-transport's
  `SocketCharge`). The account is read before a drain that starts with nothing behind it. If the
  drain took exactly one message and emptied the socket, and the account after it is nothing, the
  account before was that message's charge exactly. A datagram that came between the two reads
  shows as a second message, or as a charge after. The stream bytes it carried are the difference
  in the STREAM frame bytes its connection received (hyper-quic patch Q20,
  `stream_bytes_received`).
  - The first build took the difference in flow control's count instead, the receive streams' end
    offsets summed, which moves by a gap's bytes when a frame lands past one. A datagram read
    alone after a loss then seemed to carry more stream bytes than its whole payload. In one busy
    run at 100k/s the room told reached 1.98 MB against a buffer of 426 KB, and the node's sockets
    dropped 376,148 datagrams.
  - A sample past the message's own bounds is not taken: more stream bytes than its payload, or a
    charge below its payload, which no kernel makes.
  - Tests: `stream_bytes_received_count_what_frames_carried` (hyper-quic) and
    `a_sample_past_the_messages_own_bytes_is_not_taken`.
- **The room** (`ReceiveCharge::room`): the buffer, less the release lag, less what peers may send
  unprompted by any window, at the costliest stream byte sampled. The unprompted messages are an
  acknowledgement for each of this side's ack-eliciting packets in flight (RFC 9000 §13.2: a
  receiver may acknowledge every one) and the loss probes of the idle timeout: two at each probe
  timeout (RFC 9002 §6.2.4), the timeouts doubling (§6.2.1) from `kGranularity`, 32 for a 30-second
  timeout. Each is one datagram that comes alone. An acknowledgement and a probe are charged as the
  costliest datagram alone of the smallest datagram's size, and the few datagrams of up to the
  path's MTU a peer sends unprompted as the costliest alone (§4r, §4s); retransmissions take no new
  window, so they count here. A GRO train is one message of many datagrams and gives only
  its charge per stream byte. Its share a datagram stands in until a datagram has come alone. The
  first build charged probes as the costliest message: one train sampled made 32 probes cost 32
  trains, past the whole queue, and the room fell to nothing. In the busy A/B the publisher was
  then held 56 times at 10k/s, against 8 (`a_train_is_no_probes_charge`).
- **What the worst is taken over:** the current window epoch and the last that brought a sample.
  An epoch ends once the peers have consumed the room it began with, so every window still granted
  was granted within the two. An epoch without a sample keeps the worst it had: under load a socket
  is read behind a backlog, where no read is a sample. A drop the kernel reports
  (`SK_MEMINFO_DROPS`) forgets the samples, and the room falls back to the first window until new
  ones come.
- **Before anything is measured:** the queue is asked for the windows' demand, the least it could
  need, a byte charged a byte. The kernel grants what it will; Linux caps an unprivileged ask at
  `rmem_max`. The room is the first windows admission lets in. What a peer sends before this side
  first acknowledges it is bounded by the protocol: RFC 9000 §8.1's three times what it was sent
  before its address is validated, and RFC 9002 §7.2's initial window. On Linux an empty queue
  admits any one datagram, so the first datagram read is the first sample. A peer's socket starts
  from what the listener learned of the host's receive path.
- **Tests.**
  - hyper-transport `charge::tests`: the room at the costliest stream byte; unprompted messages
    reserved first; the worst held for the epoch after its own; epochs without samples keep what
    was learned; a drop forgets; probes doubling to the idle timeout.
  - hyper-rt `a_socket_tells_what_its_queue_is_charged` (Linux: charged past the payload, a
    quarter of the buffer the lag, nothing once read; macOS: 1,016 bytes for a 1,000-byte datagram,
    the payload and a 16-byte `sockaddr_in`, as `sbappendaddr` counts it) and
    `a_simulated_socket_is_charged_its_payload`.
  - hyper-rt-transport `a_socket_holds_the_room_told_from_its_measured_charge`, end to end on the
    real kernel: one datagram's charge sampled, the room told from it, and that room's worth sent
    unread, every one there to read and none dropped.
    - Linux 6.x in Docker: a 1,252-byte datagram charged 2,304; a 32 KiB buffer less its 8 KiB lag
      held a room of 13,354 bytes, 10 datagrams.
    - macOS: 1,268 bytes counted, the mbuf floor of 2,144 ruling; a room of 19,135, 15 datagrams.
- **Open:** Windows. Winsock's `FIONREAD` counts every queued datagram's bytes, but AFD's charge per
  datagram against `SO_RCVBUF` is documented nowhere. A self-probe at the driver's start can
  measure it: fill a probe socket of the same buffer until its count stops growing.

## 4j. A frame that carries its own checksum is summed once (2026-10-16)

Every message's prefix carries a CRC-32C of the prefix and the head, mantle's rule that a network
payload carries a checksum verified on read, which catches a framing bug the AEAD cannot. A bus
batch carries its producer's CRC-32C too (Kafka's record batch CRC, KIP-98), which its broker checks
as it appends and its consumer as it takes it, end to end (Hochschild et al., HotOS 2021). So a bus
node summed each batch three times, its prefix's on receipt, the batch's, and its prefix's again
to deliver it, and a subscriber twice: CRC-32C was 3.6 % of the node's busy samples at 100,000
messages a second (macOS, `sample`, 2026-10-16).

`Classes::embedded_checksum(kind)` says where a lane frame of a kind carries its own CRC-32C
(`Embedded { covered_from, at }`: of the bytes from `covered_from`, little-endian at `at..at + 4`,
which lies before `covered_from`). For those kinds the prefix's checksum covers the prefix and the
frame only before `covered_from`, and the receiver verifies the frame's own checksum where it would
have summed the same bytes again; each byte is covered by exactly one checksum, verified on read.
A frame whose own checksum fails is refused as corrupt, as one whose prefix's fails is, and its
lane stops (`Stats::lanes_refused`, which counts every lane a refused frame stopped). The bus
declares it for publishes and deliveries (`focal_bus::batch::EMBEDDED`, the 24 bytes before it
holding the length, the CRC and the fields its owner stamps), so a node sums each batch once and
skips the batch's own pass where the transport made it (`Batch::parse_verified`). Tests:
`frame::tests::a_frame_s_own_checksum_and_its_prefix_cover_every_byte_once`,
`a_frame_of_its_own_checksum_is_checked_by_it`.

## 4k. Acknowledgements decimated (2026-10-16)

Every connection asks its peer to acknowledge as Google QUIC's receivers do
(`TransportConfig::decimate_acks`, hyper-quic patch Q22): an ACK for every 10 ack-eliciting packets
or a quarter of the smoothed round trip after the first one waiting, at least the peer's 1 ms
granularity, from the 100th packet on; a reordering elicits one at once only where it would let the
sender declare a loss. Every peer is hyper-quic, which takes the extension, so the request always
holds.

RFC 9000 §13.2.2's default, an ACK for every other ack-eliciting packet, was a third of what a bus
node handled. On the bare-metal bench (Ryzen 9 9950X3D2, the node alone on one core, the claim
corpus's 1,150-byte messages, each a batch of its own at 100,000 a second), a node took 1.21
datagrams of a message's publish and 0.50 of acknowledgements from its subscriber, and sent 1.15 of
its delivery and 0.50 of acknowledgements to its publisher: 3.4 datagrams a message, each a system
call's share, a wake and, for each it sent, the receiver's path through the kernel. Decimated, the
acknowledgements fell to 0.14 and 0.15 (2.7 datagrams a message) and the node's CPU a message by 6 %
(6.10 µs against 6.50, three interleaved pairs): an acknowledgement is a small packet, and what a
message costs is mostly its own wake and its two datagrams.

What changes for loss recovery: the probe timeout covers the delay the peer was asked
(RFC 9002 §6.2.1), so on a path of a few milliseconds or less it is the round trip and 2 ms, where
the default's 25 ms delay held it past 25 ms; a tail loss is probed sooner. The extension's own
bounds hold: the delay asked stays under the smoothed round trip wherever the peer's granularity
allows (draft-ietf-quic-ack-frequency-14 §8.1), and the pacer, not the acknowledgements, spaces a
sender's bursts (§8.2).

## 4l. A socket's account read at an epoch's doublings (2026-10-16)

The measured room (§4i) read each socket's account, `SO_MEMINFO` (a `getsockopt`), before every
drain, and again after a drain that took one message alone: two system calls a drain wherever the
traffic comes a datagram at a time. On the bare-metal bench a bus node at 10,000 messages a second
made 8.3 system calls a message, 2.4 of them these reads (1.7 of 5.3 at 100,000).

The samples were worth taking only as the room needs them. The room is the worst charge a stream
byte over the current window epoch and the one before (§4i), and an epoch that brings no sample
keeps the worst it had, so it is enough that each epoch bring one where it can, and that a change
of traffic within an epoch be read soon. A socket now reads its account at every drain until a
message carrying stream data is measured, and then at an epoch's drains 0, 2, 6, 14, …, each read at
twice the drains before it (`SocketCharge::wants_before`): an epoch of `n` drains reads about
log₂ `n` times, and an epoch's end starts the count again. A read that finds a singleton is a sample
as before.

The reads also said when the kernel had dropped what the queue could not hold, which forgets the
samples (§4i). A drop is now told by the traffic itself: each socket asks the kernel for
`SO_RXQ_OVFL`, the count of datagrams it dropped for want of room, attached to each datagram queued
once the count is past zero (socket(7)), and the driver gives the socket's latest count to its
measure after each drain (`SocketCharge::drops`). A drop after the last datagram received is told by
the next, which is when the room is next used. Linux only; the account on macOS tells no drops, as
before. hyper-tokio keeps its reads before every drain: its sockets do not parse the count.

Measured on the bare-metal bench, interleaved A/B pairs against the same tree reading before
every drain:

| Rate, condition | Pairs | CPU a message (median) | p99 | p99.9 |
|---|---|---|---|---|
| 100,000/s | 3 | 6,082 ns (6,207 before) | 0.56 ms (0.62) | 1.80 ms (1.91) |
| 10,000/s | 3 | 7,135 ns (7,434) | 0.04 ms (0.04) | 0.14 ms (0.22) |
| 10,000/s, busy core | 3 + 6 | 7,767 and 8,057 ns (7,920 and 8,609) | 0.04 ms (0.04) | 0.30 and 0.61 ms (0.11 and 0.85) |

The busy rows are two passes, the second with other work loading the machine. No socket dropped a
datagram (`RcvbufErrors` 0). The largest room measured rose by about a fifth (1.46 MB to 1.69–1.82
MB): fewer samples see fewer of an epoch's worst charges.

Tests: `charge::tests::the_account_is_read_at_doublings_of_an_epochs_drains` (the reads' drains,
through two epochs), `a_drop_the_traffic_tells_forgets_the_samples`, and on Linux
`udp_batched::a_drop_is_told_by_the_next_datagram` (an overflowed socket's next datagram counts
every datagram it could not hold).

## 4m. A held lane's frame waits for its owner (2026-10-16)

A lossless frame the budget cannot hold waits in `Reading::Reserving`, counted in
`Stats::frames_reserving`, and an owner that can give memory back reads that count: focal's bus
node gives such a frame the room of its window's oldest batches. A lane its owner holds (§4a) is
not read, so its waiting frame is not asked again whatever the budget holds, but it stayed
counted. On the bare-metal bench a node held a publisher whose frame had been refused while its
memory was short (1,000,000 messages a second on a busy core). From then on it saw a frame
waiting at every turn and gave it the window: every batch went, the node holding 16 MB of its
1.5 GiB, and the subscriber the publisher was held for was told a gap of 1,817 offsets.

A held lane's waiting frame now waits for its owner: holding the lane takes the frame out of the
count, and letting it go puts the frame back and reads it on (`mark_lanes`). The count is also
kept for each connection, and a retry visits only the connections with a frame waiting
(`Endpoint::retry_reserving`). It used to take every connection out and put it back, which had
each one given its turn to transmit and placed again by its deadline at the next poll
(`read_touched`). `Endpoint::frames_reserving` reads the count without visiting every
connection, as `stats` does.

Test: `exchange::a_held_lanes_frame_waits_for_its_owner_not_the_budget`. Holding the waiting lane
takes it out of the count, and a retry and the budget given back read nothing of it. Let go, it
is reserved and read on, and every frame arrives in order.

## 4n. CRC-32C chosen at run time (2026-10-16)

Every prefix, opener and frame carries a CRC-32C, and a frame that carries its own (§4j) is
summed once as it arrives: a bus node sums every message it takes. The crc32c crate reaches its
hardware path only in a build for the machine's own CPU; a build for the generic target, as the
release builds are, takes a path eight times slower. crc-fast chooses its widest carry-less
multiply when it runs. On the bare-metal bench (Ryzen 9 9950X3D2, CPU 15, best of five passes):

| Bytes | crc32c, generic build | crc-fast, generic build | crc32c, native build |
|---|---|---|---|
| 1,230 (a message) | 135.5 ns | 18.1 ns | 41.1 ns |
| 16 KiB | 1,650 ns | 192 ns | 456 ns |
| 1 MiB (a batch) | 98.4 µs | 12.3 µs | 25.1 µs |

hyper-transport sums with crc-fast (`src/crc.rs`); the function and its values are the same, a
running sum going on from the inverted register. Its other crates keep the crc32c crate for now.

Tests: `crc::tests::the_checksum_is_crc32c_and_goes_on_from_any_division` (the catalogue's check
value, and every division of a buffer giving the whole's sum), and `exchange.rs`'s frame whose own
checksum a producer writes with the crc32c crate.

## 4o. A frame on an idle lane is written straight into its stream (2026-10-16)

`send_frame` copied every frame into a buffer of its own, queued it on its lane, and the lane's
flush copied it again into QUIC's stream: two copies of each frame before QUIC's own. At saturation
a bus node spent 1.43 % of its time in the first and 1.36 % in the second (perf, memcpy's callers).
A frame sent on a lane that holds none, its opening written, is now written straight from the
sender's bytes into the stream, within what its class may spend (`write_idle`); only what QUIC
leaves is copied into the frame's buffer and waits in the lane, behind nothing. The buffer is
reserved before anything is written, so a stream that takes part of a frame always leaves room for
the rest, and one that takes all of it gives the buffer back unused. `Stats::frames_direct` counts
the frames written so.

Test: `exchange::a_frame_on_an_idle_lane_is_written_straight_into_its_stream` (40 frames, each sent
once the lane is drained: the first opens the lane through its queue, the other 39 go straight in,
every frame arrives in order).

## 4p. A stream's unacknowledged data read by binary search (2026-10-16)

QUIC keeps a stream's data until it is acknowledged, a piece for each write (`SendBuffer`), and a
packet's stream frame reads its bytes from them by offset. The read walked the pieces from the front,
so every packet of new data passed every piece in flight before it: two a lane frame (its prefix and
its body), and decimated acknowledgements (§4k) keep a millisecond of them in flight, some two
hundred at 100,000 frames a second. On a busy core the walk was most of `populate_packet`'s time. Each
piece now carries the stream offset of its first byte, and a read finds its piece by binary search.
An acknowledgement in order, with none past it, moves the front without the set of ranges
acknowledged ahead, where each acknowledged frame cost an insertion into that B-tree and its removal.

Busy core, bare-metal box, three interleaved pairs, medians:

| Offered / s | Before: CPU a message | After | p50 before | p50 after |
|---|---|---|---|---|
| 100,000 | 3,978 ns | 3,926 ns | 0.34 ms | 0.32 ms |
| 1,000,000 | 1,196 ns | 1,177 ns | 12.31 ms | 11.08 ms |

Tests: `send_buffer`'s `reads_find_their_piece_among_many` (400 uneven writes, acknowledgements
trimming the front, every read against the bytes written) and
`in_order_acknowledgements_leave_the_set_alone`.
## 4q. MTU discovery searches up to the path's MTU as the kernel holds it (2026-10-16)

quinn's defaults bounded the search at 1,452 bytes and told every peer it took 1,472 (Ethernet's
MTU less the headers), whatever the path carried. A bus node then sent one 1,452-byte datagram for
each 1,150-byte message it took and one for each it delivered, every datagram encrypted and
processed alone: on a jumbo-frame bridge (MTU 9,000) it delivered no more than on Ethernet's.

**The ceiling.** Each connection's search is bounded by what the peer takes and what its path
carries (RFC 8899 §5.1.2's MAX_PLPMTU), the path read from the kernel when the driver sees the
connection established, before its first probe (`Driver::tell_ceilings`): a socket of the peer's
family is connected to it, asked and closed (`UdpSocket::path_payload_to`; connecting a datagram
socket sends nothing). Linux and Windows hold the path's MTU (`IP_MTU`/`IPV6_MTU`, the route's or
less where an ICMP "packet too big" taught it less); XNU holds none, so macOS reads the MTU of the
interface holding the socket's source address (`getifaddrs`, `if_data.ifi_mtu`) under the host's UDP
cap (`net.inet.udp.maxdgram`, 9,216 bytes by default). The payload is the MTU less the IP and UDP
headers, never past what the length fields can say (65,507 for IPv4, 65,527 for IPv6). The endpoint
tells peers it takes any datagram its receive chunks hold (`RECEIVE_CHUNK`, 65,527 bytes). A
driver that knows its links before any path (a simulation, the tests' network) sets a ceiling every
new connection starts under (`Endpoint::set_default_path_ceiling`).

**What the search does with it.** A ceiling learned mid-search bounds the rest. A probe in flight
past a lowered ceiling stays the search's, so its loss is a probe's and not congestion, but its fate
no longer moves the search. A ceiling raised past a finished search starts the next one now.

**What a larger MTU asked of the rest.** Congestion control and the pacer budgeted every datagram at
a whole segment. At an MTU of 65,527 (the most a loopback path carries) RFC 9002 §7.2's minimum
window is two datagrams: with one in flight a packet of a few hundred bytes waited for an
acknowledgement, and with nothing more to send the peer's acknowledgement waited for its delay. A
window or bucket that cannot take a whole segment but can take a datagram every QUIC path carries
(RFC 9000 §14, 1,200 bytes) now takes one cut to what it can, as TCP sends what its window allows,
and the batch ends with it (`Connection::congestion_room`). The pacer, which releases nothing until
it holds a whole datagram, is credited one datagram of a larger MTU when the MTU rises, as a
controller's window is raised to its minimum on the same news. And the driver matches its sockets to
connections established in a turn before that turn's transmissions, so no first probe leaves before
its ceiling: one of 33,363 bytes on a 1,500-byte path, refused at its send, counted in flight until
found lost, held a new connection's data behind its window for a round trip.

Busy core (the node's CPU shared with a busy loop), bare-metal box, 1,000,000 messages a second
offered, every system on a bridge of MTU 9,000, three interleaved rounds, medians:

| Node | Path MTU found | Delivered / s | p50 | p99 | p99.9 | Lost | CPU a message |
|---|---|---|---|---|---|---|---|
| bus, search to 1,452 | 1,452 | 419,117 | 11.72 ms | 18.38 ms | 19.58 ms | 0 | 1,173 ns |
| bus, search to the path | 8,972 | 878,956 | 3.75 ms | 6.78 ms | 8.34 ms | 0 | 541 ns |
| NATS 2.12 | - | 905,957 | 8.18 ms | 64.13 ms | 99.94 ms | 15,555 | - |

On the default bridge (MTU 1,500) the search finds 1,472 and nothing else moves: busy-core 100,000 a
second, 3,979 ns a message against 3,975 before.

Tests: `udp`'s `a_connected_socket_reads_its_paths_payload_from_the_kernel` (Linux; macOS and Windows
have their own) and `a_listener_reads_the_path_to_a_peer_as_a_connected_socket_would`;
`peer_sockets::a_connections_mtu_search_ends_at_its_paths_mtu_as_the_kernel_holds_it` (two drivers
on loopback, each search ending at the kernel's payload with no probe lost); `mtud`'s four;
`pacing::an_mtu_rise_leaves_a_datagram_of_the_new_size_free_to_go`; the exchange tests' network now
drops a datagram past Ethernet's payload and tells each node so (`common::LINK_PAYLOAD`).

**A probe waits for the window and the pacer** (2026-10-17). A search to the path's MTU made each
probe as large as the path allows, and a probe went whenever the connection was idle, past the
congestion window and the pacer, as upstream's did for probes of at most 1,452 bytes. Behind a relay
of 128 kbit/s in front of a loopback (MTU 65,536), the first step, about 33 KB, held the link two
seconds: focal's three nodes behind such relays never heard each other's pre-votes. A probe is now
sent only when it fits the window and the pacer lets it go (RFC 8899 §3, item 7; hyper-quic patch
Q25): a window of one round trip of the path never affords a probe that would hold the path longer,
so a slow path keeps its MTU and a fast one probes as before (`crates/hyper-quic/VENDORED.md` §36).

## 4r. A small datagram's charge, and each handshake counted once (2026-10-17)

Once MTU discovery reached each path's MTU (§4q), focal's cluster tests on macOS refused new
connections: 9 of focal-node's 414 failed (`credential_renewal`, `liveness_tests`,
`network_service`, `placement_agent`). A search's probes are the largest datagrams a peer sends,
and each comes alone. So a 9,216-byte probe (macOS's UDP cap), charged 9,248 bytes, became the
costliest datagram sampled (§4i), and the room charged every unprompted datagram as one. focal's
shared socket reserves for 32 handshake places (a quarter of 128 connections) of 30 probes each
(a 10-second idle timeout) and for two connections without sockets of their own: 1,020 probes,
which with two acknowledgements reserved 9,451,456 bytes of an 8 MiB queue. The room was nothing:
every first window was refused (`Room::first`) and every dial ended in CONNECTION_REFUSED
(`Refusal::Pending`), 1,696 times in one run.

The kernel charges a datagram by its size, and a sample now carries its payload (`Charged::bytes`).
So each kind of unprompted datagram is charged as the costliest datagram seen alone of the size it
can have:

- **An acknowledgement fits the smallest datagram.** hyper-quic acknowledges at most 64 ranges
  (`MAX_ACK_BLOCKS`; upstream's reason: "we can always fit all our ACKs in a single minimum-MTU
  packet"). That bounds an ACK frame at 1,065 bytes (RFC 9000 §19.3, each integer at most 8 bytes,
  ECN counts included), or 1,106 with a short header and the AEAD's tag. So an acknowledgement is
  charged as the costliest datagram alone of at most `MIN_DATAGRAM` (1,200 bytes).
- **So does a handshake's probe.** hyper-quic sends nothing larger than its initial MTU (1,200)
  until its handshake is done; it probes for more only once established
  (`Connection::send_when_idle`).
- **An established connection's probe** may be full-sized, and is charged as the costliest datagram
  alone, as before.
- **An MTU probe** is now counted: one for each peer that is or may be established. A search has
  one probe in flight, and a probe is declared lost only once a later packet is acknowledged
  (RFC 9002 §6.1). So a peer this side has stopped acknowledging sends no second probe.
- **A handshake place** holds a handshake's probes and one MTU probe. A client is established once
  it sends its Finished, its server only once it reads it (RFC 9001 §4.1.1), so a client may be
  probing while its server waits. A handshake that waited in a place was also counted as a
  connection, its probes at the costliest datagram, so every handshake admission let wait was
  charged twice. Now only its acknowledgements are counted beside its place. A dial's probes are a
  handshake's, and its server, established only after it, sends no MTU probe.

On the same tree, focal-node's library passes on macOS: 414 of 414, against 405 and 9 failed.

**Measured on Linux.** Docker's VM grants the default buffer (`rmem_max` 212,992, so 425,984 with
Linux's doubling, 319,488 usable past the release lag). There, focal-node failed 12 tests with this
change alone and 13 on focal's `main` before any of §4o–§4q, the same tests. Both runs refused
first windows on the shared socket (2,206 and 2,285 times): the reservation for every place passed
the queue in both, 960 handshake probes at 896 bytes each already taking 860 KB. And what MTU
discovery added on that buffer was a smaller room on each peer's own socket: a datagram there
carries up to the path's MTU (65,507 bytes on loopback), the costliest alone was 8,448 or 66,403
bytes, and an established connection's 31 probes, each charged as one, took 262 KB to 2 MB of the
319 KB. A peer's socket held less than one first window 5,798 times in the run, against 17 before.
§4s takes both.

Tests: `charge::tests::acknowledgements_and_probes_are_charged_as_small_datagrams` (the macOS
shared socket's own numbers), `a_small_datagram_fits_the_smallest_a_path_carries`, and
`exchange::a_handshake_is_reserved_in_its_place_and_an_established_peer_as_a_connection` (a dial,
a handshake waiting in its place, and both connected, read over a 10 ms round trip).

## 4s. What a peer sends unprompted at any MTU, and a place charged while held (2026-10-17)

§4r left two defects on Linux's default buffer (above). Both came from reserving what a peer could
not send.

**Probes.** §4i charged each of an established connection's 31 probes as the costliest datagram
alone. hyper-quic clamps every probe to the smallest datagram already (upstream's rule in
`Connection::allocate_datagram`, so a probe gets through a path whose MTU has shrunk). But a probe
moves its oldest packet's frames into the queue to send, and what its two datagrams cannot hold
goes beside them as the congestion window allows. So each probe timeout could still send up to a
packet of the path's MTU again, and a peer this side has stopped reading may time out fifteen times
before its idle timeout. The fix is at the sender (hyper-quic patch Q23). The first probe timeout
after an acknowledgement still repeats its oldest packet whole, so a lost reply goes again with its
probe (Chromium's rule, `crates/hyper-quic/VENDORED.md` §11). A timeout that follows an unanswered one says the peer is not
acknowledging: it has stopped reading, and what it was sent waits in its socket's queue, or the
path drops it, and the probe's acknowledgement will declare it lost. Its probes repeat no stream
data in the application space, only control frames, as upstream's did (RFC 9002 §6.2.4: previously
sent data "MAY" be sent). So a peer that has stopped reading is sent at most one packet of stream
data again, whatever the path's MTU and however many timeouts pass. The receive charge now counts
an established peer's probes as small datagrams and two of up to the path's MTU beside them: the
rest of the packet its first timeout repeats, and its MTU probe (`Unprompted::large`).

**Places.** The shared socket reserved for every place admission lets a handshake wait in, taken or
not. A place no handshake holds has no peer to send. Now only the places taken count
(`Endpoint::unprompted_shared`). The reservation for places not yet taken guarded a promise: a
window granted is never taken back, so a handshake admitted later must not pass the queue. That
promise is kept where the handshake is admitted. A first window is taken only with room for what
its peer may send unprompted (`Room::first`; `Endpoint::unprompted_handshake`, a handshake's probes
and one MTU probe), and that room is held until the driver says a capacity that counts the
handshake (`Endpoint::set_receive_capacity` takes both, from `SocketCharge::rooms`). A window still
grows only while what is left holds both for every place not taken (`kept_for_handshakes`), so a
peer that grew first never shuts out one that comes next.

**Measured, focal-node on Docker's default buffer:**

| Tree | Failed of 414 | First windows refused | A peer's socket under one first window |
|---|---|---|---|
| focal `main` | 13 | 2,285 | 17 |
| §4r alone | 12 | 2,206 | 5,798 |
| §4r and §4s | 8 | 117 | 0 |

On macOS focal-node passes, 414 of 414. The 8 that still fail on that buffer are open
(`docs/STATUS.md`). The shared socket's room there is about 31 KB: the queue charged at its costliest
stream byte, 896 bytes for a datagram carrying 120, holds two first windows. focal's enrollment
client then gives up at the first refusal (`Transport(Unavailable)`) where it could try again.

Tests: `tests::a_probe_timeout_after_an_unanswered_one_repeats_no_stream_data` (hyper-quic: the
first timeout's probes repeat stream data, the next two's none, every probe within the smallest
datagram); `credit::tests::a_first_window_is_taken_with_its_peers_unprompted_room`;
`exchange::a_connection_is_taken_only_with_room_for_its_peers_unprompted_messages`;
`exchange::a_handshake_is_reserved_in_its_place_and_an_established_peer_as_a_connection`, now with
nothing for a place no handshake holds; `charge::tests::acknowledgements_and_probes_are_charged_as_small_datagrams`.

## 4t. A window its peer stopped using (2026-10-17)

A window grows while its owner consumes it within two round trips (§4a), and each growth holds
room in the socket's queue (§4i) and bytes of the node's budget. QUIC takes back no credit it
gave: "it is not an error to advertise a smaller limit, but the smaller limit has no effect", and a
sender "MUST ignore any MAX_STREAM_DATA or MAX_DATA frames that do not increase flow control
limits" (RFC 9000 §4.1). So a window that grew stayed granted while its peer was idle, and a node
of many connections could find its room and budget held by peers that no longer send. Nothing gave
it back but the connection's end.

Two rules now take it back, both from what admission already calls idle: no exchange under way
and nothing begun, ended or carried on the connection for the quiet period, the endpoint's serve
period (`admission.rs`).

- **Set back when it is wanted.** When a window's growth, or a connection's first window, is
  refused for want of the shared socket's room or of the budget, every idle connection whose window
  grew, and that holds what was wanted, is set back to its first window (`Endpoint::take_back_windows`,
  at the endpoint's next poll). A connection on a socket of its own holds none of the shared room,
  so only a budget refusal sets it back. A window is set back only when another wants what it
  holds, never merely because its peer paused: it then grows again from the first by the doubling
  rule, as TCP restarts its congestion window after an idle period (RFC 5681 §4.1).
- **Given back as the credit is paid.** The set back is QUIC's shrink: hyper-quic holds the
  difference as a debt that the application's reads pay before the connection's limit moves again
  (patch Q24, `Connection::receive_window_debt`). The peer may still send the window and the debt
  beyond what was read. So the growth's grants go back, the latest first, each whole, while what is
  held stays at least the window, its reserve and the debt (`Core::repay`): the windows granted
  never pass the room, however the peer sends.
- **Closed if it stays quiet.** A peer that sends nothing pays nothing. A connection set back that
  has moved nothing since, for a quiet period, still owing what is wanted again, is closed with
  [`Refusal::ReceiveBuffer`], and its room and budget come back when it is gone. Admission closes an
  idle connection for a newcomer by the same rule; here it is closed only after its peer had a
  quiet period to send, and only while another wants what it holds.

Counted in `Stats::windows_set_back` and `Stats::windows_closed`.

Measured on the in-memory network (`tests/room.rs`, three nodes on the zero path, the receiver's
room `14 · first window`, eight handshakes' first windows kept, frames of 1,000 bytes read as they
come; windows in bytes, reserve excluded):

| Phase | Before | After |
|---|---|---|
| quiet peer's window once grown | 48,000 | 48,000 |
| active peer's window, its growth refused | 24,000 | 24,000 |
| quiet peer's window once another wanted its room | 48,000 | 12,000 (set back) |
| granted, before the quiet peer sends again | 76,536 | 76,536 (the credit given stands) |
| granted, once the quiet peer sent 37 frames slowly | 76,536 | 40,536 (its 36,000 of growth back) |
| active peer's window after it sends again | 24,000 | 48,000 |
| quiet peer silent: granted once it is closed and gone | 76,536, never closed | 26,268 (its first window and growth back) |

Before, the quiet peer's sending again changed nothing: each read re-extended its grown window.

Tests: `a_quiet_peers_grown_window_is_set_back_when_another_wants_the_room`,
`a_set_back_peer_that_stays_quiet_is_closed_for_the_room` and
`a_set_back_peer_that_moves_again_is_given_a_quiet_period_afresh` (a peer that sends again, less than it
owes, is set back afresh once idle, and closed only a quiet period after that; a move at the instant
of the set back counts as one, which the first build's closed a quiet period early); with the take-back
removed the first fails, the active window held at 24,000.

**Against focal's suite** (2026-10-17): focal-node's library on focal's branch `bus-land` (`7aa51dda`),
its vendored shared crates' sources from `b39a889`, the tree before the take-back, or from the take-back's
tree, nothing else differing; each binary's checksum recorded beside its verdict. On Docker's default
buffer (`rmem_max` 212,992; `rust:1.98.1`, 6 CPUs of 18), the arms alternating, four runs each, 8 of 414
failed in every run of both. The tree before failed the same eight in all four; with the take-back five of
them failed in all four, three in three, and two that never failed before failed in two and in one
(`placement_agent::tests::the_controller_expands_a_laptop_session_to_three_hosts_that_survive_one_loss`,
`credential_renewal::tests::a_joined_host_renews_its_credential_presents_it_everywhere_and_converges_after_a_crash`).
Run alone three times with the take-back, the first passed each time, one first window refused and no
window set back or closed. On macOS each tree passed 413 of 414, the same test failing on both, its
node's listen port taken by then (`network_service::tests::service_recovers_revocation_before_binding_any_ingress`,
`AddrInUse`). Over the whole library on Docker's buffer, a diagnostic build of the take-back's tree
printing every window set back, every connection closed for the room and every first window refused
found 136 first windows refused and no window set back or closed (7 of 414 failed, of the same set):
no window there grew and went idle while another wanted the room, so the take-back never acted in
focal's suite, and what moved between the arms' runs is not its. The eight are the open defect's
(`docs/STATUS.md`, the shared socket's room on that buffer).

## 4u. A socket a caller holds shares its port where it was bound to (2026-10-17)

A driver bound to an address gives each peer a socket of its own on the listener's port
(`Driver::bind`, `UdpSocket::bind_shared`). A driver given a socket already bound
(`Driver::new`) kept every peer on that socket, whatever the socket was. A caller that holds its
port from the moment it learns it hands over such a socket: a supervisor's, an adopted one
(`UdpSocket::adopt`), a test's reservation. Each such caller lost every peer's queue of its own.
focal's nodes bound their listeners so until 2026-10-17. On Linux's default receive buffer (319 KB
usable) the one queue held four connections' unprompted messages (§4s), and refused every first
window after.

`Driver::new` now shares the port of a socket bound with its port shared, as `Driver::bind` does
(`UdpSocket::shares_port`, `SO_REUSEPORT`). A socket bound plainly keeps its peers, as before.
Windows shares no port so (§4i's note on Winsock), and its sockets always keep their peers.

Test: `peer_sockets::a_driver_given_a_socket_bound_with_its_port_shared_gives_each_peer_its_own` (a
client gives a server driven on a shared socket a socket of its own where the platform shares ports,
and one driven on a plain socket none).

## 4v. A shared port is its listener's alone (2026-10-17)

Every driver bound to an address shares its port with a socket for each peer (§4u), and the socket
asked for its port with the port already shared (`SO_REUSEPORT` set before `bind`). On Linux a socket so
flagged and asking for port 0 can be given a port that another flagged socket of the same user holds, in
this process or another: the kernel's search for a free port passes over a port held only by sockets it
may share with. The two sockets then split the port's datagrams by a hash of each datagram's addresses,
and one endpoint's peer is heard by the other, which drops what it cannot read. A client whose server's
replies hashed to the other socket sent its Initial again and again and never heard the server: with
256 clients connecting to one server, one to three never connected in four runs of five, and every one
of them shared its port with another client, the server holding up to 17 handshakes for one of them.
macOS gives no held port to a port-0 bind.

`UdpSocket::bind_shared` now binds first and flags the socket after: the port is one no other socket
holds (a held port is refused `AddrInUse`, so a second listener on a node's port fails at once), and
the sockets for its peers join it with the flag set before their bind (`UdpSocket::join_shared`). Linux
makes the port's group when the first joiner binds, and the BSDs judge each bind by both sockets' flags
at that bind, so a flag set after the bind serves both (measured on each, a joiner connected to its peer
taking that peer's datagrams and the listener the rest). A port-0 bind of a socket that is flagged first
can still be given a port this one holds; hyper-rt binds no socket so.

Tests: `udp_shared_port::every_shared_bind_at_port_zero_is_given_a_port_of_its_own` (1,024 binds at
port 0; flagged first, 19 ports were given twice on Linux), `a_shared_port_is_refused_to_a_second_listener`
(flagged first, the second bind succeeded) and `a_joined_port_hands_the_joiner_its_peers_datagrams`;
hyper-rt-transport's `connection_memory` measurement connects all 256 clients in five runs of five.

## 5. Consumers

slates, focal and mantle each vendor a snapshot of the conformed crates, recording the hyper-raft revision
it came from, and drive them with their own runtimes. A refinement lands here once, after every consumer's
suite passes against it, and reaches all three.
