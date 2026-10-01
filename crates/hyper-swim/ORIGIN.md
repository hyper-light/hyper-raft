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

4. **No allocation in a period** (`CLAUDE.md` §1a; `docs/benchmarks.md`, "hyper-swim"). slates'
   detector allocated 6 times a member a period in a quiet cluster and 12 to 14.5 times while
   membership churned, with 8 to 13 reallocations. Each source and its replacement:
   - **The wire.** `SwimMessage` owned its gossip and coordinate, `encode` returned a new vector and
     `decode` built both. A message now borrows them: `GossipBatch` and `Coordinate` are either what
     the sender holds or the received bytes, checked whole by `decode` and read in place;
     `encode_into` writes into the caller's buffer.
   - **The batches.** `gossip`, `ping_gossip` and `request_indirect` returned new vectors; their
     `_into` forms fill the caller's. `apply_gossip` and `apply_gossip_from` take any iterator of
     entries, a received batch included.
   - **The coordinates.** `coordinate` returned a copy and the Vivaldi step built a unit vector;
     the coordinate is lent, the step reads each axis in place, and `learn_coordinate` overwrites a
     held coordinate in place. A learned coordinate is kept only for a member this node probes and
     dropped when it is declared dead, so they are bounded by the membership (slates kept every
     peer's for ever).
   - **The tick.** Ageing collected the suspects and the membership's alive list into new vectors,
     scanning the whole membership each period. The membership now indexes its suspects, so ageing
     visits only them; `alive` and `suspects` are iterators; a round's probe order reuses the last
     round's vector.
   - **The gossip queue.** Two `BTree`s allocated and freed nodes as reports came and went. The
     reports are now a `HashMap` and one FIFO queue per transmit count, each keeping its capacity.
     The least-transmitted report still goes first; among equals the oldest goes first, where
     slates took the lowest host id. A replaced report's old entry is skipped when reached, and the
     queues are purged whenever a record finds them over twice the pending reports, which bounds
     them at twice the membership.

## Tests

- 46 unit tests: slates' 39, plus the extension series and its bounds, exact delay by a grant, the
  lag's dilation, and the gossip queue's order, replacement and bound.
- `tests/cluster.rs`: four real member processes run the detector over hyper-datagram on real
  UDP sockets.
  - The supervisor starts them together, lets them settle, and SIGKILLs one.
  - Every survivor must report it dead within a bound derived from the timing: a probe round, the
    widest suspicion window, and a gossip spread (33 periods here).
  - No live member may ever be suspected.
  - Measured over six runs: 3 to 5 periods after the kill, with no false suspicion.
