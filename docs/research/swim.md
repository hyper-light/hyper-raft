# Research: SWIM's membership bound and its network coordinates

Source notes for `docs/timing.md` §2.7 (hyper-swim): how many members a view holds, how long it keeps
what it learned of a dead one, and the Vivaldi engine that ranks indirect-probe relays. Each entry
says what the source establishes, verified against the source text on 2026-10-02, and what it leaves
open.

## The membership: dissemination, and forgetting the dead

**Das, Gupta, Motivala, "SWIM: Scalable Weakly-consistent Infection-style Process Group Membership
Protocol", DSN 2002.**

- §3.1: a faulty member is eventually chosen as a ping target at each non-faulty member "and deleted
  from its membership list". §3.2: a member receiving `failed(Mj)` "deletes Mj from its local
  membership list"; a process joins through a contact member it knows (a well-known server or
  multicast address).
- §4.1: each member keeps "a buffer of recent membership updates, along with a local count for each
  buffer element", the times it has been piggybacked; each is piggybacked at most `λ log n` times,
  those piggybacked fewer times first when the buffer exceeds a message. By Bailey's epidemic
  analysis, after `λ log n` protocol periods the update has reached all but
  `n^{−((2−4/n)λ−2)}` members in expectation (the bound `gossip_transmits` takes, below one member
  once `λ > n/(n − 2)`). The implementation keeps two lists, the members not declared failed and
  "members that have failed recently", and draws the piggybacked elements from both.
- §4.2: incarnation numbers; a suspicion is refuted only by an `Alive` at a higher incarnation, which
  only the member itself raises; `Confirm` overrides `Alive` and `Suspect` at any incarnation.
- §4.3: round-robin probing bounds a failure's first detection at a member by twice the group size in
  protocol periods.
- Left open: how long a member keeps a failed member's record; what an update about a member it does
  not hold does (joins come through a contact member); a bound on the list. SWIM's analysis takes
  every member's protocol period as one and the same time unit.

**Dadgar, Phillips, Currey, "Lifeguard: Local Health Awareness for More Accurate Failure
Detection", arXiv 1707.00788 (2018).**

- §III-A: each update "is shared with one other member `λlog(n)` times, where `n` is the size of the
  known group and `λ` is a tunable multiplier", piggybacked on ping, ping-req and ack, those shared
  fewer times preferred. Footnote 3: alive messages override the others only at a higher incarnation,
  which only the suspected member raises (SWIM §4.2).
- §III-B: memberlist "retains the state of failed nodes for a period of time, so that information
  about failed nodes can be passed in a full state sync".
- §III-A, SWIM's probe as memberlist runs it (checked 2026-10-10): "If the original member does not
  receive any ack messages from the direct or indirect probe by the end of the protocol period, the
  probed member is considered to have failed the failure detection."
- §IV-A, "Local Health Aware Probe" (checked 2026-10-10): "Local Health Aware Probe adds a nack
  message to the fault detector protocol, which is sent in the case of failed indirect probes. This
  gives the member that initiates the indirect probe a way to check if it is receiving timely
  responses from the k members it enlists, even if the target of their indirect pings is not
  responsive." Footnote 5: "When a member is sent a ping-req message, it will send a nack back at 80%
  of the probe timeout unless it receives an ack by that time. An ack is still forwarded if it is
  received after the nack has been sent, and a member receiving a nack followed by an ack within the
  timeout period considers this as a successful indirect probe." Among the Local Health Multiplier's
  events: "Probe with missed nack: +1". `ProbeTimeout = BaseProbeTimeout·(LHM(S) + 1)`, the base
  500 ms in memberlist. So the relay's deadline is the one probe timeout every member is configured
  with, which is how the asker knows when a nack is due; the nack feeds local health and does not end
  the asker's wait, which runs to the end of the protocol period.
- §IV-C, "Buddy System" (checked 2026-10-03): "In SWIM, a suspected member is not guaranteed to
  hear of the suspicion at the first opportunity. A suspected node only learns of the suspicion
  when it receives a gossiped suspect message about itself. [...] the rules governing the
  dissemination of gossip messages include a limited number of gossip messages per piggyback,
  limited re-sends of each gossip message, and a preference for newer gossip messages. Buddy System
  replaces SWIM's piggyback message selector with one that prioritizes notifying a suspected member
  of the suspicion. This guarantees that any node that pings a suspected node (either on its own
  behalf, or for the indirect path of another node) will communicate the suspicion as part of the
  ping."
- Left open: that period, memberlist's configuration; how a member that holds another dead hears
  of its refutation, under the same limits.

**hashicorp/memberlist (Go source, `master` on 2026-10-02: `config.go`, `state.go`, `util.go`).**

- `GossipToTheDeadTime`, 30 s in the LAN configuration: "the interval after which a node has died
  that we will still try to gossip to it. This gives it a chance to refute." At each wrap of the probe
  index, `resetNodes` and `moveDeadNodes` delete the dead and left nodes whose state changed longer
  ago than that.
- `aliveNode`: "Check if we've never seen this node before, and if not, then store this node in our
  node map": an alive message about a node not in the map adds it, whatever its incarnation. A reaped
  node is added again by a stale alive message; only the 30 s stand between them.
- `DeadNodeReclaimTime` (0 by default, never): when a dead node's name may be taken by another
  address.
- `retransmitLimit`: `RetransmitMult` (4) × `⌈log10(n + 1)⌉`.
- `ProbeInterval` and `ProbeTimeout` (checked 2026-10-10): 1 s and 500 ms in the LAN configuration
  ("Failure check every second", "Reasonable RTT time for LAN"), 5 s and 3 s WAN, 1 s and 200 ms
  local. The field's comment: "Setting this lower (more frequent) will cause the memberlist cluster
  to detect failed nodes more quickly at the expense of increased bandwidth usage"; the timeout's:
  "This should be set to 99-percentile of RTT (round-trip time) on your network." The probe rate is
  a load setting, picked once, as SWIM's protocol period is (§3.1).
- The nack (`net.go`, `state.go`, checked 2026-10-10). A relay's `handleIndirectPing` registers the
  handler that forwards the target's ack for `m.config.ProbeTimeout` and, where the request asked a
  nack, starts a timer: "Setup a timer to fire off a nack if no ack is seen in time", `case
  <-time.After(m.config.ProbeTimeout)`, cancelled by the ack ("Try to prevent the nack if we've
  caught it in time"). The asker's `probeNode` waits for acks until `deadline :=
  sent.Add(probeInterval)` (its probe interval scaled by its awareness), counts `expectedNacks` from
  the peers that speak protocol version 4, and after a failed probe adds `expectedNacks − nackCount`
  to its awareness ("Update our self-awareness based on the results of this failed probe"). The
  relay's timeout is its own configuration's, the same constant across a fleet configured alike.
- `PushPullInterval` (checked 2026-10-03), 30 s in the LAN configuration, 60 s WAN, 15 s local: "the
  interval between complete state syncs. Complete state syncs are done with a single node over TCP
  and are quite expensive relative to standard gossiped messages." `pushPull` picks one node at
  random among those it holds alive; `mergeState` applies the remote states through the same alive,
  suspect and dead handlers as gossip, a remote dead or suspect as a suspicion.
- Left: the 30 s and the multiplier are chosen, and the reap does not depend on what gossip can still
  carry.

**Demers, Greene, Hauser, Irish, Larson, Shenker, Sturgis, Swinehart, Terry, "Epidemic Algorithms
for Replicated Database Maintenance", PODC 1987 (checked 2026-10-03).**

- §1.4, "Complex Epidemics": rumor mongering with a counter, a site "remaining infective for k
  cycles independent of any feedback", and its other variations "share the same fundamental
  relationship between traffic and residue: `s = e^{−m}`", `m` the updates sent a site, since a
  site misses all `nm` of them with chance `(1 − 1/n)^{nm}`. SWIM's piggybacked update, sent
  `λ log n` times and then dropped, is such a rumor: at `m` = 3, a twentieth of the members in this
  model.
- §1.5, "Backing Up a Complex Epidemic with Anti-entropy": "a complex epidemic can fail: that is,
  there is a nonzero probability that the number of infective sites will fall to zero while some
  sites remain susceptible. This event can be made extremely unlikely; nevertheless, if it occurs,
  the system will be in a stable state in which an update is known by some, but not all, sites. To
  eliminate this possibility, anti-entropy can be run infrequently to back up a complex epidemic
  [...] This ensures with probability 1 that every update eventually reaches (or is superseded at)
  every site." Anti-entropy: each site regularly chooses another at random and the two resolve every
  difference between their contents.
- §3: "the need to back up rumor mongering with anti-entropy to guarantee complete coverage".
- Left open: how often anti-entropy runs; the paper's exchanges whole databases.
- What it settles here: a refutation is an update like any other, and the cluster test found the
  stable state §1.5 names (`docs/benchmarks.md`, "The cluster test"): a live member's refutation
  known to every member but one, which held it dead and, past the record's window, forgot it.

**Derived here (2026-10-03): a refutation a rumor missed.**

- A member that holds another dead hears of its refutation only from a gossiped alive message, under
  the limits Lifeguard §IV-C names for a suspicion; and having adopted the death, it probes the
  member no more, so the buddy system, which rides on its probes, never reaches the refuted member
  from it.
- The refuted member still holds it alive and probes it every round. Its probes now carry its own
  state, alive at its incarnation, whatever the rumor's budget: the first one revives it there, held
  dead (a higher incarnation overrides) or forgotten (an update about a member the view does not
  hold adds it). This is the buddy system's guarantee turned round: any node a member pings learns
  the member's own state.
- A member held dead at the incarnation it died at, which never heard so, refutes only once told.
  Nobody probes the dead, so the answer to its probe carries the belief, as a probe of a suspect
  does: it refutes, and its next probe revives it.
- Between two members that probe each other, these two entries are Demers et al.'s anti-entropy for
  the two states that concern them, at every exchange. Two live members that each hold the other
  dead, both refutations missed, exchange nothing; a third member that holds both alive must pass
  its view of each to the other: the full exchange memberlist runs every 30 s.

**Derived here (2026-10-03): anti-entropy, and how long a split lasts.**

- The window a rumor can still arrive in. A member piggybacks an update on its next `T` messages
  and drops it, and sends at least one message a period, its probe; so an update is spent within `T`
  of its adopter's periods, and past the dissemination window `W` (`T` of the member's longest
  periods, the window its dead records are kept for) after its last adoption it reaches nobody new.
- What a rumor misses. A rumor sent a fixed count is Demers et al.'s blind counter variant (§1.4),
  whose residue follows their traffic relationship `s = e^{−m}`: a member sends `T` copies when it
  knows the update, a fraction `1 − s` of them do, so `m = T(1 − s)` and `s = e^{−T(1−s)}`. At the
  budgets `gossip_transmits` gives, `T = 3` for five members, 4 for 16, 5 for 64, 6 for 256 and 7 for
  a thousand, the residue is 6.0, 2.0, 0.70, 0.25 and 0.092 %. The relationship is the large-`n`
  limit; the cluster test's five members are where it is roughest, and its runs are the check.
- The exchange's period. A member the rumor missed never hears it by rumor once `W` has passed, so
  an exchange begun once each `W` is anti-entropy at the rumor's own pace: the window after which
  waiting longer for the rumor gains nothing is the period at which the backup runs. memberlist's
  30 s is a chosen number; `W` is the crate's own law, measured in the member's own periods.
- With whom, and the bound. The partner is the next of a shuffled cycle of the members the initiator
  holds alive, so over `m` exchanges, `m` those members, it exchanges with each; the exchange is
  push-pull, which Demers et al. find far better than push when few sites are left susceptible
  (§1.3: "either pull or push-pull is greatly preferable to push"). A member the rumor missed
  learns the update at its first exchange with a member that holds it, which each partner does
  unless the rumor missed it too: the split outlasts `W` by more than `k` exchanges with
  probability at most `s^k`, and by `1/(1 − s)` exchanges in expectation. Deterministically, one
  cycle ends it, `W + m·W` past the last adoption, when the update reached any member it holds
  alive; two, when only the update's origin holds it, whose own cycle reaches the others first.
- Checksums first. Demers et al. §1.3: "Only if the checksums disagree do the sites compare their
  entire databases." The view's digest is the wrapping sum, over its members, of SplitMix64's output
  function (Steele, Lea and Flood, OOPSLA 2014: a bijection on 64 bits whose every output bit
  depends on every input bit) applied to the member's id, incarnation and liveness in turn: order
  free, so kept by subtracting and adding one member's share at each change, and two views that
  differ in any state collide with odds of one in 2⁶⁴. An exchange opens with the digest alone;
  only views that differ are pushed, each way. A first form pushed whole views at every exchange,
  `2⌈n/r⌉/T` datagrams a member a period, `r` the entries a datagram holds beside a bare chunk:
  7.5 entries a member a period at 64 members and 25 at 256 in the comparison's quiet cluster, now
  none (`docs/benchmarks.md`).
- What is bounded. A member owes one push at a time, refusing and counting a second pull, and lets
  one opening go before the next: a cursor, an opening and a cycle no larger than the view.
- Left open: §1.3's refinement for churn, recent-update lists exchanged before the checksums, or
  digests of ranges of the view, which would push only what differs; under a refutation every
  period the views mostly differ, and the exchanges carry 5.1 entries a member a period at 64
  members and 16.4 at 256.
- What is left. An exchange carries a member's whole view, so a stale alive state at a member that
  has neither heard nor found a death yet can add back a member already forgotten, as a late rumor
  could; it is probed and condemned again within the detection bound. memberlist's push/pull has the
  same property (its `aliveNode` adds an unknown node whatever its incarnation).

**The wire, and its argument (2026-10-03).** No consumer runs hyper-swim yet (slates' session owns
its integration, `docs/STATUS.md`), so the wire changes in place:
- a probe's gossip carries the prober's own state, and an answer's the answering member's suspicion
  or death of the prober: entries of the existing gossip encoding, which a receiver of the earlier
  form applies as any gossip, so neither changes the message format;
- a view chunk is a new message, `Sync` (tag 5): the sender, its boot nonce, its view's digest, a
  pull byte and a gossip batch, empty in an opening, a golden vector pinning its layout (`codec.rs`,
  `sync_has_a_golden_encoding`).

**Derived here (2026-10-02): how long a dead member's record is kept, and what bounds the view.**

- What a forgotten member's late gossip would do. hyper-swim adopts an update about a member it does
  not hold (a join travels as gossip), so a stale `Alive` or `Suspect` of a forgotten dead member,
  at an incarnation at or below its death's, would add it back: probed, suspected and condemned
  again, and gossiped on to members that forgot it too. Its `Dead` record is what refuses those
  updates (an update at or below the death's incarnation does not override it, `membership.rs`), so
  it is kept for as long as such an update can arrive. A death of a member it does not hold changes
  nothing (2026-10-03): with anti-entropy, a member past its window otherwise took the death back as
  a newcomer's from one still inside its own, restarting its window, and pushed it on in turn, the
  record going round for as long as any member held it.

**Demers et al. 1987, §2 and §2.1 (checked 2026-10-03).** "We cannot delete an item from the
database simply by removing a local copy of the item ... the propagation mechanism will spread old
copies of the item from elsewhere in the database back to the site where we have deleted it", so
deleted items become death certificates, held "for some fixed threshold of time ... and then
discard[ed]", at the risk of obsolete items older than the threshold being resurrected; and "if a
death certificate is older than the expected time required to propagate it to all sites, then the
existence of an obsolete copy of the corresponding data item anywhere in the network is unlikely"
(§2.1). Their certificates carry a time stamp; a member's record here starts its window at its own
adoption, so a record taken again restarts it, and the record itself becomes the item resurrected.
Taking no death for a member the view does not hold is the rule that keeps a discarded certificate
discarded.
- How long that is. A member holds one pending report per member, replaced when it adopts a newer
  state, so a report of the member from before its death survives only at members the death has not
  reached. By SWIM §4.1 the death reaches every member but `n^{−((2−4/n)λ−2)}` in expectation, below
  one, within `λ ln n` periods of its first adoption, the crate's dissemination budget `T`; a report
  from before the death began its own epidemic earlier and has ended within `T` periods of that start
  by the same bound. A member's own adoption is at or after the first, so a record kept `T` periods
  past it outlasts both.
- In whose periods. SWIM counts one period for every member; hyper-swim's periods are each probe's
  deadlines, member by member. The record is kept `T` times the longest period this member has run
  or its verdicts allow: the period bound `detection_bound` uses, at least three times the longest
  span `μ + α` any of its verdicts has had. A peer slowed by its own host lengthens this member's
  round trips to it, so its periods are within that bound too.
- What is left. A member the death has not reached when the window ends, `n^{−((2−4/n)λ−2)}` in
  expectation, can still pass a stale update; the member adopted again is then probed and condemned
  again by this member's own detector, within its detection bound. The detector's verdicts are hints
  to the owner's committed membership, never durable decisions, so the cost is probes, not safety.
  Observed (2026-10-10): with margins searched over every margin, members that measured a stall
  judge with wider margins and condemn later than the others, whose windows are their own periods';
  in the simulation one such member still held a killed member suspected 30 ms after two others had
  forgotten its death, and passed its suspicion on. The death is the same one (its member and
  incarnation): the failure history counts it once.
- An isolated member, with nobody alive or suspected left, keeps its records: the dead are the only
  members it probes, and a live one among them refutes in its answer (`docs/timing.md` §2.7).
- The bound. The owner's placement says how many hosts a node can know; the view holds at most that
  many members, itself included, and refuses an update about one more, typed. A record past its
  window makes room for a newcomer at once; one inside it does not, since forgetting it early is what
  the window prevents. Every map keyed by a member (the detector's peers, coordinates, extensions, the
  gossip's reports) holds only members the view holds, so all are bounded by it.

## The network coordinates

**Dabek, Cox, Kaashoek, Morris, "Vivaldi: A Decentralized Network Coordinate System", SIGCOMM
2004.**

- §2.3: the squared-error function `E = Σ (L_ij − ‖x_i − x_j‖)²` is a spring network's energy.
- §2.4: every node starts at the origin; "Vivaldi does this [separates them] by defining `u(0)` to
  be a unit-length vector in a randomly chosen direction".
- §2.5, Eq. 2: `δ = c_c × local error / (local error + remote error)`: "an accurate node sampling
  an inaccurate node will not move much, an inaccurate node sampling an accurate node will move a
  lot, and two nodes of similar accuracy will split the difference."
- §2.6: each node keeps "a moving average of recent relative errors (absolute error divided by
  actual latency)", each sample weighted as in `δ`; "the estimate is always within a small constant
  factor of the actual error".
- §2.7, Fig. 3: `w = e_i/(e_i + e_j)`; `e_s = |‖x_i − x_j‖ − rtt| / rtt`;
  `e_i = e_s × c_e × w + e_i × (1 − c_e × w)`; `δ = c_c × w`;
  `x_i = x_i + δ × (rtt − ‖x_i − x_j‖) × u(x_i − x_j)`. "The constants `c_e` and `c_c` are tuning
  parameters."
- §3.2: a node's error is "the median of the link errors for links involving that node".
- §4.1, Fig. 5(b): "Empirically, a `c_c` value of 0.25 yields both quick error reduction and low
  oscillation."
- §5.2: by principal components "the coordinates primarily use two to three dimensions"; "Adding
  extra dimensions past three does not make a significant improvement in the fit", and more
  dimensions cost more communication: "we prefer the lowest dimensional coordinates that allow for
  accurate predictions".
- §5.4: height vectors, `[x, x_h] − [y, y_h] = [(x − y), x_h + y_h]`, `‖[x, x_h]‖ = ‖x‖ + x_h`,
  `α × [x, x_h] = [αx, αx_h]`; "Each node has a positive height element in its coordinates, so that
  its height can always be scaled up or down." Fig. 15: height vectors predict better than 2-D and
  3-D Euclidean coordinates on PlanetLab and King.
- §6.2: "Vivaldi defends against high-error nodes, but not malicious nodes."
- Left open: `c_e`'s value; a node's first height; `u(0)` in the height-vector space.

**Ledlie, Gardner, Seltzer, "Network Coordinates in the Wild", NSDI 2007.**

- §2.1, Fig. 1: Vivaldi's update as Dabek's; "Constants `c_e` and `c_c` affect the maximum impact an
  observation can have on the confidence and the coordinate"; no values.
- §3.4: by scree plots "Azureus, in particular, is dominated by a single dimension, and MIT King by
  two".
- §6.1: removing height (2-D + H to 5-D) "damaged accuracy more than the filters aided it"; 4-D + H
  came with neighbour decay and the filters at once, so the dimensions are not separated from them.
- §7.2: drift: the centroid "drifted constantly and repeatedly in a vector away from the origin";
  gravity `G = (‖x_i‖/ρ)² × u(x_i)` applied toward the origin after each update, "where `ρ` tunes
  `G` so that its pull is a small fraction of the expected diameter of the network". Table 1 (a 24-hour
  PlanetLab trace): `ρ = 2⁶` ms, 25 % error; `2⁸`, `2¹⁰`, `2¹²` and none, 10 %; centroid migration 8,
  17, 74, 163 and 179 ms. "Drift does not occur in simulation."
- Left open: `c_e`; a gravity free of units: `(‖x‖/ρ)²` is a number, read as milliseconds.

**NIST/SEMATECH e-Handbook of Statistical Methods, §6.3.2.4, "EWMA Control Charts".**
`EWMA_t = λY_t + (1 − λ)EWMA_{t−1}`, and `s²_EWMA = (λ/(2 − λ)) s²` over independent observations of
variance `s²`. An `m`-observation mean has variance `s²/m`; the two are equal at `λ = 2/(m + 1)`.

**Derived here (2026-10-02): the engine's constants.**

- `c_c = 0.25`: Dabek §4.1.
- Dimensions: two and a height, Dabek §5.2 and §5.4 (Fig. 15), with Ledlie §3.4's one to two
  intrinsic dimensions. The repository cannot record a wide-area round-trip matrix to measure against;
  its loopback and container round trips are one link's, all alike.
- `c_e`: what the estimate tracks is the node's relative error over its links (Dabek §2.6, §3.2). SWIM
  samples each link once a round of `m` periods, in a fresh permutation (§4.3), so a shorter memory
  sees a random part of the links and a longer one averages errors of coordinates since replaced. The
  estimate's memory is one round: the weight whose EWMA has the variance of an `m`-sample mean,
  `c_e = 2/(m + 1)`, the weight a sample of full trust (`w → 1`) gets.
- The first height and `u(0)`. A node starts at the origin of the height-vector space, height zero
  (Dabek §2.4). Two such nodes' difference is the zero vector, so `u(0)` applies: a unit vector of
  the height-vector space in a random direction, uniform on its unit sphere `‖v‖ + h = 1, h ≥ 0`
  (`h` of density `2(1 − h)`), drawn from the node's own seeded stream so that a simulation replays.
  After an update a height is kept at least one nanosecond, the unit round trips are measured in, so it
  stays positive (§5.4).
- The error estimate stays positive. A sample's error is floored at one nanosecond over the round
  trip: a smaller error cannot be measured, and an estimate of zero would fix `w = 0`, a node that
  never moves again. With it the estimate is positive whatever the samples, so `w` is defined
  without a neutral case, and no ceiling is needed: a large estimate is a node with no confidence,
  which its peers weigh accordingly. What keeps it finite is that a sample which would leave the
  coordinate infinite or not a number, a peer's point so far out that the distance or the sample's
  error overflows, is not taken: Vivaldi defends against peers in error (§6.2), and one such sample
  would otherwise make every later weight `∞/∞`.
- No gravity. Ledlie's `G` carries its units' scale: `(‖x‖/ρ)²` is a number read as milliseconds, so
  in seconds the same `ρ` pulls a thousand times harder, and a pull that is a small fraction of
  PlanetLab's diameter in milliseconds moves a LAN's coordinates across the network in one update.
  Nothing in the paper fixes the unit, so no `ρ` derived from a measured diameter makes it free of
  one. And the engine does not need it. Drift is a rigid motion of every coordinate, which leaves every
  distance and height, so every prediction, unchanged; the engine predicts only between its own
  coordinate and peers' learned at their last acknowledgement, at most a round old. Vivaldi's update is
  homogeneous in the coordinates and the round trips (scaling every round trip by `k` scales every
  coordinate and step by `k`), so drift is a fraction of the round trips whatever their scale: Ledlie's
  179 ms in 24 hours without gravity (Table 1) is 2.1 µs a second, under three hundred-thousandths of
  PlanetLab's 76 ms median round trip (Dabek §3.1) for a round of a second. What drift costs is a node
  that joins at the origin far from a drifted centroid: it closes the distance by `1 − c_c·w` a sample,
  `w` near one while its error dwarfs its peers', so about `log(D/r)/log(1/(1 − c_c))` samples more
  for a drift of `D` against round trips `r`: two to three for each doubling.
- Removed with gravity, being in neither paper: a separate share of each step for the height (the
  height moves inside the step by its share of the predicted round trip, §5.4's algebra), an
  "adjustment" term added to every prediction and its smoothing and clamp, and the bounds on the error
  estimate (positive by the resolution's floor; a ceiling is only less confidence). The wire carries
  what is left, the point's two components, the height and the error, and a decoder takes a coordinate
  of exactly the engine's dimensions: a point of another space means nothing in this one.
