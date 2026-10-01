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

## 2. Configuration without `Arc` (docs/transport.md §3.2)

1. **The crypto provider is `&'static CryptoProvider`.** A provider is tables of algorithm
   references; aws-lc-rs's is static data.
   - `ClientConfig::builder_with_provider`, `ServerConfig::builder_with_provider`, their
     `builder_with_details`, and `ConfigBuilder`, `ClientConfig` and `ServerConfig` hold and
     return `&'static CryptoProvider`.
   - The process default is a `OnceLock<&'static CryptoProvider>`:
     `CryptoProvider::install_default(&'static self) -> Result<(), &'static Self>` and
     `get_default() -> Option<&'static Self>`. The implicit default is
     `crypto::aws_lc_rs::DEFAULT_PROVIDER`, a `LazyLock` over `default_provider()`.
   - The webpki verifier builders read only the provider's signature algorithms, so they borrow
     it for the call: `builder_with_provider(roots, &CryptoProvider)`.
   - Tests build providers at runtime; they leak each one to get `'static`
     (`rustls_test::static_provider`, `crypto::static_provider` under `cfg(test)`). A program
     keeps its provider in a `static`.
2. **A connection holds no configuration.** The caller owns the configuration and lends it to
   each call that can advance a handshake.
   - `ConnectionCommon::process_new_packets(&mut self, config)` and `complete_io(io, config)`
     take `&mut ClientConfig` or `&mut ServerConfig`. `UnbufferedConnectionCommon` takes it in
     `process_tls_records(config, incoming)`; the kernel connection in
     `handle_new_session_ticket(config, payload)`; QUIC in `read_hs(config, plaintext)`.
   - `Stream` borrows the configuration beside the connection and socket:
     `Stream::new(conn, config, sock)`. `StreamOwned<C, T, F>` owns or borrows it through
     `F: BorrowMut<Config>`, and `into_parts` returns all three.
   - Constructors: `ClientConnection::new(&mut ClientConfig, name)` (starting a session takes a
     resumption ticket and the key-exchange hint from the stores), `ServerConnection::new(&ServerConfig)`,
     `Accepted::into_connection(&mut ServerConfig)`, and the QUIC and unbuffered equivalents.
   - The state machine's `Context` carries `config: &Settings` and the stores, `&mut` for the
     call; no handshake state has a `config` field.
   - The side-neutral `Connection` enum loses `process_new_packets` and `complete_io`: each needs
     its own side's configuration.
   - Deviation from §3.2: QUIC's `write_hs` takes no configuration. It only drains the flight a
     read already produced; it consults no setting and no store.
3. **Settings and stores are separated.** `ClientConfig` is `ClientSettings` (read-only, reached
   through `Deref`/`DerefMut`) plus its stores as public fields: `resumption`, `key_log`
   (`Box<dyn KeyLog>`) and `cert_compression_cache`. `ServerConfig` is `ServerSettings` plus
   `session_storage`, `ticketer`, `key_log` and `cert_compression_cache`. A call splits the
   configuration into `&Settings` and `&mut` stores, so a borrowed certificate key and a mutable
   store coexist in one call.
   - Every mutating store method takes `&mut self`: `StoresServerSessions::put`/`take`,
     `ProducesTickets::encrypt`/`decrypt`, `KeyLog::log`, and `ClientSessionStore`'s setters and
     takers. Lookups stay `&self`.
   - Their `Mutex`es are gone: `KeyLogFile`, `ServerSessionMemoryCache`,
     `ClientSessionMemoryCache`, the ticket rotator (rotation is `maybe_roll(&mut self, now)`),
     and the compression cache (`compression_for(&mut self)` returns a cached entry by reference
     or a fresh one by value). `src/lock.rs` and the `sync` alias module are deleted.
4. **Certificate keys are borrowed for the signing call.** `CertifiedKey` owns its
   `Box<dyn SigningKey>` and is no longer `Clone`; resolvers return `Option<&CertifiedKey>`;
   `SigningKey::choose_scheme` returns a `Box<dyn Signer + '_>` that borrows the key, and the
   aws-lc-rs key types own their key pairs. `KeyProvider::load_private_key` returns a `Box`.
   - §3.2's claim, checked against the state machine. Server, TLS 1.3: the certificate is
     selected and CertificateVerify signed while handling ClientHello. Server, TLS 1.2:
     selection and ServerKeyExchange, likewise. Both true.
   - Client: false. TLS 1.3 receives CertificateRequest in one flight and signs CertificateVerify
     in the call that handles the server's Finished; TLS 1.2 receives CertificateRequest and
     signs at ServerHelloDone. The smallest correct alternative: the client keeps the request
     (`ClientAuthRequest`: context, root hint subjects, signature schemes) and resolves its
     credentials in the signing call. Messages and their order are unchanged. Observable
     difference: the client's resolver now runs after the server's certificate is verified
     instead of on receipt of CertificateRequest, and not at all if the handshake fails between.
5. **Verifiers, resolvers and root stores are owned by their configuration**: `Box<dyn …>` for
   each verifier and resolver, `RootCertStore` by value (`with_root_certificates(RootCertStore)`,
   verifier builders take the store). `WebPkiServerVerifier`'s builder returns
   `Box<WebPkiServerVerifier>`; `WebPkiClientVerifier`'s, and `no_client_auth`, return
   `Box<dyn ClientCertVerifier>`. `ClientConfig`, `ServerConfig`, `ConfigBuilder`,
   `CertifiedKey` and `Resumption` are no longer `Clone`; an endpoint that needs a configuration
   builds its own.
6. **Session compatibility uses identities, not pointers.** Upstream refused to resume a session
   under a different verifier or client-certificate resolver by comparing `Arc` pointers (the
   session kept `Weak`s). Each installation now draws an `Identity` from a process-wide
   `AtomicU64` (relaxed; only uniqueness matters, and 2^64 installations do not occur), and a
   stored session records the two identities it was made under. `client_auth_cert_resolver` is
   private, set through `ClientConfig::set_client_auth_cert_resolver`;
   `dangerous().set_certificate_verifier` replaces the verifier. Both mint a new identity.
7. **`OtherError` owns its source**: `OtherError(pub Box<dyn ClonableError>)`, built with
   `OtherError::new`. `Error` is `Clone`, so the source is clonable (`clone_box`) instead of
   shared by `Arc`.
8. **Stored sessions own their data.** A client session's ticket and the server's certificate
   chain are owned values. A TLS 1.3 ticket moves out of the store, as before. A TLS 1.2 session
   stays in the store for reuse, so retrieving it copies the ticket and chain, where upstream
   bumped two reference counts: measured below, +2 allocations per resumed TLS 1.2 handshake.
9. **Tests.** Only the API changed in them; every assertion is unchanged.
   - `rustls-test` pairs each connection with its configuration (`TestClient`, `TestServer`,
     `TestUnbufferedClient`, `TestUnbufferedServer`, holding a `Shared` configuration,
     `Rc<RefCell<_>>`) and drives handshakes through them. Test doubles that the test inspects
     while a configuration owns them (session stores, key logs, verifiers) share their state
     through `Arc` in test code only.
   - One test's meaning changed: `client_only_attempts_resumption_with_compatible_security`.
     Its "allowed case, using `clone`" has no counterpart, because configurations are not
     `Clone`; it now resumes with the same configuration. Its two disallowed cases replace the
     resolver and the verifier on that configuration through the setters, and expect the same
     full handshakes and the same trace messages.
   - New: `tests/alloc_per_handshake.rs`, counting allocations per handshake.
   - Oracle after this change: unit 246, api 227, api_ffdhe 5, client_cert_verifier 4, ech 2,
     key_log_file_env 2, process_provider 1, server_cert_verifier 6, unbuffered 27, doctests 15
     (3 ignored), all passing, plus `alloc_per_handshake`.
10. **Allocations per handshake**, client and server together, counted by
    `tests/alloc_per_handshake.rs` on the macOS aarch64 development machine, debug profile,
    three runs each, against the same test adapted to 526c2cc's `Arc` API. Reallocations vary
    by ±2 between runs in both.

    | Handshake | 526c2cc | Now |
    |---|---|---|
    | TLS 1.3 full | 296 allocs, 34,590,450 B | 286 allocs, 34,590,126 B |
    | TLS 1.3 resumed | 219 allocs, 47,234 B | 213 allocs, 46,994 B |
    | TLS 1.2 full | 144 allocs, 29,543 B | 140 allocs, 29,556 B |
    | TLS 1.2 resumed | 99 allocs, 19,586 B | 101 allocs, 20,974 B |

    The full handshakes' bytes are dominated by the brotli compressor's tables. The TLS 1.2
    resumed rise is item 8's copy of the ticket and chain out of the store.
