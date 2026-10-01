//! The log's owner: one thread that holds everything the log knows, its groups, its segments,
//! its queue's room and the writer's batch, and that alone reads or changes it (mantle note 32
//! §3.9). Callers reach it by message through a bounded inbox and hear back through their
//! tickets; the device thread hands back each job it was given as a message too. The owner
//! never waits on the device, so it answers a caller while a frame is being flushed, as readers
//! of mantle's lock did.
//!
//! The writer's loop is mantle-log's (`writer.rs`), run as steps between messages. At the top of
//! the loop, the batch is what was held for this frame and what is queued: what the owner has
//! taken from its inbox and admitted since the last batch. When nothing is, the last frame is
//! confirmed on its own, or the owner waits for a submission, and takes the first that comes.
//! Under `Waits::Measured` it then waits for the submitters the last frame answered (`gather`),
//! and commits: a sweep of the tail is read on the device, the batch is laid into a frame after
//! it, and the frame is written and flushed on the device; once it is, the frame is published, the
//! frame before it answered, and the loop goes round.

use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TryRecvError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use hyper_block::buf::AlignedBuf;
use hyper_block::commit::Anticipation;

use crate::codec::Writer as Payload;
use crate::device::{Completion, Job, Look, Reads, Run, Wanted};
use crate::format::{self, FrameHeader, SegmentHeader};
use crate::room::{Room, Take};
use crate::state::State;
use crate::ticket::{Answer, Ticket};
use crate::writer::{self, Placement, Submission, Sweep, SweepRead, Target};
use crate::{Fetched, LogError, Params, Proposal, View, Waits};

/// What reaches the owner.
pub(crate) enum Message<F> {
    /// A submission, admitted at once, held to wait for room when `wait`, or refused.
    Submit {
        submission: Submission,
        wait: bool,
    },
    Query(Query, Ticket),
    /// A job the device has done.
    Done(Completion),
    /// A caller's look at the file, run on the device thread.
    Look(Look<F>),
    /// The log is closing: the owner answers what it holds and ends.
    Close,
}

/// What a caller asks of the log's state.
pub(crate) enum Query {
    Groups,
    View(u128),
    Term(u128, u64),
    Entries {
        group: u128,
        low: u64,
        high: u64,
        max_bytes: u64,
        into: Fetched,
    },
    Flushed,
    Fenced,
}

/// A frame flushed and not yet confirmed, and the updates it carried: at most one frame's.
struct Unconfirmed {
    sequence: u64,
    updates: Vec<Submission>,
}

/// What the writer is waiting on the device for.
enum Phase {
    /// Nothing: the loop runs at its next step.
    Idle,
    /// The tail's frames, read for a sweep, before the batch is laid out after the copies.
    Sweeping {
        batch: VecDeque<Submission>,
        whole: bool,
        read: SweepRead,
    },
    /// A frame's write and flush.
    Writing(Box<Writing>),
    /// A confirmation's write and flush; `forget` once it is done when it followed a commit.
    Confirming { frame: Unconfirmed, forget: bool },
}

/// A frame on the device.
struct Writing {
    taken: Vec<(Submission, Placement)>,
    sweep: Option<Sweep>,
    target: Target,
    tail: u64,
    sequence: u64,
}

/// The writer's wait for the submitters the last frame answered (`hyper_block::commit`).
struct Gather {
    batch: VecDeque<Submission>,
    /// Submissions in the batch sent before the answers went out.
    before: u64,
    answered: u64,
    gathered: u64,
    deadline: Instant,
}

/// Entries to fetch, waiting for their read from the file.
struct Fetch {
    reads: Reads,
    ticket: Ticket,
    /// Reads made of entries that missed, which a second miss ends.
    retries: u32,
}

/// Reads an entry that missed its place is given before it is called damaged: mantle-log looks
/// an entry up again once, should a sweep have moved it since its place was taken.
const RETRIES: u32 = 2;

pub(crate) struct Owner<F> {
    p: Params,
    state: State,
    room: Room,
    /// Admitted submissions not yet taken into a batch, in the order admitted.
    intake: VecDeque<Submission>,
    /// Submissions waiting for room, by arrival number: at most the room's waiters.
    waiting: HashMap<u64, Submission>,
    phase: Phase,
    gather: Option<Gather>,
    /// The owner waits for a submission to make the next batch of.
    parked: bool,
    /// Updates held for the next batch: those a frame passed over for room, and those whose
    /// group had an update in it.
    held: VecDeque<Submission>,
    /// Start-time fair queueing's virtual time, in charged bytes: the largest start tag laid
    /// into a frame, and the largest finish tag once the backlog empties [SFQ96 §2].
    virtual_time: u128,
    /// Each group's last finish tag, while it is ahead of the virtual time and the group is
    /// one the log holds or has an update waiting: at most `max_groups` and
    /// `queue_submissions` entries.
    finish: HashMap<u128, u128>,
    /// Frames laid out so far, which date a submission passed over.
    walks: u64,
    /// Frames written in a row that swept and carried no update while updates waited: at most
    /// `max_segments` before those waiting are refused `Full`.
    fruitless: u64,
    anticipation: Anticipation,
    /// Submissions taken into a batch so far.
    received: u64,
    /// The last frame flushed, while nothing durable yet says its flush completed: the next
    /// frame's persist record will, or a confirmation written when no frame follows at once
    /// (mantle docs/design/raft-log.md §3, §6). Its updates are answered only then.
    unconfirmed: Option<Unconfirmed>,
    /// Submissions the last confirmation answered, and those queued when it did.
    answered: u64,
    backlog: u64,
    /// The aligned buffers the last frame and the last persist record were laid out in, kept
    /// for the next: one frame's bytes at most, a segment and a block.
    frame: Option<AlignedBuf>,
    record: Option<AlignedBuf>,
    /// The first batch is the restore of a lost frame at open, which goes in one frame or not
    /// at all (mantle docs/design/raft-log.md §6).
    restoring: bool,
    fenced: bool,
    /// Frames written and flushed since the log opened, and the updates they carried.
    frames: u64,
    updates: u64,
    device: SyncSender<Job<F>>,
    device_thread: Option<JoinHandle<Option<F>>>,
    /// Fetches waiting for the device, the first of them on it: one group's each at most for
    /// every group the log holds, past which a fetch is refused `Busy`.
    fetches: VecDeque<Fetch>,
    /// Looks at the file waiting for the device, the first on it: one for each caller that
    /// waits in `Log::with_file`.
    looks: VecDeque<Look<F>>,
    looking: bool,
    closing: bool,
}

impl<F: hyper_block::block::BlockFile + 'static> Owner<F> {
    pub(crate) fn new(
        p: Params,
        state: State,
        room: Room,
        restores: Vec<Submission>,
        device: SyncSender<Job<F>>,
        device_thread: JoinHandle<Option<F>>,
    ) -> Self {
        Self {
            p,
            state,
            room,
            intake: VecDeque::new(),
            waiting: HashMap::new(),
            phase: Phase::Idle,
            gather: None,
            parked: false,
            restoring: !restores.is_empty(),
            held: restores.into(),
            virtual_time: 0,
            finish: HashMap::new(),
            walks: 0,
            fruitless: 0,
            anticipation: Anticipation::new(),
            received: 0,
            unconfirmed: None,
            answered: 0,
            backlog: 0,
            frame: None,
            record: None,
            fenced: false,
            frames: 0,
            updates: 0,
            device,
            device_thread: Some(device_thread),
            fetches: VecDeque::new(),
            looks: VecDeque::new(),
            looking: false,
            closing: false,
        }
    }

    /// Runs until the log closes and nothing is left to answer; gives the file back.
    pub(crate) fn run(mut self, inbox: &Receiver<Message<F>>) -> Option<F> {
        self.step(inbox);
        while !self.finished() {
            let message = match self.gather.as_ref().map(|g| g.deadline) {
                Some(deadline) => {
                    let left = deadline.saturating_duration_since(Instant::now());
                    match inbox.recv_timeout(left) {
                        Ok(m) => Some(m),
                        Err(RecvTimeoutError::Timeout) => None,
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                }
                None => match inbox.recv() {
                    Ok(m) => Some(m),
                    Err(_) => break,
                },
            };
            match message {
                Some(m) => self.handle(m, inbox),
                None => self.gathered(inbox),
            }
        }
        self.stop()
    }

    /// Whether the log has closed and the owner holds nothing more to answer.
    fn finished(&self) -> bool {
        self.closing
            && self.parked
            && self.intake.is_empty()
            && self.fetches.is_empty()
            && self.looks.is_empty()
            && !self.looking
    }

    /// Ends the device thread and gives back the file it held.
    fn stop(mut self) -> Option<F> {
        let thread = self.device_thread.take();
        drop(self);
        thread.and_then(|t| t.join().ok().flatten())
    }

    fn handle(&mut self, message: Message<F>, inbox: &Receiver<Message<F>>) {
        match message {
            Message::Submit { submission, wait } => self.submit(submission, wait),
            Message::Query(query, ticket) => self.query(query, ticket),
            Message::Done(completion) => self.done(completion, inbox),
            Message::Look(look) => {
                self.looks.push_back(look);
                self.look();
            }
            Message::Close => self.closing = true,
        }
        self.went_on(inbox);
    }

    /// After a message: a submission that came while the owner was parked starts a batch, and
    /// one that came while it gathered joins the batch.
    fn went_on(&mut self, inbox: &Receiver<Message<F>>) {
        if self.parked && !self.intake.is_empty() {
            self.parked = false;
            let mut batch = VecDeque::new();
            while let Some(s) = self.intake.pop_front() {
                batch.push_back(self.taken(s));
            }
            let (answered, before) = (self.answered, self.backlog);
            self.gather_then_commit(batch, answered, before, inbox);
        } else if self.gather.is_some() && !self.intake.is_empty() {
            while let Some(s) = self.intake.pop_front() {
                let s = self.taken(s);
                if let Some(g) = self.gather.as_mut() {
                    g.gathered = g.gathered.saturating_add(s.bytes);
                    g.batch.push_back(s);
                }
            }
            self.keep_gathering(inbox);
        }
    }

    /// Handles every message already waiting, without waiting for more.
    fn drain(&mut self, inbox: &Receiver<Message<F>>) {
        loop {
            match inbox.try_recv() {
                Ok(Message::Submit { submission, wait }) => self.submit(submission, wait),
                Ok(Message::Query(query, ticket)) => self.query(query, ticket),
                Ok(Message::Done(completion)) => self.done(completion, inbox),
                Ok(Message::Look(look)) => {
                    self.looks.push_back(look);
                    self.look();
                }
                Ok(Message::Close) => self.closing = true,
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return,
            }
        }
    }

    // ---- Admission ----

    /// Admits a submission, holds it to wait for room, or refuses it.
    fn submit(&mut self, mut s: Submission, wait: bool) {
        if self.fenced {
            s.ticket.answer(Err(LogError::Fenced));
            return;
        }
        let mut admitted = Vec::new();
        match self.room.take(s.group, s.bytes, wait, &mut admitted) {
            Ok(Take::Admitted) => {
                s.ticket.admit();
                self.intake.push_back(s);
            }
            Ok(Take::Waiting(seq)) => {
                self.waiting.insert(seq, s);
            }
            Err(e) => s.ticket.answer(Err(e)),
        }
        self.let_in(&admitted);
    }

    /// Tells the waiters room was handed to, each through its own ticket, and queues them.
    fn let_in(&mut self, admitted: &[u64]) {
        for seq in admitted {
            if let Some(s) = self.waiting.remove(seq) {
                s.ticket.admit();
                self.intake.push_back(s);
            }
        }
    }

    /// Answers a submission, and gives back its room in the queue to the waiters it fits.
    fn answer(&mut self, mut s: Submission, result: Result<(), LogError>) {
        s.ticket.answer(result.map(|()| Answer::Durable));
        let mut admitted = Vec::new();
        self.room.release(s.group, s.bytes, &mut admitted);
        self.let_in(&admitted);
    }

    /// Fences the log: no submission is taken from here on, and every waiter hears it.
    fn fence(&mut self) {
        self.fenced = true;
        let mut fenced = Vec::new();
        self.room.fence(&mut fenced);
        for seq in fenced {
            if let Some(mut s) = self.waiting.remove(&seq) {
                s.ticket.answer(Err(LogError::Fenced));
            }
        }
    }

    // ---- The loop ----

    /// The top of mantle's loop: the batch is what was held and what is queued.
    fn step(&mut self, inbox: &Receiver<Message<F>>) {
        loop {
            if !matches!(self.phase, Phase::Idle) || self.gather.is_some() || self.parked {
                return;
            }
            self.drain(inbox);
            let mut batch = std::mem::take(&mut self.held);
            let held = u64::try_from(batch.len()).unwrap_or(u64::MAX);
            while let Some(s) = self.intake.pop_front() {
                batch.push_back(self.taken(s));
            }
            if batch.is_empty() {
                if self.unconfirmed.is_some() {
                    // No frame would carry the confirmation: it goes on its own, at once.
                    if self.confirm(false) {
                        return;
                    }
                    self.backlog = self.queued(inbox);
                    continue;
                }
                // The backlog is empty: the busy period ends at the largest finish tag
                // [SFQ96 §2], and no group's past service counts against it any longer.
                let last = self.finish.values().copied().max().unwrap_or(0);
                self.virtual_time = self.virtual_time.max(last);
                self.finish.clear();
                self.parked = true;
                return;
            }
            let (answered, backlog) = (self.answered, self.backlog);
            self.gather_then_commit(batch, answered, backlog.saturating_add(held), inbox);
            return;
        }
    }

    /// Submissions sent before the answers about to go out: they are queued ahead of any the
    /// answered submitters send next.
    fn queued(&mut self, inbox: &Receiver<Message<F>>) -> u64 {
        self.drain(inbox);
        u64::try_from(self.intake.len()).unwrap_or(u64::MAX)
    }

    /// Counts a submission the writer took. Its room in the queue is held until it is
    /// answered, so the queue's bound covers the updates held for a later frame and those in
    /// the frame being written as well as those waiting (mantle audit S03).
    ///
    /// It is stamped for the fair queue: its start tag is the larger of the virtual time and
    /// its group's last finish tag, and its finish tag its start plus its charge [SFQ96 §2
    /// eqs. 4–5]. Every group weighs the same, so a group's share of a full frame's bytes is the
    /// others' (Theorem 1); the class orders tiers instead of weighting them.
    fn taken(&mut self, mut s: Submission) -> Submission {
        self.received = self.received.saturating_add(1);
        let start = self
            .finish
            .get(&s.group)
            .map_or(self.virtual_time, |&f| f.max(self.virtual_time));
        // Charged bytes over the log's life stay far below 2^128: saturation is unreachable,
        // and would only order the group last.
        self.finish
            .insert(s.group, start.saturating_add(u128::from(s.bytes)));
        s.tags = writer::Tags {
            seq: self.received,
            start,
            passed: None,
        };
        s
    }

    /// Waits for the submitters the last confirmation answered while waiting is expected to
    /// lower total latency, and learns how many of them return (`hyper_block::commit`); then
    /// commits. The first `before` submissions in the batch were sent before the answers.
    fn gather_then_commit(
        &mut self,
        batch: VecDeque<Submission>,
        answered: u64,
        before: u64,
        inbox: &Receiver<Message<F>>,
    ) {
        if answered == 0 || self.p.config.waits == Waits::Never {
            self.commit_unless_fenced(batch, inbox);
            return;
        }
        let gathered = batch
            .iter()
            .fold(0u64, |sum, s| sum.saturating_add(s.bytes));
        self.gather = Some(Gather {
            batch,
            before,
            answered,
            gathered,
            deadline: Instant::now(),
        });
        self.keep_gathering(inbox);
    }

    /// Waits on for one more submitter, or ends the gathering. A batch that holds a frame's
    /// worth already is not waited on: what came next would go in a later frame whatever the
    /// wait.
    fn keep_gathering(&mut self, inbox: &Receiver<Message<F>>) {
        let full = u64::try_from(self.p.frame_room).unwrap_or(u64::MAX);
        let limit = self.p.config.queue_submissions;
        let step = match &self.gather {
            Some(g) if returned(g) < g.answered && g.batch.len() < limit && g.gathered < full => {
                self.anticipation.wait(g.batch.len())
            }
            _ => None,
        };
        match (step, self.gather.as_mut()) {
            (Some(step), Some(g)) => g.deadline = deadline(step),
            _ => self.gathered(inbox),
        }
    }

    /// The gathering is over: the writer learns how many returned and commits the batch.
    fn gathered(&mut self, inbox: &Receiver<Message<F>>) {
        let Some(g) = self.gather.take() else {
            return;
        };
        self.anticipation.learn(g.answered, returned(&g));
        self.commit_unless_fenced(g.batch, inbox);
    }

    fn commit_unless_fenced(&mut self, batch: VecDeque<Submission>, inbox: &Receiver<Message<F>>) {
        if self.fenced {
            for s in batch {
                self.answer(s, Err(LogError::Fenced));
            }
            self.step(inbox);
            return;
        }
        self.commit(batch, inbox);
    }

    /// Commits a batch: a sweep of the tail first if it is due, then the frame.
    fn commit(&mut self, batch: VecDeque<Submission>, inbox: &Receiver<Message<F>>) {
        let whole = std::mem::replace(&mut self.restoring, false);
        match writer::sweepable(&self.state, &self.p) {
            Ok(true) => match writer::sweep_read(&self.state, &self.p) {
                Ok(read) => {
                    let job = Job::Sweep {
                        segment: read.segment,
                        offset: read.offset,
                        end: read.end,
                    };
                    self.phase = Phase::Sweeping { batch, whole, read };
                    self.dispatch(job, inbox);
                }
                Err(e) => self.failed(batch, Vec::new(), e, inbox),
            },
            Ok(false) => self.lay_out(batch, whole, None, Payload::default(), 0, inbox),
            Err(e) => self.failed(batch, Vec::new(), e, inbox),
        }
    }

    /// Hands a write-path job to the device. Its queue holds one such job, which the owner
    /// never exceeds; a device that ended fails the job as a failed write would.
    fn dispatch(&mut self, job: Job<F>, inbox: &Receiver<Message<F>>) {
        if self.device.try_send(job).is_err() {
            let completion = match std::mem::replace(&mut self.phase, Phase::Idle) {
                Phase::Sweeping { batch, whole, read } => {
                    self.phase = Phase::Sweeping { batch, whole, read };
                    Completion::Sweep(Err(LogError::Closed))
                }
                Phase::Writing(w) => {
                    self.phase = Phase::Writing(w);
                    Completion::Frame {
                        frame: AlignedBuf::empty(),
                        record: AlignedBuf::empty(),
                        result: Err(LogError::Closed),
                        took_ns: 0,
                    }
                }
                Phase::Confirming { frame, forget } => {
                    self.phase = Phase::Confirming { frame, forget };
                    Completion::Confirm {
                        record: AlignedBuf::empty(),
                        result: Err(LogError::Closed),
                    }
                }
                Phase::Idle => return,
            };
            self.done(completion, inbox);
        }
    }

    fn done(&mut self, completion: Completion, inbox: &Receiver<Message<F>>) {
        match completion {
            Completion::Sweep(swept) => self.swept(swept, inbox),
            Completion::Frame {
                frame,
                record,
                result,
                took_ns,
            } => {
                self.frame = Some(frame);
                self.record = Some(record);
                self.written(result, took_ns, inbox);
            }
            Completion::Confirm { record, result } => {
                self.record = Some(record);
                self.confirmed(result, inbox);
            }
            Completion::Read(reads) => self.read(reads),
            Completion::Looked => {
                self.looking = false;
                self.look();
            }
        }
    }

    /// The tail is read: its live pieces go first in the payload, then the batch.
    fn swept(
        &mut self,
        swept: Result<Vec<crate::device::Swept>, LogError>,
        inbox: &Receiver<Message<F>>,
    ) {
        let Phase::Sweeping { batch, whole, read } =
            std::mem::replace(&mut self.phase, Phase::Idle)
        else {
            return;
        };
        let mut payload = Payload::default();
        let mut records = 0u32;
        let sweep = swept.and_then(|frames| {
            writer::sweep(
                &self.state,
                &self.p,
                &read,
                &frames,
                &mut payload,
                &mut records,
            )
        });
        match sweep {
            Ok(sweep) => self.lay_out(batch, whole, Some(sweep), payload, records, inbox),
            Err(e) => self.failed(batch, Vec::new(), e, inbox),
        }
    }

    /// Lays the batch into a frame after any sweep, and hands the frame to the device. Each
    /// update laid out moves from `batch` to `taken`; refusals are answered as they are refused.
    fn lay_out(
        &mut self,
        mut batch: VecDeque<Submission>,
        whole: bool,
        sweep: Option<Sweep>,
        mut payload: Payload,
        mut records: u32,
        inbox: &Receiver<Message<F>>,
    ) {
        let mut refused = false;
        self.walks = self.walks.saturating_add(1);
        writer::order(&mut batch);
        let mut taken: Vec<(Submission, Placement)> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut new_groups = 0usize;
        while let Some(mut s) = batch.pop_front() {
            if !seen.insert(s.group) {
                self.held.push_back(s);
                continue;
            }
            let checked =
                writer::validate(&self.state, &self.p.config, &s, new_groups).and_then(|new| {
                    match writer::submission_len(s.group, &s.update, s.marks) {
                        Some(len) if len <= self.p.frame_room => Ok((new, len)),
                        Some(len) => Err(LogError::TooLarge(len)),
                        None => Err(LogError::TooLarge(usize::MAX)),
                    }
                });
            let (new, len) = match checked {
                Ok(checked) => checked,
                Err(e) => {
                    self.answer(s, Err(e));
                    refused = true;
                    continue;
                }
            };
            if payload.len().saturating_add(len) > self.p.frame_room {
                // The group stays taken for this frame: its later updates wait behind
                // this one, so its updates become durable in the order submitted. Passed
                // over, it goes ahead of every class from the next frame on, behind only
                // those passed over before it.
                s.tags.passed.get_or_insert(self.walks);
                self.held.push_back(s);
                continue;
            }
            let Some(placement) =
                writer::encode(&mut payload, &mut records, s.group, &s.update, s.marks)
            else {
                batch.push_front(s);
                self.failed(batch, taken, LogError::TooLarge(len), inbox);
                return;
            };
            self.virtual_time = self.virtual_time.max(s.tags.start);
            if new {
                new_groups = new_groups.saturating_add(1);
            }
            taken.push((s, placement));
        }
        if whole && (refused || !self.held.is_empty()) {
            // A restore in parts would overwrite the lost frame's persist record with the
            // first part's, and a crash before the last part was durable would leave the rest
            // restored nowhere: none of it is written, and the open fails (raft-log.md §6).
            let parts: Vec<Submission> = taken.into_iter().map(|(s, _)| s).collect();
            for s in parts.into_iter().chain(std::mem::take(&mut self.held)) {
                self.answer(
                    s,
                    Err(LogError::Damaged(
                        "a lost frame's restore does not fit one frame",
                    )),
                );
            }
            self.no_frame(inbox);
            return;
        }
        if taken.is_empty() && sweep.is_none() {
            self.no_frame(inbox);
            return;
        }
        self.place(payload.into_vec(), records, sweep, taken, inbox);
    }

    /// Places the frame and hands it, with its persist record, to the device; or, when no
    /// segment can take it, refuses its updates `Full`.
    fn place(
        &mut self,
        payload: Vec<u8>,
        records: u32,
        sweep: Option<Sweep>,
        taken: Vec<(Submission, Placement)>,
        inbox: &Receiver<Message<F>>,
    ) {
        let tail = match &sweep {
            Some(s) => s.next_tail,
            None => self.state.tail_incarnation(),
        };
        let advances = tail > self.state.durable_tail;
        // A frame makes room if it names a later tail, whose durability frees a segment, or if
        // its every update only frees what its group held: a compaction, a removal, a fence.
        let makes_room = advances
            || (!taken.is_empty() && taken.iter().all(|(s, _)| writer::frees(&s.update, s.marks)));
        let target = match writer::target(&self.state, &self.p, payload.len(), makes_room) {
            Ok(Some(target)) => target,
            Ok(None) => {
                self.full(taken, inbox);
                return;
            }
            Err(e) => {
                self.failed(VecDeque::new(), taken, e, inbox);
                return;
            }
        };
        let sequence = self.state.next_sequence;
        let built = self
            .frame_bytes(&target, records, &payload, tail, sequence)
            .and_then(|frame| {
                let record = format::Persist {
                    log: self.p.id,
                    sequence,
                    confirms: self.state.durable,
                    groups: taken.iter().map(|(s, _)| writer::persisted(s)).collect(),
                };
                self.record_bytes(&record).map(|record| (frame, record))
            });
        let (frame, record) = match built {
            Ok(built) => built,
            Err(e) => {
                self.failed(VecDeque::new(), taken, e, inbox);
                return;
            }
        };
        let at = if target.opens {
            match writer::slot_start(&self.p.config, target.slot) {
                Ok(at) => at,
                Err(e) => {
                    self.failed(VecDeque::new(), taken, e, inbox);
                    return;
                }
            }
        } else {
            target.offset
        };
        let record_at = match self.record_at(sequence) {
            Ok(at) => at,
            Err(e) => {
                self.failed(VecDeque::new(), taken, e, inbox);
                return;
            }
        };
        self.phase = Phase::Writing(Box::new(Writing {
            taken,
            sweep,
            target,
            tail,
            sequence,
        }));
        self.dispatch(
            Job::Frame {
                frame,
                at,
                record,
                record_at,
            },
            inbox,
        );
    }

    /// The frame's bytes: the segment's header first when it opens one, the frame's header, its
    /// payload. Laid into the last frame's buffer while it is large enough: every byte up to the
    /// padded end is written, so nothing of the frame before survives into this one.
    fn frame_bytes(
        &mut self,
        target: &Target,
        records: u32,
        payload: &[u8],
        tail: u64,
        sequence: u64,
    ) -> Result<AlignedBuf, LogError> {
        let p = self.p;
        let header = FrameHeader::header(
            p.id,
            target.incarnation,
            target.nonce,
            sequence,
            tail,
            records,
            payload,
        )
        .ok_or(LogError::TooLarge(payload.len()))?;
        let block = p.align.get();
        let header_room = if target.opens { block } else { 0 };
        let frame_len =
            usize::try_from(target.frame_len).map_err(|_| LogError::TooLarge(payload.len()))?;
        let total = header_room
            .checked_add(frame_len)
            .ok_or(LogError::TooLarge(payload.len()))?;
        let mut buf = match self.frame.take() {
            Some(buf) if buf.capacity() >= total => buf,
            _ => AlignedBuf::zeroed(total, p.align).map_err(|e| LogError::Disk(e.into()))?,
        };
        buf.clear();
        let disk = |e: hyper_block::buf::BufError| LogError::Disk(e.into());
        if target.opens {
            let header = SegmentHeader {
                log: p.id,
                incarnation: target.incarnation,
                nonce: target.nonce,
                segment_bytes: p.config.segment_bytes,
            };
            buf.extend_from_slice(&header.encode()).map_err(disk)?;
            buf.extend_zeros(block.saturating_sub(buf.len()))
                .map_err(disk)?;
        }
        buf.extend_from_slice(&header).map_err(disk)?;
        buf.extend_from_slice(payload).map_err(disk)?;
        Ok(buf)
    }

    /// A persist record's bytes, in the last record's buffer while it is large enough.
    fn record_bytes(&mut self, record: &format::Persist) -> Result<AlignedBuf, LogError> {
        let bytes = record
            .encode()
            .ok_or(LogError::TooLarge(record.groups.len()))?;
        let mut buf = match self.record.take() {
            Some(buf) if buf.capacity() >= bytes.len() => buf,
            _ => AlignedBuf::zeroed(bytes.len(), self.p.align)
                .map_err(|e| LogError::Disk(e.into()))?,
        };
        buf.clear();
        buf.extend_from_slice(&bytes)
            .map_err(|e| LogError::Disk(e.into()))?;
        Ok(buf)
    }

    /// The file offset of the persist slot of the frame of `sequence`.
    fn record_at(&self, sequence: u64) -> Result<u64, LogError> {
        let slot = crate::recover::persist_slot(&self.p.config, self.p.align)?;
        Ok(crate::recover::persist_at(slot, sequence))
    }

    /// The frame is on the device: published, the frame before answered, and the loop goes on.
    fn written(
        &mut self,
        result: Result<(), LogError>,
        took_ns: u64,
        inbox: &Receiver<Message<F>>,
    ) {
        let Phase::Writing(w) = std::mem::replace(&mut self.phase, Phase::Idle) else {
            return;
        };
        let Writing {
            mut taken,
            sweep,
            target,
            tail,
            sequence,
        } = *w;
        if let Err(e) = result {
            self.failed(VecDeque::new(), taken, e, inbox);
            return;
        }
        self.anticipation.served(took_ns);
        if let Err(e) = writer::publish(&mut self.state, &self.p, &target, sweep, &mut taken, tail)
        {
            self.failed(VecDeque::new(), taken, e, inbox);
            return;
        }
        self.frames = self.frames.saturating_add(1);
        let updates = u64::try_from(taken.len()).unwrap_or(u64::MAX);
        self.updates = self.updates.saturating_add(updates);
        let backlog = self.queued(inbox);
        let confirmed = match self.unconfirmed.take() {
            Some(before) => self.settle(before, Ok(())),
            None => 0,
        };
        // Frames that sweep and carry no update, in a row, while updates wait: each
        // copies a tail with a dead piece, and its copies are all live, so after a
        // sweep of every segment no dead piece is left to free. Past that many, the
        // log cannot make room for what waits (mantle docs/design/raft-log.md §5).
        if taken.is_empty() {
            self.fruitless = self.fruitless.saturating_add(1);
            if self.fruitless >= u64::from(self.p.config.max_segments) {
                self.refuse_held();
            }
        } else {
            self.fruitless = 0;
        }
        self.unconfirmed = Some(Unconfirmed {
            sequence,
            updates: taken.into_iter().map(|(s, _)| s).collect(),
        });
        self.answered = confirmed;
        self.forget();
        self.backlog = backlog;
        self.step(inbox);
    }

    /// No frame was written, so none confirms the last: a confirmation does, so that no answer
    /// waits on traffic that may only ever be refused.
    fn no_frame(&mut self, inbox: &Receiver<Message<F>>) {
        if self.confirm(true) {
            return;
        }
        self.forget();
        self.backlog = self.queued(inbox);
        self.step(inbox);
    }

    /// No segment can take the frame: every one holds live records the tail's sweep cannot
    /// free. Groups must compact. A frame that carried no update was a sweep making room for
    /// those held: none can come, and they are refused too.
    fn full(&mut self, taken: Vec<(Submission, Placement)>, inbox: &Receiver<Message<F>>) {
        let sweep_only = taken.is_empty();
        for (s, _) in taken {
            self.answer(s, Err(LogError::Full));
        }
        if sweep_only {
            self.refuse_held();
        }
        self.no_frame(inbox);
    }

    /// Answers every held update `Full`: no room can be made for them until groups compact.
    fn refuse_held(&mut self) {
        self.fruitless = 0;
        for s in std::mem::take(&mut self.held) {
            self.answer(s, Err(LogError::Full));
        }
    }

    /// A commit failed: the log is fenced before anyone hears of it, so no answer outruns the
    /// fence, and every update the writer holds is answered `Fenced`: those laid out, those of
    /// the batch not reached, those held for a later frame, and those of the frame before,
    /// whose confirmation will never come.
    fn failed(
        &mut self,
        batch: VecDeque<Submission>,
        taken: Vec<(Submission, Placement)>,
        _error: LogError,
        inbox: &Receiver<Message<F>>,
    ) {
        self.phase = Phase::Idle;
        self.fence();
        let held = std::mem::take(&mut self.held);
        for s in taken.into_iter().map(|(s, _)| s).chain(batch).chain(held) {
            self.answer(s, Err(LogError::Fenced));
        }
        if let Some(before) = self.unconfirmed.take() {
            self.settle(before, Err(()));
        }
        self.answered = 0;
        self.forget();
        self.backlog = self.queued(inbox);
        self.step(inbox);
    }

    /// Hands the device a confirmation of the last frame, written over the frame's own record in
    /// its own slot, now saying the frame was flushed: whether one is now on the device. With the
    /// log fenced, the frame's updates are answered `Fenced` at once.
    ///
    /// The next frame's record goes in the other slot before that frame's flush and may tear; a
    /// confirmation kept there would tear with it, and a frame answered and then damaged at rest
    /// would be taken for a torn one (mantle docs/design/raft-log.md §6). The rewrite puts at
    /// risk only the record of a frame not yet answered, which recovery may drop as it drops a
    /// torn tail.
    fn confirm(&mut self, forget: bool) -> bool {
        let Some(frame) = self.unconfirmed.take() else {
            self.answered = 0;
            return false;
        };
        if self.fenced {
            self.settle(frame, Err(()));
            self.answered = 0;
            return false;
        }
        let record = format::Persist {
            log: self.p.id,
            sequence: frame.sequence,
            confirms: frame.sequence,
            groups: frame.updates.iter().map(writer::persisted).collect(),
        };
        let job = self.record_bytes(&record).and_then(|record| {
            self.record_at(frame.sequence)
                .map(|record_at| Job::Confirm { record, record_at })
        });
        match job {
            Ok(job) => {
                self.phase = Phase::Confirming { frame, forget };
                if self.device.try_send(job).is_err() {
                    let Phase::Confirming { frame, .. } =
                        std::mem::replace(&mut self.phase, Phase::Idle)
                    else {
                        return false;
                    };
                    self.fence();
                    self.settle(frame, Err(()));
                    self.answered = 0;
                    return false;
                }
                true
            }
            Err(_) => {
                self.fence();
                self.settle(frame, Err(()));
                self.answered = 0;
                false
            }
        }
    }

    /// The confirmation is durable, or failed and fenced the log: its frame's updates are
    /// answered, and the loop goes on.
    fn confirmed(&mut self, result: Result<(), LogError>, inbox: &Receiver<Message<F>>) {
        let Phase::Confirming { frame, forget } = std::mem::replace(&mut self.phase, Phase::Idle)
        else {
            return;
        };
        let backlog = self.queued(inbox);
        self.answered = match result {
            Ok(()) => self.settle(frame, Ok(())),
            Err(_) => {
                self.fence();
                self.settle(frame, Err(()));
                0
            }
        };
        if forget {
            self.forget();
        }
        self.backlog = backlog;
        self.step(inbox);
    }

    /// Answers the updates of a frame whose confirmation is durable, `Ok`, or will never be,
    /// `Fenced`; the number answered.
    fn settle(&mut self, frame: Unconfirmed, result: Result<(), ()>) -> u64 {
        let count = u64::try_from(frame.updates.len()).unwrap_or(u64::MAX);
        for s in frame.updates {
            let answer = match result {
                Ok(()) => Ok(()),
                Err(()) => Err(LogError::Fenced),
            };
            self.answer(s, answer);
        }
        count
    }

    /// Drops the finish tags that no longer order anything: those the virtual time has passed,
    /// which a start tag would take the virtual time over, and those of groups the log no
    /// longer holds with nothing waiting, so the map is bounded by the log's groups and the
    /// queue's submissions.
    fn forget(&mut self) {
        let waiting: std::collections::HashSet<u128> = self.held.iter().map(|s| s.group).collect();
        let now = self.virtual_time;
        let groups = &self.state.groups;
        self.finish.retain(|group, finish| {
            *finish > now && (groups.contains_key(group) || waiting.contains(group))
        });
    }

    // ---- Queries ----

    fn query(&mut self, query: Query, mut ticket: Ticket) {
        let answer = match query {
            Query::Groups => Ok(Answer::Groups(self.state.groups.keys().copied().collect())),
            Query::View(group) => self.view(group).map(Answer::View),
            Query::Term(group, index) => self.term(group, index).map(Answer::Term),
            Query::Entries {
                group,
                low,
                high,
                max_bytes,
                into,
            } => {
                self.fetch(group, low, high, max_bytes, into, ticket);
                return;
            }
            Query::Flushed => Ok(Answer::Flushed(self.frames, self.updates)),
            Query::Fenced => Ok(Answer::Fenced(self.fenced)),
        };
        ticket.answer(answer);
    }

    fn view(&self, group: u128) -> Result<Option<View>, LogError> {
        if self.state.damaged.contains_key(&group) {
            return Err(LogError::Damaged(
                "the group's acknowledged records are damaged; it recovers from its peers",
            ));
        }
        let Some(g) = self.state.groups.get(&group) else {
            return Ok(None);
        };
        Ok(Some(View {
            start: g.start,
            last: g.last().ok_or(LogError::Damaged("an index past u64"))?,
            hard_state: g.hard.map(|(h, _)| h),
            proposals: g
                .proposals
                .iter()
                .map(|(&index, p)| Proposal {
                    index,
                    term: p.term,
                    bytes: p.bytes.clone(),
                })
                .collect(),
            uncertain: g.uncertain.map(|(mark, _)| mark),
        }))
    }

    fn term(&self, group: u128, index: u64) -> Result<u64, LogError> {
        let g = self
            .state
            .groups
            .get(&group)
            .ok_or(LogError::Unavailable { group, index })?;
        if index < g.start.index {
            return Err(LogError::Compacted {
                group,
                first: g.start.index,
            });
        }
        g.term(index).ok_or(LogError::Unavailable { group, index })
    }

    /// The entries of `[low, high)`, as many as `max_bytes` of payload admit and one at least:
    /// those in memory copied into the caller's reservation at once, those no longer in memory
    /// read from the file on the device, a run whose blocks touch in one read, as an update's
    /// entries lie together in its frame and a replica catching up asks for runs of them (mantle
    /// audit P07).
    fn fetch(
        &mut self,
        group: u128,
        low: u64,
        high: u64,
        max_bytes: u64,
        mut into: Fetched,
        mut ticket: Ticket,
    ) {
        into.clear();
        let runs = match self.plan(group, low, high, max_bytes, &mut into) {
            Ok(runs) => runs,
            Err(e) => {
                ticket.answer(Err(e));
                return;
            }
        };
        if runs.is_empty() {
            ticket.answer(Ok(Answer::Entries(into)));
            return;
        }
        if self.fetches.len() >= self.p.config.max_groups {
            ticket.answer(Err(LogError::Busy));
            return;
        }
        let reads = Reads {
            group,
            runs,
            into,
            missed: Vec::new(),
            result: Ok(()),
        };
        self.fetches.push_back(Fetch {
            reads,
            ticket,
            retries: 0,
        });
        if self.fetches.len() == 1 {
            self.read_next();
        }
    }

    /// What to read of `[low, high)`: entries in memory go into `into` now, the others leave a
    /// place there and are grouped into runs whose blocks touch.
    fn plan(
        &self,
        group: u128,
        low: u64,
        high: u64,
        max_bytes: u64,
        into: &mut Fetched,
    ) -> Result<Vec<Run>, LogError> {
        let g = self
            .state
            .groups
            .get(&group)
            .ok_or(LogError::Unavailable { group, index: low })?;
        let first = g.first().ok_or(LogError::Damaged("an index past u64"))?;
        if low < first {
            return Err(LogError::Compacted { group, first });
        }
        let mut runs: Vec<Run> = Vec::new();
        let mut total = 0u64;
        for index in low..high {
            let slot = g
                .slot(index)
                .ok_or(LogError::Unavailable { group, index })?;
            total = total.saturating_add(u64::from(slot.len));
            if !into.is_empty() && total > max_bytes {
                break;
            }
            if let Some(bytes) = &slot.cached {
                into.push(slot.term, bytes);
                continue;
            }
            let at = into.reserve(slot.term);
            let wanted = Wanted {
                at,
                index,
                term: slot.term,
                offset: slot.place.offset,
                len: slot.len,
            };
            add_to_runs(
                &mut runs,
                self.p.align,
                slot.place.slot,
                wanted,
                group,
                index,
            )?;
        }
        Ok(runs)
    }

    /// Hands the device the first fetch's reads.
    fn read_next(&mut self) {
        let Some(fetch) = self.fetches.front_mut() else {
            return;
        };
        let reads = std::mem::replace(
            &mut fetch.reads,
            Reads {
                group: 0,
                runs: Vec::new(),
                into: Fetched::default(),
                missed: Vec::new(),
                result: Ok(()),
            },
        );
        if self.device.try_send(Job::Read(reads)).is_err() {
            // The device ended: no fetch can be read.
            for mut fetch in std::mem::take(&mut self.fetches) {
                fetch.ticket.answer(Err(LogError::Closed));
            }
        }
    }

    /// A fetch's reads are back: entries that missed their place are looked up again, once, as
    /// one moved by a sweep since its place was taken; the fetch is answered when none is left.
    fn read(&mut self, mut reads: Reads) {
        let Some(mut fetch) = self.fetches.pop_front() else {
            return;
        };
        let group = reads.group;
        let outcome = std::mem::replace(&mut reads.result, Ok(())).and_then(|()| {
            let missed = std::mem::take(&mut reads.missed);
            self.again(group, &missed, fetch.retries)
        });
        match outcome {
            Ok(runs) if runs.is_empty() => {
                fetch.ticket.answer(Ok(Answer::Entries(reads.into)));
            }
            Ok(runs) => {
                reads.runs = runs;
                fetch.reads = reads;
                fetch.retries = fetch.retries.saturating_add(1);
                self.fetches.push_front(fetch);
            }
            Err(e) => fetch.ticket.answer(Err(e)),
        }
        self.read_next();
    }

    /// Runs reading again the entries that missed: each where the group now holds it. One where
    /// it was is damaged, as is one that missed past the retries; one the group no longer holds
    /// is unavailable.
    fn again(&self, group: u128, missed: &[Wanted], retries: u32) -> Result<Vec<Run>, LogError> {
        let mut runs = Vec::new();
        for wanted in missed {
            let corrupt = LogError::Corrupt {
                group,
                index: wanted.index,
            };
            let now = self
                .state
                .groups
                .get(&group)
                .and_then(|g| g.slot(wanted.index))
                .map(|s| s.place);
            let place = match now {
                Some(p) if p.offset != wanted.offset => p,
                Some(_) => return Err(corrupt),
                None => {
                    return Err(LogError::Unavailable {
                        group,
                        index: wanted.index,
                    });
                }
            };
            if retries.saturating_add(1) >= RETRIES {
                return Err(corrupt);
            }
            let moved = Wanted {
                offset: place.offset,
                ..*wanted
            };
            let (begin, end) =
                crate::device::span(self.p.align, moved.offset, moved.len).ok_or(corrupt)?;
            runs.push(Run {
                begin,
                end,
                slot: place.slot,
                contiguous: moved.at.saturating_add(1),
                entries: vec![moved],
            });
        }
        Ok(runs)
    }

    /// Hands the device the next caller's look at the file, one at a time.
    fn look(&mut self) {
        if self.looking {
            return;
        }
        let Some(look) = self.looks.pop_front() else {
            return;
        };
        // A device that ended drops the look, and its caller hears the log closed.
        self.looking = self.device.try_send(Job::Look(look)).is_ok();
    }
}

/// Adds `wanted` to the last run when its blocks touch that run's, in the same segment; to a
/// new run otherwise.
fn add_to_runs(
    runs: &mut Vec<Run>,
    align: hyper_block::buf::Alignment,
    slot: u32,
    wanted: Wanted,
    group: u128,
    index: u64,
) -> Result<(), LogError> {
    let (b, e) = crate::device::span(align, wanted.offset, wanted.len)
        .ok_or(LogError::Corrupt { group, index })?;
    if let Some(run) = runs.last_mut()
        && run.slot == slot
        && b >= run.begin
        && b <= run.end
        && run.contiguous == wanted.at
    {
        run.end = run.end.max(e);
        run.contiguous = wanted.at.saturating_add(1);
        run.entries.push(wanted);
        return Ok(());
    }
    runs.push(Run {
        begin: b,
        end: e,
        slot,
        contiguous: wanted.at.saturating_add(1),
        entries: vec![wanted],
    });
    Ok(())
}

/// Submitters of the batch that came back since the answers went out.
fn returned(g: &Gather) -> u64 {
    u64::try_from(g.batch.len())
        .unwrap_or(u64::MAX)
        .saturating_sub(g.before)
}

fn deadline(step: Duration) -> Instant {
    let now = Instant::now();
    now.checked_add(step).unwrap_or(now)
}
