# hyper-tls: rustls 0.23.45, conformed

Source: `vendor/rustls`, the published crates.io archive. See its `VENDORED.md` for the checksum
and upstream commit.

The oracle is upstream's own test suite from the rustls repository at that commit (`2976d90`), which
the archive omits:
- `tests/`, upstream's `rustls/tests`;
- `tests/support/rustls-test`, upstream's `rustls-test` crate pointed at this one;
- `test-ca/`;
- `tests/data/rfc-9180-test-vectors.json`, from upstream's `rustls-provider-test`;
- `tests/data/fuzz-corpus/message`, from upstream's `fuzz/corpus/message`.

Every change from the archive is listed here, in order.

## 1. One build path (2026-10-01)

- **Package and library.** Renamed to `hyper-tls`; the library is `hyper_tls`. Tests and doc examples
  refer to `hyper_tls::`. The edition stays 2021 for now, because rustls's `impl Trait` returns
  borrow under 2021's capture rules; moving to 2024 is a later step.
- **Features collapsed.** `std`, TLS 1.2, aws-lc-rs, `prefer-post-quantum`, `logging`, and RFC 8879
  certificate compression with both brotli and zlib are always built.
  - Removed: the *ring* provider, FIPS, `no_std` (`hashbrown`, the no-std lock and ticketer
    paths), `custom-provider`, `read_buf`, and the nightly `#[bench]` modules and `verifybench`.
    Their code is deleted, not compiled out.
  - The four files the aws-lc-rs provider shared with *ring* through `#[path]` (`hash`, `hmac`,
    `kx`, `quic`) now live in `crypto/aws_lc_rs/`.
  - TLS 1.2 stays because S3 clients reach mantle's HTTP/1.1 listener over TLS 1.2 as well as 1.3.
  - brotli stays alongside zlib for two reasons: Chromium implements only brotli for RFC 8879, and
    upstream's test of the compression cache's LRU eviction needs both algorithms.
  - The manual's FIPS chapter and the crate docs' feature list are replaced by one paragraph
    stating the single build.
- **Tests that ran only with *ring* now run with aws-lc-rs:** the TLS 1.2 PRF and the TLS 1.3 HKDF
  known-answer tests. Their note that aws-lc-rs "doesn't provide hmac" was stale.
- **Tests dropped:** `webpki_supported_algorithms_is_debug`, which asserts the Debug text of *ring*'s
  algorithm table, and `default_suites_are_fips`.
- **`process_provider`** keeps its one case: the implicit default is aws-lc-rs.
- **Fixture paths** are fixed to `CARGO_MANIFEST_DIR`.
- **Oracle, measured on the result:**

  | Suite | hyper-tls | Upstream default build at `2976d90` |
  |---|---|---|
  | Unit | 246 | 235 |
  | api | 227 | 224 |
  | api_ffdhe | 5 | 5 |
  | client_cert_verifier | 4 | 4 |
  | ech | 2 | 2 |
  | key_log_file_env | 2 | 2 |
  | process_provider | 1 | 1 |
  | server_cert_verifier | 6 | 6 |
  | unbuffered | 27 | 27 |
  | Doctests | 15 (3 ignored) | 15 (5 ignored) |

  All pass. The extra unit and api tests are the ported known-answer tests and the
  compression-dependent tests, which upstream's default build leaves off.
