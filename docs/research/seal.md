# Sources for `docs/seal.md`

Each section is what `docs/seal.md` cites as "R §x". Where a claim rests on mantle's research, the
mantle note is named; it holds the quoted passages (mantle `docs/research/14`, AWS-LC; `20`,
server-side encryption; `31`, integrity).

## 1. Nonces, and STREAM

- **NIST SP 800-38D** (Dworkin, 2007), *Recommendation for Block Cipher Modes of Operation: GCM and
  GMAC*. §8 "Uniqueness requirement on IVs and keys": the probability that the authenticated
  encryption function is invoked with the same IV and key on two different inputs shall be no
  greater than 2^-32. §8.2.1, deterministic construction: a fixed field identifying the device or
  context and an invocation field incremented each call; §8.2.2, the RBG-based construction, a
  random field of at least 96 bits; §8.3, with random IVs, the total number of invocations under one
  key shall not exceed 2^32. §5.2.1.1: a plaintext of at most 2^39 − 256 bits. §5.2.1.2: tag lengths;
  128 bits is the full tag. Appendix A: a repeated IV lets an adversary recover the authentication
  key and forge.
- **Hoang, Reyhanitabar, Rogaway, Vizár**, "Online Authenticated-Encryption and its Nonce-Reuse
  Misuse-Resistance", CRYPTO 2015. Defines nOAE and the STREAM construction: segment `i` sealed at
  nonce `N ‖ i ‖ b`, `b` marking the last segment, which makes truncation and reordering detectable
  under a nonce-based AEAD. `docs/seal.md` §4 is STREAM with `N` empty (one key a file) and the last
  flag in the counter's top bit, as mantle's `seal.rs` already does.

## 2. Not GCM-SIV

- **RFC 8452** (Gueron, Langley, Lindell, 2019), AES-GCM-SIV: nonce-misuse resistant, at the cost of
  two passes over the plaintext. Not among NIST's approved modes (SP 800-38 series; mantle note 20
  §8.4).

## 3. Key wrap, rotation, erase

- **NIST SP 800-38F** (Dworkin, 2012), key wrapping: KW and KWP (§6), approved for protecting keys;
  App. A.3, KW's integrity check; §5.4, no limit on the number of keys wrapped under one KEK.
- **RFC 3394** (Schaad, Housley, 2002), the AES Key Wrap algorithm, with 256-bit test vectors (§4.6);
  **RFC 5649**, with padding.
- **NIST IR 8459** (2024), *Report on the Block Cipher Modes of Operation in the NIST SP 800-38
  Series*, §9: the same plaintext key should not be wrapped twice under one KEK (mantle note 20 §8.3).
- **NIST SP 800-57 Part 1 Rev. 5** (Barker, 2020), Table 1: a symmetric key-wrapping key's
  originator-usage period up to 2 years; §5.3.6: no wrapping with a key past it; §5.6.2: a key
  protected by a weaker key is reduced to the weaker key's strength.
- **AWS S3, UpdateObjectEncryption** and SSE-S3's envelope ("each object is encrypted with a unique
  key ... SSE-S3 encrypts the key itself with a root key that it regularly rotates"), mantle note 20
  §1.5–§1.6, §8.5.
- **NIST SP 800-88 Rev. 1** (Kissel et al., 2014), *Guidelines for Media Sanitization*, §2.6:
  cryptographic erase, sanitizing by destroying the key that encrypted the data.

## 4. Recipient wrap

- **FIPS 203** (2024), ML-KEM; ML-KEM-1024's encapsulation key 1,568 bytes, ciphertext 1,568 bytes,
  shared secret 32 bytes, security category 5.
- **NIST SP 800-56C Rev. 2** (2020), key derivation by two-step extraction then expansion (§5), with
  HMAC; binding context into `FixedInfo` so a derived key is specific to its use and parties.
- **RFC 5869**, HKDF, the extract-then-expand construction §6 uses.

## 5. Names and deduplication

- **Bellare, Keelveedhi, Ristenpart**, "Message-Locked Encryption and Secure Deduplication",
  EUROCRYPT 2013, and **DupLESS** (USENIX Security 2013): convergent encryption lets anyone who can
  guess a plaintext confirm it; keyed (server-aided) naming removes the brute-force confirmation for
  anyone without the key. `docs/seal.md` §7 keys names per tenant and does not converge, since no
  consumer deduplicates across tenants.

## 6. Post-quantum parameters

- **NSA, CNSA 2.0** (Commercial National Security Algorithm Suite 2.0, September 2022, and its FAQ):
  AES-256, ML-KEM-1024, ML-DSA-87, SHA-384 or SHA-512; hybrid key establishment accepted in the
  transition.
- Grover's search against AES-256 leaves about 128 bits of security; NIST's PQC call for proposals
  (2016) uses AES-256's key search as category 5.

## 7. Keys in memory

- **mlock(2)** (Linux man-pages; macOS `mlock(2)`): locked pages stay resident and are not paged to
  swap. Linux `madvise(2)` `MADV_DONTDUMP` (since 3.4): excluded from core dumps.
- **Linux hibernation** (`Documentation/power/swsusp.rst`): the image holds all of memory; locked
  pages are not exempt. So a node that must not leak keys disables hibernation or encrypts its swap
  and image. To check for the review: macOS's treatment of wired pages in its hibernation image and
  in panic dumps, and Windows' `VirtualLock` and hibernation, from Apple's and Microsoft's own
  references; `docs/seal.md` §8 states no guarantee for either until these are read.
- **zeroize** crate: volatile writes and a compiler fence, so a wipe is not elided.

## 8. Cost

- mantle `docs/measurements/2026-09-29-sealing-at-rest.md`: AES-256-GCM sealing at about 8 GB/s and
  opening at 8.4 GB/s on one core, a key made, wrapped and unwrapped in 1.8 µs, on the owner's
  machine. `docs/seal.md` §11 measures again here under load, at every segment size, and per record.

## 9. FIPS

- **aws-lc-rs**, `fips` feature: builds against `aws-lc-fips-sys`, the AWS-LC FIPS module, which
  holds FIPS 140-3 validation for its approved services (AES-GCM, AES-KW, HKDF, HMAC, SHA-2, its
  DRBG; ML-KEM in the module versions that list it). To check for the review: the certificate
  numbers and which module version first lists ML-KEM-1024 as approved, from NIST CMVP's records.
