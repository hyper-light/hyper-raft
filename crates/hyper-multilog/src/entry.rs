//! What a log entry states, as the layer reads and writes it (`docs/multilog.md` §2.2).
//!
//! The layer's commands ride in the core's normal entries, tagged by the **last** byte of the
//! entry's data, so that a command's bytes go into the log as the owner gave them and the tag (and
//! a keyed command's key) are appended into room the owner may reserve ([`SUFFIX_BYTES`]).

use hyper_raft::proto::{Entry, EntryType};

use crate::error::{Error, Result};

/// Format: a global command's tag, the last byte of its entry's data.
pub const GLOBAL: u8 = 0x01;
/// Format: a keyed command's tag; the key's [`WORD_BYTES`] come before it.
pub const KEYED: u8 = 0x02;
/// Format: a barrier's tag; the log-0 index it names, [`WORD_BYTES`] long, comes before it.
pub const BARRIER: u8 = 0x03;
/// Format: a resize's tag, log 0 only; the count of logs it makes, [`WORD_BYTES`] long, comes
/// before it (`docs/multilog.md` §3.5).
pub const RESIZE: u8 = 0x04;
/// Format: the width of a key and of an index, a `u64` written little-endian.
pub const WORD_BYTES: usize = 8;
/// Format: the most the layer appends to a command: a key and the tag. An owner that reserves this
/// much beyond its command allocates nothing more for a proposal.
pub const SUFFIX_BYTES: usize = WORD_BYTES + 1;

/// What an entry states.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stated<'a> {
    /// The core's own: a new leader's empty entry, or a change of configuration (`docs/multilog.md`
    /// §3.4). Applied as nothing.
    Own,
    /// A global command (log 0 only).
    Global(&'a [u8]),
    /// A keyed command.
    Keyed {
        /// The key it reads and writes.
        key: u64,
        /// The command.
        command: &'a [u8],
    },
    /// A barrier (logs other than log 0): the log-0 index it names.
    Barrier(u64),
    /// A change of the count of logs (log 0 only), ordered as a global command is: the count it
    /// makes, at least one and at most what a `u32` numbers.
    Resize(usize),
    /// Bytes the layer never writes: refused alike on every member.
    Malformed,
}

/// What `entry` states.
pub fn read(entry: &Entry) -> Stated<'_> {
    if entry.entry_type != EntryType::EntryNormal {
        return Stated::Own;
    }
    let Some((tag, rest)) = entry.data.split_last() else {
        return Stated::Own;
    };
    match *tag {
        GLOBAL => Stated::Global(rest),
        KEYED => keyed(rest),
        BARRIER => barrier(rest),
        RESIZE => resize_to(rest),
        _ => Stated::Malformed,
    }
}

fn keyed(rest: &[u8]) -> Stated<'_> {
    let Some((command, key)) = rest
        .len()
        .checked_sub(WORD_BYTES)
        .and_then(|at| rest.split_at_checked(at))
    else {
        return Stated::Malformed;
    };
    word(key).map_or(Stated::Malformed, |key| Stated::Keyed { key, command })
}

fn barrier(rest: &[u8]) -> Stated<'_> {
    word(rest).map_or(Stated::Malformed, Stated::Barrier)
}

fn resize_to(rest: &[u8]) -> Stated<'_> {
    word(rest)
        .filter(|logs| *logs >= 1 && *logs <= u64::from(u32::MAX))
        .and_then(|logs| usize::try_from(logs).ok())
        .map_or(Stated::Malformed, Stated::Resize)
}

/// A `u64` from exactly [`WORD_BYTES`] little-endian bytes.
fn word(bytes: &[u8]) -> Option<u64> {
    <[u8; WORD_BYTES]>::try_from(bytes)
        .ok()
        .map(u64::from_le_bytes)
}

/// Makes room for `more` bytes past `data`'s length, refused if none can be had.
fn room(data: &mut Vec<u8>, more: usize) -> Result<()> {
    data.try_reserve(more)
        .map_err(|_| Error::Capacity("a command's tag"))
}

/// A global command's data: `command` and its tag.
pub fn global(mut command: Vec<u8>) -> Result<Vec<u8>> {
    room(&mut command, 1)?;
    command.push(GLOBAL);
    Ok(command)
}

/// A keyed command's data: `command`, its key and its tag.
pub fn keyed_command(mut command: Vec<u8>, key: u64) -> Result<Vec<u8>> {
    room(&mut command, SUFFIX_BYTES)?;
    command.extend_from_slice(&key.to_le_bytes());
    command.push(KEYED);
    Ok(command)
}

/// A resize's data, making `logs` logs: refused for none, or more than a `u32` numbers.
pub fn resize(logs: usize) -> Result<Vec<u8>> {
    let count = u32::try_from(logs).map_err(|_| Error::Settings("more logs than a u32 numbers"))?;
    if count == 0 {
        return Err(Error::Settings("no log"));
    }
    let mut data = Vec::new();
    room(&mut data, SUFFIX_BYTES)?;
    data.extend_from_slice(&u64::from(count).to_le_bytes());
    data.push(RESIZE);
    Ok(data)
}

/// A barrier's data, naming log 0's `index`.
pub fn barrier_naming(index: u64) -> Result<Vec<u8>> {
    let mut data = Vec::new();
    room(&mut data, SUFFIX_BYTES)?;
    data.extend_from_slice(&index.to_le_bytes());
    data.push(BARRIER);
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn normal(data: Vec<u8>) -> Entry {
        Entry {
            data,
            ..Entry::default()
        }
    }

    /// Every entry the layer writes reads back as written, the empty entry and a change are the
    /// core's own, and bytes the layer never writes are malformed, never another entry.
    #[test]
    fn entries_round_trip_and_foreign_bytes_are_malformed() {
        let entry = normal(global(b"g".to_vec()).unwrap());
        assert_eq!(read(&entry), Stated::Global(b"g"));
        let entry = normal(global(Vec::new()).unwrap());
        assert_eq!(read(&entry), Stated::Global(b""));
        let entry = normal(keyed_command(b"k".to_vec(), u64::MAX).unwrap());
        assert_eq!(
            read(&entry),
            Stated::Keyed {
                key: u64::MAX,
                command: b"k"
            }
        );
        let entry = normal(keyed_command(Vec::new(), 7).unwrap());
        assert_eq!(
            read(&entry),
            Stated::Keyed {
                key: 7,
                command: b""
            }
        );
        let entry = normal(barrier_naming(42).unwrap());
        assert_eq!(read(&entry), Stated::Barrier(42));
        let entry = normal(resize(3).unwrap());
        assert_eq!(read(&entry), Stated::Resize(3));
        assert!(resize(0).is_err());
        assert_eq!(read(&normal(Vec::new())), Stated::Own);
        let change = Entry {
            entry_type: EntryType::EntryConfChangeV2,
            data: vec![1, 2, 3],
            ..Entry::default()
        };
        assert_eq!(read(&change), Stated::Own);
        for hostile in [
            vec![KEYED],
            vec![1, 2, 3, KEYED],
            vec![1, 2, 3, BARRIER],
            vec![0, 0, 0, 0, 0, 0, 0, 0, 1, BARRIER],
            vec![0xff],
            vec![0],
            vec![1, 2, RESIZE],
            vec![0, 0, 0, 0, 0, 0, 0, 0, RESIZE],
            vec![0, 0, 0, 0, 1, 0, 0, 0, RESIZE],
        ] {
            assert_eq!(
                read(&normal(hostile.clone())),
                Stated::Malformed,
                "{hostile:?}"
            );
        }
    }

    /// A command whose owner reserved [`SUFFIX_BYTES`] takes its tag and key with no reallocation:
    /// its buffer is the one the owner gave.
    #[test]
    fn a_command_with_room_keeps_its_buffer() {
        let mut command = Vec::with_capacity(4 + SUFFIX_BYTES);
        command.extend_from_slice(b"abcd");
        let at = command.as_ptr();
        let data = keyed_command(command, 9).unwrap();
        assert_eq!(data.as_ptr(), at);
        let mut command = Vec::with_capacity(4 + 1);
        command.extend_from_slice(b"abcd");
        let at = command.as_ptr();
        let data = global(command).unwrap();
        assert_eq!(data.as_ptr(), at);
    }
}
