//! What the test tells a member beyond hyper-raft-e2e's own controls (`wire::Control`: where its
//! peers listen, whether it is cut off), on the same framing and checksum: where to stop, a change
//! of configuration, a flush to fail or a disk to stall, and what the member is, at length. The
//! node-pair liveness heartbeats members send one another are framed as hyper-raft-e2e's members
//! frame theirs (`hyper_raft_e2e::stream`), at the tag past these orders'.
use hyper_raft::proto::ConfChangeType;
use hyper_raft_e2e::wire::{self, Kind, Reader, Status};

/// A named durability point: where a member stops, printing `stopped <name>`, for the test to kill
/// it with `SIGKILL` there (`docs/durable.md` §12).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Point {
    /// A drive submitted a write: it is on its way to the device, not yet durable.
    Submitted,
    /// The log answered a write durable, and the member has not yet taken the answer: what
    /// waited for it has not left, and its commit is not applied.
    Durable,
    /// A drive gave out messages, and they were sent.
    Released,
    /// A change of configuration waits behind the commit fence: committed, not applied, its
    /// commit not yet durable here.
    Fenced,
    /// A change of configuration was applied.
    Changed,
    /// The member acted on an entry it acts on at start.
    Acted,
}

impl Point {
    /// Its name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Submitted => "submitted",
            Self::Durable => "durable",
            Self::Released => "released",
            Self::Fenced => "fenced",
            Self::Changed => "changed",
            Self::Acted => "acted",
        }
    }
    fn byte(self) -> u8 {
        match self {
            Self::Submitted => 1,
            Self::Durable => 2,
            Self::Released => 3,
            Self::Fenced => 4,
            Self::Changed => 5,
            Self::Acted => 6,
        }
    }
    fn of(byte: u8) -> Option<Self> {
        Some(match byte {
            1 => Self::Submitted,
            2 => Self::Durable,
            3 => Self::Released,
            4 => Self::Fenced,
            5 => Self::Changed,
            6 => Self::Acted,
            _ => return None,
        })
    }
}

/// An order of this harness's own, by tags past hyper-raft-e2e's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Order {
    /// Stop at the `count`-th time the member passes `point` from now (one for the next).
    Arm(Point, u64),
    /// Propose a change of one member, as the leader.
    Change(ConfChangeType, u64),
    /// Fail the next flush of the member's log file.
    FailFlush,
    /// Say what the member is, at length ([`Report`]).
    Report,
    /// The log answered a write: the member's own waker, relayed to its socket.
    Wake,
    /// Stall the member's log file: no flush completes again, as a disk that stops.
    StallFlush,
}

/// The first tag this harness's orders take: past those of `wire::Control`.
const FIRST_TAG: u8 = 10;

/// Puts an order.
pub fn put_order(buffer: &mut Vec<u8>, id: u64, order: &Order) {
    wire::begin(buffer, Kind::Control);
    wire::put_u64(buffer, id);
    match order {
        Order::Arm(point, count) => {
            buffer.push(FIRST_TAG);
            buffer.push(point.byte());
            wire::put_u64(buffer, *count);
        }
        Order::Change(kind, member) => {
            buffer.push(FIRST_TAG.saturating_add(1));
            buffer.push(match kind {
                ConfChangeType::AddNode => 1,
                ConfChangeType::RemoveNode => 2,
                ConfChangeType::AddLearnerNode => 3,
            });
            wire::put_u64(buffer, *member);
        }
        Order::FailFlush => buffer.push(FIRST_TAG.saturating_add(2)),
        Order::Report => buffer.push(FIRST_TAG.saturating_add(3)),
        Order::Wake => buffer.push(FIRST_TAG.saturating_add(4)),
        Order::StallFlush => buffer.push(FIRST_TAG.saturating_add(5)),
    }
}

/// Reads an order's body; none for one of `wire::Control`'s.
pub fn read_order(body: &[u8]) -> Option<(u64, Order)> {
    let mut reader = Reader::new(body);
    let id = reader.u64()?;
    let tag = reader.u8()?.checked_sub(FIRST_TAG)?;
    let order = match tag {
        0 => Order::Arm(Point::of(reader.u8()?)?, reader.u64()?),
        1 => {
            let kind = match reader.u8()? {
                1 => ConfChangeType::AddNode,
                2 => ConfChangeType::RemoveNode,
                3 => ConfChangeType::AddLearnerNode,
                _ => return None,
            };
            Order::Change(kind, reader.u64()?)
        }
        2 => Order::FailFlush,
        3 => Order::Report,
        4 => Order::Wake,
        5 => Order::StallFlush,
        _ => return None,
    };
    Some((id, order))
}

/// What a member is, at length.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// What `wire::Status` says.
    pub status: Status,
    /// The durable commit, `C_d`.
    pub durable_commit: u64,
    /// Whether it leads and every voter's durable commit reaches its configuration's change.
    pub known: bool,
    /// The voters of its configuration.
    pub voters: Vec<u64>,
    /// The span its elections draw their delays over, nanoseconds; zero before its paths are
    /// measured (timing step L-2).
    pub span_ns: u64,
    /// The round tail its elections and beats run by, nanoseconds; zero before its paths are
    /// measured.
    pub round_ns: u64,
    /// The longest its detectors in force trust a peer past its last heartbeat's expected arrival,
    /// `η + α` over its pairs (`hyper_liveness::PairReport::freshness`), nanoseconds: how long a
    /// peer's crash goes unsuspected past its last heartbeat's delay; zero before any margin
    /// judges.
    pub detection_ns: u64,
    /// The heartbeats its node-pair stream has taken from its peers, all told.
    pub taken: u64,
    /// The pairs no margin judges yet: neither a configuration of their own nor the pool's.
    pub unjudged: u64,
    /// The longest interval the heartbeats of a pair no margin judges come at
    /// (`hyper_liveness::PairReport::interval`), nanoseconds: how long the evidence such a pair is
    /// to be judged from may go without moving; zero while none is heard and unjudged.
    pub unjudged_interval_ns: u64,
    /// The restarts of its peers its stream has seen.
    pub restarts: u64,
    /// The time it has had a write of its log out, all told, nanoseconds: time its group's
    /// progress through it waited on its device (`hyper_raft_e2e::quiet`).
    pub blocked_ns: u64,
    /// How long its oldest write still out has been out, nanoseconds; zero when none is.
    pub writing_ns: u64,
    /// The longest one write of its log took, from its submission to the answer taken,
    /// nanoseconds.
    pub flush_most_ns: u64,
    /// The longest it went between two reads of its socket, nanoseconds: the longest it could not
    /// answer.
    pub turn_most_ns: u64,
    /// The suspicions it told while a heartbeat of the peer's, stamped by the kernel before the
    /// suspicion's freshness point, was still unread in its socket (hyper-raft-e2e's
    /// `Report::unread`). Zero always.
    pub unread: u64,
    /// The peers its detectors suspect.
    pub suspected: Vec<u64>,
    /// The peers its stream has taken a heartbeat from.
    pub heard: Vec<u64>,
}

/// The response tag a report takes: past those of `wire::Outcome`.
const REPORT: u8 = 10;

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
        report.durable_commit,
        u64::from(report.known),
        report.span_ns,
        report.round_ns,
        report.detection_ns,
        report.taken,
        report.unjudged,
        report.unjudged_interval_ns,
        report.restarts,
        report.blocked_ns,
        report.writing_ns,
        report.flush_most_ns,
        report.turn_most_ns,
        report.unread,
        u64::try_from(report.voters.len()).unwrap_or(u64::MAX),
    ] {
        wire::put_u64(buffer, word);
    }
    for voter in &report.voters {
        wire::put_u64(buffer, *voter);
    }
    for list in [&report.suspected, &report.heard] {
        wire::put_u64(buffer, u64::try_from(list.len()).unwrap_or(u64::MAX));
        for peer in list {
            wire::put_u64(buffer, *peer);
        }
    }
}

/// The status a report begins with.
fn read_status(reader: &mut Reader<'_>) -> Option<Status> {
    Some(Status {
        id: reader.u64()?,
        term: reader.u64()?,
        leads: reader.u64()? != 0,
        leader: reader.u64()?,
        commit: reader.u64()?,
        applied: reader.u64()?,
        last_index: reader.u64()?,
        digest: reader.u64()?,
    })
}

/// Reads a report from a response's body; at most `max_voters` voters and as many suspected.
pub fn read_report(body: &[u8], max_voters: usize) -> Option<(u64, Report)> {
    let mut reader = Reader::new(body);
    let id = reader.u64()?;
    if reader.u8()? != REPORT {
        return None;
    }
    let status = read_status(&mut reader)?;
    let mut words = [0u64; 14];
    for word in &mut words {
        *word = reader.u64()?;
    }
    let [
        durable_commit,
        known,
        span_ns,
        round_ns,
        detection_ns,
        taken,
        unjudged,
        unjudged_interval_ns,
        restarts,
        blocked_ns,
        writing_ns,
        flush_most_ns,
        turn_most_ns,
        unread,
    ] = words;
    let list = |reader: &mut Reader<'_>| {
        let count = usize::try_from(reader.u64()?).ok()?;
        if count > max_voters {
            return None;
        }
        (0..count)
            .map(|_| reader.u64())
            .collect::<Option<Vec<u64>>>()
    };
    let voters = list(&mut reader)?;
    let suspected = list(&mut reader)?;
    let heard = list(&mut reader)?;
    Some((
        id,
        Report {
            status,
            durable_commit,
            known: known != 0,
            voters,
            span_ns,
            round_ns,
            detection_ns,
            taken,
            unjudged,
            unjudged_interval_ns,
            restarts,
            blocked_ns,
            writing_ns,
            flush_most_ns,
            turn_most_ns,
            unread,
            suspected,
            heard,
        },
    ))
}
