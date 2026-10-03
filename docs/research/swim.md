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
- Left open: that period, memberlist's configuration.

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
  hyper-swim has no anti-entropy yet (`docs/timing.md` §2.7, open).

**Derived here (2026-10-02): how long a dead member's record is kept, and what bounds the view.**

- What a forgotten member's late gossip would do. hyper-swim adopts an update about a member it does
  not hold (a join travels as gossip), so a stale `Alive` or `Suspect` of a forgotten dead member,
  at an incarnation at or below its death's, would add it back: probed, suspected and condemned
  again, and gossiped on to members that forgot it too. Its `Dead` record is what refuses those
  updates (an update at or below the death's incarnation does not override it, `membership.rs`), so
  it is kept for as long as such an update can arrive.
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
