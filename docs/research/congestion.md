# Research: congestion control for the shared transport

Source notes for `docs/transport.md` §4d (Copa in hyper-quic's congestion enum). Each entry says what
the source establishes and how it was checked: against the source text on 2026-10-04, or as carried
from focal's notes (focal b18, `scratch/b18-handoff-to-mantle.md`, 2026-10-04) and not re-read here.

## Copa

**Arun, Balakrishnan, "Copa: Practical Delay-Based Congestion Control for the Internet", NSDI 2018,
pp. 329–342.** Checked against the paper's text, 2026-10-04.

- §1, Eq. (1): the target rate `λ = 1/(δ·d_q)` packets a second, `d_q` the mean per-packet queueing
  delay and `1/δ` "in units of MTU-sized packets".
- §2.1: `RTTstanding` "is the smallest RTT observed over a recent time-window, τ. We use τ=srtt/2".
  `d_q = RTTstanding − RTTmin`, where `RTTmin` is the smallest round trip over "the smaller of 10
  seconds and the time since the flow started". "The sender paces packets at a rate of
  2·cwnd/RTTstanding packets per second."
- §2.1, the steps on each acknowledgement: if `cwnd/RTTstanding ≤ λt`, `cwnd += v/(δ·cwnd)`, otherwise
  `cwnd −= v/(δ·cwnd)`; "Over 1 RTT, the change in cwnd is thus ≈ v/δ packets." The velocity:
  "Once per window, the sender compares the current cwnd to the cwnd value at the time that the latest
  acknowledged packet was sent"; the same direction as the previous window doubles `v`, another resets
  it to one, and doubling starts "only after the direction has remained the same for three RTTs".
- §2.1: "When a flow starts, Copa performs slow-start where cwnd doubles once per RTT until λ exceeds
  λt."
- §2.2: two modes, "The default mode where δ=0.5" and a competitive one. "The detector exploits a key
  Copa property that the queue is empty at least once every 5·RTT when only Copa flows with similar
  RTTs share the bottleneck (Section 3) ... if the sender sees a 'nearly empty' queue in the last 5
  RTTs, it remains in the default mode; otherwise, it switches to competitive mode." Nearly empty is
  `d_q < 0.1(RTTmax − RTTmin)`, "where RTTmax is measured over the past four RTTs".
- §2.2: "In competitive mode the sender varies 1/δ according to whatever buffer-filling algorithm one
  wishes to emulate (e.g., NewReno, Cubic, etc.). In our implementation we perform AIMD on 1/δ based on
  packet success or loss, but this scheme could respond to other congestion signals." Returning to the
  default mode resets δ to 0.5. Copa flows wrongly competing "once again begin to periodically empty
  the queue".
- §3: with similar propagation delays "comparable to (or larger than) the queuing delay", the queue
  oscillates "between having 0 and 2.5/δ̂ packets every five RTTs", `δ̂ = (Σ 1/δ_i)^−1`; direction
  changes about every 2.5 round trips. The analysis assumes `RTTmin ≈ RTT`, so "the queue length
  inferred from an ACK at time t is q(t) = w(t − RTTmin) − BDP".
- §3: the behaviour "breaks only under two conditions in practice: (1) when the propagation delay is
  much smaller than the queuing delay and (2) when different senders have very different propagation
  delays", which can make the endpoints wrongly think a buffer-filling flow is present.
- Leaves open: the §3 analysis does not cover a queue long beside the path (`1/δ` above the
  bandwidth-delay product in packets), where focal's derivation A2 finds the decision runs on the
  window now rather than the lagged one (below).

**focal's derivations for the competing mode (focal b18, 2026-10-04).** Carried from focal's notes,
with the arithmetic checked here. Measured on hyper-quic's harness over focal's grids by a rule fixed
before the runs, they were not taken: b18 lowered Copa's share beside the incumbents from 41.7% to
25.0% and left finding 3 open (`docs/benchmarks.md`, "Copa's competing mode over focal's grids").

- A1: both mode windows cover the five-round-trip cycle §2.2 and §3 state. genericCC (`rtt-window.cc`),
  slates and focal took the least and the greatest over four smoothed round trips; a window shorter
  than the cycle misses its trough (Copa alone judged competing: 13.5% of samples at 100 Mbit/s,
  20 ms, in focal's harness) or its peak.
- A2: Copa's increase test is `λ ≤ λt ⟺ W·d_q ≤ (1/δ)·MSS·RTTstanding`. Alone with a busy queue the
  sample shows the queue the window built when the sampled packet was sent, `q_lag = W_lag − BDP`.
  With `W = W_lag + Δ`, the test reads `q_lag + Δ·q_lag/W_lag ≤ 1/δ`. Where `q ≪ W` the Δ term
  vanishes and the decision runs on the lagged queue, a delayed bang-bang loop that oscillates about
  `1/δ` and empties the queue each cycle (§3). Where `q ≈ W` (`1/δ` above the product in packets, or a
  path of one or two packets) the test is about `W(t) − BDP ≤ 1/δ`, with no lag: the window locks onto
  the target, the queue stands at `1/δ` and never empties. Comparing `W_sent` (the window the
  acknowledged packet was sent under) restores §3's model. In focal's deterministic link model: at
  1 Mbit/s and 20 ms Copa alone competed 95% of the time judged by the window now and none judged by
  the window sent; at 10 Mbit/s and 20 ms, after a competitor left, `1/δ` rose to 115 and the mode
  never ended, and judged by the window sent it ended.
- B: competing, `1/δ` grows by `d_q/RTTstanding` a round trip. Copa's rate is `(1/δ)/d_q`; a classic
  sender's `W/RTT`. AIMD on `1/δ` at a packet a round trip grows Copa's rate by `1/d_q` a round trip
  against NewReno's `1/RTT`, `RTT/d_q` times faster; synchronised sawtooths leave `1/δ ≈ W` and
  Copa/NewReno `= (RTTmin + d_q)/d_q ≥ 2` with a buffer of one product, so NewReno gets a third at most
  (focal's 33.8% against a bar of 43.3% at 1 Mbit/s, 100 ms). Grown by `d_q/RTTstanding`, Copa's rate
  grows `1/RTT` a round trip, as the window's does; halving `1/δ` halves the rate as halving `W` does.
- C (open, not designed): under a single CoDel-managed queue the manager empties the queue, so "nearly
  empty" says nothing about elastic traffic, and a mark is no proof of competition either. FQ-CoDel
  isolates flows, so this is single-queue CoDel only.

**What the harness found in focal's derivations (2026-10-04).**

Measured on hyper-quic's congestion harness (`crates/hyper-quic/tests/congestion.rs`): hyper-quic
endpoints over hyper-sim's network, a dumbbell with one bottleneck each way and a queue of one
bandwidth-delay product, ECN carried, each seed's run exact.

**B holds the queueing delay fixed; the sawtooth moves it.** B matches the growth of Copa's rate
`(1/δ)/d_q` to a classic sender's `W/RTT` at a fixed `d_q`: `1/δ` grows by `d_q/RTT` a round trip.
Beside a classic sender the shared queue fills between losses, and Copa's target falls as
`1/d_q`. `d_q` is small beside the round trip, so it moves far more in proportion than `RTT`, and
a raise sized to a moment's `d_q` falls behind. Measured at 10 Mbit/s and 20 ms on drop-tail, seeds
1–8: Copa 27.7%, NewReno 69.5%; focal's law before b18 50.4% / 46.8%. Copa competed in 72% of the
run there, and in 96% with H, still carrying 30.3%. The mode is not the cause.

**The paper's emptying is continuous; a link's is by packets.** §2.2's detector reads the queue as
nearly empty within a tenth of the spread of the last round trips above `RTTmin`. Copa alone keeps a
queue of about `2.5/δ` packets (§3), five at δ = 1/2, so a tenth of its spread is half a datagram's
time on the link: the test asks for an idle link, and with whole packets the queue reaches none
only now and then. Copa alone at 100 Mbit/s and 20 ms kept a 99th percentile of 0.54 ms (4.5
datagrams) while a tenth of its spread was 0.06 ms against a datagram's 0.096 ms. It emptied once
in 9.1 smoothed round trips at the median (11.6 at most) and judged itself competing in 33.4% of
the run. At 1 Mbit/s and 20 ms: 23.8 at the median, 68.4% competing. At 10 Mbit/s and 20 ms, where
the queue reached none once in 3.0 round trips (5.0 at most), it competed 0%; its one late
"competing" sample came where an interval was 5.0 exactly, the old trough leaving the window 10 ms
before the next arrived. This is focal's finding 2, without the pacing it was suspected of.

**Pacing is not the cause.** Paced at `2·cwnd/RTTstanding` (§2.1) instead of the connection's five
quarters of the window a smoothed round trip, Copa alone emptied its queue at the same intervals on
three of the five paths (1M/20, 1M/100, 10M/20: the same counts and quantiles), and carried the same
share beside NewReno and CUBIC at 10 Mbit/s and 20 ms. Its rate held steady: at 100 Mbit/s and
20 ms, p1 198 and p99 204 Mbit/s, so pacing on the standing round trip, which the delay signal also
reads, drives no oscillation (focal's question).

**One empty moment is not a sender alone.** The paper grants it (§2.2: "The queue may be nearly
empty even in the presence of a competing buffer-filling flow (e.g., because of a recent packet
loss)"). With a queue of one product, a classic sender's halving leaves the queue empty once a
sawtooth, and Copa left the competing mode each time: 61–74% competing beside NewReno at 10 Mbit/s
and 20 ms. Copa alone empties its queue cycle after cycle (§3); a competitor's backoff does it once.

**Arun, Balakrishnan, genericCC (the authors' implementation; `rtt-window.cc`, `markoviancc.cc`).**
Carried from slates' and focal's notes: the mode window of four round trips for the least and the
greatest, competitive AIMD on `1/δ` at most once a round trip, the velocity capped at `cwnd·δ`
packets (`update_amt`).

**mvfst (Meta), `Copa.cpp`.** Carried from slates' and focal's notes: slow start doubling once a
smoothed round trip until the first decrease, a loss ignored in the default mode, persistent
congestion collapsing to the least window, a reversal at speed resetting the velocity
(`changeDirection`), Nichols filters for the windows. focal's notes add: no competing mode at all,
δ fixed per connection (`copaDeltaParam`); `Copa2` was removed on 2026-01-29.

**Jiang, Li, Liu, Wu, Huang, Shan, Wang, "Copa+", INFOCOM 2022; IEEE/ACM Transactions on Networking,
2024, DOI 10.1109/TNET.2023.3278677.** Carried from focal's notes: finds independently that Copa
"fails to … clear the bottleneck buffer occupancy periodically" and "enters its competitive mode by
mistake", and that its competitive mode "fails to guarantee friendliness". Proposes a per-RTT step
adaptation, a spectral mode test over 20·RTTmin and a CUBIC-like competing mode. Its code has no
licence: read only.

**Goyal, Narayan, Cangialosi, Narayana, Alizadeh, Balakrishnan, "Elasticity Detection: A Building
Block for Internet Congestion Control", SIGCOMM 2022 (Nimbus).** Carried from focal's notes:
elasticity detected by asymmetric sinusoidal pulses and an FFT over 5 s; Copa made 28 wrong switches
in the elastic period against NimbusCC's 1. Its constants (200 ms pulses) do not scale to datacenter
round trips, and it needs a pulser/watcher election among a bottleneck's flows: a candidate for C.

## Harm and fairness

**Ware, Mukerjee, Seshan, Sherry, "Beyond Jain's Fairness Index: Setting the Bar for the Deployment of
Congestion Control Algorithms", HotNets 2019.** Carried from focal's notes: judge a new algorithm by
the harm it does to an incumbent against the incumbent's own kind, not by equal shares. focal's bar
is `min(incumbent beside its own kind, incumbent beside CUBIC)`.

## The RFCs the law answers to

Checked against the RFC text, 2026-10-04.

- **RFC 9002 §7.2:** "Endpoints SHOULD use an initial congestion window of ten times the maximum
  datagram size (max_datagram_size), while limiting the window to the larger of 14,720 bytes or twice
  the maximum datagram size." The minimum window: "The RECOMMENDED value is 2 * max_datagram_size."
- **RFC 9002 §7.3.2:** "A recovery period ends and the sender enters congestion avoidance when a packet
  sent during the recovery period is acknowledged." The law's mark recovery follows it.
- **RFC 9002 §7.8:** "When bytes in flight is smaller than the congestion window and sending is not
  pacing limited, the congestion window is underutilized", and the window is then not increased.
- **RFC 9002 §B.1:** `kLossReductionFactor`, "Section 7 recommends a value of 0.5."
- **RFC 3168 §5**, **RFC 9438** (CUBIC's 0.7) and **RFC 8511** (ABE's experimental 0.8): the other
  backoffs focal measured for a mark, carried from focal's notes.
