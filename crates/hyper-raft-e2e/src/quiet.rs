//! The rule both E2E harnesses' waits keep (`docs/timing.md` §2.9): a wait goes on while the group
//! moves, and fails once a quiet period of the members' own law passes with nothing moved. Quiet is
//! only time in which the test heard every member.
//!
//! - **A look** asks every member up for its report. A look that did not hear a member decides
//!   nothing: the member is waited on while its process runs, and its answer afterwards counts as
//!   movement. A member's thread answers nothing while it waits on its log, and a device a
//!   machine's processes share holds every member's writes at once (Docker Desktop's virtual
//!   machine held every member's flush 1.8 s together), so a look that heard no one is no evidence
//!   that the group stopped.
//! - **The time in writes.** The time the members report spending in their writes since the watch
//!   last heard them extends the watch by the most any one of them spent: a member whose write is
//!   out moves nothing through itself, whatever its thread does meanwhile.
//! - **Silence.** A member's silence, from its first unanswered ask to its latest less the
//!   retransmission timeout the test waited on the latest (an ask whose answer was lost costs the
//!   test, not the member), is excused only up to the longest one write any member has reported, or
//!   the stall the test ordered, and the quiet period. Past it the member answers nothing, whatever
//!   holds it, and the wait fails naming it rather than waiting for good. So is a member that
//!   answers while its oldest write has been out that long, less the same timeout
//!   (hyper-durable-e2e's, whose log's threads do its writes): its group moves through it only once
//!   the write is durable, a write that never ends would otherwise extend the watch for good, and
//!   the timeout keeps the write judged as a member's thread held in it would be, by a look whose
//!   ask waits that long for its answer.
//! - **The device.** Before a member is judged held, the harness flushes its own file on the
//!   members' device ([`crate::device::Probe`]) and states the flush's time ([`Quiet::device`]),
//!   which joins what excuses a write: a write slower than any before it is a member's only if the
//!   device answers the test's flush faster. A device that answers no flush within the kernel's
//!   own bound has failed ([`Stuck::Device`]).
//!
//! The harnesses feed it what their looks saw ([`Quiet::look`]) and the asks they made
//! ([`Quiet::asked`]); it reads no clock of its own.
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use crate::wire::Status;

/// RFC 6298 §2.1 and §2.4: the retransmission timeout before any round trip is measured, and the
/// least it is ever set to after, one second. An ask waits this long for its answer, and no quiet
/// period is shorter: past it the test could not tell a member that did not move from an answer it
/// did not wait for.
pub const RTO: Duration = Duration::from_secs(1);
/// The rounds an ask and its answer take beside an election: the ask, the broadcast that commits
/// it, and the answer.
pub const ANSWER_ROUNDS: u32 = 3;
/// The rounds an election takes past its delay: pre-vote, vote, and the new leader's first append
/// (`docs/timing.md` §2.3).
pub const ELECTION_ROUNDS: u32 = 3;

/// How long a member's law lets its group go with nothing moving, from what its report states: the
/// longest its detectors take to suspect a crash (`η + α` over its pairs), or, while a pair no
/// margin judges takes heartbeats at a longer interval, that interval, the most a wait that goes on
/// while those heartbeats move can see none; then its election's span and rounds, and an ask's
/// rounds. A live group suspects a dead leader within its stated detection, elects or starts a new
/// term within an election, and answers within an ask's rounds.
pub fn law(detection_ns: u64, unjudged_interval_ns: u64, span_ns: u64, round_ns: u64) -> Duration {
    let rounds = Duration::from_nanos(round_ns)
        .saturating_mul(ELECTION_ROUNDS.saturating_add(ANSWER_ROUNDS));
    Duration::from_nanos(detection_ns.max(unjudged_interval_ns))
        .saturating_add(Duration::from_nanos(span_ns))
        .saturating_add(rounds)
}

/// What a wait watches move in one member's report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    /// Its term.
    pub term: u64,
    /// What it knows committed.
    pub commit: u64,
    /// What it applied.
    pub applied: u64,
    /// The last index its log holds.
    pub last_index: u64,
    /// The restarts of its peers its stream has seen.
    pub restarts: u64,
    /// Its pairs no margin judges yet.
    pub unjudged: u64,
    /// While it has a pair no margin judges, the heartbeats its stream has taken: the evidence its
    /// detectors are built from; zero once every pair is judged.
    pub judging: u64,
}

impl Progress {
    /// The progress a member's report states: its `status`, the restarts its stream has seen, its
    /// pairs no margin judges and the heartbeats its stream has taken.
    pub fn of(status: &Status, restarts: u64, unjudged: u64, taken: u64) -> Self {
        Self {
            term: status.term,
            commit: status.commit,
            applied: status.applied,
            last_index: status.last_index,
            restarts,
            unjudged,
            judging: if unjudged > 0 { taken } else { 0 },
        }
    }
}

/// A member a look heard.
#[derive(Clone, Copy, Debug)]
pub struct Heard {
    /// The member.
    pub id: u64,
    /// What its report states of its progress.
    pub progress: Progress,
    /// The time it says it has spent in the writes of its log, all told, nanoseconds.
    pub blocked_ns: u64,
    /// How long its oldest write still out has been out, nanoseconds: zero for a member whose
    /// thread makes its writes, which answers only between them.
    pub writing_ns: u64,
}

/// What a wait last saw of each member, and until when it waits without seeing more.
#[derive(Debug)]
pub struct Watch {
    seen: BTreeMap<u64, Progress>,
    /// What each member said it had spent in its writes when this watch last heard it.
    blocked: BTreeMap<u64, u64>,
    /// The members a look of this watch did not hear since it last heard them.
    unheard: BTreeSet<u64>,
    until: Instant,
}

/// Why a wait gave up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stuck {
    /// Nothing moved for the quiet period, in looks that heard every member.
    Quiet(Duration),
    /// A member answered nothing for `silence`, past the `excuse` the members' longest write (or
    /// the stall the test ordered) and the quiet period make.
    Silent {
        /// The member.
        member: u64,
        /// How long it left the test's asks unanswered.
        silence: Duration,
        /// What the members' own measures excused.
        excuse: Duration,
    },
    /// A member answered with a write out for `writing`, which less a retransmission timeout is
    /// past the `excuse` the members' longest write (or the stall the test ordered) and the quiet
    /// period make.
    Held {
        /// The member.
        member: u64,
        /// How long its oldest write still out had been out.
        writing: Duration,
        /// What the members' own measures excused.
        excuse: Duration,
    },
    /// The members' device answered no flush of the test's own for `waited`, the kernel's own
    /// bound on a flush ([`crate::device::FLUSH_BOUND`]): the device failed, not a member.
    Device {
        /// How long the test's flush was waited for.
        waited: Duration,
    },
}

impl std::fmt::Display for Stuck {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Quiet(period) => write!(f, "nothing moved for {period:?}"),
            Self::Silent {
                member,
                silence,
                excuse,
            } => write!(
                f,
                "member {member} answered nothing for {silence:?}, past the {excuse:?} the members' longest write and the quiet period excuse"
            ),
            Self::Held {
                member,
                writing,
                excuse,
            } => write!(
                f,
                "member {member} had a write out for {writing:?}, past a retransmission timeout and the {excuse:?} the members' longest write and the quiet period excuse"
            ),
            Self::Device { waited } => write!(
                f,
                "the members' device answered no flush of the test's for {waited:?}, the kernel's own bound on a flush"
            ),
        }
    }
}

/// What the looks of a test have seen, for its dumps: the looks that did not hear every member,
/// the longest silence and what was excused then, and how far waits were extended for members in
/// their writes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Seen {
    /// Looks in which a member up did not answer.
    pub unheard_looks: u64,
    /// The longest a look found a member silent.
    pub silence_most: Duration,
    /// What the members' measures excused when it did.
    pub excused_then: Duration,
    /// The time waits were extended for members in their writes, all told, nanoseconds.
    pub extended_ns: u64,
    /// The most one look extended a wait, nanoseconds.
    pub extended_most_ns: u64,
    /// The longest a look heard a member's oldest write out.
    pub writing_most: Duration,
}

/// The test's account of its members for its waits: each one's law from its latest report, the
/// asks each left unanswered, the longest write any has reported, the stall the test ordered, and
/// what its looks have seen. One a test, as the members it starts are; a member is forgotten when
/// it goes ([`Quiet::gone`]), so it holds no more than the group's members.
#[derive(Debug, Default)]
pub struct Quiet {
    law: BTreeMap<u64, Duration>,
    /// For each member that left asks unanswered since it last answered: when the first of them
    /// was sent, and when the latest gave up.
    unanswered: BTreeMap<u64, (Instant, Instant)>,
    /// The longest one write any member has reported, nanoseconds.
    write_most: u64,
    stall: Duration,
    /// The latest flush of the test's own on the members' device took this long ([`Quiet::device`]).
    device: Duration,
    seen: Seen,
}

/// `at` moved on by `by`; `at` itself where the clock cannot hold the sum.
fn later(at: Instant, by: Duration) -> Instant {
    at.checked_add(by).unwrap_or(at)
}

impl Quiet {
    /// An account with no member in it.
    pub fn new() -> Self {
        Self::default()
    }

    /// An ask to `id` sent at `sent` was answered (`gave_up` none), or given up at `gave_up`
    /// without its answer.
    pub fn asked(&mut self, id: u64, sent: Instant, gave_up: Option<Instant>) {
        match gave_up {
            None => {
                self.unanswered.remove(&id);
            }
            Some(gave_up) => {
                self.unanswered
                    .entry(id)
                    .and_modify(|(_, latest)| *latest = gave_up)
                    .or_insert((sent, gave_up));
            }
        }
    }

    /// Member `id` reported: what its law takes ([`law`]) and the longest one write of its log.
    pub fn reported(&mut self, id: u64, law: Duration, write_most_ns: u64) {
        self.law.insert(id, law);
        self.write_most = self.write_most.max(write_most_ns);
    }

    /// Member `id` went (killed, or ended as the test expected): its law and its asks go with it.
    pub fn gone(&mut self, id: u64) {
        self.law.remove(&id);
        self.unanswered.remove(&id);
    }

    /// The test's own flush on the members' device took `took` ([`crate::device::Probe`]): a write
    /// that long is the device's, not a member's.
    pub fn device(&mut self, took: Duration) {
        self.device = took;
    }

    /// The test ordered every member's device to answer no flush for `stall`: a silence that long
    /// is the test's own doing.
    pub fn order_stall(&mut self, stall: Duration) {
        self.stall = self.stall.max(stall);
    }

    /// The longest one write any member has reported.
    pub fn write_most(&self) -> Duration {
        Duration::from_nanos(self.write_most)
    }

    /// How long the group may go with nothing moving before a wait gives up: the longest any
    /// member's law takes, from its latest report, and never less than a retransmission timeout. A
    /// wait has no count of elections: it goes on while the group moves.
    pub fn period(&self) -> Duration {
        self.law.values().copied().max().unwrap_or(RTO).max(RTO)
    }

    /// What the looks have seen so far.
    pub fn seen(&self) -> Seen {
        self.seen
    }

    /// A fresh watch over the group's progress, from `now`.
    pub fn watch(&self, now: Instant) -> Watch {
        Watch {
            seen: BTreeMap::new(),
            blocked: BTreeMap::new(),
            unheard: BTreeSet::new(),
            until: later(now, self.period()),
        }
    }

    /// Judges a look begun at `looked` and ended at `now`, which heard the members in `heard` and
    /// not those in `unheard`, whose processes run. The watch is extended by a quiet period from
    /// `now` when any member heard moved, or answered after a look that did not hear it; by the
    /// most any member heard spent in its writes since the watch last heard it otherwise. The look
    /// decides only if it heard every member and began after the watch's end: a look begun before
    /// counts as movement unseen, for an ask whose answer was lost spends the test's own timeout,
    /// not the group's. A member unheard, or heard with its oldest write out, past what the
    /// members' measures excuse ends the wait.
    pub fn look(
        &mut self,
        watch: &mut Watch,
        looked: Instant,
        now: Instant,
        heard: &[Heard],
        unheard: &[u64],
    ) -> Result<(), Stuck> {
        let mut moved = false;
        let mut excused = 0u64;
        let mut held = None;
        for member in heard {
            let writing = Duration::from_nanos(member.writing_ns);
            self.seen.writing_most = self.seen.writing_most.max(writing);
            if held.is_none_or(|(_, longest)| writing > longest) {
                held = Some((member.id, writing));
            }
            let changed = watch.seen.insert(member.id, member.progress) != Some(member.progress);
            let back = watch.unheard.remove(&member.id);
            if changed || back {
                moved = true;
            }
            if let Some(before) = watch.blocked.insert(member.id, member.blocked_ns) {
                excused = excused.max(member.blocked_ns.saturating_sub(before));
            }
        }
        watch.unheard.extend(unheard.iter().copied());
        let period = self.period();
        let excuse = period.saturating_add(self.write_most().max(self.stall).max(self.device));
        let silent = self.silence(unheard, excuse);
        // Judged as a member's thread held in the write would be: its silence is counted less the
        // timeout a look's ask waits for its answer.
        let held = held.filter(|(_, writing)| writing.saturating_sub(RTO) > excuse);
        if !unheard.is_empty() {
            self.seen.unheard_looks = self.seen.unheard_looks.saturating_add(1);
        }
        self.seen.extended_ns = self.seen.extended_ns.saturating_add(excused);
        self.seen.extended_most_ns = self.seen.extended_most_ns.max(excused);
        watch.until = if moved {
            later(now, period)
        } else {
            later(watch.until, Duration::from_nanos(excused))
        };
        if let Some((member, silence)) = silent {
            return Err(Stuck::Silent {
                member,
                silence,
                excuse,
            });
        }
        if let Some((member, writing)) = held {
            return Err(Stuck::Held {
                member,
                writing,
                excuse,
            });
        }
        if looked < watch.until || !unheard.is_empty() {
            Ok(())
        } else {
            Err(Stuck::Quiet(period))
        }
    }

    /// The first of the members in `unheard` silent past `excuse`, and how long. A member's
    /// silence runs from its first unanswered ask to its latest, less the retransmission timeout
    /// the test waited on the latest: one lost ask is no silence, and each ask after it adds the
    /// time between them.
    fn silence(&mut self, unheard: &[u64], excuse: Duration) -> Option<(u64, Duration)> {
        let mut silent = None;
        for id in unheard {
            let Some((first, latest)) = self.unanswered.get(id) else {
                continue;
            };
            let silence = latest.saturating_duration_since(*first).saturating_sub(RTO);
            if silence > self.seen.silence_most {
                self.seen.silence_most = silence;
                self.seen.excused_then = excuse;
            }
            if silence > excuse && silent.is_none() {
                silent = Some((*id, silence));
            }
        }
        silent
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::arithmetic_side_effects,
    clippy::disallowed_methods,
    clippy::disallowed_macros
)]
mod tests {
    use super::*;

    fn heard(id: u64, writing: Duration) -> Heard {
        Heard {
            id,
            progress: Progress {
                term: 1,
                commit: 1,
                applied: 1,
                last_index: 1,
                restarts: 0,
                unjudged: 0,
                judging: 0,
            },
            blocked_ns: 0,
            writing_ns: u64::try_from(writing.as_nanos()).unwrap(),
        }
    }

    /// A write out past the quiet period and the members' longest write is held; the same write
    /// is excused once the test's own flush on the device took as long, and held again past that.
    #[test]
    fn the_devices_measured_flush_excuses_a_write_as_slow() {
        let start = Instant::now();
        let mut quiet = Quiet::new();
        // One member, its law one second, its longest write 100 ms: the excuse is 1.1 s.
        quiet.reported(1, Duration::from_secs(1), 100_000_000);
        let mut watch = quiet.watch(start);
        // A first look sets the member's progress; nothing is out yet.
        assert!(
            quiet
                .look(&mut watch, start, start, &[heard(1, Duration::ZERO)], &[])
                .is_ok()
        );
        // Its write out 2.2 s: past the 1.1 s excuse and the retransmission timeout.
        let later = start + Duration::from_millis(10);
        let held = quiet.look(
            &mut watch,
            later,
            later,
            &[heard(1, Duration::from_millis(2_200))],
            &[],
        );
        assert!(
            matches!(held, Err(Stuck::Held { member: 1, .. })),
            "{held:?}"
        );
        // The device answered the test's flush in 2 s: the write is the device's.
        quiet.device(Duration::from_secs(2));
        let excused = quiet.look(
            &mut watch,
            later,
            later,
            &[heard(1, Duration::from_millis(2_200))],
            &[],
        );
        assert!(excused.is_ok(), "{excused:?}");
        // A write out past the device's time, the quiet period and the timeout is held again.
        let held = quiet.look(
            &mut watch,
            later,
            later,
            &[heard(1, Duration::from_millis(4_100))],
            &[],
        );
        assert!(
            matches!(held, Err(Stuck::Held { member: 1, .. })),
            "{held:?}"
        );
    }
}
