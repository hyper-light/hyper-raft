# hyper-swim: origin

- **Source.** slates' sans-io SWIM detector at slates `5cce86a`:
  - `crates/cluster/src/{detector,membership,gossip,coordinates,fixed}.rs`;
  - the pure codec half of `swim.rs` (lines 1–597, with its codec tests).
- **Why slates'.** It is the most complete of the three, and the only one free of a runtime (mantle
  note 32 §3.6). It already met the union lint wall: no changes were needed for it.
- **Left in slates.** slates' driver half of `swim.rs` (probing over slates' transport) and its
  `tests/swim.rs`, which drive slates' runtime and transport.

## Changes

1. **Identity.** `HostId` is this crate's own (`pub struct HostId(pub u64)`), not slates' database
   type.
2. **Witnessed extensions** (note 32 S13, from focal's `liveness/suspicion.rs`).
   - A suspected host asks for time with a progress witness it cannot fake while stuck
     (`Detector::request_extension`).
   - An overloaded host is never extended.
   - The witness must rise, and a grant is made at most once a period.
   - focal's millisecond grants and its literal cap of five are replaced by derived bounds in
     periods:
     - grants halve from half the base suspicion window, never below one period;
     - all grants together never exceed one base window.
   - A grant lengthens the subject's window, composing with corroboration and health dilation.
3. **The node's own lag** (mantle `node.md` §3.5).
   - `Detector::observe_self_lag` takes the measured delay between a probe's arrival and its
     handling.
   - The effective health multiplier is the larger of Lifeguard's score (missed and refuted
     probes) and the lag in whole periods, within `health_max`. They measure different things, so
     both stand (note 32 §6, item 4).

## Tests

- 43 unit tests: slates' 39, plus the extension series and its bounds, exact delay by a grant, and
  the lag's dilation.
- `tests/cluster.rs`: four real member processes run the detector over hyper-datagram on real
  UDP sockets.
  - The supervisor starts them together, lets them settle, and SIGKILLs one.
  - Every survivor must report it dead within a bound derived from the timing: a probe round, the
    widest suspicion window, and a gossip spread (33 periods here).
  - No live member may ever be suspected.
  - Measured over six runs: 3 to 5 periods after the kill, with no false suspicion.
