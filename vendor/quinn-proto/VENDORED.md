# quinn-proto, vendored

- **Upstream.** `quinn-proto` 0.11.18 from crates.io.
  - Archive SHA-256: `a9746dbde176634f4f2f1faf2404e30a31b2bc1e9cafb5329c95d8177a18c9fc`, equal to the
    `cksum` the crates.io index records. Verified 2026-10-01.
  - Built from quinn-rs/quinn at commit `eaec0db4bcb698f76df736d89938743c88a2ad5f`
    (`.cargo_vcs_info.json`, `path_in_vcs = "quinn-proto"`).
  - `Cargo.toml` is the registry's normalized manifest; `Cargo.toml.orig` is upstream's own.
- **Licence.** MIT or Apache-2.0, at the licensee's option (`LICENSE-MIT`, `LICENSE-APACHE`).
- **Why.** The shared transport is standard QUIC (RFC 9000, 9001, 9002) through this crate, conformed
  to the workspace's rules (`docs/transport.md`).
- **State.** Staged. This directory is byte-for-byte the published archive. It sits outside the workspace
  (`exclude = ["vendor"]`) and the lint wall.
  - The conformed member crate is `crates/hyper-quic`. Its `VENDORED.md` records every change from these
    sources.
  - Upstream's own tests pass here on their own: 296 unit tests and 3 doctests. slates' session measured
    this on 2026-10-01; the slates session also counted the conformance work in its A-52 audit.
