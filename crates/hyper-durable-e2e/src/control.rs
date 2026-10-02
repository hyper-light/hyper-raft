//! What the test tells a member beyond hyper-raft-e2e's own controls (`wire::Control`: where its
//! peers listen, whether it is cut off), on the same framing and checksum: where to stop, a change
//! of configuration, a flush to fail, what its failure detectors believe of its peers, and what the
//! member is, at length; and the probes members time their paths by.
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
    /// The member's detectors suspect this member's node (timing step L-2). The test kills
    /// members, so it knows: it is the members' detector until L-3's stream and L-4's harness.
    Suspect(u64),
    /// The member's detectors trust this member's node again.
    Trust(u64),
    /// The member's detectors saw this member's node start again, a new incarnation.
    Restarted(u64),
    /// A peer times its path to this member: answered at once with the same stamp.
    Probe(u64),
    /// The answer to a probe stamped as it was sent, on the prober's clock.
    Echo(u64),
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
        Order::Suspect(word) => tagged(buffer, 5, *word),
        Order::Trust(word) => tagged(buffer, 6, *word),
        Order::Restarted(word) => tagged(buffer, 7, *word),
        Order::Probe(word) => tagged(buffer, 8, *word),
        Order::Echo(word) => tagged(buffer, 9, *word),
    }
}

/// An order's tag, `FIRST_TAG` and `offset`, and its one word.
fn tagged(buffer: &mut Vec<u8>, offset: u8, word: u64) {
    buffer.push(FIRST_TAG.saturating_add(offset));
    wire::put_u64(buffer, word);
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
        5 => Order::Suspect(reader.u64()?),
        6 => Order::Trust(reader.u64()?),
        7 => Order::Restarted(reader.u64()?),
        8 => Order::Probe(reader.u64()?),
        9 => Order::Echo(reader.u64()?),
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
        u64::try_from(report.voters.len()).unwrap_or(u64::MAX),
    ] {
        wire::put_u64(buffer, word);
    }
    for voter in &report.voters {
        wire::put_u64(buffer, *voter);
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

/// Reads a report from a response's body; at most `max_voters` voters.
pub fn read_report(body: &[u8], max_voters: usize) -> Option<(u64, Report)> {
    let mut reader = Reader::new(body);
    let id = reader.u64()?;
    if reader.u8()? != REPORT {
        return None;
    }
    let status = read_status(&mut reader)?;
    let durable_commit = reader.u64()?;
    let known = reader.u64()? != 0;
    let span_ns = reader.u64()?;
    let round_ns = reader.u64()?;
    let count = usize::try_from(reader.u64()?).ok()?;
    if count > max_voters {
        return None;
    }
    let voters = (0..count)
        .map(|_| reader.u64())
        .collect::<Option<Vec<u64>>>()?;
    Some((
        id,
        Report {
            status,
            durable_commit,
            known,
            voters,
            span_ns,
            round_ns,
        },
    ))
}
