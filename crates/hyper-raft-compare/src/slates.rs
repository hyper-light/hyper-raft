//! slates' own core, `slates_cluster::raft::RaftNode` (slates `crates/cluster/src/raft.rs` at
//! `5cce86a`), driven through its own API the way slates' timed simulation drives it
//! (`crates/cluster/tests/support/timed.rs`: `lead`, `follow`, `answer`), with slates' own
//! election timer (`timing::ElectionTimer`) and its retained-state publication
//! (`saved`/`retained`, as `tests/explore.rs` and `tests/multilog.rs` publish it).
//!
//! slates' core leaves to its caller what focal's line decides inside the core: when a leader
//! sends. This adapter is the thinnest caller that sends what is owed and nothing else:
//! - a leader sends a follower an append when the follower is owed entries it was not yet sent,
//!   or a commit it was not yet told (`replicate_to`, else `install_snapshot_for` when the
//!   follower is below the snapshot), and once a period whatever it owes (the heartbeat, as
//!   `lead` sends one every period);
//! - an invitation to campaign (`TimeoutNow`) is acted on when it arrives, as focal's line acts
//!   on `MsgTimeoutNow`, where slates' simulation keeps it for the next period;
//! - the timer runs at focal's shell's settings: a period is `heartbeat_tick` ticks, so the
//!   election base and span are each `election_tick / heartbeat_tick` periods (slates derives
//!   them from measured round trips; on this in-memory network there are none to measure);
//! - the byte budget of one append and the window are focal's shell's `max_size_per_msg`, the
//!   same bound every other core runs with (slates derives its budget from the packet size).
use hyper_measure::alloc;
use slates_cluster::{
    raft::{FastVote, RaftNode, SavedRaft, TimeoutNow},
    raft_wire::RaftMessage,
    timing::{ElectionTimer, ElectionTiming, FollowerStep},
};
use slates_db::register::HostId;

use crate::core::{App, Core, Envelope, Fast, Settings};

/// `PUBLISH`: whether the retained state is published after each transition, as slates' server
/// does (`server/src/retention.rs`); without it the row is the core alone.
pub struct Node<const PUBLISH: bool> {
    raft: RaftNode,
    id: u64,
    peers: Vec<u64>,
    timer: ElectionTimer,
    timing: ElectionTiming,
    /// The leader-contact count the timer reads (Raft Figure 2's two resets).
    contact: u64,
    budget: usize,
    /// For each peer, the last index and the commit it was sent.
    sent_through: Vec<u64>,
    sent_commit: Vec<u64>,
    app: App,
    /// The last publication of the retained state.
    retained: Option<SavedRaft>,
    state_bytes: usize,
}

impl<const PUBLISH: bool> Node<PUBLISH> {
    fn position(&self, peer: u64) -> Option<usize> {
        self.peers.iter().position(|p| *p == peer)
    }
    fn send(&self, to: u64, message: RaftMessage, out: &mut Vec<Envelope<RaftMessage>>) {
        alloc::aside();
        out.push(Envelope {
            from: self.id,
            to,
            message,
        });
        alloc::back();
    }
    fn broadcast(&self, messages: Vec<RaftMessage>, out: &mut Vec<Envelope<RaftMessage>>) {
        alloc::aside();
        for (to, message) in self.peers.iter().zip(messages) {
            out.push(Envelope {
                from: self.id,
                to: *to,
                message,
            });
        }
        alloc::back();
    }
    /// A leader sends each follower what it owes; with `heartbeat`, every follower gets an append
    /// whether or not it is owed one.
    fn lead(&mut self, heartbeat: bool, out: &mut Vec<Envelope<RaftMessage>>) {
        if !self.raft.is_leader() {
            return;
        }
        let last = self.raft.last_log_index();
        let commit = self.raft.commit_index();
        for at in 0..self.peers.len() {
            let owed = self.sent_through[at] < last || self.sent_commit[at] < commit;
            if !heartbeat && !owed {
                continue;
            }
            let peer = HostId(self.peers[at]);
            let message = match self.raft.replicate_to(peer, self.budget) {
                Some(append) => {
                    self.sent_through[at] = append.prev_log_index + append.entries.len() as u64;
                    self.sent_commit[at] = append.leader_commit;
                    Some(RaftMessage::AppendEntries(append))
                }
                None => self.raft.install_snapshot_for(peer).map(|snapshot| {
                    self.sent_through[at] = snapshot.last_included_index;
                    RaftMessage::InstallSnapshot(snapshot)
                }),
            };
            if let Some(message) = message {
                self.send(peer.0, message, out);
            }
        }
        if let Some((to, invitation)) = self.raft.take_timeout_now() {
            self.send(to.0, RaftMessage::TimeoutNow(invitation), out);
        }
    }
    fn route_vote(&mut self, vote: FastVote, out: &mut Vec<Envelope<RaftMessage>>) {
        match self.raft.leader() {
            Some(leader) if leader.0 == self.id => self.raft.on_fast_vote(vote),
            Some(leader) => self.send(leader.0, RaftMessage::FastVote(vote), out),
            None => {}
        }
    }
    fn invited(&mut self, invitation: TimeoutNow, out: &mut Vec<Envelope<RaftMessage>>) {
        let votes = self.raft.on_timeout_now(invitation);
        if !votes.is_empty() {
            self.timer.rebaseline(self.contact);
            self.broadcast(
                votes.into_iter().map(RaftMessage::RequestVote).collect(),
                out,
            );
        }
    }
    fn reset_progress(&mut self) {
        self.sent_through.iter_mut().for_each(|sent| *sent = 0);
        self.sent_commit.iter_mut().for_each(|sent| *sent = 0);
    }
}

impl<const PUBLISH: bool> Core for Node<PUBLISH> {
    type Message = RaftMessage;
    const NAME: &'static str = if PUBLISH {
        "slates 5cce86a"
    } else {
        "slates 5cce86a, core alone"
    };

    fn open(id: u64, voters: &[u64], settings: &Settings, _seed: u64) -> Self {
        let mut raft = RaftNode::new(HostId(id), voters.iter().copied().map(HostId).collect());
        let budget = usize::try_from(settings.max_size_per_msg).unwrap();
        // One batch ahead, slates' window on a network whose round trip is within a period
        // (`ElectionTiming::window_budget`).
        raft.set_window_budget(budget);
        let periods = u32::try_from(settings.election_tick / settings.heartbeat_tick).unwrap();
        let peers: Vec<u64> = voters.iter().copied().filter(|v| *v != id).collect();
        Self {
            raft,
            id,
            timer: ElectionTimer::new(),
            timing: ElectionTiming {
                base_periods: periods,
                span_periods: periods,
                broadcast_rtt_tail_ns: 0,
                broadcast_rtt_spread_ns: 0,
                samples: 0,
            },
            contact: 0,
            budget,
            sent_through: vec![0; peers.len()],
            sent_commit: vec![0; peers.len()],
            peers,
            app: App::default(),
            retained: None,
            state_bytes: settings.state_bytes,
        }
    }
    fn id(&self) -> u64 {
        self.id
    }
    fn leader(&self) -> u64 {
        self.raft.leader().map_or(0, |leader| leader.0)
    }
    fn term(&self) -> u64 {
        self.raft.term()
    }
    fn applied(&self) -> u64 {
        self.app.index
    }
    fn digest(&self) -> u64 {
        self.app.digest
    }
    fn campaign(&mut self, out: &mut Vec<Envelope<RaftMessage>>) {
        let pre_votes = self
            .raft
            .on_election_timeout()
            .expect("a term to campaign in");
        self.timer.rebaseline(self.contact);
        self.broadcast(
            pre_votes.into_iter().map(RaftMessage::PreVote).collect(),
            out,
        );
        self.flush(out);
    }
    fn period(&mut self, out: &mut Vec<Envelope<RaftMessage>>) {
        if self.raft.is_leader() {
            self.lead(true, out);
            if self.timer.leader_period(&self.timing) {
                self.raft.check_quorum();
            }
        } else {
            match self
                .timer
                .follower_period(self.contact, &self.timing, HostId(self.id), 0)
            {
                FollowerStep::Follow => {}
                FollowerStep::LeaderLapsed => self.raft.forget_leader(),
                FollowerStep::Campaign => {
                    let pre_votes = self.raft.on_election_timeout().expect("a term");
                    self.timer.rebaseline(self.contact);
                    self.broadcast(
                        pre_votes.into_iter().map(RaftMessage::PreVote).collect(),
                        out,
                    );
                }
            }
        }
        self.flush(out);
    }
    fn propose(&mut self, data: Vec<u8>) -> bool {
        self.raft.append_command(data)
    }
    fn propose_fast(&mut self, data: Vec<u8>, out: &mut Vec<Envelope<RaftMessage>>) -> Fast {
        let Some(proposal) = self.raft.propose_fast(data) else {
            return Fast::Refused;
        };
        // To every voter, its own vote cast here (`timed.rs`, `propose_at`).
        for peer in self.peers.clone() {
            self.send(peer, RaftMessage::FastPropose(proposal.clone()), out);
        }
        if let Some(vote) = self.raft.on_fast_propose(proposal) {
            self.route_vote(vote, out);
        }
        self.flush(out);
        Fast::Proposed
    }
    fn open_fast(&mut self) -> bool {
        self.raft.open_fast_track()
    }
    fn transfer(&mut self, to: u64, out: &mut Vec<Envelope<RaftMessage>>) -> bool {
        let done = self.raft.transfer_leadership(HostId(to)).is_ok();
        self.flush(out);
        done
    }
    fn step(&mut self, from: u64, message: RaftMessage, out: &mut Vec<Envelope<RaftMessage>>) {
        let was_leader = self.raft.is_leader();
        match message {
            RaftMessage::PreVote(pre) => {
                let reply = self.raft.on_pre_vote(pre);
                self.send(from, RaftMessage::PreVoteReply(reply), out);
            }
            RaftMessage::RequestVote(vote) => {
                let reply = self.raft.on_request_vote(vote);
                if reply.granted {
                    self.contact += 1;
                }
                self.send(from, RaftMessage::VoteReply(reply), out);
            }
            RaftMessage::AppendEntries(append) => {
                let term = append.term;
                let reply = self.raft.on_append_entries(append);
                if term >= reply.term {
                    self.contact += 1;
                }
                self.send(from, RaftMessage::AppendReply(reply), out);
            }
            RaftMessage::InstallSnapshot(snapshot) => {
                let term = snapshot.term;
                alloc::aside();
                let state = App::decode(&snapshot.state);
                alloc::back();
                let reply = self.raft.on_install_snapshot(snapshot);
                if term >= reply.term {
                    self.contact += 1;
                }
                if reply.match_index > 0 && reply.match_index > self.app.index {
                    self.app = state;
                }
                self.send(from, RaftMessage::InstallSnapshotReply(reply), out);
            }
            RaftMessage::TimeoutNow(invitation) => self.invited(invitation, out),
            RaftMessage::PreVoteReply(reply) => {
                if let Some(votes) = self.raft.on_pre_vote_reply(reply) {
                    self.broadcast(
                        votes.into_iter().map(RaftMessage::RequestVote).collect(),
                        out,
                    );
                }
            }
            RaftMessage::VoteReply(reply) => self.raft.on_vote_reply(reply),
            RaftMessage::AppendReply(reply) => {
                if !reply.success
                    && let Some(at) = self.position(from)
                {
                    self.sent_through[at] = 0;
                }
                self.raft.on_append_reply(reply);
            }
            RaftMessage::FastPropose(proposal) => {
                if let Some(vote) = self.raft.on_fast_propose(proposal) {
                    self.route_vote(vote, out);
                }
            }
            RaftMessage::FastVote(vote) => self.raft.on_fast_vote(vote),
            RaftMessage::InstallSnapshotReply(reply) => self.raft.on_install_snapshot_reply(reply),
        }
        if self.raft.is_leader() != was_leader {
            self.reset_progress();
        }
    }
    fn flush(&mut self, out: &mut Vec<Envelope<RaftMessage>>) {
        self.lead(false, out);
        alloc::aside();
        let base = self.raft.snapshot_index();
        let committed = self.raft.committed_entries();
        let from = self.app.index.max(base);
        let skip = usize::try_from(from - base).unwrap();
        for (at, entry) in committed.iter().enumerate().skip(skip) {
            self.app.apply(base + at as u64 + 1, &entry.command);
        }
        if PUBLISH && self.raft.retention_pending() {
            self.retained = Some(self.raft.saved());
            self.raft.retained();
        }
        alloc::back();
    }
    fn compact(&mut self) {
        alloc::aside();
        let index = self.app.index;
        if index > self.raft.snapshot_index() {
            self.raft.compact(index, self.app.encode(self.state_bytes));
        }
        if PUBLISH && self.raft.retention_pending() {
            self.retained = Some(self.raft.saved());
            self.raft.retained();
        }
        alloc::back();
    }
}
