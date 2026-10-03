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
   - focal's millisecond grants and its literal cap of five are replaced by derived bounds in the
     base window's unit:
     - grants halve from half the base window, never below one;
     - all grants together never exceed one base window.
   - Since change 5 the base window is the one probe that tells a suspect, so a grant is one more
     told probe.
3. **The node's own lag** (mantle `node.md` §3.5). Superseded by change 5: the node's lateness is
   measured into its granularity, its round trips and the moment it judges, so
   `observe_self_lag` and the health multiplier are gone.

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

5. **Timing from measurement** (`docs/timing.md` §2.7). slates' detector ticked once a period its
   caller picked and took `DetectorTiming` (suspicion window, its floor, Lifeguard's `K` and
   multiplier cap, the dissemination budget) from its caller. Now:
   - each member's probes of a peer are that pair's NFD-E detector: a `hyper_timing::LinkEstimator`
     per peer fed the probes' round trips, a pool estimator over all of them for pairs not yet
     configured, and `detector_at`'s margin at the pair's interval;
   - the detector is polled (`poll(now)`) and says when next (`wake`): a probe's acknowledgement is
     due at `s + μ + α`, relays are asked past it and their answers are due after the slowest
     relay's span plus the target's; a period lasts what its probe needs;
   - a suspect is condemned when the probe that told it also misses and the member has since had
     an answer from another member; the confirmation curve, the multiplier and `fixed.rs`'s
     suspicion window are gone;
   - a member with nobody alive or suspected left probes the members it holds dead and tells
     them, so a live one refutes;
   - the dissemination budget is SWIM §4.1's bound in the membership size, and the relay count the
     fewest at least as reliable as the direct probe;
   - pings and ping-requests carry the detector's nonce, and acknowledgements are matched to it.

6. **A bounded view that forgets the dead** (`docs/timing.md` §2.7, `docs/research/swim.md`).
   slates' view, and this crate's until then, adopted every member gossip named and never forgot
   one. The view now holds at most the members the owner's placement says this node can know
   (`Detector::new`'s `members`) and refuses one more, typed (`membership::Full`); a dead member's
   record is kept for SWIM's dissemination budget of this member's longest periods past its adoption
   of the death, then forgotten with its estimator, coordinate, extensions and gossip report.

7. **The Vivaldi engine as the paper gives it** (`docs/timing.md` §2.7, `docs/research/swim.md`).
   slates' engine followed hyperscale's: the error folded in seconds into an estimate floored at
   0.05 as a relative one (50 ms, so on a LAN the confidence weights did nothing), eight dimensions,
   a separate height share, an adjustment term with a ±1 s clamp, and a 0.99 gravity. It is now
   Dabek's Fig. 3 in §5.4's height vectors, two dimensions and a height, `c_c` §4.1's and `c_e` one
   round's moving average; a received coordinate must have exactly the engine's dimensions, and one
   that is not a number is not learned.

## Tests

- 46 unit tests: slates' membership, gossip, codec and coordinate tests, the extension series and
  its bounds, the gossip queue's order, replacement and bound, and the measured timing's: nothing
  judged before the estimates exist, the deadline is `μ + α`, a silent member is suspected, told and
  condemned, an isolated member condemns nobody, an indirect answer spares, a refutation clears a
  pending condemnation, an extension buys one told probe, the allowance is `Σβ`, the dissemination
  budget is SWIM's bound and the relay count the fewest that suffice; and one for each failure the
  cluster runs found (`docs/benchmarks.md`, "The cluster test"): members holding one another dead
  heal, a lost measurement probe ends at its expected arrival, a refused reconfiguration leaves the
  verdict in force, a re-adopted suspicion keeps its told probes, and the detection bound does not
  shrink with the round.
- `tests/cluster.rs`: four real member processes run the detector over hyper-datagram on real
  UDP sockets, as the library configures it.
  - The supervisor starts them together, waits until every member judges every peer by a
    configured verdict, and SIGKILLs one.
  - Every survivor must hold it dead within the detection bound its own detector stated.
  - Suspicions and condemnations of live members, summed over the cluster, must stay within the
    configured detectors' allowance `Σβ`.
  - The counts over hundreds of runs on macOS and Linux are in `docs/benchmarks.md`.
