//! The merge (`docs/multilog.md` §4): a pure function of what each log committed, applying their
//! commands in one application order every member reaches alike.
//!
//! It holds, for each log, the index of its next entry to consume, and the log-0 index of the last
//! global command it applied (its *epoch*). It reads each log where the log's storage holds it
//! ([`Logs`]), copying nothing, and consumes, until no log moves:
//! - a log `k ≥ 1` in order: a keyed command, applied in the current epoch; a barrier naming `h`,
//!   passed once log 0 has been consumed through `h`, the log stopping there until it has;
//! - log 0 in order: a keyed command, applied in the current epoch; a global command at `g`,
//!   applied once every other log stands at a barrier naming `g` or later, the epoch becoming `g`,
//!   the log stopping there until they do;
//! - in any log, the core's own entries as nothing, and entries out of place, refused alike on
//!   every member ([`Refusal`]).

use hyper_raft::{StorageError, proto::Entry};

use crate::entry::{self, Stated};
use crate::error::{Error, Result};
use crate::route::log_of;

/// What the merge reads: each log's committed entries, where its storage holds them.
pub trait Logs {
    /// How many logs.
    fn count(&self) -> usize;
    /// The last index of `log` the merge may read: committed, and durable here.
    fn through(&self, log: usize) -> u64;
    /// Gives `visit` each entry of `log` from `from` through `through`, in order and where it is
    /// held, until `visit` returns true.
    fn walk(
        &self,
        log: usize,
        from: u64,
        through: u64,
        visit: &mut dyn FnMut(&Entry) -> bool,
    ) -> std::result::Result<(), StorageError>;
}

/// A command, in its place in the application order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Command<'a> {
    /// The log it was committed in.
    pub log: usize,
    /// Its index there.
    pub index: u64,
    /// Its key; none for a global command.
    pub key: Option<u64>,
    /// The command, where its log's storage holds it.
    pub data: &'a [u8],
    /// The log-0 index of the last global command applied before it (zero before any): the global
    /// state it sees.
    pub epoch: u64,
}

/// Why the merge refuses an entry: it consumes it as nothing, alike on every member
/// (`docs/multilog.md` §2.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Bytes the layer never writes.
    Malformed,
    /// A global command in a log other than log 0.
    GlobalOutsideLogZero,
    /// A barrier in log 0.
    BarrierInLogZero,
    /// A keyed command in a log its key does not route to.
    Misrouted {
        /// The key.
        key: u64,
    },
}

/// What the merge hands the owner, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Applied<'a> {
    /// A command to apply.
    Command(Command<'a>),
    /// An entry refused, consumed as nothing.
    Refused {
        /// Its log.
        log: usize,
        /// Its index there.
        index: u64,
        /// Why.
        why: Refusal,
    },
}

/// Whether the merge goes on after what it handed the owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flow {
    /// It goes on.
    Continue,
    /// It stops right after it: an owner taking an image after a global command stops it there,
    /// at the command's canonical cut (`docs/multilog.md` §5.2).
    Stop,
}

/// What one call of the merge did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Advance {
    /// Entries it consumed: commands, refusals and the core's own.
    pub consumed: u64,
    /// Whether it stopped before running dry: its budget was spent, or the owner said stop.
    pub more: bool,
}

/// The merge's position: each log's next index to consume, and the epoch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cut {
    next: Vec<u64>,
    epoch: u64,
}

impl Cut {
    /// The position before anything, over `logs` logs: every log at its first index, no global
    /// applied. Refused for no log.
    pub fn origin(logs: usize) -> Result<Self> {
        if logs == 0 {
            return Err(Error::Settings("no log"));
        }
        let mut next = Vec::new();
        next.try_reserve_exact(logs)
            .map_err(|_| Error::Capacity("logs"))?;
        next.resize(logs, 1);
        Ok(Self { next, epoch: 0 })
    }
    /// The position `next` and `epoch` state, refused unless it can be one: a log at least,
    /// every index from one on, and the epoch below log 0's next index.
    pub fn new(next: Vec<u64>, epoch: u64) -> Result<Self> {
        let Some(&zero) = next.first() else {
            return Err(Error::Settings("no log"));
        };
        if next.contains(&0) {
            return Err(Error::Settings("a log's next index is zero"));
        }
        if epoch >= zero {
            return Err(Error::Settings("an epoch log 0 has not reached"));
        }
        Ok(Self { next, epoch })
    }
    /// Each log's next index to consume.
    pub fn next(&self) -> &[u64] {
        &self.next
    }
    /// The log-0 index of the last global command applied.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    /// How many logs.
    pub fn logs(&self) -> usize {
        self.next.len()
    }
    /// Whether a member at this position holds the canonical cut `other` (`docs/multilog.md`
    /// §5.1): with more than one log, holding log 0 past a global is having applied it, and a
    /// canonical cut is comparable with every position a member reaches, so log 0 decides.
    pub fn holds(&self, other: &Self) -> bool {
        self.next.first() >= other.next.first()
    }
}

/// The merge's state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Merge {
    next: Vec<u64>,
    epoch: u64,
    /// For each log, the index the barrier it stands at names, once the merge has read it there.
    head: Vec<Option<u64>>,
    /// Whether the last entry consumed was a global command applied: the position is that
    /// command's canonical cut until anything else is consumed.
    after_global: bool,
}

/// One call's account: what it may spend, and what it did.
struct Run {
    budget: u64,
    spent: u64,
    consumed: u64,
    halted: bool,
}

impl Run {
    fn took(&mut self, bytes: usize, flow: Flow) {
        self.consumed = self.consumed.saturating_add(1);
        self.spent = self
            .spent
            .saturating_add(u64::try_from(bytes).unwrap_or(u64::MAX));
        if flow == Flow::Stop || self.spent >= self.budget {
            self.halted = true;
        }
    }
}

/// What a step does with the entry it reads.
enum Take<'a> {
    /// Consumes it, handing the owner this, if anything.
    Consume(Option<Applied<'a>>),
    /// Stops the log at it.
    Hold,
}

impl Merge {
    /// A merge at the start of `logs` logs; refused for none.
    pub fn new(logs: usize) -> Result<Self> {
        Ok(Self::at(&Cut::origin(logs)?))
    }
    /// A merge resuming at `cut`, as a member restarting from an image taken there, or one that
    /// installed it (`docs/multilog.md` §5.3, §5.5).
    pub fn at(cut: &Cut) -> Self {
        Self {
            next: cut.next.clone(),
            epoch: cut.epoch,
            head: vec![None; cut.next.len()],
            after_global: false,
        }
    }
    /// How many logs.
    pub fn logs(&self) -> usize {
        self.next.len()
    }
    /// The next index the merge consumes in `log`.
    pub fn next(&self, log: usize) -> Option<u64> {
        self.next.get(log).copied()
    }
    /// The log-0 index of the last global command applied.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    /// The index the barrier `log` stands at names, once the merge has read it.
    pub fn head(&self, log: usize) -> Option<u64> {
        self.head.get(log).copied().flatten()
    }
    /// The merge's position.
    pub fn cut(&self) -> Cut {
        Cut {
            next: self.next.clone(),
            epoch: self.epoch,
        }
    }
    /// Whether the position is canonical (`docs/multilog.md` §5.1): comparable with every position
    /// any member reaches, so that an image taken here can be installed by any. With one log,
    /// always; before anything, always; otherwise right after a global command is applied and
    /// before anything else is consumed, each other log standing at its first barrier naming the
    /// command or later. A log that has gone on to a later barrier since is not there: a member
    /// whose log 0 runs ahead while that log lags is neither ahead of it nor behind.
    pub fn canonical(&self) -> bool {
        self.next.len() == 1
            || self.after_global
            || (self.epoch == 0 && self.next.iter().all(|next| *next == 1))
    }

    /// Consumes what the logs `logs` now allow, in the merged order, handing each command and
    /// refusal to `apply`, until no log moves, the commands' bytes reach `budget` (one entry at
    /// least is consumed if any can be), or `apply` says stop.
    pub fn advance<L: Logs + ?Sized>(
        &mut self,
        logs: &L,
        budget: u64,
        apply: &mut dyn FnMut(Applied<'_>) -> Flow,
    ) -> Result<Advance> {
        if logs.count() != self.next.len() {
            return Err(Error::Invariant("a merge read over another count of logs"));
        }
        let mut run = Run {
            budget,
            spent: 0,
            consumed: 0,
            halted: false,
        };
        loop {
            let mut moved = false;
            for log in (1..self.next.len()).chain(std::iter::once(0)) {
                moved |= self.consume(logs, log, &mut run, apply)?;
                if run.halted {
                    return Ok(Advance {
                        consumed: run.consumed,
                        more: true,
                    });
                }
            }
            if !moved {
                return Ok(Advance {
                    consumed: run.consumed,
                    more: false,
                });
            }
        }
    }

    /// Consumes `log`'s entries while it may; whether it consumed any.
    fn consume<L: Logs + ?Sized>(
        &mut self,
        logs: &L,
        log: usize,
        run: &mut Run,
        apply: &mut dyn FnMut(Applied<'_>) -> Flow,
    ) -> Result<bool> {
        let from = self.next(log).ok_or(Error::NoLog(log))?;
        let through = logs.through(log);
        if from > through || self.waits(log) {
            return Ok(false);
        }
        let mut moved = false;
        let mut failed = None;
        let walked = logs.walk(log, from, through, &mut |entry| match self
            .step(log, entry, run, apply)
        {
            Ok(true) => {
                moved = true;
                run.halted
            }
            Ok(false) => true,
            Err(error) => {
                failed = Some(error);
                true
            }
        });
        if let Some(error) = failed {
            return Err(error);
        }
        match walked {
            Ok(()) => Ok(moved),
            Err(error) => storage(error, moved),
        }
    }

    /// Whether `log` stands at a barrier the merge read, and log 0 has not been consumed through
    /// what it names.
    fn waits(&self, log: usize) -> bool {
        log != 0
            && self
                .head(log)
                .is_some_and(|named| self.next.first().is_none_or(|zero| named >= *zero))
    }

    /// The step at `entry` of `log`: whether it was consumed.
    fn step(
        &mut self,
        log: usize,
        entry: &Entry,
        run: &mut Run,
        apply: &mut dyn FnMut(Applied<'_>) -> Flow,
    ) -> Result<bool> {
        let next = self.next(log).ok_or(Error::NoLog(log))?;
        if entry.index != next {
            return Err(Error::Invariant("a log gave the merge another index"));
        }
        let take = if log == 0 {
            self.take_designated(entry)
        } else {
            self.take_other(log, entry)
        };
        let Take::Consume(handed) = take else {
            return Ok(false);
        };
        let global = matches!(handed, Some(Applied::Command(Command { key: None, .. })));
        let flow = handed.map_or(Flow::Continue, &mut *apply);
        self.moved_past(log, next)?;
        self.after_global = global;
        run.took(entry.data.len(), flow);
        Ok(true)
    }

    /// `log` consumed its entry at `index`: it stands at the next, read afresh.
    fn moved_past(&mut self, log: usize, index: u64) -> Result<()> {
        let after = index
            .checked_add(1)
            .ok_or(Error::Invariant("a log past the last index"))?;
        let (Some(next), Some(head)) = (self.next.get_mut(log), self.head.get_mut(log)) else {
            return Err(Error::NoLog(log));
        };
        *next = after;
        *head = None;
        Ok(())
    }

    /// A step in log 0.
    fn take_designated<'e>(&mut self, entry: &'e Entry) -> Take<'e> {
        let index = entry.index;
        match entry::read(entry) {
            Stated::Global(data) => self.take_global(index, data),
            Stated::Keyed { key, command } => self.take_keyed(0, index, key, command),
            Stated::Barrier(_) => refuse(0, index, Refusal::BarrierInLogZero),
            Stated::Malformed => refuse(0, index, Refusal::Malformed),
            Stated::Own => Take::Consume(None),
        }
    }

    /// A step in log `log ≥ 1`.
    fn take_other<'e>(&mut self, log: usize, entry: &'e Entry) -> Take<'e> {
        let index = entry.index;
        match entry::read(entry) {
            Stated::Barrier(named) => self.take_barrier(log, named),
            Stated::Keyed { key, command } => self.take_keyed(log, index, key, command),
            Stated::Global(_) => refuse(log, index, Refusal::GlobalOutsideLogZero),
            Stated::Malformed => refuse(log, index, Refusal::Malformed),
            Stated::Own => Take::Consume(None),
        }
    }

    /// A global command at log-0 `index`: applied once every other log stands at a barrier naming
    /// it or later.
    fn take_global<'e>(&mut self, index: u64, data: &'e [u8]) -> Take<'e> {
        let reached = self
            .head
            .iter()
            .skip(1)
            .all(|head| head.is_some_and(|named| named >= index));
        if !reached {
            return Take::Hold;
        }
        let epoch = self.epoch;
        self.epoch = index;
        Take::Consume(Some(Applied::Command(Command {
            log: 0,
            index,
            key: None,
            data,
            epoch,
        })))
    }

    /// A keyed command in `log`: applied in the current epoch where its key routes there.
    fn take_keyed<'e>(&self, log: usize, index: u64, key: u64, data: &'e [u8]) -> Take<'e> {
        if log_of(key, self.next.len()) != log {
            return refuse(log, index, Refusal::Misrouted { key });
        }
        Take::Consume(Some(Applied::Command(Command {
            log,
            index,
            key: Some(key),
            data,
            epoch: self.epoch,
        })))
    }

    /// A barrier in `log` naming log 0's `named`: passed once log 0 has been consumed through it.
    fn take_barrier<'e>(&mut self, log: usize, named: u64) -> Take<'e> {
        let passed = self.next.first().is_some_and(|zero| named < *zero);
        if passed {
            return Take::Consume(None);
        }
        if let Some(head) = self.head.get_mut(log) {
            *head = Some(named);
        }
        Take::Hold
    }
}

fn refuse<'e>(log: usize, index: u64, why: Refusal) -> Take<'e> {
    Take::Consume(Some(Applied::Refused { log, index, why }))
}

/// What a log's storage refusing a walk means to the merge: entries not in memory yet are read
/// later; any other refusal is of entries the core said it holds, or ones compacted before the
/// merge consumed them, and the layer's state no longer adds up with the log's.
fn storage(error: StorageError, moved: bool) -> Result<bool> {
    match error {
        StorageError::LogTemporarilyUnavailable => Ok(moved),
        StorageError::Compacted => Err(Error::Invariant(
            "a log compacted past what the merge consumed",
        )),
        StorageError::Unavailable
        | StorageError::SnapshotTemporarilyUnavailable
        | StorageError::Other(_) => Err(Error::Invariant(
            "a log's storage refused what the core gave to apply",
        )),
    }
}
