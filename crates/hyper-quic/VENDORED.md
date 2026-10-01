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

## 2. Configuration without `Arc` (docs/transport.md §3.1), in progress

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
