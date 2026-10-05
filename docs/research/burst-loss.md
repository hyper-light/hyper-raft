# Research: loss in bursts, in time

What hyper-sim's network needs to lose packets the way measured paths lose them, and what a
sender that repeats a packet learns from it. Sources read on 2026-10-05 at their primary copies;
numbers are quoted as printed, and every derived number shows its arithmetic.

## 1. The question

hyper-quic sends each handshake flight twice (`docs/research/quic-overhead.md` §4.1). With
independent loss at 5% each way, a copy is lost with its original 5% of the time, and the first
reply's p90 at 500 ms one way fell from 1,899 to 180 ms. A copy goes right behind its original:
microseconds apart on the wire. If losses come in bursts longer than that, the copy dies with the
original, and the copies buy nothing on exactly the paths that lose the most. Google's deployment
report says they do (IETF 99 MAPRG, "The QUIC Transport Protocol: Design and Internet-Scale
Deployment", slide 18: "Multiple packet losses within RTT common"; it removed QUIC's XOR FEC for
that reason).

## 2. The model: Gilbert–Elliott

- **Gilbert, "Capacity of a burst-noise channel"**, BSTJ 39(5), 1960, and **Elliott, "Estimates
  of error rates for codes on burst-noise channels"**, BSTJ 42(5), 1963: a channel with a good and
  a bad state, a Markov chain between them, and a loss probability in each.
- **Jiang and Schulzrinne, "Modeling of Packet Loss and Delay and Their Effect on Real-Time
  Multimedia Service Quality"**, NOSSDAV 2000 (Columbia's copy, read 2026-10-05), §2.2 states the
  form fitted to Internet traces: a packet is lost exactly in the bad state; `p` is "the probability
  that the next packet is lost, provided the previous one has arrived", `q` the opposite; "1 − q is
  the conditional loss probability (clp)"; the stationary loss rate (the ulp) is `π₁ = p/(p+q)`;
  loss runs are geometric, `p_k = (1 − q)^(k−1)·q`. Their worked example (trace 1): `p = 0.004`,
  `q = 0.851`, ulp 0.47%, clp 14.9%.

The chain in these fits steps once a packet, and every trace behind them sends at a fixed spacing
(below). A step is therefore a fixed time, and the fitted chain is a statement about time: given a
loss now, the chance of a loss `δ` later. A sender whose packets are not evenly spaced, such as a
handshake's flights, needs the chain in time.

**The chain in time.** A two-state continuous-time Markov chain with mean sojourns `G` (good) and
`B` (bad) has, after `δ`, `P(bad | bad) = π_b + π_g·e^(−δ/τ)` and `P(bad | good) = π_b·(1 −
e^(−δ/τ))`, where `π_b = B/(G+B)`, `π_g = 1 − π_b` and `1/τ = 1/G + 1/B`: the forward equation
`dP_b/dt = (1 − P_b)/G − P_b/B` has the solution `P_b(δ) = π_b + (P_b(0) − π_b)·e^(−δ/τ)`. With loss exactly in the bad state, `ulp = π_b` and, for a probe spacing `δ`,
`clp(δ) = ulp + (1 − ulp)·e^(−δ/τ)`, so a measured (ulp, clp) at a spacing `δ` gives

    τ = δ / ln((1 − ulp) / (clp − ulp)),   B = τ / (1 − ulp),   G = τ / ulp.

Embedded at a fixed spacing, this is exactly the packet-stepped Gilbert chain of Jiang and
Schulzrinne with `1 − p − q = e^(−δ/τ)`, so it reproduces every fit at its own spacing.

## 3. Measured traces

**Bolot, "End-to-End Packet Delay and Loss Behavior in the Internet"**, SIGCOMM 1993 (the
SIGCOMM'93 proceedings copy, read 2026-10-05). UDP probes every `δ` ms between INRIA and the
University of Maryland (Table 1, the route, July 1992), ten-minute runs. Table 3, ulp and clp by
spacing (the scan prints the 500 ms ulp as "0.97", unreadable beside its clp of 0.09; it is not
used):

| δ (ms) | 8 | 20 | 50 | 100 | 200 |
|---|---|---|---|---|---|
| ulp | 0.23 | 0.16 | 0.12 | 0.10 | 0.11 |
| clp | 0.60 | 0.42 | 0.27 | 0.18 | 0.18 |
| τ from §2 (ms) | 10.9 | 17.1 | 28.3 | 41.3 | 78.7 |

Bolot reads the correlation as the bottleneck buffer staying full across consecutive probes: the
shorter the spacing, the likelier the next probe finds it still full.

**Jiang and Schulzrinne** (above), Table 1: six traces of 36-byte UDP packets between Columbia,
GMD, UMass, UCSC and HP, 1997–2000:

| trace | path | spacing | ulp | clp | τ from §2 (ms) |
|---|---|---|---|---|---|
| 1 | CU–GMD | 30 ms | 0.47% | 14.9% | 15.5 |
| 2 | CU–UMass | 30 ms | 9% | 33% | 22.5 |
| 3 | UCSC–CU | 30 ms | 5.67% | 10.6% | 10.2 |
| 4 | UCSC–UMass | 30 ms | 2.82% | 44.1% | 35.0 |
| 5 | CU–UCSC | 30 ms | 0.63% | 14.7% | 15.3 |
| 6 | CU–HP | 10 ms | 0.096% | 31.3% | 8.6 |

Worked for trace 4: `(0.441 − 0.0282)/(1 − 0.0282) = 0.4248`, `τ = 30/ln(1/0.4248) = 30/0.856 =
35.0 ms`.

**What the traces agree on.** Every measurement has clp well above ulp at spacings of 8 to 30 ms,
so correlation at those spacings is the rule, not one path's quirk. Bolot's column shows it fading
over tens to a few hundred milliseconds, and not as one exponential: τ fitted at each spacing grows
with the spacing (10.9 ms at 8 ms, 78.7 ms at 200 ms), a heavier tail than one two-state chain
has. A single chain is fitted here at the spacing nearest a copy's, and the tail is kept as a
second condition.

**What none of them measures.** No trace spaces probes closer than 8 ms. A copy sent back to back
with its original follows it by a datagram's serialization time; the chain in time takes the
correlation there to its limit, `clp → 1` as `δ → 0`, which is Bolot's mechanism (a full drop-tail
buffer stays full for the microseconds between two datagrams). The packet-stepped chain cannot
say this: on an idle flow its next step is the copy, whatever the time between.

## 4. The conditions hyper-sim runs

Both at the geo harness's 5% mean loss, so they differ from the independent condition only in how
the losses fall (`B = τ/(1 − 0.05)`, `G = τ/0.05`):

- **Bursty, measured** (`τ` = 35.0 ms, Jiang and Schulzrinne's trace 4, the most correlated of the
  eleven at the 30 ms spacing nearest the handshake's packets): mean burst `B` = 36.8 ms, mean gap
  `G` = 700 ms.
- **Bursty, long tail** (`τ` = 78.7 ms, Bolot's 200 ms column, the longest correlation measured):
  `B` = 82.8 ms, `G` = 1,574 ms.

A copy `s` after its original is then lost with it with probability `0.05 + 0.95·e^(−s/τ)`: at
τ = 35 ms, 1.00 at 0 ms, 0.76 at 10 ms, 0.28 at 50 ms, 0.105 at 100 ms, 0.053 at 200 ms.

## 5. What a sender can do about it

- **Michel, De Coninck and Bonaventure, "QUIC-FEC: Bringing the benefits of Forward Erasure
  Correction to QUIC"**, IFIP Networking 2019 (arXiv 1904.11326, read 2026-10-05). §III.B: the XOR
  scheme "can only recover the loss of one Source Symbol. Experiments carried out by Google showed
  that this is insufficient on the Internet because losses can occur in bursts. Our implementation
  uses interleaving to recover from burst losses with the XOR FEC Scheme. Sending successive packets
  in different FEC Blocks enables the XOR FEC Schemes to better handle burst losses at the expense
  of delay." §V.C, with a Gilbert–Elliott loss model: FEC "performs badly when the r parameter of
  the Gilbert-Elliott model is low" (long bursts; r ≤ 12%). Separation in time is their answer to
  bursts, and its price is delay.
- **RFC 9265** (IRTF, Informational, 2022) §4: repair symbols go "within what the congestion window
  or calculated rate allows", and coding "should not hide congestion signals". It says nothing on
  when within the window a repair goes; spacing a copy is within its rules as long as the copy stays
  under the window, the pacer and the anti-amplification limit and the original's loss is still
  declared and answered.

**The spacing that minimises the expected delay.** A copy `s` behind its original costs `s` when
the original is lost and the copy arrives; when both are lost the flight waits for the probe
timeout, `C` (the space's PTO). Per lost original the expected added delay is

    f(s) = s·(1 − c(s)) + C·c(s),   c(s) = r + (1 − r)·e^(−s/τ),

and for `C ≫ s`, `f(s) ≈ s + C·c(s)`, whose minimum is at `f'(s) = 1 − (1 − r)·(C/τ)·e^(−s/τ) =
0`:

    s* = τ·ln((1 − r)·C/τ)      (s* = 0 when (1 − r)·C ≤ τ).

The sender knows `C` (its PTO for the space) and not `r`; taking `1 − r = 1` errs late by
`τ·ln(1/(1 − r))`, 1.8 ms at 5% and τ = 35 ms. At the geo harness's first PTO, `C` = 999 ms:
`s*` = 35·ln(999/35) = 117 ms at τ = 35 ms, and 78.7·ln(999/78.7) = 200 ms at the long tail. On a
LAN, where the PTO is a few milliseconds, `s*` is zero and copies go as they do now: a copy waits
only where a lost flight costs more than the burst lasts.

## 6. Measured, and what was built (2026-10-05)

The geo harness (`crates/hyper-quic/tests/geo.rs`, `print_the_burst_table`), 500 ms one way with
±100 ms reordering, a fresh dial with the large certificate then a resumed dial with its request in
0-RTT, seeds 1 to 32; the first reply above its floor of round trips, p90 / maximum, ms. Before is
`line` at 1a385a2 with only the burst model added; after is the spacing below and the 1-RTT hold.

| condition | no copies | copies back to back (before) | copies spaced (after) |
|---|---|---|---|
| independent 5%, fresh | 1,947 / 2,319 | 136 / 818 | 317 / 1,168 |
| independent 5%, resumed | 2,445 / 3,126 | 108 / 146 | 235 / 265 |
| bursts τ = 35.0 ms, fresh | 2,182 / 3,229 | **2,749** / 5,776 | **394** / 10,258 |
| bursts τ = 35.0 ms, resumed | 3,924 / 5,035 | 141 / 2,972 | 155 / 3,164 |
| bursts τ = 78.7 ms, fresh | 2,005 / 9,737 | **2,004** / 5,023 | **676** / 10,258 |
| bursts τ = 78.7 ms, resumed | 3,548 / 5,035 | 88 / 141 | 270 / 3,139 |
| bytes, all seeds (indep. / τ 35 / τ 78.7) | 1.00 / 1.00 / 0.99 MB | 1.68 / 1.74 / 1.70 MB | 1.49 / 1.52 / 1.54 MB |

**Copies died with their originals.** Under bursts at the same mean, the fresh dial's p90 with
back-to-back copies was 2,749 ms, worse than no copies (2,182 ms): the copies cost a datagram of the
window each and bought nothing where a burst took both.

**Built:**
- **A copy waits `s = τ·ln(C/τ)` behind its original** (§5), `C` the probe timer's interval before
  backoff, `τ` 35 ms by default (`TransportConfig::handshake_copy_burst`), one spacing for every
  space so a datagram's coalesced packets are copied together. At the first probe timeout (999 ms)
  `s` is 117.3 ms. A copy whose original is acknowledged or declared lost first is never sent, which
  is why spaced copies cost fewer bytes (+48% over no copies under independent loss, against +67%).
  The copy stays under the window, the pacer and the anti-amplification limit, and a lost
  original is declared lost and answered as before.
- **1-RTT packets that reach a server before the client's Finished are held** until the handshake
  completes (RFC 9001 §5.7: "Received packets protected with 1-RTT keys MAY be stored and later
  decrypted and used once the handshake is complete"), within the same bound as the held Handshake
  and 0-RTT packets. Spacing the copies made a request's copy overtake the Finished's copy under
  reordering; upstream discarded the request and it went again after a probe timeout.

**Kept τ at 35 ms, not 78.7 ms.** The longer correlation spaces copies 200 ms and did better on its
own condition's fresh p90 (429 against 676 ms) and on the bursty maxima, and worse on the 35 ms
condition's resumed p90 (290 against 155 ms) and under independent loss (933 against 317 ms fresh).
35 ms is the fit at the spacing nearest a handshake's packets.

**The price under independent loss.** Where losses are independent, a copy needs no spacing, and the
117 ms is paid on every recovered loss: the fresh p90 rose from 136 to 317 ms and the resumed from
108 to 235 ms. Every trace in §3 shows correlation at short spacings; the independent condition is
the model none of them measured.

**What is left.** The fresh maximum, 10.3 s, is one seed (14) where a burst took the server's whole
first flight, ten datagrams the window and the anti-amplification limit had let go at once; the
copies then wait for the window, and the flight is recovered by probes of two datagrams with backoff.
No copy can be sent past the window or the limit (RFC 9002 §7, RFC 9000 §8.1). The resumed maximum,
3.2 s, is a seed whose 0-RTT request and its copy both met bursts.

## 7. τ per path, learned from the connection's own losses (2026-10-05)

The default τ of 35 ms costs a path whose losses are independent (§6). This section decides how a
sender learns its path's τ, from what evidence, and how few losses suffice.

**What a sender sees.** RFC 9002 declares each lost packet with its send time. For each lost
ack-eliciting packet, take its *neighbour*: the next ack-eliciting packet its space sent, `gap`
later. The neighbour's fate, lost or acknowledged, is one Bernoulli sample at `gap`. Under the
chain of §2 it is lost with probability `r + (1 − r)·e^(−gap/τ)`; under independent loss, with
`r`.

**Three pitfalls, each measured on the geo harness before it was handled:**
- *Spurious loss.* RFC 9002 declares a packet lost on reordering as well as on loss (§6.1). At
  ±100 ms of jitter, most early declarations in the bursty runs were reordered packets whose
  neighbours had arrived, and the first estimator took bursty paths for independent ones. The fix:
  a packet declared lost counts only once a packet of the same space, sent more than the delay's
  variation after it, is acknowledged while it is not. That is RACK's "a later send was delivered"
  (RFC 8985 §6.2), with the window set to four mean deviations (RFC 6298 §2's `K`). One
  acknowledged after its declaration counts as delivered.
- *Unacknowledged but delivered.* Packets of the handshake's flights can arrive and never be
  acknowledged: 0-RTT refused, keys not yet held or already discarded (RFC 9001 §4.6.2, §4.9).
  More generally, "QUIC does not guarantee receipt of an acknowledgment for every packet that the
  receiver processes" (RFC 9000 §13.2.3). Only packets sent after the handshake's flights count, and
  only acknowledgements of the same space settle a loss.
- *A model that is too sure.* The chain takes the neighbour's loss probability to 1 as the gap
  closes, so one delivered neighbour beside a counted loss refutes bursts outright: one such
  anomaly in 16 seeds flipped a bursty path to "independent". No trace measured 1. The highest
  conditional loss measured at the shortest spacing is Bolot's 0.60 at 8 ms (Table 3), and the
  model's probability is capped there.

**The decision: Wald's sequential probability ratio test** (Wald, "Sequential Tests of
Statistical Hypotheses", Ann. Math. Statist. 16(2), 1945). Three hypotheses are weighed:
independent loss, τ = 35.0 ms and τ = 78.7 ms. Each neighbour adds its log-likelihood under each,
with `r` taken by Laplace's rule of succession, `(lost + 1)/(fates + 2)`. A hypothesis replaces the
configured 35 ms once it is `(1 − β)/α = 19` times as likely, with α = β = 0.05. Until then the
configuration stands.

**How few losses.**
- At 5% loss, a neighbour sent with its lost packet (gap ≈ 0) and delivered weighs
  `log2((1 − r)/(1 − 0.6)) ≈ 1.25` bits toward independence. Wald's ratio of 19 is 4.25 bits, so
  four such pairs decide (`four_neighbours_sent_with_lost_packets_and_delivered_are_independent_loss`).
- At a 100 ms gap the burst model's probability is `0.05 + 0.95·e^(−100/35) ≈ 0.10`, a pair weighs
  `log2(0.95/0.90) ≈ 0.08` bits, and some fifty pairs are needed.
- Neighbours lost together favour bursts, which the default already assumes.

**Bolot's spacing dependence.** τ fitted at one spacing grows with the spacing (§3: 10.9 ms at
8 ms, 78.7 ms at 200 ms). Evidence gathered at short gaps therefore says little about correlation
at the copy's 117 ms, and it is never used to *shorten* the spacing below the default's bursts:
the only hypotheses are independence, the default and the longest correlation measured. Short-gap
evidence decides independence well. Telling 35 ms from 78.7 ms needs evidence at the long gaps,
which a connection rarely has; in every run measured it left the default standing.

**Memory.** An endpoint keeps the evidence per remote IP address (`LossMemory`), separately from
Careful Resume's `CongestionMemory`, with the same bound and lifetime (256 addresses, one hour;
`EndpointConfig::loss_memory`). Every connection to an address starts from the memory and adds to
it, so evidence accumulates across connections. Each endpoint learns its own sending direction,
which is the direction its copies travel.

**The liveness stream.** hyper-liveness's per-pair heartbeats come at a fixed interval, Bolot's own
probe design, but they do not fit the boundary yet. A receiver sees gaps in the heartbeats'
sequence numbers, and a gap is either a loss or a slot the sender skipped while it was behind
(`PairReport::skipped`, known only at the sender). A heartbeat would have to carry its sender's
skipped count before the receiver could count losses. Even then, hyper-quic would take the
resulting fit as a value its owner passes in, never a dependency. Not built: the change is to
hyper-liveness's wire format, outside this step, and §8 shows the QUIC evidence alone suffices
once a path has carried traffic.

## 8. Measured (2026-10-05)

`print_the_burst_table` over 128 seeds (32 left the p90 of a cliff-shaped distribution to chance).
The "two dials before" rows run two dials of 256 KiB each way first, with Careful Resume off so
only the copies' spacing differs between rows. First reply over its floor, p90 of the fresh dial /
p90 of the resumed dial, ms:

| condition | back to back | spaced 35 ms | spaced as learned |
|---|---|---|---|
| independent, no dial before: fresh / resumed | 221 / 130 | 998 / 228 | 998 / 228 (nothing to learn from) |
| independent, two dials before: fresh / resumed | 734 / 150 | 986 / 266 | 1,032 / **122** |
| bursts 35 ms, two dials before: fresh / resumed | 1,917 / 165 | 3,200 / 235 | 3,200 / 235 |
| bursts 78.7 ms, two dials before: fresh / resumed | 2,891 / 2,628 | 1,089 / 272 | 1,089 / 272 |

- **Independent loss, resumed dial:** learning takes the p90 from 266 to 122 ms, back to the
  back-to-back copies' 150 ms. Both endpoints' evidence settled on independence in 12 of the 16
  seeds examined, and one of the two in the other four.
- **Independent loss, fresh dial:** no gain at p90. After the learning dials, every variant's fresh
  dial (734 to 1,032 ms) sits at the one-probe-timeout cliff in a tenth of the seeds, so the cliff
  sets the p90, not the spacing.
- **Bursts:** the learned rows equal the 35 ms rows exactly. The evidence never left the default,
  so learning costs nothing where losses come in bursts.
- **Without a dial before**, nothing is learned, since handshake packets are not counted, and the
  rows equal the 35 ms ones.
