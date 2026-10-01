//! A member's durable log: one append-only file of checksummed records, written whole and
//! flushed with the platform's full flush before what it holds is acted on.
//!
//! A record is `[length][crc32c][kind][payload]`: the length of the payload, the checksum of
//! the kind and the payload, and a payload that is a hard state or an entry in raft-rs's
//! encoding. Opening replays the file: the last record of each entry index wins, and the last
//! hard state. A process killed mid-write leaves at most its last record torn, so a damaged
//! record that ends the file is cut off, and one with records after it is corruption, which
//! refuses the open.
//!
//! The flush is `File::sync_data`: `fdatasync(2)` on Linux, `fcntl(F_FULLFSYNC)` on macOS (the
//! standard library's choice there, since `fsync(2)` leaves data in the drive's cache), and
//! `FlushFileBuffers` on Windows. A file that did not exist is followed by a flush of its
//! directory on Unix, so that the name survives too; Windows offers no directory handle to the
//! standard library, and NTFS journals the creation.
//!
//! This module is the harness's device writer, as `hyper-log`'s device module is the shared
//! log's: the one place it writes files.
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};

use hyper_raft::{
    StorageError,
    proto::{ConfState, Entry, HardState, Snapshot, protocompat::PbMessage},
};

use crate::wire::crc32c;

/// A record's length and checksum, before its kind.
const RECORD_HEADER: usize = 8;
/// The kind of a record holding a hard state: this format's own number.
const HARD_STATE: u8 = 1;
/// The kind of a record holding an entry: this format's own number.
const ENTRY: u8 = 2;

/// Why the log could not do what it was asked.
#[derive(Debug)]
pub enum WalError {
    /// The file system refused.
    Io(std::io::Error),
    /// A record that is not the last does not read back as written.
    Corrupt(&'static str),
    /// The log holds as many entries as it was opened to hold.
    Full,
}

impl std::fmt::Display for WalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "the log's file: {error}"),
            Self::Corrupt(what) => write!(f, "the log is corrupt: {what}"),
            Self::Full => f.write_str("the log is full"),
        }
    }
}

impl std::error::Error for WalError {}

impl From<std::io::Error> for WalError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

/// What a replay found.
struct Replayed {
    hard: HardState,
    entries: Vec<Entry>,
    /// The bytes of whole records; past them is a torn tail.
    valid: usize,
}

/// One record read, or the end of what reads whole.
enum Next {
    Record {
        kind: u8,
        payload: (usize, usize),
        end: usize,
    },
    End,
}

fn next_record(bytes: &[u8], at: usize) -> Result<Next, WalError> {
    let Some(header) = bytes.get(at..at.saturating_add(RECORD_HEADER)) else {
        return Ok(Next::End);
    };
    let (length, crc) = header.split_at(4);
    let length = usize::try_from(u32::from_le_bytes(length.try_into().unwrap_or([0; 4])))
        .map_err(|_| WalError::Corrupt("a record's length"))?;
    let crc = u32::from_le_bytes(crc.try_into().unwrap_or([0; 4]));
    let start = at.saturating_add(RECORD_HEADER);
    let end = start.saturating_add(1).saturating_add(length);
    let Some(body) = bytes.get(start..end) else {
        // It runs past the file: the write that made it did not finish.
        return Ok(Next::End);
    };
    if crc32c(body) != crc {
        return if end == bytes.len() {
            Ok(Next::End)
        } else {
            Err(WalError::Corrupt("a record that is not the last"))
        };
    }
    let kind = body.first().copied().unwrap_or(0);
    Ok(Next::Record {
        kind,
        payload: (start.saturating_add(1), end),
        end,
    })
}

/// Places an entry read back: it replaces the entry at its index and every one after.
fn place(entries: &mut Vec<Entry>, entry: Entry) -> Result<(), WalError> {
    let position = usize::try_from(entry.index.saturating_sub(1))
        .map_err(|_| WalError::Corrupt("an entry's index"))?;
    if entry.index == 0 || position > entries.len() {
        return Err(WalError::Corrupt("an entry after a gap"));
    }
    entries.truncate(position);
    entries.push(entry);
    Ok(())
}

fn replay(bytes: &[u8], max_entries: usize) -> Result<Replayed, WalError> {
    let mut replayed = Replayed {
        hard: HardState::default(),
        entries: Vec::new(),
        valid: 0,
    };
    while let Next::Record { kind, payload, end } = next_record(bytes, replayed.valid)? {
        let payload = bytes.get(payload.0..payload.1).unwrap_or(&[]);
        match kind {
            HARD_STATE => {
                replayed.hard =
                    HardState::decode(payload).map_err(|_| WalError::Corrupt("a hard state"))?;
            }
            ENTRY => {
                let entry = Entry::decode(payload).map_err(|_| WalError::Corrupt("an entry"))?;
                place(&mut replayed.entries, entry)?;
                if replayed.entries.len() > max_entries {
                    return Err(WalError::Full);
                }
            }
            _ => return Err(WalError::Corrupt("a record of no kind")),
        }
        replayed.valid = end;
    }
    Ok(replayed)
}

/// Appends one record of `kind` holding `message` to `buffer`.
fn record(buffer: &mut Vec<u8>, kind: u8, message: &impl PbMessage) -> Result<(), WalError> {
    let length =
        u32::try_from(message.encoded_len()).map_err(|_| WalError::Corrupt("a record too long"))?;
    let start = buffer.len();
    buffer.extend_from_slice(&length.to_le_bytes());
    buffer.extend_from_slice(&[0; 4]);
    buffer.push(kind);
    message
        .encode(buffer)
        .map_err(|_| WalError::Corrupt("an encoding"))?;
    let body = start.saturating_add(RECORD_HEADER);
    let crc = crc32c(buffer.get(body..).unwrap_or(&[]));
    if let Some(slot) = buffer.get_mut(start.saturating_add(4)..body) {
        slot.copy_from_slice(&crc.to_le_bytes());
    }
    Ok(())
}

/// Cuts a torn tail off the file. The one place the harness shortens a file.
#[expect(
    clippy::disallowed_methods,
    reason = "the harness's device writer cuts a torn record off its own log, as hyper-log's device module may"
)]
fn cut(file: &File, valid: usize) -> Result<(), WalError> {
    let length = u64::try_from(valid).map_err(|_| WalError::Corrupt("a log too long"))?;
    file.set_len(length)?;
    file.sync_data()?;
    Ok(())
}

/// Flushes the directory that holds `path`, so that a file just created keeps its name.
#[cfg(unix)]
fn flush_directory(path: &Path) -> Result<(), WalError> {
    let directory = path.parent().unwrap_or_else(|| Path::new("."));
    File::open(directory)?.sync_all()?;
    Ok(())
}
#[cfg(not(unix))]
fn flush_directory(_path: &Path) -> Result<(), WalError> {
    Ok(())
}

/// The durable log of one member, and its contents in memory, which the core reads.
pub struct Wal {
    file: File,
    hard: HardState,
    configuration: ConfState,
    entries: Vec<Entry>,
    /// What one write encodes into, kept from one write to the next.
    buffer: Vec<u8>,
    max_entries: usize,
    /// Entries handed over that could not be placed.
    damaged: Option<WalError>,
}

impl Wal {
    /// The log at `path` of a group of `voters`, holding at most `max_entries` entries; made
    /// empty if there is none.
    pub fn open(path: &Path, voters: Vec<u64>, max_entries: usize) -> Result<Self, WalError> {
        let created = !path.exists();
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        if created {
            file.sync_all()?;
            flush_directory(path)?;
        }
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        let replayed = replay(&bytes, max_entries)?;
        if replayed.valid < bytes.len() {
            cut(&file, replayed.valid)?;
        }
        file.seek(SeekFrom::End(0))?;
        Ok(Self {
            file,
            hard: replayed.hard,
            configuration: ConfState {
                voters,
                ..ConfState::default()
            },
            entries: replayed.entries,
            buffer: Vec::new(),
            max_entries,
            damaged: None,
        })
    }
    /// Writes `entries` and `hard` and flushes them; then the log in memory holds them. The
    /// entries are taken whole: the log keeps them, copying none.
    pub fn persist(
        &mut self,
        entries: Vec<Entry>,
        hard: Option<&HardState>,
    ) -> Result<(), WalError> {
        self.write(&entries, hard)?;
        self.keep(entries);
        self.damage()
    }
    /// Writes `entries` and `hard` where they are, and flushes them. The log in memory takes
    /// the entries when they are handed over ([`Wal::keep`]); until then it reads them from the
    /// member, which holds them while they are written (`RawNode::ready_in_place`).
    pub fn write(&mut self, entries: &[Entry], hard: Option<&HardState>) -> Result<(), WalError> {
        if entries.is_empty() && hard.is_none() {
            return Ok(());
        }
        let end = entries.last().map_or(0, |entry| {
            usize::try_from(entry.index).unwrap_or(usize::MAX)
        });
        if end > self.max_entries {
            return Err(WalError::Full);
        }
        self.buffer.clear();
        for entry in entries {
            record(&mut self.buffer, ENTRY, entry)?;
        }
        if let Some(hard) = hard {
            let mut hard = hard.clone();
            hard.commit = hard.commit.max(self.hard.commit);
            record(&mut self.buffer, HARD_STATE, &hard)?;
        }
        self.file.write_all(&self.buffer)?;
        self.file.sync_data()?;
        if let Some(hard) = hard {
            let commit = hard.commit.max(self.hard.commit);
            self.hard = hard.clone();
            self.hard.commit = commit;
        }
        Ok(())
    }
    /// Takes entries already written, as they are, into the log in memory. A place it cannot
    /// take them at is kept as damage, which [`Wal::damage`] reports.
    pub fn keep(&mut self, entries: Vec<Entry>) {
        for entry in entries {
            if let Err(error) = place(&mut self.entries, entry) {
                self.damaged.get_or_insert(error);
                return;
            }
        }
    }
    /// Whether the log in memory took every entry it was handed.
    pub fn damage(&mut self) -> Result<(), WalError> {
        self.damaged.take().map_or(Ok(()), Err)
    }
    /// The entries of `[first, last]`, where the log holds them.
    pub fn held(&self, first: u64, last: u64) -> Result<&[Entry], WalError> {
        let low =
            usize::try_from(first.saturating_sub(1)).map_err(|_| WalError::Corrupt("an index"))?;
        let high = usize::try_from(last).map_err(|_| WalError::Corrupt("an index"))?;
        self.entries
            .get(low..high)
            .ok_or(WalError::Corrupt("a range the log does not hold"))
    }
    /// The hard state, its commit as last known.
    pub fn hard_state(&self) -> &HardState {
        &self.hard
    }
    /// The commit moved; it is written with the next record.
    pub fn set_commit(&mut self, commit: u64) {
        self.hard.commit = self.hard.commit.max(commit);
    }
    /// The entries held, from index one.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }
    fn range(&self, low: u64, high: u64) -> Result<&[Entry], StorageError> {
        if low == 0 || low > high {
            return Err(StorageError::Unavailable);
        }
        let low = usize::try_from(low.saturating_sub(1)).map_err(|_| StorageError::Unavailable)?;
        let high =
            usize::try_from(high.saturating_sub(1)).map_err(|_| StorageError::Unavailable)?;
        self.entries.get(low..high).ok_or(StorageError::Unavailable)
    }
}

impl hyper_raft::Storage for Wal {
    fn initial_state(&self) -> Result<hyper_raft::InitialState, StorageError> {
        Ok(hyper_raft::InitialState {
            hard_state: self.hard.clone(),
            configuration: self.configuration.clone(),
            proposals: Vec::new(),
        })
    }
    fn entries(
        &self,
        low: u64,
        high: u64,
        max_bytes: u64,
        into: &mut Vec<Entry>,
    ) -> Result<(), StorageError> {
        let range = self.range(low, high)?;
        let mut bytes = 0u64;
        let mut kept = 0usize;
        for entry in range {
            bytes = bytes.saturating_add(u64::try_from(entry.encoded_len()).unwrap_or(u64::MAX));
            if kept > 0 && bytes > max_bytes {
                break;
            }
            kept = kept.saturating_add(1);
        }
        into.try_reserve_exact(kept)
            .map_err(|_| StorageError::Unavailable)?;
        into.extend(range.iter().take(kept).cloned());
        Ok(())
    }
    fn any_entry(
        &self,
        low: u64,
        high: u64,
        predicate: &mut dyn FnMut(&Entry) -> bool,
    ) -> Result<bool, StorageError> {
        Ok(self.range(low, high)?.iter().any(predicate))
    }
    fn term(&self, index: u64) -> Result<u64, StorageError> {
        if index == 0 {
            return Ok(0);
        }
        let position =
            usize::try_from(index.saturating_sub(1)).map_err(|_| StorageError::Unavailable)?;
        self.entries
            .get(position)
            .map(|entry| entry.term)
            .ok_or(StorageError::Unavailable)
    }
    fn first_index(&self) -> Result<u64, StorageError> {
        Ok(1)
    }
    fn last_index(&self) -> Result<u64, StorageError> {
        u64::try_from(self.entries.len()).map_err(|_| StorageError::Unavailable)
    }
    fn snapshot(&self, _request_index: u64, _to: u64) -> Result<Snapshot, StorageError> {
        // The harness keeps its whole log: no snapshot is ever needed.
        Err(StorageError::SnapshotTemporarilyUnavailable)
    }
}
