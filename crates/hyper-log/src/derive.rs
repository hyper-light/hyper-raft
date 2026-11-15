//! A log's [`Config`] derived from what its node and its device state ([`Facts`]), so that no
//! caller copies the log's own constants (the per-group submissions a handle keeps out, a frame's
//! headers) into a number of its own, which would go stale silently when they change.
//!
//! Each field follows from the facts as `docs/durable.md` §6 states it: the segment from the
//! largest entry and the device's block, the file's segments from the disk budget, the groups from
//! the node's admission bound, a group's retention and recent bytes, and the queue from the groups.
//! The log's design is mantle's (`ORIGIN.md`), and the section of mantle's
//! `docs/design/raft-log.md` each rule follows is named beside it below. Facts that cannot hold
//! one frame, or a budget under the log's least file, are refused ([`Unfit`]), never clamped: a
//! clamped configuration would run, and fail later at a write the facts promised.
use hyper_block::buf::{Alignment, MAX_BUFFER};

use crate::{Config, LogError, Waits, format, frame_room, recover, room};

/// What a node and its device state, from which [`Config::derive`] derives a log's configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Facts {
    /// The block the log's file is laid out in (raft-log.md §2's `B`): the file's
    /// `BlockFile::layout_block`, its transfer alignment when direct and the device's write unit
    /// when buffered. Every segment is a multiple of it.
    pub align: Alignment,
    /// Whether the log is sealed (`Log::create_sealed`): a sealed frame also holds its MAC and a
    /// key record, and each record its tag.
    pub sealed: bool,
    /// Bytes of the largest entry an owner submits: the log holds it in one frame of its own.
    pub largest_entry: usize,
    /// Bytes the node's disk budget gives the log's file, its persist area included.
    pub disk_bytes: u64,
    /// The node's admission bound on groups: the most it may be placed, never its current
    /// placement, since groups arrive while the log runs.
    pub max_groups: usize,
    /// Entries between an owner's checkpoints: after one, the group's log starts past it.
    pub cadence_entries: u64,
    /// Payload bytes between an owner's checkpoints, at most.
    pub cadence_bytes: u64,
    /// Entries a group may hold uncommitted past its checkpoint cadence.
    pub uncommitted_entries: u64,
    /// Payload bytes a group may hold uncommitted past its checkpoint cadence.
    pub uncommitted_bytes: u64,
    /// Bytes of recent entries the node gives the log's cache across all its groups.
    pub cache_bytes: u64,
}

/// Why facts give no configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Unfit {
    /// A fact that must be positive is zero.
    #[error("{0} is zero")]
    Zero(&'static str),
    /// No segment the log can read in one buffer holds a frame of the largest entry.
    #[error("an entry of {largest} bytes is past the {most} bytes a segment's frame can hold")]
    Entry {
        /// The stated largest entry.
        largest: usize,
        /// The largest entry a segment of one buffer holds alone.
        most: usize,
    },
    /// The disk budget is under the log's least file: its persist area, three working segments,
    /// and the segments one group's largest entry takes as the file stores it.
    #[error("the log needs {needed} bytes of disk at least, and the budget is {budget}")]
    Disk {
        /// The least file, in bytes.
        needed: u64,
        /// The stated budget.
        budget: u64,
    },
    /// A group's retained bytes cannot hold the largest entry.
    #[error("a group retains {retained} bytes, under the largest entry's {largest}")]
    Retention {
        /// The derived retention, in bytes.
        retained: u64,
        /// The stated largest entry.
        largest: usize,
    },
    /// A derived quantity past its type.
    #[error("{0} past its bound")]
    Overflow(&'static str),
}

impl From<Unfit> for LogError {
    fn from(unfit: Unfit) -> Self {
        LogError::Unfit(unfit)
    }
}

/// Bytes an entry's record takes in a frame besides its payload.
fn entry_overhead(sealed: bool) -> Result<usize, Unfit> {
    format::encoded_len(
        &format::Record::Entries {
            group: 0,
            first: 0,
            entries: &[(0, &[])],
        },
        if sealed { format::TAG_LEN } else { 0 },
    )
    .ok_or(Unfit::Overflow("an entry's record"))
}

/// Bytes of a segment past its frame's payload: the segment's header block, the frame's header,
/// and in a sealed log the frame's MAC and a key record (`frame_room`).
fn frame_overhead(block: usize, sealed: bool) -> Result<usize, Unfit> {
    let sealing = if sealed {
        format::MAC_LEN
            .checked_add(format::KEY_RECORD_LEN)
            .ok_or(Unfit::Overflow("a key record"))?
    } else {
        0
    };
    block
        .checked_add(format::FRAME_HEADER_LEN)
        .and_then(|n| n.checked_add(sealing))
        .ok_or(Unfit::Overflow("a frame's headers"))
}

/// Segments of the file that hold no group's retention: the head, its successor and one to
/// reclaim into (mantle `docs/design/raft-log.md` §5), the least file's three.
const WORKING_SEGMENTS: u64 = 3;

/// Bytes a retained entry takes in the file besides its payload, as the sweep counts it when it
/// moves the entry (`writer::sweepable`: each live piece with a relocated record's header, as if no
/// two ran together): the entry's header, its record's header, and in a sealed log its tag.
fn stored_per_entry(sealed: bool) -> u64 {
    let tag = if sealed { format::TAG_LEN } else { 0 };
    let bytes = format::ENTRY_HEADER_LEN
        .saturating_add(format::ENTRIES_HEADER_LEN)
        .saturating_add(tag);
    u64::try_from(bytes).unwrap_or(u64::MAX)
}

/// Bytes a group's records other than its entries take in the file at most while live: its hard
/// state, its start and its released mark, the latest of each.
fn stored_per_group(sealed: bool) -> Result<u64, Unfit> {
    let tag = if sealed { format::TAG_LEN } else { 0 };
    let records = [
        format::Record::HardState {
            group: 0,
            state: format::HardState::default(),
        },
        format::Record::Start {
            group: 0,
            start: format::Start::default(),
        },
        format::Record::Released {
            group: 0,
            through: 0,
        },
    ];
    records.iter().try_fold(0u64, |sum, record| {
        format::encoded_len(record, tag)
            .and_then(|len| u64::try_from(len).ok())
            .and_then(|len| sum.checked_add(len))
            .ok_or(Unfit::Overflow("a group's records"))
    })
}

/// Bytes of a segment's room for frames: all of it but its header's block.
fn segment_room(segment_bytes: u64, align: Alignment) -> Result<u64, Unfit> {
    let block = u64::try_from(align.get()).map_err(|_| Unfit::Overflow("the block"))?;
    segment_bytes
        .checked_sub(block)
        .ok_or(Unfit::Overflow("a segment's room"))
}

/// The bytes of the file one group's retention may take, as the file stores it: half the room of
/// the segments past the working ones. Half, because a frame goes whole into one segment: one that
/// does not fit a segment's rest opens the next, so two segments in a row hold more than one
/// segment's room of frames, and a segment holds at least half its room once the sweep has packed
/// its live records into frames (whatever their size: an entry just past half a segment fills one
/// segment alone). The share is the whole file's, not divided among the groups the node admits: an
/// admission bound is the most groups a node may be placed, which fill together only where the
/// owner sized the file for them ([`Config::disk_for`]); divided, a node of 4,096 groups on a GiB
/// left each less than one entry of 8 MiB.
fn group_share(segment_bytes: u64, max_segments: u32, align: Alignment) -> Result<u64, Unfit> {
    let room = segment_room(segment_bytes, align)?;
    let retaining = u64::from(max_segments).saturating_sub(WORKING_SEGMENTS);
    Ok(room
        .checked_mul(retaining)
        .ok_or(Unfit::Overflow("the file's room"))?
        .checked_div(2)
        .unwrap_or(0))
}

/// Bytes a group's retention of `entries` and `bytes` of payload takes of the file as it stores
/// it: each entry's headers beside its payload, and the group's other records.
fn stored(entries: u64, bytes: u64, sealed: bool) -> Result<u128, Unfit> {
    Ok(u128::from(entries)
        .saturating_mul(u128::from(stored_per_entry(sealed)))
        .saturating_add(u128::from(bytes))
        .saturating_add(u128::from(stored_per_group(sealed)?)))
}

/// What a group retains by the checkpoint cadence, `(entries, payload bytes)`: two cadences and
/// what may be uncommitted past them (mantle `docs/design/raft-log.md` §4).
fn cadenced(facts: &Facts) -> Result<(u64, u64), Unfit> {
    let cadence_entries = positive(facts.cadence_entries, "the checkpoint cadence's entries")?;
    let cadence_bytes = positive(facts.cadence_bytes, "the checkpoint cadence's bytes")?;
    let entries = cadence_entries
        .checked_mul(2)
        .and_then(|n| n.checked_add(facts.uncommitted_entries))
        .ok_or(Unfit::Overflow("a group's retained entries"))?;
    let bytes = cadence_bytes
        .checked_mul(2)
        .and_then(|n| n.checked_add(facts.uncommitted_bytes))
        .ok_or(Unfit::Overflow("a group's retained bytes"))?;
    Ok((entries, bytes))
}

/// A group's retention, `(entries, payload bytes)`, held to what `share` bytes of the file store:
/// what the checkpoint cadence asks if the file holds it; else the payload bound held to half the
/// share (never under the largest entry), and the entries to what the rest holds of their headers
/// beside the group's other records, so that a group is refused `Backlog`, its owner compacting, before the file is refused
/// `Full` (found by mantle: a file sized for its groups' payload filled with one block's frame an
/// entry of a few hundred bytes, and every write was refused `Full`, 2026-11-07). The payload
/// bound gives way last: an owner's checkpoint cadence is its bytes, and a group of small entries
/// reaches the entries' bound first, which their headers make the file's.
fn held_retention(
    (entries, bytes): (u64, u64),
    share: u64,
    largest: u64,
    sealed: bool,
) -> Result<(u64, u64), Unfit> {
    let per_entry = stored_per_entry(sealed);
    let fixed = stored_per_group(sealed)?;
    if stored(entries, bytes, sealed)? <= u128::from(share) {
        return Ok((entries, bytes));
    }
    // Half the share, and never under the largest entry, which the least file holds whole.
    let bytes = bytes.min((share / 2).max(largest));
    let headers = share.saturating_sub(fixed).saturating_sub(bytes);
    let entries = entries.min(headers.checked_div(per_entry).unwrap_or(0));
    Ok((entries, bytes))
}

fn up(align: Alignment, n: u64) -> Result<u64, Unfit> {
    align.up_u64(n).ok_or(Unfit::Overflow("a segment"))
}

fn positive(value: u64, name: &'static str) -> Result<u64, Unfit> {
    if value == 0 {
        Err(Unfit::Zero(name))
    } else {
        Ok(value)
    }
}

impl Config {
    /// The bytes of a disk budget that hold every group the node admits retaining all its
    /// checkpoint cadence asks at once, as the file stores it (`docs/durable.md` §6): the persist
    /// area's segment, the three working segments, and twice the groups' retention, counting each
    /// entry's headers and each group's other records, in whole segments. On this budget or more,
    /// [`Config::derive`] gives every group the cadence's bounds, and every group filling at once
    /// is refused `Backlog` before the file is refused `Full`; on less, each group's bounds are
    /// held to what the file holds of that group alone.
    pub fn disk_for(facts: &Facts) -> Result<u64, LogError> {
        let segment_bytes = Self::segment_for(facts)?;
        let room = segment_room(segment_bytes, facts.align)?;
        let (entries, bytes) = cadenced(facts)?;
        let groups = u128::try_from(facts.max_groups).map_err(|_| Unfit::Overflow("the groups"))?;
        let all = stored(entries, bytes, facts.sealed)?
            .saturating_mul(groups)
            .saturating_mul(2);
        let retaining = all.div_ceil(u128::from(room.max(1)));
        let segments = retaining
            .saturating_add(u128::from(WORKING_SEGMENTS))
            .saturating_add(1);
        u64::try_from(segments.saturating_mul(u128::from(segment_bytes)))
            .map_err(|_| Unfit::Overflow("the disk the groups' retention needs").into())
    }

    /// The segment `facts` give: the least multiple of the block whose frame holds the largest
    /// entry in a record of its own, four blocks and two persist slots at least, and one I/O
    /// buffer at most ([`Config::derive`]).
    fn segment_for(facts: &Facts) -> Result<u64, LogError> {
        let largest = facts.largest_entry;
        positive(
            u64::try_from(largest).map_err(|_| Unfit::Overflow("the largest entry"))?,
            "the largest entry",
        )?;
        let max_groups = facts.max_groups;
        if max_groups == 0 {
            return Err(Unfit::Zero("the admission bound on groups").into());
        }

        let block = facts.align.get();
        let block_bytes = u64::try_from(block).map_err(|_| Unfit::Overflow("the block"))?;
        let around = frame_overhead(block, facts.sealed)?
            .checked_add(entry_overhead(facts.sealed)?)
            .ok_or(Unfit::Overflow("a frame's headers"))?;
        let buffer = u64::try_from(MAX_BUFFER).map_err(|_| Unfit::Overflow("a buffer"))?;
        // The largest segment recovery reads in one buffer, at the block: what it holds alone.
        let most_segment = buffer
            .checked_div(block_bytes)
            .and_then(|blocks| blocks.checked_mul(block_bytes))
            .ok_or(Unfit::Overflow("the block"))?;
        let most = usize::try_from(most_segment)
            .ok()
            .and_then(|segment| segment.checked_sub(around))
            .unwrap_or(0);
        let holding = largest
            .checked_add(around)
            .and_then(|n| u64::try_from(n).ok())
            .ok_or(Unfit::Entry { largest, most })?;
        let four = block_bytes
            .checked_mul(4)
            .ok_or(Unfit::Overflow("four blocks"))?;
        let slots = format::persist_len(max_groups)
            .and_then(|len| len.checked_add(if facts.sealed { format::MAC_LEN } else { 0 }))
            .and_then(|len| u64::try_from(len).ok())
            .and_then(|len| facts.align.up_u64(len))
            .and_then(|slot| slot.checked_mul(2))
            .ok_or(Unfit::Overflow("the persist slots"))?;
        let segment_bytes = up(facts.align, holding)?
            .max(four)
            .max(up(facts.align, slots)?);
        if segment_bytes > most_segment {
            return Err(Unfit::Entry { largest, most }.into());
        }
        Ok(segment_bytes)
    }

    /// The configuration `facts` give (`docs/durable.md` §6; the sections named are mantle's
    /// `docs/design/raft-log.md`, which the log's design comes from):
    ///
    /// - `segment_bytes`: the least multiple of the block whose frame holds the largest entry in
    ///   a record of its own (§2, §3: an update past one frame is split, and one entry is never
    ///   split), four blocks at least and two persist slots of `max_groups` groups at least
    ///   (§2), and no more than one I/O buffer, through which recovery reads a segment.
    /// - `max_segments`: the segments the disk budget holds after the persist area, one
    ///   segment's length (§2); at least the head, its successor and one to reclaim into (§5),
    ///   and what one group's largest entry takes past them as the file stores it.
    /// - `max_groups`: the node's admission bound; reaching it is `TooManyGroups`.
    /// - `group_entries`, `group_bytes`: a group retains its entries until its engine makes them
    ///   durable, plus a window for lagging followers (§4). The engine's durable point is its
    ///   checkpoint, so a group retains two cadences, the one being checkpointed and the one
    ///   after it, and what may be uncommitted past them; past that a follower is sent the
    ///   checkpoint, and a submission that would pass it is refused `Backlog` until the owner
    ///   compacts. A group's retention fits the file as the file stores it: each entry with its
    ///   headers, and a segment counted at half its room (`group_share`); where the cadence asks
    ///   more, the payload bound is held to half the file's share and the entries' bound to the
    ///   rest, so a group is refused `Backlog`, its own bound, before the file is refused `Full`,
    ///   whatever its updates' size, down to one entry an update. Every group at once does so on
    ///   a budget of [`Config::disk_for`] or more.
    /// - `group_cache`: the node's cache shared evenly by its groups (§4).
    /// - `queue_submissions`: the most writes every group's handle keeps in the log at once, so
    ///   the queue never refuses a handle's write for count (§3; hyper-raft docs/durable.md §6).
    /// - `waits`: `Measured`, what a node runs (§3).
    ///
    /// The result passes the check `Log::create` and `Log::open` make.
    pub fn derive(facts: &Facts) -> Result<Config, LogError> {
        let largest = facts.largest_entry;
        let max_groups = facts.max_groups;
        let segment_bytes = Self::segment_for(facts)?;
        let cache = positive(facts.cache_bytes, "the cache")?;

        // The persist area, one segment's length, then the segments.
        // The least file: the persist area, the working segments, and what one group's largest
        // entry takes as the file stores it, counted at half a segment's room (`group_share`).
        let one = stored(
            1,
            u64::try_from(largest).map_err(|_| Unfit::Overflow("the largest entry"))?,
            facts.sealed,
        )?;
        let room = u128::from(segment_room(segment_bytes, facts.align)?);
        let least = one
            .saturating_mul(2)
            .div_ceil(room.max(1))
            .saturating_add(u128::from(WORKING_SEGMENTS));
        let needed = u64::try_from(least.saturating_add(1))
            .ok()
            .and_then(|segments| segments.checked_mul(segment_bytes))
            .ok_or(Unfit::Overflow("the least file"))?;
        let segments = facts
            .disk_bytes
            .checked_sub(segment_bytes)
            .and_then(|room| room.checked_div(segment_bytes))
            .unwrap_or(0);
        if u128::from(segments) < least {
            return Err(Unfit::Disk {
                needed,
                budget: facts.disk_bytes,
            }
            .into());
        }
        let max_segments =
            u32::try_from(segments).map_err(|_| Unfit::Overflow("the file's segments"))?;

        let (group_entries, group_bytes) = held_retention(
            cadenced(facts)?,
            group_share(segment_bytes, max_segments, facts.align)?,
            u64::try_from(largest).map_err(|_| Unfit::Overflow("the largest entry"))?,
            facts.sealed,
        )?;
        if group_entries == 0
            || u64::try_from(largest).map_or(true, |largest| group_bytes < largest)
        {
            return Err(Unfit::Retention {
                retained: group_bytes,
                largest,
            }
            .into());
        }
        let groups = u64::try_from(max_groups).map_err(|_| Unfit::Overflow("the groups"))?;
        let group_cache = positive(
            cache.checked_div(groups).unwrap_or(0),
            "the cache shared by every group",
        )?;
        let queue_submissions = max_groups
            .checked_mul(room::GROUP_SUBMISSIONS)
            .ok_or(Unfit::Overflow("the queue's submissions"))?;

        let config = Config {
            segment_bytes,
            max_segments,
            max_groups,
            group_entries,
            group_bytes,
            group_cache,
            queue_submissions,
            waits: Waits::Measured,
        };
        recover::check(&config, facts.align, facts.sealed)?;
        // By construction; checked so a change to a frame's layout fails here, not at a write.
        let one = entry_overhead(facts.sealed)?;
        let room = frame_room(&config, facts.align, facts.sealed)?;
        if room.checked_sub(one).is_none_or(|room| room < largest) {
            return Err(LogError::Config(
                "a derived frame does not hold the largest entry",
            ));
        }
        Ok(config)
    }
}
