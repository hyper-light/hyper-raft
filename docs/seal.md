# Sealing at rest: `hyper-seal`

> Status (2026-10-05): designed, for focal's review. Approved by the owner on 2026-10-04 as the one
> at-rest construction for mantle, focal and slates, and with it hybrid post-quantum key exchange
> only between our own nodes (§10). Sources, and what each establishes, are in
> `docs/research/seal.md`, cited here as "R §x".

Every byte a consumer keeps of a tenant's data is sealed before it reaches a device, and opened only
by a process that holds the tenant's key. `hyper-seal` is the one place that does it. It holds the
keys and their hierarchy, the seals of whole files and of appended logs, the wrap of a key to
another machine, the names a store may give sealed content, and the memory that holds keys. The
log (`hyper-log`) and the durable shell's images (`hyper-durable`) seal through it, and each
consumer seals its own stores through it.

It is designed from mantle's sealing of objects (mantle `docs/design/encryption.md`,
`crates/s3/src/seal.rs`), from focal's and slates' requirements (§1), and from the literature. Where
one of the three already does something well it is kept; where all three need more, it is built
here.

## 1. What the three need

| | mantle | focal | slates |
|---|---|---|---|
| Sealed | object data (64 KiB segments), the shared log, range images | ledger WAL frames (a few hundred bytes, group-committed), durable images, content-addressed chunks and manifests, client op journals, enrollment secrets and identity keys, backups | content chunks (16 base pages: 64 KiB Linux, 256 KiB macOS), shard images (~220 B a chunk), op-log records (100 B to KiB), consensus logs, NFSv4 open state, archives |
| Key levels | root key generations, a key per file, a customer's key (SSE-C) | node root (generations), tenant, a key per file | tenant, volume lineage, a key per chunk; clones share their parent's sealed chunks |
| Erase | | per tenant (GDPR) | per volume, per tenant |
| Key source | a 0600 file off the data devices, or a key service | a 0600 file off the data device; OS keychain, Secure Enclave or TPM where present; key service | keychain, Secure Enclave, TPM or key service by default; an operator's key file read-only; slates never writes a key to disk |
| Offline | | restart with no network; offline readers with a key file; restore to another machine | |
| Another machine | | ML-KEM-1024 recipient wrap for backups and hand-off; replication re-seals at the destination | holders keep ciphertext only; a successor takes the volume key wrapped to it (ML-KEM to its certificate's identity, or to the region authority); archives to an ML-KEM-1024 recipient |
| Names | | an object's ID stable across key rotation in its tenant; the plaintext hash never outside the tenant's sealed metadata; the on-disk name a per-tenant keyed hash | a keyed per-tenant ID; replication's missing-set exchange uses it |
| Reads | byte ranges, segment by segment | reads open only what they touch | 4 KiB to 1 MiB page-aligned reads; a segment of 4 to 16 KiB opens alone |
| Compliance | | FIPS 140-3 selectable; CNSA 2.0 parameters | FIPS 140-3 selectable; CNSA 2.0; keys locked in memory, kept out of core dumps, wiped |
| Cost | sealing at 8 GB/s a core (mantle's measurement) | a frame's tag and one GCM call are noise beside its flush | a key made and wrapped in single-digit µs; a 4 KiB open ≤ ~1 µs p99 under load; sealing ≤ 10% of a chunk's seal cost; replication at line rate; energy per byte measured on Apple silicon and x86 laptops |

## 2. The construction, and why

**AES-256-GCM** seals every byte, through aws-lc-rs (`aws-lc-sys`, or `aws-lc-fips-sys` under the
`fips` feature, §9). AES-256 meets CNSA 2.0's symmetric requirement (R §6) and keeps 128-bit
security against Grover's search (R §6). GCM is a NIST mode (SP 800-38D), streams in one pass, and
runs at memory bandwidth on AES-NI and ARMv8 cryptography extensions (mantle measured 8 GB/s a core
sealing, 8.4 GB/s opening; R §8). Every tag is the full 128 bits (SP 800-38D §5.2.1.2).

**Nonces are never random.** Every key here seals one sequence of things, each at a position that
never repeats under it, and the nonce is that position: SP 800-38D §8.2.1's deterministic
construction, its fixed field constant because the key is used for one sequence, its invocation
field the position (R §1). A random 96-bit nonce would bound a key to 2^32 seals (§8.3) and its
safety would rest on the generator; a position bounds nothing and rests on nothing but the rule that
a key never seals two things at one position. Each construction below states why its positions
cannot repeat, including across a crash, since that is where a repeated nonce comes from: a writer
that comes back and writes again where a lost write went.

**Not AES-GCM-SIV.** It survives a repeated nonce (RFC 8452), but takes two passes, cannot stream,
and is not a NIST mode (R §2). The constructions here never repeat a nonce, so its one advantage buys
nothing, and the FIPS build could not use it.

**Keys are wrapped with AES-256 key wrap** (SP 800-38F KW; RFC 3394): deterministic, approved for
keys, 40 bytes for a 256-bit key, with an integrity check whose forgery probability is 2^-64 (SP
800-38F App. A.3), and no limit on the keys one wrapping key wraps (§5.4). A wrapping key is
AES-256, since a 256-bit key wrapped under AES-128 is reduced to 128 bits (SP 800-57 §5.6.2).
Random 256-bit keys meet under one wrapping key with probability about n²/2^257, so "the same
plaintext key should not be encrypted twice under the same key-wrapping key" (NIST IR 8459 §9)
holds.

**CRC-32C stays** on every stored record, over the bytes as stored (ciphertext). It is the
storage layer's check that the device returned what was written, which feeds repair (CLAUDE.md §6).
A CRC is not authentication: anyone who can write the device can change bytes and recompute it.
Only bytes under a GCM tag or a MAC (§5.1) are authenticated, and only those are trusted against an
adversary. A record whose CRC holds and whose tag or MAC fails was sealed under another key, at
another place, or altered by someone who recomputed the CRC; it is a typed `Open` or `Tampered`
error, which the consumer reports as corruption and never as a torn tail.

**Every key commits.** AES-GCM is not key-committing: one ciphertext can be made to open validly
under two chosen keys (Dodis, Grubbs, Ristenpart and Woodage, "invisible salamanders", CRYPTO 2018;
Len, Grubbs and Ristenpart, partitioning oracles, USENIX Security 2021; R §10). Keys here are
random, but clones share chunks under wrapped lineage keys and a tenant controls what it wraps, so
every file header (§4) and key frame (§5) carries a 32-byte commitment,
`HMAC-SHA-256(data key, "hyper-seal commit" ‖ file or session ID)`, checked before the first open
under that key. A key that does not match its commitment opens nothing (the idea of Chan and
Rogaway's CTX, ESORICS 2022, made with an approved MAC).

## 3. Keys

### 3.1 The hierarchy

```
key source (wrap/unwrap)            — never leaves its source where the source is hardware
  └─ root key, generation g          — 32 bytes, wrapped by the source, kept in the node's key file
      └─ tenant key t                — 32 bytes, random, wrapped by root g; one record per tenant
          └─ lineage key (optional)  — 32 bytes, random, wrapped by tenant t (slates' volumes)
              └─ data key            — 32 bytes, random, wrapped by its parent, in its file's header
```

Every key is random (§3.4), never derived from another, so learning a data key reveals nothing above
it, and erasing a parent erases every child whose only copy is wrapped under it. A level is a
`WrappingKey` whatever its depth; the hierarchy is the consumer's records of who wraps whom, and
`hyper-seal` gives each level the same three calls: make a child, wrap it, unwrap it.

- **Erase.** Destroying the one wrapped copy of a tenant's key (or a volume's lineage key) erases
  every byte sealed under it, wherever its ciphertext lies: on replicas, in backups, in archives
  (R §3). This is NIST SP 800-88r1's cryptographic erase. A consumer that keeps the tenant record
  replicated destroys every replica's copy, and records the destruction (§3.3).
- **Rotation is a rewrap.** A new root generation wraps the tenant keys again; nothing below them
  moves. A tenant key rotated wraps its data keys again: 40 bytes of each file's header, no data, as
  S3's UpdateObjectEncryption changes an object's key "without any data movement" (R §3). A
  key-wrapping key's originator-usage period is at most two years (SP 800-57 Pt 1 Table 1); a
  wrapping key past it is never used to wrap (§5.3.6), only to unwrap for a rewrap.
- **Clones.** A slates clone shares its parent's sealed chunks: the clone's lineage key wraps the
  parent's lineage key (not its chunks' keys), so a clone opens its parent's chunks with one unwrap,
  and erasing the parent's lineage erases what only the parent held while the clone keeps its own
  copy of the wrapped parent key only as long as it shares chunks.

### 3.2 Key sources

```rust
pub trait KeySource {
    /// The source's name and generation, recorded beside each key it wraps.
    fn id(&self) -> SourceId;
    fn wrap(&mut self, key: &Secret32) -> Result<Wrapped, SealError>;
    fn unwrap(&mut self, wrapped: &Wrapped) -> Result<Secret32, SealError>;
    /// A trusted monotonic counter the source keeps (a TPM NV counter, the Secure Enclave), where
    /// it has one: read, and advanced past a value. `None` where the source has none (§5.2).
    fn monotonic(&mut self) -> Option<&mut dyn Monotonic> { None }
}
```

The root key is never on a data device. `hyper-seal` gives two sources and the trait for the rest:

- **`FileSource`**: 32 bytes in a file the configuration names, created owner-only (0600 on Unix, an
  ACL of the owner alone on Windows), refused at open if any other principal can read it, read-only
  where the operator says so (slates).
- **`MemorySource`**: a key held only in locked memory (§8), for tests and for a key a consumer was
  handed (a successor's unwrapped volume key, §6).
- **Hardware and services** implement the trait in the consumer or its platform module: the macOS
  Keychain and Secure Enclave, a TPM 2.0, a key service. Each wraps and unwraps; the root key's
  bytes never leave a hardware source. These are platform bindings the consumers own because each
  ties to the consumer's deployment (slates' daemon, focal's enrollment); the trait is the contract.

### 3.3 Records

A wrapped key is stored with what is needed to unwrap it and nothing more:

| Field | Bytes | |
|---|---|---|
| version | 1 | 1 |
| parent | 16 | the wrapping key's ID (a random 128-bit ID given at its making) |
| generation | 4 | the parent's generation |
| wrapped | 40 | AES-256-KW of the key |

61 bytes. A key's ID is random, never derived from the key. Destruction of a key is recorded by the
consumer (focal's audit, slates' volume log): `hyper-seal` gives `destroy`, which wipes the key in
memory and returns the record to keep.

### 3.4 Generation

Keys are read from the operating system's generator through `getrandom`, which returns a typed
error where AWS-LC's own generator aborts the process. Under `fips`, keys come from AWS-LC's
approved DRBG through its fallible interface, behind the unwind boundary (§9).

## 4. Sealing a file written once

A file written once, whole or in order and never rewritten (mantle's objects and parts, focal's
chunks and manifests and backups, slates' chunks and archives, every image `hyper-durable` writes),
is sealed by **STREAM** (Hoang, Reyhanitabar, Rogaway and Vizár, CRYPTO 2015; R §1): its bytes in
segments of `S` plaintext bytes, each an AES-256-GCM seal under the file's data key, at nonce

```
nonce = 0x00000000 ‖ (index | LAST·[last segment])       (32 + 64 bits; LAST = 2^63)
```

with the file's ID (16 bytes, random, in its header) as additional data. A segment opens only at
its own index and only as last if it was sealed last, so a file cut short, extended, reordered or
spliced with another file's segments fails to open (STREAM's nonce-based OAE security, R §1).

- **Why its positions cannot repeat.** One key seals one file; the file is written once. A writer
  that fails mid-file and comes back writes a new file under a new key; it never continues a file
  it did not finish (mantle's resumed uploads already work this way, encryption.md §3).
- **Segment size** is the consumer's, fixed per file and recorded in its header: mantle 64 KiB (its
  chunk store's checksum block), slates one base page to 16 KiB (its reads), focal 64 KiB. A segment
  adds 16 bytes: 0.024% at 64 KiB, 0.39% at 4 KiB. The bound is GCM's 2^39 − 256 bits a seal (SP
  800-38D §5.2.1.1), so `S` is refused above 64 GiB; it is also refused below 512 bytes, where the
  tag is more than 3% of the bytes.
- **Sizes follow from plaintext.** A segment of `n` bytes is `n + 16` stored bytes, so a byte's stored
  offset follows from its plaintext offset, and a read opens only the segments its range covers.
- **The file's header** carries the wrapped data key record (§3.3), the file ID, `S`, a version, and
  the key's commitment (§2).
  It is authenticated: every field but the key record is the additional data of segment 0 alongside
  the file ID, and the key record's key is pinned by the commitment, so a header re-pointed at
  another key fails its commitment and one with any other field changed fails segment 0. The key
  record stays out of segment 0's additional data because a rotation rewrites it (§3.1).

## 5. Sealing an appended log

The shared log (`hyper-log`) appends frames to segments of a fixed size, reuses segments, and after
a crash continues writing in its head segment at the end of what it recovered, over the bytes of a
torn tail. A torn frame and the frame written over it may hold different records at the same
offset. So a log's keys are not a key per file.

**A key per writer session per segment.** The writer draws a new data key whenever it starts writing
a segment: when it opens a segment, and when it continues the head segment after recovery. The key
of a segment's opening session is wrapped in the segment's header. A continuation first writes a
*key frame*, a frame holding only the new key's wrapped record, at the offset where it continues;
every frame after it in that segment, until the next key frame, is under that key.

**Records are sealed one by one.** Each record's payload (an entry's bytes, a proposal's bytes) is
sealed under its session's key at

```
nonce = 0x00000000 ‖ offset          (offset: the payload's byte offset in its segment, 64 bits)
```

with additional data the log's ID, the segment's incarnation, the group, the index and the term:
everything that says which record this is. A record read alone (`entry_at`) opens alone, as focal
and slates need, and costs one GCM call; a frame of many records costs one call each.

- **Why its positions cannot repeat.** Within one session the writer only appends: every payload goes
  at an offset past every payload before it in that segment. A crash ends the session; the next
  writes under a new key. So no key ever seals two payloads at one offset. A torn frame's records
  are under the dead session's key; a reader that finds them finds the next key frame first in the
  offsets after them and knows they are not the new session's. The order matters and is kept: the
  key frame is written in the same write as the first frame it keys, and recovery treats a frame
  whose key record it has not read as damage past the torn tail, which it already does for any
  frame after the last valid one.
- **Framing stays readable, under a MAC** (§5.1). Frame headers, persist records, group IDs,
  indices, terms and hard states are positions, not data, and recovery reads them first; but a
  term, a vote or a frame boundary an adversary rewrote would let a member vote twice in a term,
  which breaks Election Safety. So they are not left to their CRCs. What they reveal is the log's
  shape: how many groups, how many entries, their sizes. A consumer for whom the shape is sensitive
  pads its own payloads.
- **Cost.** A record grows by 16 bytes, and a session costs one key made and wrapped (single-digit
  µs) at a segment's opening, which happens once per segment's worth of writes. §11 measures group
  commit sealed against unsealed, open loop under load, before this is committed as the log's
  default.
- **Format.** A sealed log is format 4. Its segment header gains the wrapped key record, the key's
  commitment and a sealing flag, and every frame header and persist record a 32-byte MAC; a
  format-3 log (mantle's files, unsealed) still opens and is sealed from its next segment on, never
  rewritten in place.

### 5.1 The log's authentication key

Each log has an authentication key, a child of the tenant key (or of the root, for a node's own
log), wrapped in a record the log keeps beside its segments and unwrapped at open with the rest. Every
frame header, every persist record and every segment header carries
`HMAC-SHA-256(auth key, "hyper-seal frame" ‖ log ID ‖ bytes)` over its bytes, CRC included,
truncated to nothing: the full 32 bytes. Recovery checks the CRC first (the device's check, which
tells a torn write from a good one) and then the MAC, before it trusts a term, a vote, an index or a
frame boundary. A frame whose CRC holds and whose MAC fails is `Tampered`, a typed error the log
reports and never reads as its torn tail. HMAC rather than GMAC because its key may be used for any
number of messages without a nonce, and a persist record is rewritten in its slot.

### 5.2 Rollback

The tags and MACs bind every record to its place, not to its time. An adversary who can write the
device can put back an older, validly sealed segment or persist record: a member then forgets a vote
or an acknowledgement, which can lose an entry a quorum counted on. Detecting it needs state the
adversary cannot roll back: a trusted monotonic counter (`KeySource::monotonic`: a TPM NV counter,
the Secure Enclave). Such counters are slow (a TPM 2.0 NV increment takes milliseconds) and wear
out at a vendor-specified endurance, which is why Memoir (Parno et al., IEEE S&P 2011) and ROTE
(Matetic et al., USENIX Security 2017) exist; one advance per group commit would put milliseconds on
every acknowledgement and exhaust the counter in days. So the counter is bound where a rollback
breaks safety and changes are rare: **the hard state**. Before a term change or a cast vote is
persisted and the vote answered, the counter is advanced and its value bound into that hard state's
MAC; recovery refuses a hard state whose counter is behind the source's. That keeps Election Safety
(no second vote in a term after a rollback) at a few advances an election. The bound is stated and
checked at open: the measured election rate's advances an hour against the counter's specified
endurance over the node's service life, refused at configuration when it does not fit. A rollback of
entries (a member losing acknowledged entries) is caught only through the quorum, whose other
members hold them, and is stated so; Memoir's approach can tighten it later. Without a counter,
rollback at rest is a stated non-goal (§13).

## 6. A key to another machine

A backup, an archive, a hand-off to a successor or a restore on a fresh machine needs a key there
that the source machine does not share a root with. It is wrapped to the recipient's public key:

```
(ct, ss) = ML-KEM-1024.Encaps(recipient_ek)                     (FIPS 203)
kek      = HKDF-SHA-384(salt = context, ikm = ss, info = "hyper-seal recipient" ‖ recipient_id ‖ key_id)
record   = ct ‖ AES-256-KW(kek, key)
```

Two modes, recorded in the record's first byte:

- **CNSA** (the only mode under `fips`): ML-KEM-1024 alone, as above. ML-KEM-1024 is CNSA 2.0's
  key-establishment algorithm and NIST's category 5 (R §6).
- **Hybrid** (the default otherwise): ML-KEM-1024 and ECDH over P-384, their shared secrets
  concatenated as SP 800-56C Rev. 2 §2 allows (`ikm = ss_kem ‖ ss_ecdh`, with both ciphertexts and
  both public keys in `info`), so the key stays safe while either problem stays hard. Backups and
  archives live for decades, and ANSSI's and BSI's guidance asks for hybrid key establishment through
  the transition (R §6). P-384 rather than X25519 because it is FIPS-approved, so the hybrid's
  classical half is inside the module too.
 The KDF is
SP 800-56C's two-step (extract, expand) with SHA-384, CNSA 2.0's hash. Binding the recipient's ID
and the key's ID into `info` keeps a record from being replayed as another key's or another
recipient's (R §4). A CNSA record is 1 + 1,568 + 40 bytes; a hybrid record adds P-384's 97-byte
ephemeral public key. The recipient's decapsulation key is a key like
any other: held by a key source, wrapped at rest.

Replication between our own nodes is not this: holders keep ciphertext and never a key (slates), or
re-seal at the destination under its own keys (focal). A successor that must open a volume takes
its key wrapped this way to its certificate's identity (an ML-KEM encapsulation key the certificate
names), or to the region authority's.

## 7. Names for sealed content

A store that names content by its plaintext hash tells anyone who can list it whether a given
plaintext is present; convergent encryption makes it worse, letting a reader confirm a guessed
plaintext (Bellare, Keelveedhi and Ristenpart: message-locked encryption, EUROCRYPT 2013, and
DupLESS, USENIX Security 2013; R §5). Neither focal nor slates deduplicates across tenants, so neither needs convergence:

- **The on-disk name is a keyed hash**: HMAC-SHA-256 under the tenant's naming key of the plaintext
  hash, truncated to 128 bits. Without the tenant's key a name confirms nothing. The naming key is a
  child of the tenant key (§3.1), so it erases with the tenant.
- **The content's ID stays the plaintext hash** inside the tenant's sealed metadata (focal's
  `artifact:ID@HASH`, custody proofs), stable across data-key rotation, since rotation rewraps keys
  and never changes plaintext.
- **Deduplication within a tenant** compares keyed names, which are equal exactly when the plaintext
  hashes are. Each copy still has its own data key unless the consumer shares the sealed file, which
  it does by reference, not by sealing twice.
- **Replication's missing-set exchange** uses keyed names, which reveal nothing to a holder without
  the tenant's key.

HMAC-SHA-256 rather than keyed BLAKE3 because it is in the FIPS boundary and in the workspace already;
a consumer that names by BLAKE3 hashes the plaintext with BLAKE3 and keys the name with HMAC.

## 8. Keys in memory

A key in memory is in `Secret32`: 32 bytes in a page that is locked against swap (`mlock` on Linux
and macOS, `VirtualLock` on Windows), kept out of core dumps where the OS offers it (`MADV_DONTDUMP`
on Linux), and wiped on drop (volatile writes and a compiler fence). Locked pages are still written
to a hibernation image: macOS writes them to its image (encrypted under FileVault), Windows to
`hiberfil.sys`, Linux to its swap (R §7). So:

- **Servers** turn hibernation off at install, and say so.
- **Laptops** (focal, days of use): the hibernation image is protected by full-disk encryption at
  rest, which the consumer's install requires, and every key in the arena is wiped on the OS's
  suspend notification where it gives one (`Arena::wipe_all`), the keys unwrapped again from the
  key source on resume. Keys share locked pages from one arena,
so a process locks a bounded number of pages (a stated count, refused past it), not a page a key.
The arena's size is the consumer's stated count of keys held at once.

A consumer that holds plaintext in RAM between writes (slates' "at rest is idle RAM") seals it with
§4's construction under a volume key in a `Secret32`, and keeps only the sealed bytes; its plaintext
buffers are the consumer's to wipe.

## 9. FIPS mode

`hyper-seal` builds against `aws-lc-sys` by default and `aws-lc-fips-sys` under the `fips` feature
(aws-lc-rs's FIPS module: AWS-LC FIPS 3.0, FIPS 140-3 certificate #5314, the first validated
module to include ML-KEM, ML-KEM-1024 among it; R §9). Every primitive here is in the module's approved
set: AES-256-GCM, AES-KW, HKDF with SHA-384, HMAC-SHA-256, ML-KEM-1024, its DRBG. A consumer selects
FIPS at build time; `hyper_seal::fips()` reports which module it runs, so a node can refuse to start
when its configuration demands FIPS and the binary is not. Every call into AWS-LC runs behind the
unwind boundary (CLAUDE.md §1); none is expected to unwind, but a panic there is a typed error, not a
process abort.

## 10. Between our own nodes

`hyper-quic` between our own nodes admits only the hybrid groups `X25519MLKEM768` and
`SecP256r1MLKEM768` and TLS 1.3's 256-bit suites (`TLS_AES_256_GCM_SHA384`,
`TLS_CHACHA20_POLY1305_SHA256`). Initial packets stay AES-128-GCM, which RFC 9001 §5.2 fixes, since
their keys come from the connection ID and protect nothing secret. This is node-to-node
configuration in `hyper-quic` and `hyper-tls`, not in `hyper-seal`; clients reaching an S3 or NFS
endpoint keep the standard sets.

## 11. Measurement, before the log seals by default

- **Group commit, sealed against unsealed**: hyper-log's open-loop bench at the frame sizes of
  focal (frames of a few hundred bytes, group commit at its single-digit-ms flush), slates (100 B to
  KiB) and mantle, p50, p99 and p999 of the acknowledgement, under the machine's own load, recorded;
  sealing is the default only if its added latency is within a margin stated from the measured flush
  distribution.
- **Open of one record**: a 4 KiB and a 300 B record, p50 and p99 under load (slates: ≤ ~1 µs p99 at
  4 KiB).
- **Key make and wrap**: p50 and p99 (slates: single-digit µs).
- **Throughput**: seal and open GB/s a core at each segment size of §4.
- **Energy per byte**: hyper-measure's energy readings (RAPL on Linux x86, IOKit on macOS) sealed
  against unsealed, on Apple silicon and an x86 laptop.
- **Allocations**: none on the seal and open paths after setup (the workspace's allocation law).

## 12. Tests

- Known-answer vectors: GCM test cases 13–16 (McGrew–Viega), RFC 3394's 256-bit wrap vectors, RFC
  5869's HKDF vectors (SHA-384 from Wycheproof), ML-KEM-1024 from NIST's ACVP vectors, HMAC-SHA-256
  from RFC 4231.
- STREAM: every segment opens at its index only; truncation, extension, reordering and splicing
  between two files each fail typed; a flipped bit anywhere fails typed and never panics.
- Log: a crash at every write of a session (the deterministic disk of `hyper-sim`), then a
  continuation: every acknowledged record opens, no key ever seals two payloads at one offset (the
  test records every (key, offset) sealed and checks it), a torn frame's records never open under
  the new session's key.
- Commitment: a file or session whose key does not match its commitment opens nothing; a header
  re-pointed at another key fails before any open.
- Tamper: a frame header, persist record or segment header rewritten with a recomputed CRC fails
  its MAC as `Tampered`, never as a torn tail; every hard-state field changed alone is caught.
- Recipient: both modes round-trip; a record replayed under another recipient or key ID fails; the
  hybrid fails if either half's shared secret is replaced.
- Erase: a tenant key destroyed leaves every file under it unopenable, and its records report it.
- Rotation: a rewrap opens every file and changes only key records.
- Keys in memory: the arena refuses past its count; a dropped key's bytes are zero (read through the
  arena in a test build).
- End to end: hyper-log and hyper-durable's E2E suites run sealed on all six targets.

## 13. What this does not do

- It does not hide the shape of a log or a store (§5, §7): sizes and counts are visible to a reader
  of the device.
- It does not protect a running process's memory from its own host's root; that is the consumer's
  isolation.
- It does not choose a consumer's key source; the consumer binds the hardware it runs on.
- It does not detect rollback at rest without a trusted monotonic counter (§5.2).
