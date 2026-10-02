//! Node-pair liveness: one heartbeat stream per pair of nodes that share a consensus group, shared
//! by every group they share, each heartbeat proving a recent durable flush of the sender's log
//! (`docs/timing.md` §2.1, §2.8; step L-3).
//!
//! **What it replaces.** A Raft group's leader heartbeats each follower every `heartbeat_tick`,
//! and each follower runs an election timer: per-group messages and timers that grow with the
//! groups, not the machines, each a constant someone picked. Here a node keeps one stream to each
//! node it shares a group with ([`Liveness::attach`] counts the groups), sends one heartbeat on it
//! every `η`, and judges the peer's stream with NFD-E (Chen, Toueg and Aguilera 2002) through
//! `hyper_timing::LinkEstimator`, configured by `hyper_timing::qos` from measured floors. A group
//! sends nothing of its own for liveness; its core asks this crate whether the leader's node is
//! suspected (L-2). A pair with no group in common sends nothing at all.
//!
//! **The flush proof** (CockroachDB's store liveness, `docs/research/timing.md`). A heartbeat leaves
//! only once the sender's log has made a write durable after the previous heartbeat to that peer
//! was due: the owner reports each durable completion ([`Liveness::on_durable`]), and when none
//! came in time the stream asks for one ([`Output::flush`]) and sends on its completion. A node
//! whose disk stalls therefore stops heartbeating and is suspected as a crashed one is. Each
//! heartbeat carries the sender's count of durable writes and the age of the latest; a receiver
//! takes a heartbeat only when the count moved and the flush came after the previous heartbeat was
//! due, so a sender that heartbeats without flushing is not trusted either.
//!
//! **Timing, all measured.** The sender's interval is the one the receiver's configurator chose
//! (Chen et al.'s adaptive scheme, the receiver asking in its own heartbeats), never shorter than
//! the sender's stability floor `E[flush] + G` (Lindley 1952; `docs/timing.md` §2.6); before the
//! receiver has chosen, the floor. `G` is the measured lateness of the owner's wakes
//! (`hyper_timing::Wakes`), `E[flush]` the mean of the durable completions reported, one heartbeat
//! in each margin (Theorem 7's single factor, which assumes no independence between heartbeats a
//! young link cannot vouch for), the MTBF the Jeffreys posterior over
//! the pairs watched and the restarts seen (`hyper_timing::Exposure`), and the election cost the
//! owner's (`Liveness::set_election`, the election law's `T_E`).
//!
//! **The bound.** Each suspicion states when the sender's last heartbeat was due on the sender's
//! clock and the bound past it within which NFD-E suspects, `η + α + E(D)`, with `E(D)` bounded by
//! the echoed round trip of the heartbeats in the expected arrival's window (`bound`), which no
//! clock synchronization or path symmetry enters.
//!
//! **Sans-io.** The crate is fed `now`, messages with their kernel receive stamps (hyper-tokio's
//! `PlaneSocket`), and durable completions, and returns heartbeats to queue on the datagram plane,
//! flush requests, trust changes and the time to be polled next. It never reads a clock, spawns or
//! opens a socket. Once each pair is configured, a heartbeat sent and one received allocate
//! nothing (`benches/allocs.rs`).

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::disallowed_macros,
        clippy::cast_possible_truncation,
        clippy::cognitive_complexity
    )
)]

mod bound;
mod codec;
mod pair;

use std::collections::BTreeMap;
use std::time::Duration;

pub use codec::{Echo, Heartbeat, KIND, MAX_BYTES, VERSION, is_liveness};
use hyper_timing::{Configuration, Detector, ExchangeRtt, Exposure, Flushes, Trust, Wakes};
use pair::Pair;

/// A node's identity, as the owner names it (the datagram plane's `PeerId`).
pub type PeerId = u64;

/// Why the crate refused: every failure is one of these, never a panic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// A limit given to [`Liveness::new`] is zero.
    Limits,
    /// The node already keeps [`Settings::max_peers`] pairs.
    TooManyPeers,
    /// A pair cannot count more groups than a `u32` holds.
    TooManyGroups,
    /// No pair with this peer: nothing attached it.
    UnknownPeer,
    /// The peer is this node.
    FromSelf,
    /// The message is shorter than a heartbeat.
    Truncated,
    /// The message is not a liveness message.
    NotLiveness,
    /// A wire version this build does not read.
    BadVersion,
    /// The message does not parse.
    Malformed,
    /// A heartbeat no newer than the latest taken from the peer's run.
    Stale,
    /// A heartbeat whose flush proof does not hold: its count of durable writes did not move, or
    /// its latest flush is older than the previous heartbeat's schedule.
    Unproven,
    /// The receiver's timer granularity is not measured yet, so no estimator can be built; the
    /// heartbeat's echo is kept.
    Unmeasured,
    /// A heartbeat too far from its run's schedule for the estimator to sum: a sender that
    /// restarted its schedule.
    OutOfRange,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, formatter)
    }
}

impl std::error::Error for Refusal {}

/// A node's liveness settings.
#[derive(Clone, Copy, Debug)]
pub struct Settings {
    /// This node.
    pub local: PeerId,
    /// This run: a value the node never reuses across restarts (a boot nonce from the OS's random
    /// source, or the plane's epoch).
    pub boot: u64,
    /// The most pairs the node keeps: the nodes placement lets it share groups with. Past it,
    /// [`Liveness::attach`] refuses.
    pub max_peers: usize,
    /// The fleet's failure history so far, the MTBF's prior evidence ([`Exposure::new`] for a
    /// fleet with none).
    pub history: Exposure,
}

/// Which write became durable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Write {
    /// A log write the node made for its groups: the shell's completion.
    Log,
    /// The write [`Output::flush`] asked for.
    Liveness,
}

/// A suspicion of a peer: who, when, and the bound it kept.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Suspicion {
    /// The suspected node.
    pub peer: PeerId,
    /// The freshness point that passed with no newer heartbeat, on this node's clock.
    pub at_ns: u64,
    /// When the owner's poll noticed it, on this node's clock.
    pub noticed_ns: u64,
    /// The latest heartbeat taken from the peer: its number, its arrival (the kernel's stamp) on
    /// this node's clock, and when it was due and sent on the peer's.
    pub last: Option<Last>,
    /// The bound from the last heartbeat's schedule to `at_ns`, `η + α + E(D)` with `E(D)` bounded
    /// by the echoed round trips of the window's heartbeats (the `bound` module); `None` while a
    /// heartbeat in the window carried no echo.
    pub detection: Option<Duration>,
    /// The detector in force when it suspected, if configured (the pair's configuration's
    /// `current`, [`Liveness::configuration`]).
    pub detector: Option<Detector>,
}

/// The latest heartbeat taken from a peer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Last {
    /// Its number in the peer's run.
    pub seq: u64,
    /// Its kernel receive stamp, on this node's clock.
    pub arrival_ns: u64,
    /// When it was due, on the peer's clock.
    pub due_ns: u64,
    /// When it was sent, on the peer's clock.
    pub sent_ns: u64,
}

/// A change in what this node believes of a peer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Change {
    /// The peer's freshness passed: suspected.
    Suspected(Suspicion),
    /// A fresh heartbeat came from a peer suspected or not yet trusted.
    Trusted {
        /// The peer.
        peer: PeerId,
        /// The heartbeat's arrival.
        at_ns: u64,
    },
}

/// What a pair has done and promised, for the owner and its tests.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PairReport {
    /// Groups the pair shares.
    pub groups: u32,
    /// Heartbeats sent to the peer.
    pub sent: u64,
    /// Heartbeats taken from the peer.
    pub taken: u64,
    /// Heartbeats refused for their flush proof.
    pub unproven: u64,
    /// Whether a configured detector judges the peer.
    pub configured: bool,
    /// Configurations made.
    pub configurations: u64,
    /// Suspicions of the peer.
    pub suspicions: u64,
    /// Theorem 7's allowance for them: `Σβ` over every freshness point judged while a margin was in
    /// force, each `β` at that margin from the estimates as they stood, the expected number of
    /// suspicions were the peer alive throughout (`β` bounds the chance of a mistake at each,
    /// `η/β` the mistake recurrence).
    pub allowance: f64,
}

/// Where the crate's work goes: the owner queues heartbeats on the plane, makes the liveness
/// write, and acts on trust changes.
pub trait Output {
    /// A heartbeat to queue on the plane for `peer`, now.
    fn heartbeat(&mut self, peer: PeerId, message: &[u8]);
    /// Make a write on the log device and flush it; report it with [`Write::Liveness`].
    fn flush(&mut self);
    /// A change in trust.
    fn change(&mut self, change: Change);
}

/// The durable writes seen: their count and the latest one's completion.
#[derive(Clone, Copy, Debug, Default)]
struct Durable {
    count: u64,
    latest_ns: Option<u64>,
}

/// A node's pairs: what it sends each peer and what it believes of each.
pub struct Liveness {
    local: PeerId,
    boot: u64,
    max_peers: usize,
    pairs: BTreeMap<PeerId, Pair>,
    wakes: Wakes,
    flushes: Flushes,
    durable: Durable,
    /// Whether a liveness write is out.
    flushing: bool,
    exposure: Exposure,
    last_poll_ns: Option<u64>,
    message: [u8; MAX_BYTES],
}

impl Liveness {
    /// A node with no pairs.
    pub fn new(settings: Settings) -> Result<Self, Refusal> {
        if settings.max_peers == 0 {
            return Err(Refusal::Limits);
        }
        Ok(Self {
            local: settings.local,
            boot: settings.boot,
            max_peers: settings.max_peers,
            pairs: BTreeMap::new(),
            wakes: Wakes::new(),
            flushes: Flushes::new(),
            durable: Durable::default(),
            flushing: false,
            exposure: settings.history,
            last_poll_ns: None,
            message: [0; MAX_BYTES],
        })
    }

    /// One more group shared with `peer`: the pair's stream starts with its first.
    pub fn attach(&mut self, peer: PeerId) -> Result<(), Refusal> {
        if peer == self.local {
            return Err(Refusal::FromSelf);
        }
        if !self.pairs.contains_key(&peer) && self.pairs.len() >= self.max_peers {
            return Err(Refusal::TooManyPeers);
        }
        let pair = self.pairs.entry(peer).or_insert_with(Pair::new);
        pair.groups = pair.groups.checked_add(1).ok_or(Refusal::TooManyGroups)?;
        Ok(())
    }

    /// One group fewer shared with `peer`: with its last the pair's stream ends and its state
    /// goes. A peer let go while suspected counts as a failure in the MTBF's evidence.
    pub fn detach(&mut self, peer: PeerId) -> Result<(), Refusal> {
        let pair = self.pairs.get_mut(&peer).ok_or(Refusal::UnknownPeer)?;
        pair.groups = pair.groups.saturating_sub(1);
        if pair.groups == 0 {
            if pair.trust() == Trust::Suspected {
                self.exposure.on_failure();
            }
            self.pairs.remove(&peer);
        }
        Ok(())
    }

    /// The expected time from a suspicion of `peer` to a new leader, `T_E`, which its detector is
    /// configured to charge each election (`hyper_timing::Costs::election`): the election law's
    /// span over the groups whose leader that node is (the mean over them minimizes their summed
    /// unavailability, which is linear in `T_E`).
    pub fn set_election(&mut self, peer: PeerId, election: Duration) -> Result<(), Refusal> {
        let pair = self.pairs.get_mut(&peer).ok_or(Refusal::UnknownPeer)?;
        pair.election = Some(election);
        Ok(())
    }

    /// A write on this node's log became durable: started at `started_ns`, durable at
    /// `durable_ns`. Every durable completion is evidence a heartbeat may carry, and its time
    /// feeds `E[flush]`, the sender's floor.
    pub fn on_durable(&mut self, write: Write, started_ns: u64, durable_ns: u64) {
        self.durable.count = self.durable.count.saturating_add(1);
        self.durable.latest_ns = Some(
            self.durable
                .latest_ns
                .map_or(durable_ns, |l| l.max(durable_ns)),
        );
        // A full fold keeps its mean.
        let _ = self.flushes.on_flush(started_ns, durable_ns);
        if write == Write::Liveness {
            self.flushing = false;
        }
    }

    /// A liveness message from `from`, received at `arrival_ns` on this node's clock (the kernel's
    /// stamp where the owner has one). The peer is judged at the arrival first, so a freshness
    /// point that passed before the message came is a suspicion in whatever order messages and
    /// polls are fed; trust changes go to `out`. A heartbeat from a new run of the peer counts as
    /// a failure in the MTBF's evidence: the peer restarted.
    pub fn on_heartbeat(
        &mut self,
        from: PeerId,
        message: &[u8],
        arrival_ns: u64,
        out: &mut impl Output,
    ) -> Result<(), Refusal> {
        if from == self.local {
            return Err(Refusal::FromSelf);
        }
        let beat = Heartbeat::decode(message)?;
        let pair = self.pairs.get_mut(&from).ok_or(Refusal::UnknownPeer)?;
        let context = pair::Context {
            granularity: self.wakes.granularity(),
            mtbf: self.exposure.mtbf(),
        };
        let mut changes = [None, None];
        let taken = pair.take(from, &beat, arrival_ns, &context, &mut changes);
        for change in changes.into_iter().flatten() {
            out.change(change);
        }
        if taken? {
            self.exposure.on_failure();
        }
        Ok(())
    }

    /// Advances to `now_ns`: suspects peers whose freshness passed, sends the heartbeats due that
    /// a flush proves, and asks for a flush where none does. Call it at every [`wake`](Self::wake)
    /// and after each message or completion fed in.
    pub fn poll(&mut self, now_ns: u64, out: &mut impl Output) {
        self.wakes.woke(now_ns);
        self.expose(now_ns);
        let mut wants_flush = false;
        let sender = pair::Sender {
            local_boot: self.boot,
            floor: self.floor(),
            granularity: self.wakes.granularity(),
            durable_count: self.durable.count,
            durable_ns: self.durable.latest_ns,
        };
        for (&peer, pair) in &mut self.pairs {
            if let Some(change) = pair.judge(peer, now_ns) {
                out.change(change);
            }
            match pair.send(&sender, now_ns, &mut self.message) {
                pair::Sent::Message(length) => {
                    if let Some(message) = self.message.get(..length) {
                        out.heartbeat(peer, message);
                    }
                }
                pair::Sent::NeedsFlush => wants_flush = true,
                pair::Sent::Nothing => {}
            }
        }
        if wants_flush && !self.flushing {
            self.flushing = true;
            out.flush();
        }
        self.wakes.ask(self.wake_after(now_ns));
    }

    /// The node time watched since the last poll: each pair's peer, for the time between polls.
    fn expose(&mut self, now_ns: u64) {
        if let Some(last) = self.last_poll_ns {
            let peers = u32::try_from(self.pairs.len()).unwrap_or(u32::MAX);
            self.exposure.on_exposure(
                Duration::from_nanos(now_ns.saturating_sub(last)).saturating_mul(peers),
            );
        }
        self.last_poll_ns = Some(self.last_poll_ns.map_or(now_ns, |last| last.max(now_ns)));
    }

    /// When to [`poll`](Self::poll) next: the earliest heartbeat due and freshness point. A
    /// heartbeat waiting on a flush is sent when the flush is reported.
    pub fn wake(&self) -> Option<u64> {
        self.wake_after(self.last_poll_ns.unwrap_or(0))
    }

    fn wake_after(&self, now_ns: u64) -> Option<u64> {
        self.pairs
            .values()
            .flat_map(|pair| [pair.next_due().filter(|due| *due > now_ns), pair.deadline()])
            .flatten()
            .min()
    }

    /// The sender's stability floor, `E[flush] + G`: `None` before a flush is measured.
    pub fn floor(&self) -> Option<Duration> {
        let flush = self.flushes.mean()?;
        Some(flush.saturating_add(self.wakes.granularity().unwrap_or(Duration::ZERO)))
    }

    /// `E[flush]`, the mean time from a log write's start to its durability, once one is reported:
    /// the vote round's flush in the election law's ballot (`hyper_timing::Ballot::measure`).
    pub fn flush_mean(&self) -> Option<Duration> {
        self.flushes.mean()
    }

    /// `G`, the measured lateness of the owner's wakes.
    pub fn granularity(&self) -> Option<Duration> {
        self.wakes.granularity()
    }

    /// The latest the owner has woken past a wake asked, or is past one at `now_ns`.
    pub fn latest_wake(&self, now_ns: u64) -> Duration {
        Duration::from_nanos(self.wakes.latest_ns(now_ns))
    }

    /// What this node believes of `peer`, if they share a group.
    pub fn trust(&self, peer: PeerId) -> Option<Trust> {
        self.pairs.get(&peer).map(Pair::trust)
    }

    /// The peers this node suspects.
    pub fn suspected(&self) -> impl Iterator<Item = PeerId> + '_ {
        self.pairs
            .iter()
            .filter(|(_, pair)| pair.trust() == Trust::Suspected)
            .map(|(peer, _)| *peer)
    }

    /// The detector judging `peer` (the election law's base is its `current.interval +
    /// current.margin`, `hyper_timing::ElectionTiming::derive`).
    pub fn configuration(&self, peer: PeerId) -> Option<Configuration> {
        self.pairs.get(&peer).and_then(Pair::configuration)
    }

    /// The round trip to `peer` the echoes measure, network and kernel only (each side's
    /// schedule and flush taken out): a path for the election law's ballot.
    pub fn round_trip(&self, peer: PeerId) -> Option<&ExchangeRtt> {
        self.pairs.get(&peer).map(Pair::round_trip)
    }

    /// What the pair with `peer` has done and promised.
    pub fn report(&self, peer: PeerId) -> Option<PairReport> {
        self.pairs.get(&peer).map(Pair::report)
    }

    /// The MTBF the detectors are configured with, once there is exposure.
    pub fn mtbf(&self) -> Option<Duration> {
        self.exposure.mtbf()
    }
}
