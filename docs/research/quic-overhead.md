# Research: what hyper-quic adds above the round trips at 500 ms one way

Source notes for `crates/hyper-quic/VENDORED.md` §11 and §12, `crates/hyper-tls/VENDORED.md` §7 and
`docs/transport.md` §4f. The owner's goal is to cut what the stack adds above the physical floor of
round trips by 10x at 500 ms one way, giving up no correctness. Three costs were left open by the
last round (`docs/benchmarks.md`, "hyper-quic at 500 ms one way"): the initial window's stall at the
start of a burst, probe timeouts under loss, and 0-RTT falling back on the fourth dial. Each entry says
what the source establishes and how it was read: the RFC text at rfc-editor.org, or a stack's source
at the revision named, on 2026-10-04.

## 1. The initial window and carrying congestion state between connections

### What the stall is

**RFC 9002 §7.2.** "Endpoints SHOULD use an initial congestion window of ten times the maximum
datagram size (max_datagram_size), while limiting the window to the larger of 14,720 bytes or twice
the maximum datagram size." At QUIC's 1,200-byte minimum that is 12,000 bytes. **§7.8**: a window
that is not fully used "SHOULD NOT be increased in either slow start or congestion avoidance", so a
connection that has carried only small exchanges still has its initial window when a burst begins.
The open loop at 100 requests a second over a 1 s round trip keeps about 15 kB in flight; the
requests past 12,000 bytes wait for the first acknowledgements, up to about 200 ms.

Nothing in the standards lets a sender exceed the initial window on a path it has no measurement of.
The options below all carry a measurement from an earlier connection on the same path.

### RFC 9959, Careful Resume (Standards Track, May 2026)

Read at https://www.rfc-editor.org/rfc/rfc9959.html.

- **What is saved (§3.1).** "The currently utilised capacity for the connection is measured as the
  volume of bytes sent during an RTT and is recorded in the saved_cwnd. This could be computed by
  measuring the volume of data acknowledged in one RTT." "The minimum RTT at the time of observation
  is saved as the saved_rtt." "A sender MUST NOT retain more than one set of CC parameters for a
  Remote Endpoint, but the set of CC parameters SHOULD be updated (or replaced) after a later
  observation." "If the measured CWND is less than four times the IW, the sender can choose to not
  save the CC parameters." §4.1 adds, for a measurement taken in slow start, that the saved_cwnd
  could be "the validated pipe size (i.e., CWND / 2)"; bytes acknowledged in a round trip are
  delivered bytes, already validated.
- **Remote Endpoint (§2.2).** An identifier of the sending interface and one of the destination,
  e.g. the destination IP address; optionally the DSCP.
- **Reconnaissance (§3.2).** The connection starts at the IW under normal congestion control. It
  stops using Careful Resume on congestion, when "another connection has already started to use the
  saved_cwnd", on a different Remote Endpoint or a path change, when the Lifetime is exceeded, or
  when the minimum RTT seen is at most half the saved_rtt. §4.2.1: "A current RTT that is more than
  ten times the saved_rtt is indicative of a path change." "When the sender has received an ACK that
  acknowledges all the initial data (usually the IW) without reported congestion, it MAY then enter
  the Unvalidated Phase", and "this transition MAY be deferred to the time at which more data is
  sent than would have been normally permitted by the CC algorithm". §4.2: the sender limits "the
  initial data, sent in the first RTT of transmitted data, to no more than the IW".
- **Unvalidated (§3.3).** "jump_cwnd MUST be no more than half of the saved_cwnd. Hence, jump_cwnd
  is less than or equal to Min(max_jump,(saved_cwnd/2)). CWND = jump_cwnd." PipeSize starts at the
  flight size and grows by each ACK's newly acknowledged bytes. "All packets sent in the Unvalidated
  Phase MUST use pacing based on the current RTT" (§4.3: ITT = current RTT × MPS / jump_cwnd). The
  phase ends when the flight size reaches the CWND, when an ACK covers the first packet sent in it,
  or after more than one RTT; then, if the flight size is below the IW or at most the PipeSize, the
  CWND becomes the PipeSize (not below the IW without congestion) and Careful Resume ends, otherwise
  the CWND becomes the flight size and the Validating Phase follows. Congestion enters Safe Retreat.
- **Validating (§3.4).** PipeSize grows with each ACK; the CWND follows normal congestion control;
  Careful Resume ends when the last packet sent in the Unvalidated Phase is acknowledged; congestion
  enters Safe Retreat.
- **Safe Retreat (§3.5).** The saved parameters are deleted; "The CWND MUST be reduced to no more
  than (PipeSize/2)" (in QUIC not below two packets); the CWND is not increased; when the last
  Unvalidated packet is acknowledged, "ssthresh MUST be set to no larger than the most recently
  measured PipeSize * Beta", Beta 0.5 by default.
- **Why half (§1.5, §4.3).** The jump is "restricted to a fraction (1/2) of the saved_cwnd, to avoid
  starving other flows that may have started or increased their capacity after the last capacity
  measurement".
- **Lifetime (§2.4, §4.3.1).** Configurable; minutes on dynamic paths, hours on stable ones, with
  RFC 7661's five-minute bound for non-validated periods noted as one Careful Resume may exceed
  because of its own safety checks.
- **Evidence (§1.4).** A geostationary-satellite example: a 5.3 MB transfer in 9 s with standard
  congestion control and 4 s with Careful Resume; the RFC's analysis references [CR25] and
  IETF 111's MAPRG presentation. The University of Aberdeen's analysis (aura.abdn.ac.uk, "Analysis
  of Careful Resumption of Internet Congestion Control from Retained Path State") was run on a
  Careful Resume implementation in Cloudflare's quiche, in the lab and over LEO satellite links.

### RFC 9040, TCP Control Block Interdependence

Read at https://www.rfc-editor.org/rfc/rfc9040.html. §6.1's temporal-sharing table initialises a
new connection's `sendcwnd`, RTT and RTTVAR from the cached `old_sendcwnd`, `old_RTT` and
`old_RTTVAR`; §6.2 merges the cache at CLOSE by a function it leaves open. §7 shares an ensemble's
cwnd among concurrent connections. §8 names the risk of cwnd sharing (joining connections take
their own windows while an existing one keeps its converged window) and that "sharing ssthresh
between short flows can deteriorate the performance of individual connections". It states no
path check, no fraction and no retreat on loss. RFC 9959 §4.3 rules out exactly its cwnd reuse: "a
sender must not directly use the previous saved_cwnd to directly initialise a new flow causing it
to resume sending at the same rate." The RTT half of temporal sharing was tried here in the last
round and measured worse (`docs/benchmarks.md`: the resumed first reply's p90 rose from 3,899 to
4,146 ms).

### What the major stacks do

- **Chromium/Google QUIC, bandwidth resumption** (quiche.googlesource.com,
  `quic/core/http/quic_server_session_base.cc`). Server-side, opt-in by the client's connection
  options `kBWRE`/`kBWMX`. The server puts `CachedNetworkParameters` (its bandwidth estimate and
  minimum RTT) in the address token, and on a resumed connection calls `ResumeConnectionState` only
  if the cached serving region matches and the estimate is at most an hour old
  (`seconds_since_estimate <= kNumSecondsPerHour`). Updates are sent on a 50% change, rate-limited
  by time and packet count. It resumes the full estimate, with no half and no retreat phase.
- **msquic** (github.com/microsoft/msquic at 900ef64, `docs/api/QUIC_SETTINGS.md`).
  `InitialWindowPackets`, default 10, set by the application; no congestion state carried between
  connections was found.
- **s2n-quic** (github.com/aws/s2n-quic at b0a0920, `quic/s2n-quic-core/src/recovery/cubic.rs`).
  The initial window is RFC 9002's, `min(10 × max_datagram_size, max(14720, 2 ×
  max_datagram_size))`, unless the application sets `initial_congestion_window`; no congestion
  state carried between connections was found.
- **Cloudflare quiche** (github.com/cloudflare/quiche at 3fc9bc1). The upstream tree has no
  Careful Resume (no match for it in the code); the Aberdeen implementation above was on a branch.

### The decision

RFC 9959 is the better-evidenced design: a Standards Track RFC with measured transfers and an
implementation on a production stack behind it, and the only one of the three with a path check,
a fraction of the measurement and a retreat on loss. RFC 9040's cwnd reuse is what RFC 9959 rules
out, and Chromium's is the full estimate with none of those guards. hyper-quic implements RFC 9959
in both roles, since both send: each endpoint remembers its own measurement per remote IP address.

What it does not change: a connection with no measurement of its path, like the open loop's kept
connection after four dials of one request each, still starts at the IW, because no standard lets
it do otherwise. The stall there is the network's safety rule, not the stack's overhead.

## 2. Probe timeouts and what a probe carries

### The timer

**RFC 9002 §6.2.1.** `PTO = smoothed_rtt + max(4*rttvar, kGranularity) + max_ack_delay`.
**§5.3**: on the first sample, `smoothed_rtt = latest_rtt` and `rttvar = latest_rtt / 2`, so the
first PTO after it is three smoothed RTTs: about 3 s at 500 ms one way, falling by a quarter of the
variation a sample. **§6.2**: "A PTO timer expiration event does not indicate packet loss and MUST
NOT cause prior unacknowledged packets to be marked as lost"; the window is not reduced. The 4 is
RFC 6298's RTO, whose expiry collapses TCP's window; a QUIC probe that fires early costs the one or
two packets it sends.

- **RFC 8985 (RACK-TLP) §7.2.** "the default PTO interval is 2*SRTT. By that time, it is prudent to
  declare that an ACK is overdue since under normal circumstances, i.e., no losses, an ACK
  typically arrives in one SRTT." With one segment in flight it MAY add the peer's delayed-ACK
  time; the probe is capped at the RTO; with no RTT sample the PTO SHOULD be 1 s. §7.3: a probe sends
  new data if any, else retransmits the highest-sequence segment sent. §9.3 compares recovery of a
  tail loss: "2*RTT + 4*RTT" with RACK-TLP against "RTO + 4*RTT" without.
- **Linux** (`net/ipv4/tcp_output.c`, `tcp_schedule_loss_probe`, torvalds/linux master). The probe
  timeout is `tp->srtt_us >> 2`, two smoothed RTTs (srtt_us holds eight times the RTT), plus the
  minimum RTO with one packet out, and is capped at the RTO's remaining time; with no sample,
  `TCP_TIMEOUT_INIT`. `tcp_send_loss_probe` retransmits `skb_rb_last(&sk->tcp_rtx_queue)`.
- **Chromium/Google QUIC** (quiche.googlesource.com, `quic/core/quic_sent_packet_manager.cc` and
  `quic_constants.h`). `GetProbeTimeoutDelay` is `smoothed_rtt + max(kPtoRttvarMultiplier ×
  mean_deviation, kAlarmGranularity) + peer_max_ack_delay`, with `kPtoRttvarMultiplier = 2` ("The
  multiplier of RTT variation when calculating PTO timeout"); before any sample, `3 × initial_rtt`
  (`kPtoMultiplierWithoutRttSamples`).
- **msquic** (900ef64, `src/core/loss_detection.c`, `QuicLossDetectionComputeProbeTimeout`):
  `SmoothedRtt + 4 * RttVariance + MaxAckDelay`, RFC 9002's.
- **Cloudflare quiche** (3fc9bc1, `quiche/src/recovery/congestion/recovery.rs`, `pto()`):
  `rtt + max(rttvar * 4, GRANULARITY)`, RFC 9002's.

### What the probe carries

**RFC 9002 §6.2.4.** "When there is no data to send, the sender SHOULD send a PING or other
ack-eliciting frame in a single packet"; "a sender SHOULD send ack-eliciting packets from other
packet number spaces with in-flight data, coalescing packets if possible"; "the sender MAY
retransmit unacknowledged data". All three stacks that were read retransmit the oldest packets'
data, STREAM frames included:

- **Chromium**: `MaybeSendProbePacket` and `RetransmitDataOfSpaceIfAny` retransmit the oldest
  outstanding packets with retransmittable frames.
- **msquic** (`QuicLossDetectionScheduleProbe`): two probe packets ("The spec says that 1 probe
  packet is a MUST but 2 is a MAY. Based on GQUIC's previous experience, we go with 2."), new
  stream data first, then "retransmit the data in the oldest packets", then a PING.
- **Cloudflare quiche** (`on_loss_detection_timeout`): "Retransmit the frames from the oldest sent
  packets on PTO. However the packets are not actually declared lost (so there is no effect to
  congestion control), we just reschedule the data they carried", one packet's frames per probe.

quinn-proto's probe took only the oldest packet's control frames, never its STREAM frames, so a
lost reply went again only once the probe's own acknowledgement declared it lost: a round trip
after a probe that came three smoothed RTTs late.

### The decision

- **What a probe carries**: the oldest in-flight packet's frames, STREAM frames included, as
  Chromium, msquic and quiche do and RFC 9002 §6.2.4 permits; and a PTO probes the Data space too
  when it has keys and data in flight, which §6.2.4 asks for ("other packet number spaces with
  in-flight data"), so a lost Finished and the request coalesced with it go again together.
- **When**: the variation's weight in the probe timer is 2, Chromium's production
  constant; at the first sample this is RACK-TLP's 2·SRTT. The timer before any sample stays RFC
  9002's (999 ms from kInitialRtt), which the last round measured as better than seeding it. Every
  period RFC 9002 and RFC 9000 define as a multiple of the PTO keeps the RFC's weight of 4:
  persistent congestion (§7.6.1), key discard and draining (RFC 9000 §10.2), so no congestion
  response and no state's lifetime is shortened.

## 3. Session tickets for dials that end at one round trip

- **RFC 8446 §4.6.1.** "Note: Although the resumption master secret depends on the client's second
  flight, a server which does not request client authentication MAY compute the remainder of the
  transcript independently and then send a NewSessionTicket immediately upon sending its Finished
  rather than waiting for the client Finished."
- **RFC 8446 Appendix C.4.** "Clients SHOULD NOT reuse a ticket for multiple connections. Reuse of a
  ticket allows passive observers to correlate different connections." So a client spends a ticket
  per resumed dial.
- **RFC 9001 §4.** NewSessionTicket goes in CRYPTO frames in 1-RTT packets; **§8.3**: a QUIC client
  sends no EndOfEarlyData, so on a handshake without client authentication the client's second flight
  is its Finished alone, which the server can compute.
- **rustls 0.23.45** (vendored as hyper-tls): `send_tls13_tickets` defaults to 2, sent in
  `ExpectFinished` after the client's Finished is verified.
- **msquic** (`QUIC_SETTINGS.md`, `ServerResumptionLevel`): "The server app must call
  ConnectionSendResumptionTicket to send a resumption ticket to the client", so when tickets go is
  the application's choice.

A resumed dial that ends at its first reply, one round trip in, closes half a round trip before
tickets sent after the client's Finished arrive. More tickets per full handshake only defer the
fall-back: every such dial spends one and gets none, so with N tickets dial N + 2 falls back. Sending
the tickets with the server's Finished on a handshake that does not authenticate the client (every
resumed handshake, since a PSK handshake carries no CertificateRequest, and a full handshake whose
server asks for no certificate) puts them in the same flight as the reply, so each such dial
replaces the ticket it spent. A handshake that requests a client certificate keeps sending them
after the client's Finished, as the RFC's condition requires.

## 4. Round three: what is left, and what closes it (2026-10-05)

Round two left the tenfold cut unmet in two places (`docs/benchmarks.md`, "probe timeouts, tickets
and Careful Resume"): the lossy path's first reply, 2.4x at p90 fresh (4,650 to 1,899 ms) and 3.3x
resumed (3,899 to 1,177 ms); and the burst on a path no connection has measured, 1.2x (291 to
241 ms). The sources below were read on 2026-10-04 and 2026-10-05: RFC text from rfc-editor.org
(RFC 9000 from its plain-text form, quoted as written), papers from their publishers' pages.

### 4.1 The lossy path's first reply

**Where the time goes.** Per seed (the harness's 32 seeds, 5% loss each way, ±100 ms reordering,
a fresh dial with a large certificate, then a resumed dial with its request in 0-RTT), every seed
above 300 ms of overhead lost a packet of the handshake's flights: the client's first Initial (a
probe timeout of 999 ms from kInitialRtt, RFC 9002 §6.2.2, since no RTT sample exists yet), the
server's second flight, the client's Finished and the request beside it, the server's reply, or the
resumed dial's ClientHello datagram with its 0-RTT request. A lost packet is known lost a round trip
after it was sent at the soonest, and its copy then takes the one-way delay: at 500 ms one way,
each such loss costs a second or more whatever the timer.

**RACK-TLP (RFC 8985).** §7.2's probe timeout is 2·SRTT, already round two's at the first sample
(the variation weighed by 2, Chromium's constant). §6.2's reordering window starts at min_RTT/4 and
grows on DSACK, bounded by SRTT; RFC 9002 §6.1.2 keeps a time threshold of 9/8 and notes that
"Algorithms that increase the reordering threshold after spuriously detecting losses, such as RACK,
have proven useful in TCP". Adapting the window acts on spurious loss declarations; in every slow
seed the delay came from a real loss on the critical path, so it would not move them, and it was
not built.

**Seeding the variation from a previous connection.** RFC 9002 §6.2.2 lets a resumed connection
take "the previous connection's final smoothed RTT value as the resumed connection's initial RTT";
round one tried the smoothed RTT and variation together and measured it worse (the first probe
waits about 1.3 s instead of 999 ms). Measured again here, both ways, on the resumed dial:

- the variation alone, taken at the first sample in place of half the sample (§5.3): p90 of the
  first reply 2,100 ms against 1,177 ms;
- smoothed RTT and variation before any sample: p90 1,201 ms, maximum 3,093 ms against 3,890 ms;
- with the handshake's flights sent twice (below), either way moved the median (11 or 5 ms against
  32 ms) and not the tail (p90 110 and 105 ms against 103 ms, maximum 159 and 150 against 120).

The data does not support it, and it was not kept.

**Forward erasure correction.** RFC 9265 (IRTF, Informational, July 2022; read from its plain text)
states the condition a coding scheme must meet: "FEC coding mechanisms should not hide congestion
signals" (abstract). Its §4, FEC within the transport, is the design taken here: "The repair
symbols are sent within what the congestion window or calculated rate allows", and "For small
files, sending repair symbols when there is no more data to transmit could help to reduce the
transfer time. Sending repair symbols can avoid the silence period between the transmission of the
last packet in the send buffer and 1) firing a retransmission of lost packets or 2) the
transmission of new packets." Michel, De Coninck and Bonaventure ("QUIC-FEC: Bringing the
benefits of Forward Erasure Correction to QUIC", IFIP Networking 2019) measured FEC in QUIC to cost
completion time on long transfers or at low loss and delay, and to cut it "drastically" with "high
packet loss rates and long delays or smaller files", "by avoiding costly retransmission timeouts";
they kept the congestion signal by "distinguishing the packets that have been received from the
packets that have been recovered". Google's QUIC removed its XOR FEC (IETF 99 MAPRG, "The QUIC Transport Protocol: Design and
Internet-Scale Deployment", slide 18: "Conclusion: Benefits not worth the pain", "Multiple packet
losses within RTT common", "Gains really at tail, where aggressive TLP wins"). At a 1 s round trip an aggressive probe still costs two
seconds, and the handshake's flights are each a packet or a few: for a flight that fits a packet,
the code that recovers any single loss is a repetition.

Emulated on the harness first, each datagram duplicated on the wire with an independent loss draw,
the resumed dial's p90 fell from 1,177 to 84 ms (the emulation's copies are free of the window and
the anti-amplification limit, so it overstates the fresh dial). Built as copies of frames, in
packets of their own numbers:

- **What is copied: the handshake's flights.** Every Initial and Handshake packet; the 0-RTT and
  0.5-RTT data sent while the handshake runs; and on a connection without 0-RTT, the data beside the
  client's Finished, the application's first data, which could go no sooner. On real sockets the
  forms measured cost the burst that starts with a resumed dial's reply (worst latency, 242 ms with
  no copies): copying every packet until the handshake's confirmation, 680 ms (the client confirms a
  round trip into the burst, and its copies took the initial window); copying the data beside the
  Finished on a resumed connection too, 273 ms (the burst's first requests ride with it); the form
  kept, 252 ms, the copy of the Finished itself.
- **When.** Once a space has nothing new to send, its streams' data included in the Data space, so
  a copy never goes before new data, and under
  the congestion window, pacing and the anti-amplification limit like any packet: every lossy seed
  keeps the limit (`duplicate_initials_are_acknowledged_once_and_grow_the_allowance`).
- **The congestion signal.** A lost original is still declared lost and answered (RFC 9265); the
  copy's own number is acknowledged. Stream data acknowledged through one copy is never sent again
  for the other's loss: the send buffer drops acknowledged ranges from what it retransmits.
- **What the simulation does not show.** Its losses are independent; a copy goes right behind its
  original, and a burst that loses both, which Google's measurement found common, costs what it cost
  before.
- **Measured alternatives.** Copying every later flight once the connection has declared a loss
  (loss-adaptive, as Michel et al. recommend adapting to the path) took the fresh p90 to 107 ms
  where the handshake's flights alone give 180 ms, both within the tenfold cut; it doubles every
  small message on a lossy connection for the rest of its life, and was not taken.

**A defect the copies uncovered.** A server dropped a 0-RTT packet that arrived when the connection
existed but the ClientHello was not yet whole ("dropping unexpected 0-RTT packet"): a ClientHello in
two datagrams whose first arrived, then a datagram of 0-RTT data. RFC 9001 §4.1.4: an endpoint
"SHOULD buffer received packets if they might be processed using keys that are not yet available";
§5.7: a server "MAY retain these packets for later decryption in anticipation of receiving a
ClientHello". The server now holds them with its other undecryptable packets, within the same
bound, and decrypts them when the 0-RTT keys come (or drops them once the ClientHello is read
without 0-RTT).

### 4.2 The burst on a path no connection has measured

**What binds.** RFC 9002 §7.2: "Endpoints SHOULD use an initial congestion window of ten times the
maximum datagram size", at most 14,720 bytes, 12,000 at QUIC's 1,200-byte minimum; §7.7: "Senders
SHOULD limit bursts to the initial congestion window". The open loop offers 100 requests a second of
131-byte packets over a 1 s round trip, 13.1 kB a round trip: the last 1.1 kB of the first round
trip wait for its acknowledgements, about 240 ms at worst.

- **Pacing the initial window over the first round trip** spreads what the window allows; it adds
  nothing to it, and the open loop's requests are already spread by the application.
- **Careful Resume** (RFC 9959 §3.1) needs a measurement of the path; the kept connection's earlier
  dials each carried one request, below the four initial windows §3.1 asks for.
- **A larger initial window.** RFC 6928 (Experimental) set TCP's at ten segments; the only proposal
  to go past it on an unmeasured path, Allman's "Removing TCP's Initial Congestion Window"
  (draft-allman-tcpm-no-initwin-00, an individual draft, expired, with no IETF standing), would let a
  sender choose any window it paces evenly over the first round trip. No standards-track document
  permits more than the initial window on a path without a measurement.
- **A larger datagram.** The initial window is ten datagrams: RFC 9002 §7.2, "If the maximum
  datagram size changes during the connection, the initial congestion window SHOULD be recalculated
  with the new size"; at 1,452 bytes it is 14,520, past the open loop's 13.1 kB. RFC 9000 §14.2:
  "QUIC implementations that implement any kind of PMTU discovery therefore SHOULD maintain a maximum
  datagram size for each combination of local and remote IP addresses"; §14.3.1, a sender "can
  therefore enter the DPLPMTUD BASE state ... when the QUIC connection handshake has been completed";
  RFC 8899 §3 (item 9), the PMTU "MAY also be stored with the corresponding entry associated with the
  destination ... and used by other PL instances". hyper-quic's controllers did not recalculate; they
  now do, while the window is still the initial one and no congestion has been met. Measured with
  path MTU discovery on the bench (the deployed `hyper-transport` endpoint enables it; the bench did
  not) and the discovered size kept per remote address: the dials close before the binary search
  passes 1,326 bytes, the probes take window during the burst, and the worst rose to 352 ms. That
  part was not kept.

**A defect behind a second stall.** RFC 9002 §7.8 lets the window grow only while it is used:
"When bytes in flight is smaller than the congestion window and sending is not pacing limited, the
congestion window is underutilized". hyper-quic (as upstream quinn) judged each acknowledgement by
its last transmission alone: a sender whose window filled, then sent what it held as
acknowledgements freed room and went idle until its next request, took every later acknowledgement
of that round trip for one of an unused window. The window grew 1.8 kB in the round trip after the
stall where slow start grows it by what is acknowledged, and the stall came again a round trip later,
and again. Linux keeps a window limited for the data in flight when the limit was met
(`tcp_cwnd_validate` in `net/ipv4/tcp_output.c` sets `is_cwnd_limited` until `snd_una` passes
`max_packets_seq`); hyper-quic now counts the window used from a block until a packet sent after it
is acknowledged.

**What is left, and why.** The first round trip of a burst on a connection with no measurement of
its path: about 240 ms at worst for an offered 13.1 kB against 12,000 bytes. Closing it tenfold asks
for about 13 kB in that round trip, which RFC 9002 §7.2 and §7.7 do not allow without a measurement
or a larger datagram size, and no standards-track mechanism provides either before the burst.

## 5. Round four: measuring the path before its first burst (2026-10-05)

Round three left the burst on a path no connection has measured at 252 ms worst (§4.2). Every
standards-track way past the initial window carries a measurement of the path (§1), and a node's
connections to its peers rarely make one: their exchanges are small, and RFC 9959 §3.1 keeps no
measurement below four initial windows delivered in a round trip. The sources below were read on
2026-10-05: RFC 9959 at https://www.rfc-editor.org/rfc/rfc9959.txt, draft-ietf-ccwg-bbr-06 at the
IETF datatracker.

### 5.1 What RFC 9959 asks of a measurement

- **§3.1**: "If the measured CWND is less than four times the IW, the sender can choose to not save
  the CC parameters."
- **§4.1**: "It is inappropriate to use an overshoot in the CWND as a basis for estimating the
  capacity", and "When the sender is rate limited or in the RTT following a burst of transmission, a
  sender typically transmits less data than allowed by the CWND. Such observations could be
  discounted when estimating the saved_cwnd".
- Nothing in it limits what traffic the measurement is taken from. A measurement is what the path
  delivered in a round trip while the sender filled its window, which is what slow start from the
  initial window does with any ack-eliciting packets.

**The packets.** RFC 9000 §14.4 already sends packets for no data of the application's own: PMTU
probes, "PING and PADDING frames" (§19.1, §19.2), ack-eliciting and counted in flight (RFC 9002
§2: a packet is in flight when "ack-eliciting or contain[ing] a PADDING frame"). The warm-up's
packets are the same, a PING padded to the path's MTU, sent under the congestion window and the
pacer like any packet; one lost is congestion, answered as any loss is, and ends the warm-up.

### 5.2 The design

- **When.** A connection whose endpoint holds no measurement for its remote IP address, and that no
  other connection to the remote is measuring, warms its path up once the handshake is confirmed
  (RFC 9001 §4.1.2; before it a server may drop 1-RTT packets, §5.7), and only while idle in both
  directions: nothing waiting in its streams (one with data waiting carries a transfer that measures
  the path itself), and no ack-eliciting packet received in the last smoothed round trip (an
  incoming flight's acknowledgements share the path the warm-up would load; measured, a mebibyte
  reply at 500 ms one way came 2.8 ms later when the receiver warmed up beside it).
- **How far.** Until four initial windows are acknowledged in one round trip, §3.1's floor: the
  least measurement Careful Resume keeps. The measurement goes to the endpoint at once, so a
  connection made while this one is open resumes from it.
- **Its bound.** Four times the target, sixteen initial windows: slow start doubles the window a
  round trip and a round trip's count may straddle two of the sender's rounds, so the target is met
  for certain once a window of twice it has been sent, and the rounds before carried less than that
  again (a geometric sum). A warm-up that meets congestion or spends its budget is recorded, and no
  connection to the remote warms up again until Careful Resume's lifetime has passed: a path the
  warm-up cannot measure costs one budget a lifetime, not one a connection. A connection closed
  before or during its warm-up releases the remote for the next.
- **Round trips by packet number.** The measurement counts what is acknowledged in a round trip, and
  a round trip is a packet-timed one, as BBR counts them (draft-ietf-ccwg-bbr-06 §5.5.1): it begins
  at an acknowledgement and ends at the first acknowledgement of a packet sent after it. Counted on
  the clock by the smoothed RTT, as hyper-quic did before, a round trip shorter than the gap between
  acknowledgements closed a count at every acknowledgement, and a warm-up on such a path spent its
  whole budget without a measurement.
- **The window it uses counts as used.** A warm-up packet sent leaves the sender not limited by the
  application, and one held back by the window or the pacer leaves the window used (RFC 9002
  §7.8), so slow start grows the window as it would for data.

### 5.3 The jump, taken only where it gains

The warm-up's measurement is four initial windows, so its jump (§3.3: "no more than half of the
saved_cwnd") is two. The Unvalidated Phase holds the jumped window for a round trip and paces it
(§3.3), where slow start would double the window over that round trip. Measured on the geo harness:
a mebibyte reply resumed from a measurement of 70.8 kB jumped from slow start's 29.8 kB window to
35.4 kB and came 185 ms later than slow start alone. RFC 9959 §3.2 makes the jump a MAY; it is now
taken only where it gains: in congestion avoidance whenever it exceeds the window, and in slow start
when it is at least twice the window (slow start's own window a round trip on), or when everything
in flight and waiting in the streams fits in it, so the burst ends within the round trip.

### 5.4 Measured

The numbers are `docs/benchmarks.md`, "Measuring the path before its first burst". What is left,
and why: a burst that starts as the first connection to a new peer completes, before any idle
round trip, still meets the initial window (RFC 9002 §7.2), which no standard lets a sender exceed
without a measurement; and a jumped window is paced over its round trip (RFC 9959 §3.3), the MUST
that leaves a burst larger than the application's own pacing a share of a round trip above the
floor.
