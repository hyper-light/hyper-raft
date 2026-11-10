# Experimental certificate-type compression policy

This clean pinned-baseline experiment is **unadopted**. No Shards vendor pin or
production endpoint architecture changed. The rejected input-derived Brotli window
branch `p/managed-certificate-window` / `bc2bc83` remains separate: its improvement
on small RPKs did not excuse its increased allocation and CPU above the old window
boundary through uint24 maximum. This branch does not change `compress.rs`.

Coordinator: root; implementer: resource/performance lane. Binding Shards policy
`c921a0b12b06d495b354907162e991376edffd41`, development-rules SHA256
`1ed0b1ad00e9228d813d1892d6f55cf330a5485647aff19527843ff8bf616b3d`.
All R01–R16, current handoff, upstream CLAUDE and the stricter Slates/Focal/Mantle
consumer policies were read. Scope: the four approved compression-selection/offer
conditions in client/hs.rs, client/tls13.rs and server/tls13.rs, focused tests and
provenance. Root owns review, adoption, native comparison and combined gates.

## Actual cause and alternatives

A real Shards pinned-owner QUIC handshake allocated a 33,554,432-byte Rust H10
forest. Stack capture attributed it to Brotli8.0.4 certificate compression; the
handshake-ceiling hypothesis was discarded. Existing compression-cache hits avoid
the cold cost but do not remove it. No cache is added. In the separately retained
probe the generated Ed25519 RPK Certificate body is53B, Brotli payload57B and
CompressedCertificate body65B. Ordinary certificate sending avoids both that extra
12B and optional encoder work for this actual identity.

- [RFC8879 §§3–5](https://www.rfc-editor.org/rfc/rfc8879#section-3) makes capabilities
  directional and compressed sending optional; RPK altered Certificate bodies may
  be compressed. It does **not** require an RPK policy. Verification and size limits
  must remain those of the ordinary Certificate path.
- [OpenSSL3.6.0 client](https://github.com/openssl/openssl/blob/7b371d80d959ec9ab4139d09d78e83c090de9779/ssl/statem/statem_clnt.c#L402)
  and [server](https://github.com/openssl/openssl/blob/7b371d80d959ec9ab4139d09d78e83c090de9779/ssl/statem/statem_srvr.c#L430)
  explicitly restrict compressed sending to negotiated X509. This established
  certificate-type policy plus actual RPK cost/expansion motivates this candidate.
- [rustls0.23.45](https://github.com/rustls/rustls/tree/v/0.23.45/rustls/src)
  offers configured decompression and chooses an encoder without an RPK-type
  exclusion. It is the pinned old-peer oracle and the demonstrated cold baseline.
- [GnuTLS3.8.10](https://github.com/gnutls/gnutls/blob/3.8.10/lib/ext/compress_certificate.c#L115)
  exposes session compression-method configuration used for requesting peer
  compression and selecting local sending. This inspected seam alone is not
  evidence of a negotiated RPK exclusion in all GnuTLS sending paths.
- Official Brotli size-hint/one-shot API experiments did not bound initial H10:
  BrotliMakeHasher initializes H10 with one_shot=false and input_size0. They kept
  roughly34.26MB peak on the53B input. Lower-level FINISH saved only about4KiB;
  preallocating maximum output increased large-input peak. Raw results remain in
  the rejected branch. Correcting that upstream algorithm remains an alternative
  requiring its own all-input proof; no arbitrary window or size cutoff is selected.

## Contract and ownership

The sending conditions use the same resolver `only_raw_public_keys()` predicate
as existing certificate-type validation. Server hs.rs validates both directions
before TLS13 handling; client hs.rs validates EncryptedExtensions against its
resolver and verifier. In this API true accepts RPK and false accepts X509/default;
a dynamically mixed resolver is unsupported. The selected certificate is not
reinterpreted, and no trust, SPKI, key, signature, quality or codec changes occur.
An RPK receiver omits compression capability; an RPK sender chooses the ordinary
Certificate path even if an old peer still offers compression. Ordinary Certificate
remains valid for a compressed-capable old peer. X509 retains existing selection,
encoding, decoder and message-size boundaries.

Sender-only and receiver-only experiments expose different old-peer costs; neither
is sufficient alone for both directions. The candidate is explicit production
certificate-type behavior, never a hidden benchmark compression switch. The test
variable HYPER_MATRIX_EXPECT only declares which frozen source is being checked.
No new resource authority is created: host/run reservations remain authoritative;
there is no learned grant expansion, new cache, buffer or task. Existing X509
encoder allocation and generic fallible-allocation coverage remain open.

## Frozen source and method

Native Linux amd64/glibc Fedora runner8fadc1ec213332e16cb0d915a3fb913f, admitted
4CPU/8GiB/10GiB, private synced managed-rpk-negotiation checkout and owned
`~/managed-certificate-target`. No local build; other lanes/root clones untouched.
Pinned base f116ac276d2d53296de24c2a43837baf66ed1cbd; unrelated upstream687244f
work is excluded.

| Frozen variant | Source | Result |
| --- | --- | --- |
| baseline initial red |1495155|0/1, cargo101: observed RPK compressions(1,1), expected(0,0) |
| baseline complete allocation phase |849fbf8|32 valid cases passed |
| sender only |043891e|32 valid cases passed |
| receiver only |a7f998c|32 valid cases passed |
| both |7e9c163|32 valid cases, old-peer regression and8 wrong-pin negatives passed;8 existing compression API tests passed |
| fixture typing/complexity corrections |edf3a1c|same3 focused tests and strict clippy passed |

The initial phase at1495155 constructed configs before counting and dropped them
inside counting, producing negative live totals. Those early raw totals are not
valid memory accounting. From849fbf8, config construction, handshake, encrypted wire
Vecs and connection/config teardown share one counting phase. Returned64B flight
Vec remains live at phase end. A single OnceLock provider replaces per-case leaked
providers. C crypto allocation is outside the Rust allocator; process high-water
footprint is cumulative and cannot be treated as a per-case memory delta.

The harness transfers real encrypted TLS13 records, verifies actual signatures and
exact pinned SPKI or real fixture X509 roots, then round-trips binary application
bytes. It does not open a socket. Each matrix covers2×2 old/new roles,2×2 server/
client RPK-or-X509 types, and offered/unoffered compression (32cases). Counts use
forwarding codec spies; old peers are unmodified rustls0.23.45. Flight pairs are
successive drains `(client TLS bytes, server TLS bytes)`, not QUIC packets or a
network RTT measurement. Each negative case deliberately pins the other Ed25519
fixture identity and requires typed ApplicationVerificationFailure before payload.

## Results and limits

Single phase samples, RPK/RPK with compression configured. Requested/peak bytes are
Rust fixture totals; they are not native process/kernel memory claims.

| Source policy / peers | Client/server encoder calls | Encrypted TLS bytes | Requested Rust bytes | Peak live Rust bytes |
| --- | --- | ---: | ---: | ---: |
| baseline new/new |1/1|3388|68,891,045|34,339,114|
| sender new/new |0/0|3364|131,203|79,737|
| receiver new/new |0/0|3346|130,529|79,692|
| both new/new |0/0|3346|130,409|79,692|
| both new client / old server |0/0|3355|130,930|80,021|
| both old client / new server |0/0|3355|124,010|72,512|

Both policies together remove both RPK encoder invocations even with an old peer.
Same-build total wire shrinks42B:24B from two uncompressed RPK messages and18B from
two omitted capability extensions. The transfer/drain count remains two pairs.
Sender-only saves24B with two new peers. Against one old peer sender-only leaves
one old encoder invocation; receiver-only leaves the opposite encoder invocation.
These single samples do not establish latency distributions or a4–5× overall win.
All-X509 exact wire/count equivalence is checked by summarize.py across variants.

Existing `api compress` tests cover RSA2048 X509 cold/cache reuse, optional encoder
failure fallback, corrupt/decompression failure and oversized decompressed chain
refusal. They passed8/0. Full compression boundary through TLS uint24 maximum is
unchanged-source evidence only on this candidate; the separate rejected algorithm
campaign's full boundary results are not this candidate's passing gate.

## Repeat commands and owed gates

With each frozen source synced, in the admitted private runner checkout:

```sh
export CARGO_TARGET_DIR="$HOME/managed-certificate-target"
HYPER_MATRIX_EXPECT=baseline cargo test --locked -j4 --release -p hyper-tls \
  --test rpk_compression_policy certificate_type_and_old_peer_matrix -- \
  --ignored --exact --test-threads=1 --nocapture
# Use sender/receiver/both only with the respective frozen source above.
HYPER_MATRIX_EXPECT=both cargo test --locked -j4 --release -p hyper-tls \
  --test rpk_compression_policy -- --include-ignored --test-threads=1 --nocapture
cargo test --locked -j4 --release -p hyper-tls --test api compress -- --test-threads=1
cargo clippy --locked -j4 --release -p hyper-tls --lib --test rpk_compression_policy -- -D warnings
export MANAGED_RPK_EVIDENCE="$HOME/managed-rpk-evidence/mutations"
python3 docs/research/measurements/managed-rpk-negotiation/mutations.py
```

At signed source233b7fcef7a363327c5439b191724a38065062cd, all four independent
guard removals failed with the intended observed one-direction compressor call
(cargo101), then restored source passed3/0 and strict clippy. Each mutation's
source diff/hash, command, raw log and exit are in [results/mutations](results/mutations/).
Actual compile logs are retained; restoration rewrites owned files with current
mtimes, and git diff confirms no mutation remains. [Raw results](results/) include
the initial fixture compile/lint failures as well as passing checkpoints;
[summary.json](results/summary.json) verifies all-X509 exact wire/count equivalence.
Native
real-socket equivalent2N/shared cold/warm/allocation/latency and QUIC packet counts,
noise, parent/kernel/socket/cgroup charges, page faults/copies and instrumentation
calibration remain owed. The independent observer reconnect refusal is a separate
quality lane; no retry, poll loop, connection-cap increase or new cache masks it.
Full upstream gates, all consumers' suites and supported seven-target Shards/actual
host VM validation are unrun. Root reviews before any adoption/re-vendor/re-pin.

## R01–R16 evidence account

| Rule | Evidence / remaining gap |
| --- | --- |
|R01|Four production conditions add no allocations/arithmetic/panic preconditions. Strict clippy passes; upstream existing allocation failure guarantees are not newly proved. Test failures use typed oracle/IO/progress outcomes.|
|R02|Pinned RFC/OpenSSL/rustls/GnuTLS and actual source/allocation comparisons above; no architecture adoption.|
|R03|No production literal, threshold or tuning value added; certificate TYPE is the existing negotiation authority.|
|R04|Existing certificate-type validation and ordinary/optional compressed message states retained; no new implicit lifecycle.|
|R05|No new cache/wait. Existing compression cache cold/hit behavior measured separately; cold allocation addressed at selection seam.|
|R06|Four local conditions; fixture enumerates role/type pairs directly and passes strict complexity lint.|
|R07|Comments explain connection to validated type and VENDORED§10; rationale and limitations here.|
|R08|Pinned comparable oracle measured; no native latency/resource4–5× claim; target remains owed.|
|R09|Actual32MiB allocation stack and red live TLS RPK codec invocation captured before fix.|
|R10|Frozen identities/raw results retained; four independent guard removals reproduce actual encoder regressions, restored3/0 and clippy pass.|
|R11|Linux amd64/glibc only; remaining seven-target/native VM combinations unverified, Intel Mac unsupported, Windows ARM VM paused untouched.|
|R12|Existing TLS/verifier/resolver/codec path reused; no crypto/framework/root/installed binary change.|
|R13|Minimal upstream-only production delta; no rejected window code or unrelated687 diff.|
|R14|Rust allocation/reallocation/free/requested/peak fixture phase recorded; C/kernel/socket/fault/copy/native distributions remain owed.|
|R15|Scoped implementer, no children/main merge; signed named-path checkpoints and all owed work returned to root.|
|R16|Private caches/source; no guard bypass or other-lane cleanup; Mac30GiB floor and no local builds.|
