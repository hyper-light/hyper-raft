# Research: SWIM's membership bound

Source notes for `docs/timing.md` §2.7 (hyper-swim): how many members a view holds, and how long it
keeps what it learned of a dead one. Each entry says what the source establishes, verified against
the source text on 2026-10-02, and what it leaves open.

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
- Left: the 30 s and the multiplier are chosen, and the reap does not depend on what gossip can still
  carry.

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
