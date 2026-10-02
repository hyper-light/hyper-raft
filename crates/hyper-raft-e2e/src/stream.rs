//! A member's node-pair liveness stream (`hyper_liveness`, timing step L-3) as the harness carries
//! it: the heartbeats members send one another, what the stream asks of its member, the election
//! timing hyper-timing's law derives from what the stream measured, and the report a member gives
//! the test of its law and its detectors.
//!
//! Heartbeats go as the test's control datagrams, framed as hyper-durable-e2e frames its own, so
//! the two harnesses' members carry the stream alike.
use std::time::Duration;

use hyper_liveness::{Change, Liveness, Output, PeerId};
use hyper_raft::Timing;
use hyper_timing::{Ballot, Span};

use crate::wire::{self, Kind, Reader, Status};

/// The control tag a heartbeat takes: past the test's instructions and hyper-durable-e2e's
/// orders (`wire::Control` takes 1 and 2, hyper-durable-e2e's orders 10 to 15), the tag
/// hyper-durable-e2e's heartbeats take.
const HEARTBEAT: u8 = 16;
/// The control tag of the test's ask for a member's [`Report`]: the next past `wire::Control`'s.
const REPORT_ASK: u8 = 3;
/// The response tag a [`Report`] takes: the next past `wire::Outcome`'s.
const REPORT: u8 = 7;

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
    let granularity = liveness.granularity().filter(|g| !g.is_zero())?;
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
    let mut words = [0u64; 8];
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
            suspected,
            heard,
        },
    ))
}
