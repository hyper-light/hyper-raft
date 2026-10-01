//! What the writer process (`hyper-log-writer`) and the test that kills it agree on: the log's
//! settings, how the file is opened, and the bytes of every entry, which the test checks every
//! acknowledged entry against after recovery.
//!
//! The writer appends to `groups` groups in rounds: every group's next entry is submitted, then
//! every answer is waited for and each acknowledged one printed as `ack <group> <index>
//! <start>`, its stdout flushed after each line. A group's entry carries a hard state whose
//! commit is the entry, and every [`KEEP`]th entry a start that compacts the group to its last
//! [`KEEP`] entries, so the log reclaims segments as a node's does.

use std::path::Path;

use hyper_block::buf::Alignment;
use hyper_block::file::{CachingRequest, DeviceFile};
use hyper_log::{Config, Entries, Entry, HardState, Start, Update, Waits};

/// The log's ID.
pub const ID: u128 = 0x6879_7065_722d_6c6f_672d_6532_6500;

/// Entries a group keeps behind its last when it compacts: mantle's log benchmark's window.
pub const KEEP: u64 = 64;

/// The direct-I/O alignment the file is opened with: the 4 KiB every device this repository
/// runs on reads and writes in (mantle `docs/design/raft-log.md` §2 takes the largest of 4 KiB
/// and the device's block sizes; none of these exceeds it).
pub const BLOCK: usize = 4096;

/// Bytes of a segment: 64 blocks of [`BLOCK`], small so that a run of a few thousand appends
/// reclaims segments many times over.
pub const SEGMENT_BYTES: u64 = 64 * 4096;

/// The log's settings for `groups` groups: segments of 64 blocks, eight of them, so a run of a
/// few thousand appends reclaims segments many times over.
pub fn config(groups: usize) -> Config {
    Config {
        segment_bytes: SEGMENT_BYTES,
        max_segments: 8,
        max_groups: groups,
        group_entries: KEEP.saturating_mul(4),
        group_bytes: 1 << 20,
        group_cache: 4 << 10,
        queue_submissions: groups.saturating_mul(2),
        waits: Waits::Measured,
    }
}

/// The log's file at `path`, with direct I/O where the file system takes it.
pub fn open(path: &Path, create: bool) -> Result<DeviceFile, hyper_block::DiskError> {
    let align = Alignment::new(BLOCK).map_err(hyper_block::DiskError::Buf)?;
    DeviceFile::open(path, create, CachingRequest::PreferDirect, align)
}

/// The bytes of `group`'s entry `index`: up to 300 of them, from a hash of both, so a byte
/// from another entry or another group shows.
pub fn payload(group: u128, index: u64) -> Vec<u8> {
    let bytes = group.to_le_bytes();
    let low = bytes
        .first_chunk::<8>()
        .map_or(0, |b| u64::from_le_bytes(*b));
    let mut state = low.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(index);
    let len = usize::try_from(index.wrapping_mul(37).checked_rem(300).unwrap_or(0)).unwrap_or(0);
    (0..len)
        .map(|_| {
            state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            (z ^ (z >> 31)).to_le_bytes().first().copied().unwrap_or(0)
        })
        .collect()
}

/// The update that appends `group`'s entry `index`, its hard state, and a start every
/// [`KEEP`]th entry.
pub fn update(group: u128, index: u64) -> Update {
    let compacts = index > KEEP && index.is_multiple_of(KEEP);
    Update {
        start: compacts.then(|| Start {
            index: index.saturating_sub(KEEP),
            term: 1,
        }),
        entries: Some(Entries {
            first: index,
            entries: vec![Entry {
                term: 1,
                bytes: payload(group, index),
            }],
        }),
        hard_state: Some(HardState {
            term: 1,
            vote: 0,
            commit: index,
        }),
        ..Update::default()
    }
}
