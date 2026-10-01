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
