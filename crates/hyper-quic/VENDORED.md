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
