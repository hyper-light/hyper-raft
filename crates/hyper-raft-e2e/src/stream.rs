//! A member's node-pair liveness stream (`hyper_liveness`, timing step L-3) as the harness carries
//! it: the heartbeats members send one another, what the stream asks of its member, the election
//! timing hyper-timing's law derives from what the stream measured, and the report a member gives
//! the test of its law and its detectors.
//!
//! Heartbeats go as the test's control datagrams, framed here for both harnesses: hyper-durable-e2e's
//! members send and read theirs with [`put_heartbeat`] and [`read_heartbeat`], so the two harnesses'
//! members carry the stream alike.
use std::time::Duration;

use hyper_liveness::{Change, Liveness, Output, PeerId};
use hyper_raft::Timing;
use hyper_timing::{Ballot, Span};

use crate::wire::{self, Kind, Reader, Status};

/// The control tag a heartbeat takes, in both harnesses: past the test's instructions and
/// hyper-durable-e2e's orders (`wire::Control` takes 1 and 2, hyper-durable-e2e's orders 10 to 15).
const HEARTBEAT: u8 = 16;
/// The control tag of the test's ask for a member's [`Report`]: the next past `wire::Control`'s.
const REPORT_ASK: u8 = 3;
/// The response tag a [`Report`] takes: the next past `wire::Outcome`'s.
const REPORT: u8 = 7;
/// The control tag of the test's ask for a member's [`Account`] of its detectors.
const ACCOUNT_ASK: u8 = 4;
/// The control tag of the test's order that a member's device answer no flush for a time.
const STALL: u8 = 5;
/// The control tag of the test's order that a member hold its thread, outside any write of its
/// log, until released.
const HOLD: u8 = 6;
/// The response tag an [`Account`] takes.
const ACCOUNT: u8 = 8;

/// Puts a node-pair liveness heartbeat from member `from`, its message as `hyper_liveness` made it.
pub fn put_heartbeat(buffer: &mut Vec<u8>, from: u64, message: &[u8]) {
    wire::begin(buffer, Kind::Control);
    wire::put_u64(buffer, from);
    buffer.push(HEARTBEAT);
    buffer.extend_from_slice(message);
}

/// Reads a heartbeat's sender and message; none for anything else.
pub fn read_heartbeat(body: &[u8]) -> Option<(u64, &[u8])> {
    let mut reader = Reader::new(body);
    let from = reader.u64()?;
    if reader.u8()? != HEARTBEAT {
        return None;
    }
    Some((from, reader.rest()))
}

/// Puts the test's ask for a member's report.
pub fn put_report_ask(buffer: &mut Vec<u8>, id: u64) {
    wire::begin(buffer, Kind::Control);
    wire::put_u64(buffer, id);
    buffer.push(REPORT_ASK);
}

/// The id of an ask for a report; none for anything else.
pub fn read_report_ask(body: &[u8]) -> Option<u64> {
    let mut reader = Reader::new(body);
    let id = reader.u64()?;
    (reader.u8()? == REPORT_ASK && reader.rest().is_empty()).then_some(id)
}

/// Puts the test's ask for a member's account of its detectors.
pub fn put_account_ask(buffer: &mut Vec<u8>, id: u64) {
    wire::begin(buffer, Kind::Control);
    wire::put_u64(buffer, id);
    buffer.push(ACCOUNT_ASK);
}

/// The id of an ask for an account; none for anything else.
pub fn read_account_ask(body: &[u8]) -> Option<u64> {
    let mut reader = Reader::new(body);
    let id = reader.u64()?;
    (reader.u8()? == ACCOUNT_ASK && reader.rest().is_empty()).then_some(id)
}

/// Puts the test's order that a member's device answer no flush for `stall`, answered `Done`.
pub fn put_stall(buffer: &mut Vec<u8>, id: u64, stall: Duration) {
    wire::begin(buffer, Kind::Control);
    wire::put_u64(buffer, id);
    buffer.push(STALL);
    wire::put_u64(buffer, u64::try_from(stall.as_nanos()).unwrap_or(u64::MAX));
}

/// The id and the stall of an order that a member's device answer no flush for a time; none for
/// anything else.
pub fn read_stall(body: &[u8]) -> Option<(u64, Duration)> {
    let mut reader = Reader::new(body);
    let id = reader.u64()?;
    if reader.u8()? != STALL {
        return None;
    }
    let stall = Duration::from_nanos(reader.u64()?);
    reader.rest().is_empty().then_some((id, stall))
}

/// Puts the test's order that a member hold its thread, outside any write of its log, until a
/// byte comes on its standard input, once it has answered `Done`: a member that stays up and
/// answers nothing, as one deadlocked does, on a platform with no signal that stops a process
/// (Windows).
pub fn put_hold(buffer: &mut Vec<u8>, id: u64) {
    wire::begin(buffer, Kind::Control);
    wire::put_u64(buffer, id);
    buffer.push(HOLD);
}

/// The id of an order that a member hold its thread; none for anything else.
pub fn read_hold(body: &[u8]) -> Option<u64> {
    let mut reader = Reader::new(body);
    let id = reader.u64()?;
    (reader.u8()? == HOLD && reader.rest().is_empty()).then_some(id)
}

/// What the node-pair stream asked of its member during one call into it.
#[derive(Default)]
pub struct Asked {
    /// Heartbeats to send, each to its peer.
    pub heartbeats: Vec<(PeerId, Vec<u8>)>,
    /// A durable write to make on the member's log.
    pub flush: bool,
    /// Changes of trust, for the member's core.
    pub changes: Vec<Change>,
}

impl Output for Asked {
    fn heartbeat(&mut self, peer: PeerId, message: &[u8]) {
        self.heartbeats.push((peer, message.to_vec()));
    }
    fn flush(&mut self) {
        self.flush = true;
    }
    fn change(&mut self, change: Change) {
        self.changes.push(change);
    }
}

/// The group's election timing by hyper-timing's law over what the stream measured
/// (`docs/timing.md` §2.3, §2.9), as hyper-durable's `Replica::measure` derives it: the ballot
/// over the echoed round trips to the other `voters` (`Liveness::round_trip`), the mean flush of
/// the member's log writes (`Liveness::flush_mean`; a member's log writes are its vote writes
/// too), and the member's measured timer granularity; and the span the law chose, whose expected
/// election `T_E` each pair is charged. None before a quorum's paths and the granularity are
/// measured: the core then draws no delay and does not campaign (§3, item 10).
pub fn timing(liveness: &Liveness, id: u64, voters: &[u64]) -> Option<(Timing, Span)> {
    let granularity = liveness.granularity()?;
    let paths = voters
        .iter()
        .filter(|voter| **voter != id)
        .filter_map(|voter| liveness.round_trip(*voter));
    let durable = liveness.flush_mean().unwrap_or(Duration::ZERO);
    let ballot = Ballot::measure(paths, voters.len(), durable, granularity)?;
    let span = ballot.span(granularity)?;
    Some((Timing::of(&ballot, &span), span))
}

/// What a member says of itself, its law and its detectors, for the test to wait on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// What `wire::Status` says.
    pub status: Status,
    /// The span its elections draw their delays over, nanoseconds; zero before its paths are
    /// measured.
    pub span_ns: u64,
    /// The round tail its elections and beats run by, nanoseconds; zero before its paths are
    /// measured.
    pub round_ns: u64,
    /// The longest its detectors in force trust a peer past its last heartbeat's expected
    /// arrival, `η + α` over its pairs (`hyper_liveness::PairReport::freshness`), nanoseconds;
    /// zero before any margin judges.
    pub detection_ns: u64,
    /// The heartbeats its stream has taken from its peers, all told.
    pub taken: u64,
    /// The pairs no margin judges yet: neither a configuration of their own nor the pool's.
    pub unjudged: u64,
    /// The longest interval the heartbeats of a pair no margin judges come at
    /// (`hyper_liveness::PairReport::interval`), nanoseconds: how long the evidence such a pair is
    /// waiting for can go without a heartbeat; zero when every pair is judged.
    pub unjudged_interval_ns: u64,
    /// The writes its durable log holds: its entries that are not a leader's empty one. A leader
    /// proposes no write its log holds, so each is held once.
    pub writes: u64,
    /// The restarts of its peers its stream has seen.
    pub restarts: u64,
    /// The time its thread has spent in the writes of its log, all told, nanoseconds: time it
    /// could neither answer nor move its group.
    pub blocked_ns: u64,
    /// The longest one write of its log took to be durable, nanoseconds.
    pub flush_most_ns: u64,
    /// The longest it went between two reads of its socket, nanoseconds: the longest it could not
    /// answer.
    pub turn_most_ns: u64,
    /// The askers it keeps waiting, writes and reads.
    pub waiting: u64,
    /// The writes it keeps waiting that no apply will answer: with no entry above what it
    /// applied in the log of the term it leads that writes the value asked, or kept while it
    /// leads no term. Zero always; any other count is a write that waits for good.
    pub stray: u64,
    /// The suspicions it told while a heartbeat of the peer's, stamped by the kernel before the
    /// suspicion's freshness point, was still unread in its socket: each found when that heartbeat
    /// is taken. Zero always: the stream judges by the heartbeats it was given, and is polled only
    /// at a clock read before a drain that emptied the socket (`Liveness::poll`); any other count
    /// is a suspicion the member's own reading made.
    pub unread: u64,
    /// The peers its detectors suspect.
    pub suspected: Vec<u64>,
    /// The peers its stream has taken a heartbeat from.
    pub heard: Vec<u64>,
}

/// Puts a report as a response to `id`.
pub fn put_report(buffer: &mut Vec<u8>, id: u64, report: &Report) {
    wire::begin(buffer, Kind::Response);
    wire::put_u64(buffer, id);
    buffer.push(REPORT);
    let s = &report.status;
    for word in [
        s.id,
        s.term,
        u64::from(s.leads),
        s.leader,
        s.commit,
        s.applied,
        s.last_index,
        s.digest,
        report.span_ns,
        report.round_ns,
        report.detection_ns,
        report.taken,
        report.unjudged,
        report.unjudged_interval_ns,
        report.writes,
        report.restarts,
        report.blocked_ns,
        report.flush_most_ns,
        report.turn_most_ns,
        report.waiting,
        report.stray,
        report.unread,
    ] {
        wire::put_u64(buffer, word);
    }
    for list in [&report.suspected, &report.heard] {
        wire::put_u64(buffer, u64::try_from(list.len()).unwrap_or(u64::MAX));
        for peer in list {
            wire::put_u64(buffer, *peer);
        }
    }
}

/// Reads a report from a response's body; at most `max_peers` peers in each list.
pub fn read_report(body: &[u8], max_peers: usize) -> Option<(u64, Report)> {
    let mut reader = Reader::new(body);
    let id = reader.u64()?;
    if reader.u8()? != REPORT {
        return None;
    }
    let status = Status {
        id: reader.u64()?,
        term: reader.u64()?,
        leads: reader.u64()? != 0,
        leader: reader.u64()?,
        commit: reader.u64()?,
        applied: reader.u64()?,
        last_index: reader.u64()?,
        digest: reader.u64()?,
    };
    let mut words = [0u64; 14];
    for word in &mut words {
        *word = reader.u64()?;
    }
    let [
        span_ns,
        round_ns,
        detection_ns,
        taken,
        unjudged,
        unjudged_interval_ns,
        writes,
        restarts,
        blocked_ns,
        flush_most_ns,
        turn_most_ns,
        waiting,
        stray,
        unread,
    ] = words;
    let mut list = || {
        let count = usize::try_from(reader.u64()?).ok()?;
        if count > max_peers {
            return None;
        }
        (0..count)
            .map(|_| reader.u64())
            .collect::<Option<Vec<u64>>>()
    };
    let suspected = list()?;
    let heard = list()?;
    Some((
        id,
        Report {
            status,
            span_ns,
            round_ns,
            detection_ns,
            taken,
            unjudged,
            unjudged_interval_ns,
            writes,
            restarts,
            blocked_ns,
            flush_most_ns,
            turn_most_ns,
            waiting,
            stray,
            unread,
            suspected,
            heard,
        },
    ))
}

/// What one of a member's pairs measured, the detector it runs, and its suspicions against the
/// allowance its detectors promised (`hyper_liveness::PairReport`, `hyper_timing::Configuration`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PairAccount {
    /// The peer.
    pub peer: u64,
    /// Whether a configuration of the pair's own judges it.
    pub configured: bool,
    /// Whether a margin judges it, its own or its node's.
    pub judged: bool,
    /// Heartbeats sent to the peer.
    pub sent: u64,
    /// Slots of the stream to the peer this member was behind for and never sent.
    pub skipped: u64,
    /// Heartbeats taken from the peer.
    pub taken: u64,
    /// Heartbeats from the peer refused for their flush proof.
    pub unproven: u64,
    /// Configurations made.
    pub configurations: u64,
    /// Suspicions of the peer.
    pub suspicions: u64,
    /// The allowance for them: the suspicions expected were the peer alive throughout.
    pub allowance: f64,
    /// What the configurator was last fed: the chance the next arrival's lateness is past every
    /// one seen.
    pub unseen: f64,
    /// The mean lateness of an arrival past its expected arrival, nanoseconds.
    pub lateness_ns: u64,
    /// Its standard deviation, nanoseconds.
    pub deviation_ns: u64,
    /// The detector in force: `η`, nanoseconds.
    pub interval_ns: u64,
    /// `α`, nanoseconds.
    pub margin_ns: u64,
    /// The mistake recurrence it promises, `η / β`, nanoseconds.
    pub recurrence_ns: u64,
    /// The unavailability `U` it was chosen for.
    pub unavailability: f64,
}

/// What a member's detectors measured and were configured to: its node's floors and the expected
/// election each pair is charged, and each pair's account.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Account {
    /// `G`, the measured lateness of the member's wakes, nanoseconds.
    pub granularity_ns: u64,
    /// `E[flush]`, nanoseconds.
    pub flush_ns: u64,
    /// `T_E`, the expected election each pair is charged, nanoseconds; zero before it is derived.
    pub election_ns: u64,
    /// Each pair.
    pub pairs: Vec<PairAccount>,
}

/// The account of `liveness`'s pairs with `peers`, each charged `election`.
pub fn account(liveness: &Liveness, peers: &[PeerId], election: Option<Duration>) -> Account {
    let nanos = |d: Duration| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX);
    let pairs = peers
        .iter()
        .filter_map(|peer| {
            let report = liveness.report(*peer)?;
            let configuration = liveness.configuration(*peer);
            let link = configuration.map(|c| c.link);
            let current = configuration.map(|c| c.current);
            Some(PairAccount {
                peer: *peer,
                configured: report.configured,
                judged: report.judged,
                sent: report.sent,
                skipped: report.skipped,
                taken: report.taken,
                unproven: report.unproven,
                configurations: report.configurations,
                suspicions: report.suspicions,
                allowance: report.allowance,
                unseen: link.map_or(0.0, |l| l.unseen),
                lateness_ns: link.map_or(0, |l| nanos(l.lateness)),
                deviation_ns: link.map_or(0, |l| nanos(l.deviation)),
                interval_ns: current.map_or(0, |d| nanos(d.interval)),
                margin_ns: current.map_or(0, |d| nanos(d.margin)),
                recurrence_ns: current.map_or(0, |d| nanos(d.mistake_recurrence)),
                unavailability: current.map_or(0.0, |d| d.unavailability),
            })
        })
        .collect();
    Account {
        granularity_ns: liveness.granularity().map_or(0, nanos),
        flush_ns: liveness.flush_mean().map_or(0, nanos),
        election_ns: election.map_or(0, nanos),
        pairs,
    }
}

/// Puts an account as a response to `id`.
pub fn put_account(buffer: &mut Vec<u8>, id: u64, account: &Account) {
    wire::begin(buffer, Kind::Response);
    wire::put_u64(buffer, id);
    buffer.push(ACCOUNT);
    for word in [
        account.granularity_ns,
        account.flush_ns,
        account.election_ns,
        u64::try_from(account.pairs.len()).unwrap_or(u64::MAX),
    ] {
        wire::put_u64(buffer, word);
    }
    for pair in &account.pairs {
        for word in [
            pair.peer,
            u64::from(pair.configured),
            u64::from(pair.judged),
            pair.sent,
            pair.skipped,
            pair.taken,
            pair.unproven,
            pair.configurations,
            pair.suspicions,
            pair.allowance.to_bits(),
            pair.unseen.to_bits(),
            pair.lateness_ns,
            pair.deviation_ns,
            pair.interval_ns,
            pair.margin_ns,
            pair.recurrence_ns,
            pair.unavailability.to_bits(),
        ] {
            wire::put_u64(buffer, word);
        }
    }
}

/// Reads an account from a response's body; at most `max_peers` pairs.
pub fn read_account(body: &[u8], max_peers: usize) -> Option<(u64, Account)> {
    let mut reader = Reader::new(body);
    let id = reader.u64()?;
    if reader.u8()? != ACCOUNT {
        return None;
    }
    let granularity_ns = reader.u64()?;
    let flush_ns = reader.u64()?;
    let election_ns = reader.u64()?;
    let count = usize::try_from(reader.u64()?).ok()?;
    if count > max_peers {
        return None;
    }
    let mut pairs = Vec::with_capacity(count);
    for _ in 0..count {
        let mut words = [0u64; 17];
        for word in &mut words {
            *word = reader.u64()?;
        }
        let [
            peer,
            configured,
            judged,
            sent,
            skipped,
            taken,
            unproven,
            configurations,
            suspicions,
            allowance,
            unseen,
            lateness_ns,
            deviation_ns,
            interval_ns,
            margin_ns,
            recurrence_ns,
            unavailability,
        ] = words;
        pairs.push(PairAccount {
            peer,
            configured: configured != 0,
            judged: judged != 0,
            sent,
            skipped,
            taken,
            unproven,
            configurations,
            suspicions,
            allowance: f64::from_bits(allowance),
            unseen: f64::from_bits(unseen),
            lateness_ns,
            deviation_ns,
            interval_ns,
            margin_ns,
            recurrence_ns,
            unavailability: f64::from_bits(unavailability),
        });
    }
    Some((
        id,
        Account {
            granularity_ns,
            flush_ns,
            election_ns,
            pairs,
        },
    ))
}
