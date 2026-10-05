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
