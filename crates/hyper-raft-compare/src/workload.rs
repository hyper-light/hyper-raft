//! The workloads, one driver for every core.
//!
//! A group is `n` members on an in-memory network that loses nothing unless a member is cut
//! off. The network delivers in waves: every message in flight is stepped into its member,
//! then every member that received one flushes once, as a shell steps what its socket holds
//! before it asks for the next `Ready`. A wave's messages go out in the next wave; a group is
//! quiet when a wave sends nothing.
//!
//! What is measured is a set of segments of a run (`Meter`): time, the allocator's counts and
//! the OS's page faults between `start` and `stop`. What a workload does to set up a segment
//! (electing, cutting a member off, filling the log) is outside every segment.
use std::time::{Duration, Instant};

use hyper_measure::{alloc, faults};

use crate::core::{Core, Envelope, Fast, Settings, splitmix};

/// What a measured run says.
#[derive(Clone, Copy, Debug, Default)]
pub struct Measured {
    pub ops: u64,
    pub elapsed: Duration,
    pub total: alloc::Counts,
    pub aside: alloc::Counts,
    pub faults: faults::Faults,
}

/// Measures segments of a run and adds them up.
struct Meter {
    measured: Measured,
    started: Option<(Instant, faults::Faults)>,
}

impl Meter {
    fn new() -> Self {
        assert!(
            alloc::installed(),
            "the counting allocator is not installed"
        );
        alloc::begin();
        alloc::pause();
        Self {
            measured: Measured::default(),
            started: None,
        }
    }
    fn start(&mut self) {
        let before = faults::read().expect("the OS counts faults");
        alloc::resume();
        self.started = Some((Instant::now(), before));
    }
    fn stop(&mut self) {
        let now = Instant::now();
        alloc::pause();
        let after = faults::read().expect("the OS counts faults");
        let (started, before) = self.started.take().expect("a segment was started");
        self.measured.elapsed += now - started;
        let charged = after.since(&before);
        let sum = &mut self.measured.faults;
        sum.minor += charged.minor;
        sum.major = Some(sum.major.unwrap_or(0) + charged.major.unwrap_or(0));
        sum.task = match (sum.task, charged.task) {
            (None, task) => task,
            (Some(mut held), Some(more)) => {
                held.faults += more.faults;
                held.pageins += more.pageins;
                held.cow_faults += more.cow_faults;
                Some(held)
            }
            (held, None) => held,
        };
    }
    fn finish(mut self, ops: u64) -> Measured {
        self.measured.total = alloc::end();
        self.measured.aside = alloc::read_aside();
        self.measured.ops = ops;
        self.measured
    }
}

/// The most waves a group may take to go quiet: a bound on the run, far above what any
/// workload here needs (an election takes four, a round of appends two), so that a core that
/// never quiets fails the run instead of hanging it.
const MAX_WAVES: usize = 100_000;
/// The most periods an election may take: twice the longest randomized timeout of every
/// member in turn, past which the run fails.
const MAX_ELECTION_PERIODS: usize = 400;

pub struct Group<C: Core> {
    pub nodes: Vec<C>,
    cut: Vec<bool>,
    inflight: Vec<Envelope<C::Message>>,
    next: Vec<Envelope<C::Message>>,
    touched: Vec<bool>,
    random: u64,
}

impl<C: Core> Group<C> {
    pub fn new(members: usize, settings: &Settings, seed: u64) -> Self {
        let voters: Vec<u64> = (1..=members as u64).collect();
        let mut random = seed;
        let nodes = voters
            .iter()
            .map(|id| C::open(*id, &voters, settings, splitmix(&mut random)))
            .collect();
        Self {
            nodes,
            cut: vec![false; members + 1],
            // Room for every message a wave may hold, so the network's own queues never grow
            // inside a segment.
            inflight: Vec::with_capacity(1 << 16),
            next: Vec::with_capacity(1 << 16),
            touched: vec![false; members],
            random,
        }
    }
    fn at(&mut self, id: u64) -> &mut C {
        &mut self.nodes[(id - 1) as usize]
    }
    /// Delivers until nothing is in flight.
    pub fn quiet(&mut self) {
        let mut waves = 0;
        while !self.next.is_empty() {
            waves += 1;
            assert!(waves < MAX_WAVES, "{}: the group never went quiet", C::NAME);
            std::mem::swap(&mut self.inflight, &mut self.next);
            let Self {
                nodes,
                cut,
                inflight,
                next,
                touched,
                ..
            } = self;
            for envelope in inflight.drain(..) {
                if cut[envelope.from as usize] || cut[envelope.to as usize] {
                    continue;
                }
                let at = (envelope.to - 1) as usize;
                nodes[at].step(envelope.from, envelope.message, next);
                touched[at] = true;
            }
            for (at, node) in nodes.iter_mut().enumerate() {
                if std::mem::take(&mut touched[at]) {
                    node.flush(next);
                }
            }
        }
    }
    /// The one leader among the members that are not cut off, of the highest term.
    pub fn leader(&self) -> Option<u64> {
        let mut found: Option<(u64, u64)> = None;
        for node in &self.nodes {
            if self.cut[node.id() as usize] || !node.is_leader() {
                continue;
            }
            if found.is_none_or(|(_, term)| node.term() > term) {
                found = Some((node.id(), node.term()));
            }
        }
        found.map(|(id, _)| id)
    }
    pub fn campaign(&mut self, id: u64) {
        let Self { nodes, next, .. } = self;
        nodes[(id - 1) as usize].campaign(next);
        self.quiet();
    }
    /// One period of the clock at every member that is not cut off.
    pub fn period(&mut self) {
        let Self {
            nodes, cut, next, ..
        } = self;
        for node in nodes.iter_mut() {
            if !cut[node.id() as usize] {
                node.period(next);
            }
        }
        self.quiet();
    }
    pub fn cut(&mut self, id: u64, cut: bool) {
        self.cut[id as usize] = cut;
    }
    /// `count` proposals of `bytes` bytes each at the leader, sent at once.
    pub fn propose(&mut self, leader: u64, count: usize, bytes: usize) {
        let fill = (splitmix(&mut self.random) & 0xff) as u8;
        for _ in 0..count {
            alloc::aside();
            let data = vec![fill; bytes];
            alloc::back();
            assert!(
                self.at(leader).propose(data),
                "{}: a proposal refused",
                C::NAME
            );
        }
        let Self { nodes, next, .. } = self;
        nodes[(leader - 1) as usize].flush(next);
        self.quiet();
    }
    pub fn compact(&mut self) {
        for node in &mut self.nodes {
            node.compact();
        }
    }
    /// Whether every member that is not cut off applied what the leader applied, and the same.
    pub fn agreed(&self, leader: u64) -> bool {
        let lead = &self.nodes[(leader - 1) as usize];
        self.nodes.iter().all(|node| {
            self.cut[node.id() as usize]
                || (node.applied() == lead.applied() && node.digest() == lead.digest())
        })
    }
    /// Elects `id` from a group that has no leader.
    pub fn elect(&mut self, id: u64) {
        self.campaign(id);
        assert_eq!(self.leader(), Some(id), "{}: {id} was not elected", C::NAME);
        // A leader of slates' line appends nothing at its election; every core commits one
        // entry of its term before it is counted elected.
        self.propose(id, 1, 8);
        assert!(
            self.agreed(id),
            "{}: the first entry did not reach every member",
            C::NAME
        );
    }
    /// Periods until `done` holds, failing the run past `MAX_ELECTION_PERIODS`.
    fn periods_until(&mut self, what: &str, done: impl Fn(&Self) -> bool) {
        for _ in 0..MAX_ELECTION_PERIODS {
            if done(self) {
                return;
            }
            self.period();
        }
        panic!(
            "{}: {what} did not happen within {MAX_ELECTION_PERIODS} periods",
            C::NAME
        );
    }
}

/// What each workload is, as the tables name it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Workload {
    /// Proposals at the leader, `batch` at a time, committed and applied by every member.
    Steady,
    /// The leader hands over to the next member, which commits an entry of its term.
    Transfer,
    /// The leader is cut off; the others elect one of themselves by their timers, which commits
    /// an entry; the old leader returns and follows.
    Failover,
    /// A follower cut off while `ops` entries were committed returns and catches up from the log.
    CatchUp,
    /// A follower cut off while the leader compacted past it returns and is sent a snapshot.
    Snapshot,
    /// Proposals by the fast track from a follower, committed and applied by every member.
    Fast,
}

impl Workload {
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "steady" => Self::Steady,
            "transfer" => Self::Transfer,
            "failover" => Self::Failover,
            "catchup" => Self::CatchUp,
            "snapshot" => Self::Snapshot,
            "fast" => Self::Fast,
            _ => return None,
        })
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Steady => "steady",
            Self::Transfer => "transfer",
            Self::Failover => "failover",
            Self::CatchUp => "catchup",
            Self::Snapshot => "snapshot",
            Self::Fast => "fast",
        }
    }
}

/// One workload's shape.
#[derive(Clone, Copy, Debug)]
pub struct Spec {
    pub workload: Workload,
    pub voters: usize,
    /// Proposals sent at once.
    pub batch: usize,
    /// The bytes of each proposal.
    pub bytes: usize,
    /// Rounds of the workload measured.
    pub rounds: usize,
}

/// A steady run compacts every member's log each time this many entries were committed since
/// the last: the log stays bounded, as a member's log is (the thesis's §5.1 snapshot at a size),
/// so a long run measures the core and not a log that only grows. The same for every core.
pub const COMPACT_ENTRIES: usize = 4096;
/// The entries a catch-up or snapshot round commits while the follower is away.
pub const AWAY_ENTRIES: usize = 1024;

/// Runs `spec` on core `C` and says what it cost. `None` when the core lacks what the workload
/// needs (the fast track).
pub fn run<C: Core>(spec: &Spec, seed: u64) -> Option<Measured> {
    let state_bytes = match spec.workload {
        Workload::Snapshot => AWAY_ENTRIES * spec.bytes,
        _ => 0,
    };
    let settings = Settings::shell(spec.workload == Workload::Fast, state_bytes);
    let mut group: Group<C> = Group::new(spec.voters, &settings, seed);
    group.elect(1);
    match spec.workload {
        Workload::Steady => Some(steady(&mut group, spec)),
        Workload::Transfer => Some(transfer(&mut group, spec)),
        Workload::Failover => Some(failover(&mut group, spec)),
        Workload::CatchUp => Some(away(&mut group, spec, false)),
        Workload::Snapshot => Some(away(&mut group, spec, true)),
        Workload::Fast => fast(&mut group, spec),
    }
}

fn steady<C: Core>(group: &mut Group<C>, spec: &Spec) -> Measured {
    let compact_every = (COMPACT_ENTRIES / spec.batch).max(1);
    // Warm: the allocator and the members' queues reach their steady sizes.
    for _ in 0..compact_every {
        group.propose(1, spec.batch, spec.bytes);
    }
    group.compact();
    let mut meter = Meter::new();
    meter.start();
    for round in 0..spec.rounds {
        group.propose(1, spec.batch, spec.bytes);
        if (round + 1) % compact_every == 0 {
            group.compact();
        }
    }
    meter.stop();
    assert!(group.agreed(1), "{}: the members differ", C::NAME);
    meter.finish((spec.rounds * spec.batch) as u64)
}

fn transfer<C: Core>(group: &mut Group<C>, spec: &Spec) -> Measured {
    let members = spec.voters as u64;
    let mut leader = 1;
    let mut meter = Meter::new();
    for _ in 0..spec.rounds {
        let target = leader % members + 1;
        meter.start();
        {
            let Group { nodes, next, .. } = group;
            assert!(nodes[(leader - 1) as usize].transfer(target, next));
        }
        group.quiet();
        assert_eq!(
            group.leader(),
            Some(target),
            "{}: the lead was not handed over",
            C::NAME
        );
        group.propose(target, 1, spec.bytes);
        meter.stop();
        assert!(group.agreed(target), "{}: the members differ", C::NAME);
        leader = target;
        group.compact();
    }
    meter.finish(spec.rounds as u64)
}

fn failover<C: Core>(group: &mut Group<C>, spec: &Spec) -> Measured {
    let mut meter = Meter::new();
    for _ in 0..spec.rounds {
        let old = group.leader().expect("a leader");
        let term = group.nodes[(old - 1) as usize].term();
        meter.start();
        group.cut(old, true);
        group.periods_until("an election", |group| {
            group
                .leader()
                .is_some_and(|leader| group.nodes[(leader - 1) as usize].term() > term)
        });
        let new = group.leader().expect("a leader");
        group.propose(new, 1, spec.bytes);
        assert!(group.agreed(new), "{}: the members differ", C::NAME);
        group.cut(old, false);
        group.periods_until("the old leader's return", |group| {
            group.nodes[(old - 1) as usize].leader() == new && group.agreed(new)
        });
        meter.stop();
        group.compact();
    }
    meter.finish(spec.rounds as u64)
}

/// A follower away while the group commits, then back: caught up from the log, or, with
/// `compact`, from a snapshot.
fn away<C: Core>(group: &mut Group<C>, spec: &Spec, compact: bool) -> Measured {
    let follower = spec.voters as u64;
    let mut meter = Meter::new();
    for _ in 0..spec.rounds {
        group.cut(follower, true);
        for _ in 0..AWAY_ENTRIES / spec.batch {
            group.propose(1, spec.batch, spec.bytes);
        }
        if compact {
            group.compact();
        }
        meter.start();
        group.cut(follower, false);
        group.periods_until("the follower's catch-up", |group| group.agreed(1));
        meter.stop();
        group.compact();
    }
    let ops = if compact {
        spec.rounds
    } else {
        spec.rounds * AWAY_ENTRIES
    };
    meter.finish(ops as u64)
}

fn fast<C: Core>(group: &mut Group<C>, spec: &Spec) -> Option<Measured> {
    if !group.nodes[0].open_fast() {
        panic!("{}: the leader would not open the fast track", C::NAME);
    }
    // The members learn the track is open from the leader's next append.
    group.period();
    let proposer = 2u64;
    let compact_every = (COMPACT_ENTRIES / spec.batch).max(1);
    let round = |group: &mut Group<C>| -> Option<()> {
        for _ in 0..spec.batch {
            alloc::aside();
            let data = vec![0xa5u8; spec.bytes];
            alloc::back();
            let Group { nodes, next, .. } = &mut *group;
            match nodes[(proposer - 1) as usize].propose_fast(data, next) {
                Fast::Unsupported => return None,
                Fast::Refused => panic!("{}: a fast proposal refused", C::NAME),
                Fast::Proposed => {}
            }
        }
        group.quiet();
        // Every member learns the commit from the leader's next append.
        let Group { nodes, next, .. } = &mut *group;
        nodes[0].flush(next);
        group.quiet();
        Some(())
    };
    for _ in 0..compact_every {
        round(group)?;
    }
    group.compact();
    let mut meter = Meter::new();
    meter.start();
    for at in 0..spec.rounds {
        round(group)?;
        if (at + 1) % compact_every == 0 {
            group.compact();
        }
    }
    meter.stop();
    assert!(group.agreed(1), "{}: the members differ", C::NAME);
    Some(meter.finish((spec.rounds * spec.batch) as u64))
}
