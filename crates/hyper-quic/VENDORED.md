# hyper-quic: quinn-proto 0.11.18, conformed

Source: `vendor/quinn-proto`, the published crates.io archive. See its `VENDORED.md` for the
checksum and upstream commit. Every change from those sources is listed here, in order.
Upstream's own tests are the oracle that conformance changed no behaviour.

## 1. One build path (2026-10-01)

- **Package.** Renamed to `hyper-quic`. The manifest is upstream's `Cargo.toml.orig` with the
  workspace inheritance replaced by explicit versions.
- **Crypto.** rustls on AWS-LC is always on: the provider mantle and focal vendor, and the one
  slates has queued.
  - Removed: the `ring` provider, the `rustls-ring` default, `platform-verifier`, the FIPS
    features, the `__rustls-post-quantum-test` feature, and the wasm target dependencies. Their
    source paths are deleted, not compiled out.
  - `ClientConfig::with_platform_verifier` and `try_with_platform_verifier` are gone; a consumer
    supplies roots through `with_root_certificates`.
- **Address-validation tokens.** `bloom` is always on. NEW_TOKEN tokens are issued (`sent: 2`) and
  their reuse is detected by `BloomTokenLog`. RFC 9000 §8.1.4 says a server SHOULD protect
  against replay of these tokens; with bloom off, quinn sends none.
- **qlog.** `qlog` is always compiled. Whether a connection emits it is runtime configuration
  (`QlogConfig`), not a build mode.
- **`arbitrary`.** Kept as the one feature: fuzzing derives, never in a shipped build.
- **Oracle.** 303 unit tests and 3 doctests pass. That is upstream's suite under this one
  configuration; the ring default ran 296 unit tests because the bloom-gated token tests were
  off.

## 2. Configuration without `Arc` (docs/transport.md §3.1)

1. **Congestion control is the closed enum `congestion::Congestion`, carried by value.**
   - `ControllerFactory` and `TransportConfig::congestion_controller_factory` are gone; use
     `TransportConfig::congestion`.
   - Each controller owns its config.
2. **A connection owns its `TransportConfig`**, which is now `Clone` plain data.
   - `ServerConfig::transport` and `ClientConfig`'s transport hold it by value.
   - A connection keeps only `grease_quic_bit` from the endpoint's configuration.
3. **qlog belongs to the connection.**
   - `QlogStream` owns its streamer, with no `Arc<Mutex<_>>`.
   - It is passed to `Endpoint::connect` and `Endpoint::accept`; `TransportConfig::qlog_stream`
     is gone.
   - `QlogConfig::into_stream` returns `Result<QlogStream, QlogError>`. A missing writer or a
     failed header write used to be logged and dropped.
4. **The endpoint owns its `EndpointConfig` by value.**
   - The reset key is AWS-LC's `hmac::Key`; the provider is no longer selectable.
   - The CID-generator factory (`Arc<dyn Fn>`) is replaced by a generator value. Each endpoint
     takes a copy through the new required method `ConnectionIdGenerator::clone_box`.
   - One behavioural difference: two endpoints built from one configuration with the default
     `HashedConnectionIdGenerator` now share its hash key, where upstream's factory drew a key per
     endpoint. The key only lets an endpoint cheaply reject CIDs it did not issue, and upstream
     already documents sharing it (`from_key`) to keep CIDs valid across restarts.
5. **Address-validation tokens have single owners.**
   - The endpoint owns the server's `TokenLog` (`Endpoint::set_token_log`; default
     `BloomTokenLog`) and the client's `TokenStore` (`Endpoint::set_token_store`; default
     `TokenMemoryCache`).
   - Both traits take `&mut self`, and both default implementations lose their `Mutex`.
   - `ValidationTokenConfig::log` and `ClientConfig::token_store` are gone.
   - A client connection that receives NEW_TOKEN reports it as an endpoint event
     (`EndpointEventInner::NewToken`), and the endpoint stores it. A connection's initial token is
     taken from the store at `connect`.
   - Two upstream test helpers were changed to match: `server_config_with_cert` no longer installs
     a test log, which existed only for builds without `bloom`; `use_same_token_twice` installs its
     store on the client endpoint.
6. **TLS is `hyper-tls`.** `rustls` is now a Cargo rename of `hyper-tls` (`crates/hyper-tls`), so the
   source still refers to `rustls::`. `HandshakeData::negotiated_key_exchange_group` is always
   present, as an `Option`; upstream gated it behind a test feature and filled it with `.expect`.
7. **The crypto provider is borrowed for `'static`.** Every TLS configuration uses
   `rustls::crypto::aws_lc_rs::DEFAULT_PROVIDER`, where upstream built a fresh
   `Arc<CryptoProvider>` per configuration. The classical-key-exchange test client keeps its
   provider in a `LazyLock`.

8. **TLS configuration lives in the endpoint's `Configs` slab** (`config/configs.rs`).
   - The endpoint owns every server and client configuration in a slab of
     `EndpointConfig::config_slots` slots (default 4, derived: one current and one draining
     configuration per side; a rotation is days apart, far longer than a connection lives).
     `ServerConfig` holds its crypto configuration, handshake-token key and time source as
     `Box`es, and `ClientConfig` its crypto configuration and initial-CID provider; neither is
     `Clone` any more.
   - A connection and a pending `Incoming` hold a generation-checked key. The endpoint counts
     them per slot: up when an attempt is surfaced or a connection created, down when the attempt
     is accepted, refused, retried or ignored and when a connection's `Drained` event arrives. A
     superseded configuration is dropped when its count reaches zero; a slot whose generation
     counter is exhausted is never reused. A full slab refuses the new configuration with
     `ConfigsFull`.
   - Endpoint API: `Endpoint::new` takes `Option<ServerConfig>` by value;
     `set_server_config(Option<ServerConfig>) -> Result<(), ConfigsFull>` supersedes the current
     one; `insert_client_config`/`retire_client_config` and
     `insert_server_config`/`retire_server_config` manage the others; `connect` takes a
     `ClientConfigHandle` (connections made with one handle share its TLS session cache, so a
     later connection resumes); `accept` takes an optional `ServerConfigHandle`; `configs()` and
     `configs_mut()` lend the slab. `ConnectError::UnknownConfig` is new.
   - Connection API: `handle_event(event, &mut Configs)` and
     `poll_transmit(now, max_datagrams, buf, &Configs)`. A datagram can carry CRYPTO data, and a
     TLS read can change the configuration's stores (session cache, ticketer, key log), so that
     call borrows mutably; this deviates from §3.1's `&Configs`. A transmit can carry NEW_TOKEN,
     which reads the token key and time source. `handle_timeout` needs no configuration.
   - The values a connection read from its `Arc<ServerConfig>` without a TLS step (`migration`,
     whether a preferred address was sent, `ValidationTokenConfig::sent`) are copied into the
     connection at accept.
   - The crypto traits borrow: `crypto::ClientConfig::start_session(&mut self, …)`,
     `crypto::ServerConfig::start_session(&self, …)`, and
     `Session::read_handshake(&mut self, SessionConfig<'_>, buf)`, where `SessionConfig` lends
     the client's or server's crypto configuration for the call. Both configuration traits are
     `Any`, so the rustls session recovers its own `QuicClientConfig` or `QuicServerConfig`; a
     configuration of the wrong side or implementation is a typed `INTERNAL_ERROR`, unreachable
     through the endpoint. `QuicClientConfig` and `QuicServerConfig` own their rustls
     configuration; `with_initial` and `TryFrom` take it by value.
9. **`TokenMemoryCache` owns each server name once.** Upstream shared it between the lookup map
   and the LRU entry through `Arc<str>`. The map now owns it (`Box<str>`); evicting the least
   recently used name finds its map entry by slot, a scan bounded by `max_server_names` and paid
   only when a new name evicts. Storing a token for a known name no longer allocates a name.
10. **Tests.** Only the API changed in them; every assertion is unchanged.
    - The harness's `connect_with`/`begin_connect` insert a configuration per connection and
      retire it at once; `add_client_config` with `connect_with_shared`/`begin_connect_shared`
      serve the tests that reused one configuration through `clone` (0-RTT, tokens).
      `zero_rtt_rejection` changes ALPN through `Configs::client_config_mut` where upstream used
      `Arc::get_mut`.
    - `pending_incoming_can_retry_after_disabling_server` reused one `ServerConfig` through
      `clone`, sharing its token key. It now builds two configurations around one token key, so
      the second still validates the first's retry token.
    - The fake time source is a clonable handle (`Arc<Mutex<SystemTime>>`, test code).
    - New: four unit tests of the slab (refusal when full, reclamation after the last user,
      immediate reclamation when unused, a current configuration kept without users), and
      `tests/handshake.rs`: two endpoints driven through the public API complete a handshake and
      exchange data both ways, and allocations per handshake are counted.
    - Oracle: 313 unit tests (309 and the 4 new) and 3 doctests pass, and `tests/handshake.rs`'s
      2 tests.
11. **Wire behaviour and allocations.** `tests/handshake.rs` with each datagram's size printed,
    run at 526c2cc (adapted to its `Arc` API) and here: the same 63 datagrams with the same sizes
    in the same order, across a warm-up, a full and a resumed handshake. Allocations per
    handshake, client and server together, macOS aarch64 development machine, debug profile,
    three runs each (reallocations vary by ±3 in both):

    | Handshake | 526c2cc | Now |
    |---|---|---|
    | Full | 505 allocs, ~174,300 B | 494 allocs, ~174,100 B |
    | Resumed | 511 allocs, ~129,880 B | 501 allocs, ~129,530 B |

## 3. Connection attempts (2026-10-01)

hyper-tls prefers X25519MLKEM768, as Chromium and Firefox now do. Its 1,184-byte key share
(draft-ietf-tls-ecdhe-mlkem) spreads a ClientHello over two Initial datagrams, and running upstream's
suite on it exposed two defects in quinn-proto 0.11.18's server:

- **An attempt reached the application before its payload was authenticated.** `handle_first_packet`
  removed header protection and surfaced an `Incoming`; only `accept` decrypted the payload. A
  forged Initial with a garbage payload therefore became a connection attempt, and its token was
  spent in the token log first.
- **A straggler became a second attempt.** When the server answered a two-datagram ClientHello's
  first datagram with Retry, the second datagram arrived afterwards for a CID with no state. It was
  surfaced as a new `Incoming` with half a ClientHello and an already-spent token. One client
  attempt produced two decisions, and possibly a second Retry. Upstream's own `use_token_then_retry`
  fails on this.

The fix:

- `handle_first_packet` authenticates the payload before anything else acts on the packet. `accept`
  no longer decrypts again.
- An attempt surfaces only with the first byte of its ClientHello: CRYPTO data at offset 0
  (RFC 9001 §4.1.3).
- An authenticated Initial carrying more of a ClientHello is held per initial CID (`Endpoint::held`).
  When the start arrives, the held datagrams join the attempt's buffered datagrams. A straggler
  after Retry is never joined; it expires three probe timeouts after arrival. That is RFC 9000
  §10.2's allowance for stray packets after state ends, with the PTO computed by RFC 9002 §6.2.1
  from the server's configured initial RTT.
- Held entries count toward `max_incoming` and the incoming byte limits.
- An Initial carrying only a CONNECTION_CLOSE, or no valid frames, before any ClientHello is
  dropped, because no handshake can follow it.

Tests:

- `tests::first_flight` (new) covers: post-quantum negotiation; the two-datagram ClientHello; a
  retried attempt surfacing once, with the straggler expiring; a reversed ClientHello surfacing once
  with no time passing and no loss; a forged payload surfacing nothing and buffering nothing; and a
  first flight that fills but never exceeds three times the bytes received, using a certificate
  whose random names compression cannot shrink.
- Upstream tests changed:
  - `server_can_send_3_inital_packets` and the two `zero_rtt_incoming_buffer_size` tests use a
    classical-key-exchange client (`client_config_classical`). Their expected counts derive from a
    one-datagram ClientHello. The post-quantum layout is covered by the new tests.
  - `instant_close_1` now asserts that the server surfaces and keeps nothing. Upstream surfaced the
    close as an attempt and then lost the connection.
  - `known_connections`' bookkeeping assertion excludes held Initials, which share the
    initial-CID routing table but are not connections.

Result: 309 unit tests (upstream's 303 and 6 new) and 3 doctests pass.

## 4. No panics in shipped code (2026-10-01)

The crate takes the workspace's lint table (`[lints] workspace = true`; its own `[lints.rust]`
and the crate-root `allow(clippy::cognitive_complexity)` are gone): no `unwrap`, `expect`,
`panic!`, `unreachable!` or assert family, no indexing or slicing that can go out of bounds, no
overflowing arithmetic, no narrowing `as`, `cognitive_complexity` at most 10, every `const`
documented. No item or crate in shipped code allows any of them; test code opts out at the crate
root (`#![cfg_attr(test, allow(...))]`, the same list as hyper-tls) and at `tests/handshake.rs`'s.

### Classes of change

1. **Lookups that cannot miss, by construction.** The packet number spaces are a `Spaces`
   struct reached by `SpaceId` (`get`/`get_mut`), not `[PacketSpace; 3]` indexed by `space as
   usize`; the per-direction stream state is `PerDir<T>`; the timer table is destructured by
   `Timer`; BBR's gain cycle by its offset. The endpoint's `Index<ConnectionHandle> for
   Slab<ConnectionMeta>` is gone: each lookup is `get`/`get_mut`/`try_remove`, and an event for a
   connection the endpoint no longer knows is ignored.
2. **Fallible signatures where a failure has a caller** (each an API change):
   - `Endpoint::new` returns `Result<Self, RngUnavailable>`: the system RNG failing to seed it
     was an `expect`.
   - `Endpoint::refuse` returns `Option<Transmit>`, `None` where the close cannot be protected.
   - `ClientConfig::with_root_certificates` returns `ClientConfigError` (the verifier's error, or
     the TLS configuration's: no TLS 1.3, no initial cipher suite), `QuicClientConfig::new` and
     `QuicServerConfig::new` return the `rustls::Error`; upstream `expect`ed both.
   - `ConnectError::InvalidTlsConfig` is new: a client TLS configuration that cannot start a QUIC
     session (one without TLS 1.3, which `TryFrom<rustls::ClientConfig>` does not check) was an
     `unwrap` in `start_session`.
   - The crypto traits: `crypto::ServerConfig::start_session` returns
     `Result<Box<dyn Session>, TransportError>` and `retry_tag` `Result<[u8; 16], CryptoError>`;
     `HeaderKey::encrypt`/`decrypt` and `PacketKey::encrypt` return `Result<(), CryptoError>`;
     `HandshakeTokenKey::aead_from_hkdf` returns `Result<Box<dyn AeadKey>, CryptoError>`.
   - `StreamId::new` returns `Option`: an index at `MAX_STREAM_COUNT` or past it has no ID.
   - Internal: `PacketNumber::new` and `PacketSpace::get_tx_number` return `Option`;
     `PacketBuilder::finish` returns `Option`; `PartialEncode::finish` returns `Result`;
     `Connection::write_crypto`, `upgrade_crypto`, `update_keys` and `set_peer_params` return
     `Result<_, TransportError>`; `Token::encode` returns `Option`;
     `AckFrequencyState::next_sequence_number` returns `Option`.
3. **The connection's own invariants, as typed errors.** Where the connection finds its own
   state inconsistent (no keys for a space it is sending in, the 1-RTT keys without the next
   phase's, keys handed over after the last space, the 0-RTT outcome unknown after the
   handshake) it returns `TransportError::INTERNAL_ERROR`, or kills the connection with it where
   the caller cannot return one (`PacketBuilder`). A packet being built when this happens is
   taken back out of the buffer: nothing unprotected is sent.
4. **Checked arithmetic and `get`.** Every offset and length the peer influences is checked:
   packet and header decoding (the payload length, the token's position, the packet number),
   frame decoding (ACK ranges, lengths, CIDs), transport parameters (lengths, the reserved
   parameter), CRYPTO data's end (RFC 9000 §19.6: past 2^62 - 1 is a FRAME_ENCODING_ERROR),
   header protection's sample, Retry integrity, stateless reset tokens.
5. **Saturation where it is the stated meaning**, each with a comment: statistics counters
   (`ConnectionStats`, ECN counts); byte counts and offsets bounded by an in-memory buffer;
   required sizes, which can only over-state and so write less (`fits`, `min_size`,
   `NewToken::size`); congestion windows, byte totals and BBR's bandwidth samples, which saturate
   at a size no connection reaches; durations (PTO, RTT estimates, delays), which can only
   lengthen. A deadline past what `Instant` represents is never reached: the timer is not armed
   (idle, keep-alive, key discard, path validation, MTU reactivation), except the close timer,
   which then fires at once so that a closing connection cannot be held forever.
6. **Float to integer without `as`**: `float::saturating_u64`/`saturating_u32` give `as`'s
   meaning (fraction dropped, saturating, NaN to zero), and `float::scale_duration` gives
   `Duration::mul_f32`'s without its panic; tests check them against `as` and `mul_f32`.
7. **Debug-only assertions removed** where they checked an internal precondition the callers
   hold (packet builder sizes, GSO alignment, ACK-only writes, `discard_space`'s space, upgrade
   order, `handle_first_packet`'s side, CID sequence ordering, MTU discovery's phase). Release
   builds never evaluated them, so no behaviour changes; `SentFrames::is_ack_only`, used only by
   one of them, is gone.
8. **Restructured so the type system proves the invariant**: `set_key_discard_timer` starts
   from `now`, which every caller had made the previous keys' end time (upstream `expect`ed
   both the previous keys and their end); `PacketNumber::decode` takes the header's first byte,
   whose two low bits are the length, instead of a length upstream matched with
   `unreachable!`; `LongHeaderType::from_byte` matches the two type bits exhaustively;
   `TransportParameterId::try_from` finds the ID among `SUPPORTED`; `get_or_insert_recv`
   re-opens a freed stream by construction; the `IncomingImproperDropWarner` is dismissed by a
   flag instead of `mem::forget`.
9. **Split for `cognitive_complexity`** into named steps, behaviour unchanged:
   `poll_transmit` (`close_pending`, `queue_ack_frequency`, `fill_spaces`, `fill_step`,
   `start_datagram`, `congestion_blocks`, `finish_for_next_datagram`, `allocate_datagram`,
   `write_close`, `encode_close`, `send_off_path_response`, `finish_last_packet`,
   `write_mtu_probe`, over a `TransmitState`), `populate_packet` (`write_signal_frames`,
   `write_ack_frequency`, `write_path_frames`, `write_crypto_frames`, `write_cid_frames`,
   `write_datagram_frames`, `write_new_tokens`), `handle_packet` (`packet_result`,
   `on_authentication_failure`, `on_decrypted_packet`, `drops_authenticated`, `after_packet`,
   `state_after_error`), `process_decrypted_packet` (`process_established`,
   `process_while_closed`, `process_retry`, `process_handshake_packet`,
   `on_client_handshake_complete`, `process_initial_packet`, `process_version_negotiate`),
   `process_payload` (`process_data_frame`, `process_stream_frame`, one handler per frame
   kind, `after_payload`), `on_ack_received` (`record_largest_acked`, `on_packets_acked`,
   `update_rtt`, `on_ack_ecn`), `detect_lost_packets` (`find_lost_packets`, `on_packets_lost`),
   `handle_timeout` (`on_timer`), `Endpoint::handle` (`negotiate_version`, `buffer_incoming`,
   `handle_unknown`), `handle_first_packet` (`admit_first_packet`,
   `authenticate_first_packet`), `TransportParameters::write` (`write_param` and one writer per
   kind), `StreamsState::write_control_frames` and `write_stream_frames`, `RecoveryMetrics`.
10. **Bounded loops**: `Endpoint::new_cid` draws at most 155 CIDs, `(3/4)^155 < 2^-64` given
    `cids_exhausted` keeps a quarter of short CID spaces free; a generator that keeps giving
    used CIDs refuses the connection (`CidsExhausted`) instead of spinning. The STREAM copy loop
    stops on an empty read instead of spinning.
11. **Every `const` in `src/` carries a `///`** with its derivation or citation. Upstream's
    tunables keep upstream's values and say so. `scripts/check-contracts.py` now treats a file
    a `#[cfg(test)] mod name;` declares as test code, as it treated an inline `#[cfg(test)] mod`,
    and no longer stops scanning the declaring file at that line.
13. **No `allow(missing_docs)`**: each `FrameStats` field names its frame type, where upstream
    allowed the struct undocumented.
12. **Tests**: `tests/handshake.rs`'s `unsafe` counting allocator is replaced by
    `hyper_measure::alloc::Counting`; upstream tests changed only where the API did
    (`.unwrap()` on the new `Result`s and `Option`s) and in one MTU discovery test (below).

### Behaviour changes: former panics and what they are now

| Reached by | Upstream | Now |
|---|---|---|
| **Local configuration and data**: an initial MTU above 16 KiB with 0-RTT data (any long header packet larger than its two-byte length field) | `assert!` in `PartialEncode::finish`, active in release builds | the packet builder caps each long header packet at what the field holds (`PartialEncode::length_limit`); the data goes in the next packet (`zero_rtt_long_header_packets_fit_their_length_field`, which panics on upstream's code) |
| The API: `recv_stream` on a stream this side opened unidirectionally, `send_stream` on one the peer did | `assert!` | every operation fails with `ClosedStream` (`stream_used_against_its_direction_is_closed`) |
| The API: `poll_transmit` with `max_datagrams == 0` | `assert!` | `None`, nothing sent (`poll_transmit_with_no_datagrams_sends_nothing`) |
| The API: `Endpoint::new` with the system RNG unavailable | `expect` | `RngUnavailable` |
| The API: an `Incoming` from another endpoint passed to `accept`/`refuse`/`retry`/`ignore` | slab `remove` panicked | `accept` refuses it (`INTERNAL_ERROR`), `retry` hands it back, `refuse`/`ignore` release no buffer or configuration of this endpoint's |
| The API: a client TLS configuration without TLS 1.3 | `unwrap` at `connect` | `ConnectError::InvalidTlsConfig` |
| The API: a CID generator that keeps giving used CIDs | an endless loop | the connection is refused (`CidsExhausted`) |
| The API: a 4-byte CID on a 32-bit target | `cids_exhausted` overflowed `usize` | counted in `u64` |
| The API: `mtu_discovery_config` interval or black-hole cooldown, `keep_alive_interval` or `max_idle_timeout` past `Instant`'s range | `Instant + Duration` panicked | never reached: not armed |
| The API: `time_threshold` negative or not finite | `Duration::mul_f32` panicked | a loss delay of the timer granularity, the floor upstream applies (`float::scale_duration` gives zero) |
| A peer, after 2^31 unacknowledged packets of a space | `panic!("packet number too large to encode")` | the connection is killed (`INTERNAL_ERROR`), nothing more sent (`packet_number_beyond_any_encoding_is_refused`) |
| The connection's own packet numbers reaching 2^62 | `assert!` | killed without sending anything further, as RFC 9000 §12.3 requires |
| A peer's Initial token or payload length past `usize` (32-bit targets) | truncated by `as` | `InvalidHeader`, the packet dropped |
| The peer's parameters arriving during an MTU search | `debug_assert!` (debug builds only) | recorded; upstream's `should_panic` test now asserts that (`mtu_discovery_with_peer_max_udp_payload_size_during_search_records_it`) |
| A forged or unexpected Retry with no ClientHello kept | `unwrap` | the Retry is discarded |
| The connection's own state inconsistent (class 3) | `unwrap`, `expect`, `unreachable!` | `INTERNAL_ERROR`, unreachable by construction |
| Every other site | `unwrap`, `expect`, assert, indexing, overflow | a typed refusal, unreachable by construction |

No panic in upstream's receive path was found reachable from the wire on a 64-bit target: each
peer-influenced quantity upstream did not check is a varint below 2^62 added to another below
2^16, or was checked before the site (the header protection sample length, the ACK ranges by
`scan_ack_blocks`, the transport parameters' bounds). Those sites are checked now regardless.

### Oracle

319 unit tests pass: upstream's 309 (§3), `float`'s 2, and the new
`zero_rtt_long_header_packets_fit_their_length_field`, `stream_used_against_its_direction_is_closed`,
`poll_transmit_with_no_datagrams_sends_nothing` and `packet_number_beyond_any_encoding_is_refused`,
with upstream's `mtu_discovery_with_peer_max_udp_payload_size_during_search_panics` turned into
the test of what now happens instead. `tests/handshake.rs` 2 and the 3 doctests pass.

### Allocations per handshake

`tests/handshake.rs`, client and server together, macOS aarch64 development machine, debug
profile, three runs each; the counts before are the same test at 3fa28ed (this section's base)
with its own counting allocator, after with `hyper_measure::alloc`, which counts the same calls:

| Handshake | Before | Now |
|---|---|---|
| Full | 494 allocations, 55–58 reallocations | 494 allocations, 56–58 reallocations |
| Resumed | 501 allocations, 54–58 reallocations | 501 allocations, 53–56 reallocations |

No path gained an allocation. A first draft built `TransportError`s, whose reason is a `String`,
eagerly in `ok_or(...)` on the success path, which cost 9–10 allocations a handshake; they are
built only on failure (`ok_or_else`).

## 5. What the application layer reads (2026-10-01)

Three items made public for hyper-transport (mantle note 32 §3.4); no behaviour changes.

- **Patch Q5: the assembler's span limit.** `MAX_CHUNKS` (1,024, `connection/assembler.rs`), past
  which a stream's out-of-order spans close the connection ("too many gaps in stream buffer"), is
  public and re-exported as `hyper_quic::MAX_STREAM_CHUNKS`, so that a stream window is derived from
  it instead of from a literal (node.md §3.3: a window of at most `2 · 1,024 · d` cannot reach it).
- **`Streams::write_limit`.** The stream bytes the connection may still take from the application
  now: the peer's connection credit not yet spent and room in the send window, whichever is less
  (the existing `StreamsState::write_limit`). quinn keeps one connection window, so an application
  that keeps a credit reserve for its more urgent classes (note 32 T16) needs to see it.
- **`Connection::peer_max_ack_delay`.** The peer's `max_ack_delay` transport parameter (RFC 9000
  §18.2; the default before its parameters arrive), which the connection already computed for its
  own timers. hyper-transport judges an exchange by whether its period heard the peer, so it
  refuses a period no longer than this (`Endpoint::open`, `Refusal::Configuration`).

## 6. The host clock, stated (2026-10-02)

The workspace denies `Instant::now`, `SystemTime::now`, thread spawns and environment reads
(`clippy.toml`, `docs/sim.md` §3.9). Upstream reads the host clock in two shipped places, each kept
and allowed at the site with its reason; no behaviour changes:
- `config/mod.rs`, `StdSystemTime::now`: the default `TimeSource` of address-validation tokens,
  which an owner replaces through `ServerConfig`.
- `config/transport.rs`, `QlogConfig::default`: qlog's start time, taken when a qlog configuration
  is made (the `qlog` feature).

## 7. Allocations after hyper-tls lends its TLS 1.2 sessions (2026-10-03)

No change here. hyper-tls's session store now moves a server name in where it cloned it, and a
spent TLS 1.3 ticket's certificate chain moves into the connection where it was copied
(`crates/hyper-tls/VENDORED.md` §5). `tests/handshake.rs`, debug profile, this machine: 492
allocations a full handshake (494 before), 497 a resumed one (501 before), against 526c2cc's 505 and
511.

## 8. End to end, against upstream (2026-10-03)

`tests/e2e.rs` (`harness = false`): real processes over real UDP sockets on loopback (CLAUDE.md
§1a). The test binary is the client and spawns itself as the server, as a relay and, for one
scenario, as the client, each a process that shares nothing with it but the kernel's sockets.
Every scenario runs three ways: hyper-quic on both sides, a hyper-quic client against an upstream
quinn-proto 0.11.18 server, and an upstream client against a hyper-quic server (the
`upstream-quinn-proto` dev-dependency, unmodified, on rustls 0.23.45 with AWS-LC; slates'
`docs/wip/transport-quic.md` §4, stage 5). A killed peer runs where hyper-quic is the survivor.
The scenarios follow the QUIC interop runner's cases (quic-interop-runner `testcases_quic.py` at
`740c05a`):

| Scenario | What it does and checks |
|---|---|
| `handshake` | a full handshake with a mebibyte each way, then a resumed one whose stream takes its bytes before the handshake ends: the client's `accepted_0rtt` and the server's 0-RTT keys both say the early data was taken (`resumption`, `zerortt`) |
| `streams` | three bidirectional streams at once, 2, 3 and 5 MiB each way (`transfer`'s files) |
| `lossy` | 2 MiB each way through a relay process that drops one full-size datagram in fifty in each direction (`transferloss`'s 2 %) and holds one in fifty until the next in its direction has passed; the relay reports its drops and holds, and both senders report packets lost and recovered |
| `migration` | 2 MiB each way while the client moves to a new socket mid-transfer and tells its connection (RFC 9000 §9.5: a new connection ID on the new path); the server sends PATH_CHALLENGE on the new path and follows the client to it, the client answers and retires a connection ID (`connectionmigration`) |
| `rebinding` | the same with the client's socket replaced unannounced, as a NAT rebinding does (RFC 9000 §9.3, `rebind-port`) |
| `killed-server` | the server process is killed with SIGKILL mid-upload; the client's connection ends `TimedOut` no sooner than its idle timeout after the server's last datagram (RFC 9000 §10.1), and a new server process answers |
| `killed-client` | the client process is killed mid-upload; the server's connection ends the same way, and the server serves the next client |

Every stream carries a pattern no lost, duplicated or misplaced byte can match, checked byte by
byte on both sides. A process is killed mid-stream on a fact: the server once a stream has brought
it a mebibyte, the client once flow control (RFC 9000 §4.1) shows the server has taken a stream
window of its upload.

Every wait is for a fact: a datagram, a timer the connection set, or an event it reports. A wait
ends when its fact holds or when the connection reports its end; the quiet rule (as
`hyper_raft_e2e::quiet`, in the protocol's terms) fails it once no stream byte has moved for the
connection's idle timeout of listening, 30 s (`TransportConfig`'s default; RFC 9308 §3.2 finds
shorter timeouts make transient interruptions harder to survive): QUIC itself ends a connection
that hears nothing that long, so one that hears its peer and moves nothing that long is stuck.
Only time the driver spent listening on its socket counts, so a starved process does not count its
starvation as the peer's silence. A wait for a killed peer's end is the connection's own idle
timer; its failure guard is a second idle timeout of listening.

Results, debug profile, this machine, four runs of all 19 at load 21–46 (other sessions building
and testing), every one passing: a full and a 0-RTT handshake with a mebibyte each way in 0.33–0.56
s; 10 MiB each way on three streams in 0.47–1.30 s; through the relay 0.28–0.56 s, the relay
dropping 30 and holding 30 of about 1,515 full-size datagrams each way and the senders recovering
30 losses each (31 once); a migration or rebinding in 0.23–0.31 s, the server sending two
PATH_CHALLENGE frames (three twice) and the client answering, and retiring a connection ID after an
announced move; a killed server timed out 30.050–30.062 s after its last datagram, a killed client
30.001–30.029 s after its own. Earlier runs found four defects in the harness itself, each fixed
before these: a connection forgotten as it drained before its end was read (the killed scenarios
waited for an end already gone), a server that took its client's stream mark for the end of its
connection, and two hooks that read a stream before the server's limits let it open (one run in
four at load 58).
