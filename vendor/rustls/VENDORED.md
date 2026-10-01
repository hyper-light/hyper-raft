# rustls, vendored

- **Upstream.** `rustls` 0.23.45 from crates.io.
  - Archive SHA-256: `0d41d731c7d2f962d1ccc364cec258de3c0e93b38c2fb3ba97ac74513048d634`, equal to the
    `cksum` the crates.io index records. Verified 2026-10-01.
  - Built from rustls/rustls at commit `2976d90fd1c2db6b518700dd101b714069cfcb17`
    (`.cargo_vcs_info.json`, `path_in_vcs = "rustls"`).
  - This is the version the workspace already resolved for quinn-proto 0.11.18.
- **Licence.** Apache-2.0, ISC or MIT, at the licensee's option (`LICENSE-APACHE`, `LICENSE-ISC`,
  `LICENSE-MIT`).
- **Why.** The owner's decision (mantle note 32 §6): rustls is vendored and conformed, with no
  exception for `Arc` at its configuration signatures (docs/transport.md §3.1).
- **State.** Staged. This directory is byte-for-byte the published archive, outside the workspace and
  the lint wall. It becomes the member crate `crates/hyper-tls`, whose `VENDORED.md` will record every
  change.
- **Size, measured.** About 48,200 lines of Rust in `src/`, and 333 lines that mention `Arc`.
