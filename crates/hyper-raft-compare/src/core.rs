//! What every core is asked to do, in one shape, so that one workload drives
//! them all. Each adapter drives its core through the core's own API, the
//! way that core's owner drives it; what an adapter adds is said where it
//! is.

/// A message on its way, with who sent it and who it is for.
pub struct Envelope<M> {
    pub from: u64,
    pub to: u64,
    pub message: M,
}

/// The settings every core runs with: focal's shell's (hyper-raft
/// `tests/support/mod.rs`, `Settings::shell`), which mantle's replica also
/// takes. A core that names a setting differently is given the same value
/// under its name.
#[derive(Clone, Copy, Debug)]
pub struct Settings {
    /// Ticks without the leader before an election.
    pub election_tick: usize,
    /// Ticks between heartbeats; one period of the workload is this many
    /// ticks.
    pub heartbeat_tick: usize,
    /// The bytes of entries one append carries.
    pub max_size_per_msg: u64,
    /// Appends sent ahead of their answers.
    pub max_inflight_msgs: usize,
    pub max_uncommitted_size: u64,
    pub max_committed_size_per_ready: u64,
    /// Whether the group has the fast track.
    pub fast: bool,
    /// The bytes the application holds besides its digest: what a snapshot
    /// of it carries.
    pub state_bytes: usize,
}

impl Settings {
    pub fn shell(fast: bool, state_bytes: usize) -> Self {
        Self {
            election_tick: 10,
            heartbeat_tick: 2,
            max_size_per_msg: 4 * 1024 * 1024 + 1024,
            max_inflight_msgs: 128,
            max_uncommitted_size: 32 * 1024 * 1024,
            max_committed_size_per_ready: 16 * 1024 * 1024,
            fast,
            state_bytes,
        }
    }
}

/// What a proposal by the fast track came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fast {
    /// The core has no fast track.
    Unsupported,
    Proposed,
    Refused,
}

/// One member of a group, as the workload drives it.
pub trait Core: Sized {
    type Message;
    /// The name the tables give it.
    const NAME: &'static str;
    fn open(id: u64, voters: &[u64], settings: &Settings, seed: u64) -> Self;
    fn id(&self) -> u64;
    /// The leader this member knows; zero for none.
    fn leader(&self) -> u64;
    fn is_leader(&self) -> bool {
        self.leader() == self.id()
    }
    fn term(&self) -> u64;
    /// The index applied through.
    fn applied(&self) -> u64;
    /// What the application holds: a digest of every entry applied, in
    /// order, so that the members can be compared.
    fn digest(&self) -> u64;
    /// Campaign now.
    fn campaign(&mut self, out: &mut Vec<Envelope<Self::Message>>);
    /// One heartbeat period of the owner's clock.
    fn period(&mut self, out: &mut Vec<Envelope<Self::Message>>);
    /// Proposes `data`; what to send is given at the next [`Core::flush`].
    fn propose(&mut self, data: Vec<u8>) -> bool;
    /// Proposes every one of `batch` at once, by the core's own way of taking several: one
    /// `MsgPropose` of them all for raft-rs's line (which each core steps as it steps a
    /// forwarded proposal), one `append_command` each for slates'. The message the owner builds
    /// is the owner's.
    fn propose_batch(&mut self, batch: Vec<Vec<u8>>) -> bool;
    /// Proposes `data` by the fast track, from any member.
    fn propose_fast(&mut self, data: Vec<u8>, out: &mut Vec<Envelope<Self::Message>>) -> Fast;
    /// A leader opens the fast track where the core asks for it to be
    /// opened; focal's line opens it with the group (`Config::fast`).
    fn open_fast(&mut self) -> bool {
        true
    }
    /// The indexes this member committed by a fast quorum while it led: zero for a core that
    /// does not say.
    fn fast_committed(&self) -> u64 {
        0
    }
    /// Hands the lead to `to`.
    fn transfer(&mut self, to: u64, out: &mut Vec<Envelope<Self::Message>>) -> bool;
    /// A message from the network. What it asks to send is given here or at
    /// the next [`Core::flush`].
    fn step(&mut self, from: u64, message: Self::Message, out: &mut Vec<Envelope<Self::Message>>);
    /// Whatever the member has to persist, send and apply.
    fn flush(&mut self, out: &mut Vec<Envelope<Self::Message>>);
    /// Everything applied becomes a snapshot of the application, and the log
    /// before it is dropped. The owner's work.
    fn compact(&mut self);
}

/// The application every core applies to: a digest of each entry's index,
/// length and first and last words, and a count. It costs the same for an
/// entry of any size, so that what a run measures is the core and not the
/// application; that the members applied the same entries is the
/// differential's to prove (hyper-raft `tests/differential.rs`), and this
/// digest only checks the run. The snapshot of it is its twenty-four bytes and
/// the bytes the workload says the application holds besides.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct App {
    pub digest: u64,
    pub count: u64,
    pub index: u64,
}

impl App {
    pub fn apply(&mut self, index: u64, data: &[u8]) {
        let word = |bytes: Option<&[u8]>| {
            bytes.map_or(0, |bytes| u64::from_le_bytes(bytes.try_into().unwrap()))
        };
        let first = word(data.get(..8));
        let last = word(data.len().checked_sub(8).and_then(|at| data.get(at..)));
        let mut digest = self.digest;
        for value in [index, data.len() as u64, first, last] {
            digest = (digest ^ value).wrapping_mul(0x0000_0100_0000_01b3);
        }
        self.digest = digest;
        self.count += 1;
        self.index = index;
    }
    pub fn encode(&self, state_bytes: usize) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(24 + state_bytes);
        for word in [self.digest, self.count, self.index] {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        bytes.resize(24 + state_bytes, 0x5a);
        bytes
    }
    pub fn decode(bytes: &[u8]) -> Self {
        let word = |at: usize| u64::from_le_bytes(bytes[at * 8..at * 8 + 8].try_into().unwrap());
        Self {
            digest: word(0),
            count: word(1),
            index: word(2),
        }
    }
}

/// SplitMix64 (Steele, Lea and Flood, OOPSLA 2014): a run is its seed.
pub fn splitmix(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut drawn = *state;
    drawn = (drawn ^ (drawn >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    drawn = (drawn ^ (drawn >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    drawn ^ (drawn >> 31)
}
