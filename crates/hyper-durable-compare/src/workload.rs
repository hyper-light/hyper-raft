//! What both shells commit: mantle's range entries, applied by mantle's engine and layer alike on
//! both sides, so the shells are what differs. A range of the Name layer, founded as mantle's
//! group test founds it (`crates/range/tests/group.rs` at `1c179e8`).
use mantle_meta::apply::Layer;
use mantle_meta::engine::{Engine, Model};
use mantle_meta::name::{self, Preconditions, Put};
use mantle_meta::record::{Version, Versioning};
use mantle_meta::session::Rules;
use mantle_meta::wire::{Command, Entry, Sessioned};

/// The layer every member applies.
pub const LAYER: Layer = Layer::Name;

/// The session rules: mantle's group test's, with room for every registration a run makes.
pub const RULES: Rules = Rules {
    lifetime_ns: 3_600_000_000_000,
    max_sessions: 1 << 20,
    max_answers: 16,
    max_answer_bytes: usize::MAX,
    expiries_per_entry: 8,
};

/// The engine of a cell's first Name range, before any entry: its lineage, holding every key.
pub fn first_range() -> Model {
    let mut m = Model::default();
    m.install(0, name::first(1).expect("the first range"))
        .expect("installed");
    m.persist().expect("persisted");
    m
}

/// What an entry carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    /// A session's registration: mantle's group test's entry, some tens of bytes.
    Register,
    /// An object version's put whose etag makes the entry 1 KiB: §13's closed-loop entries.
    Put1k,
}

impl Shape {
    pub fn name(self) -> &'static str {
        match self {
            Self::Register => "register",
            Self::Put1k => "put-1KiB",
        }
    }

    /// Entry `n` of this shape.
    pub fn entry(self, n: u64) -> Entry {
        let command = match self {
            Self::Register => Command::Register,
            Self::Put1k => put(n),
        };
        Entry {
            at_ns: n,
            commands: vec![Sessioned {
                session: 0,
                serial: n,
                unanswered: 0,
                command,
            }],
        }
    }
}

fn put(n: u64) -> Command {
    // The etag fills the entry to 1 KiB of encoding with the rest of the command.
    let etag: String = std::iter::repeat_n('e', 900).collect();
    Command::Name(Box::new(name::Command::Put(Put {
        bucket: "b".into(),
        incarnation: 1,
        key: format!("k{n}"),
        versioning: Versioning::Enabled,
        preconditions: Preconditions::default(),
        at_ns: n,
        ordered_ns: None,
        version: Version {
            marker: false,
            null: false,
            modified_ns: n,
            etag,
            size: 1,
            checksum: None,
            file: Some(1),
            owner: "o".into(),
            headers: Vec::new(),
            retention: None,
            legal_hold: None,
            listing: None,
        },
        default: None,
        id: 0,
        deadline_ns: u64::MAX,
    })))
}
