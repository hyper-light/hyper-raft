//! The member's log reads back what it wrote, cuts a torn last record off, and refuses a damaged
//! record that is not the last.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::disallowed_macros,
    clippy::cast_possible_truncation
)]

use std::{
    fs::OpenOptions,
    io::{Read, Seek, SeekFrom, Write},
    path::PathBuf,
};

use hyper_raft::proto::{Entry, HardState};
use hyper_raft_e2e::wal::{Wal, WalError};

#[expect(
    clippy::disallowed_methods,
    reason = "a test removes the files it made in its own target directory"
)]
fn fresh(name: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("wal-{}-{name}", std::process::id()));
    let _ = std::fs::remove_file(&path);
    path
}

fn entry(index: u64, term: u64) -> Entry {
    Entry {
        index,
        term,
        data: format!("{index}/{term}").into_bytes(),
        ..Entry::default()
    }
}

fn held(wal: &Wal) -> Vec<(u64, u64)> {
    wal.entries().iter().map(|e| (e.index, e.term)).collect()
}

#[test]
fn what_is_written_reads_back_and_a_later_entry_replaces_an_earlier() {
    let path = fresh("back");
    let mut wal = Wal::open(&path, vec![1, 2, 3], 16).unwrap();
    let hard = HardState {
        term: 2,
        vote: 1,
        commit: 1,
    };
    wal.persist(vec![entry(1, 1), entry(2, 1), entry(3, 1)], Some(&hard))
        .unwrap();
    wal.persist(vec![entry(2, 2), entry(3, 2)], None).unwrap();
    drop(wal);
    let wal = Wal::open(&path, vec![1, 2, 3], 16).unwrap();
    assert_eq!(held(&wal), vec![(1, 1), (2, 2), (3, 2)]);
    assert_eq!(wal.hard_state(), &hard);
}

#[test]
fn a_torn_last_record_is_cut_off() {
    let path = fresh("torn");
    let mut wal = Wal::open(&path, vec![1], 16).unwrap();
    wal.persist(vec![entry(1, 1), entry(2, 1)], None).unwrap();
    drop(wal);
    let whole = std::fs::metadata(&path).unwrap().len();
    // The first bytes of a record whose write did not finish.
    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(&[40, 0, 0, 0, 1, 2]).unwrap();
    drop(file);
    let mut wal = Wal::open(&path, vec![1], 16).unwrap();
    assert_eq!(held(&wal), vec![(1, 1), (2, 1)]);
    assert_eq!(std::fs::metadata(&path).unwrap().len(), whole);
    // What is written after the cut reads back.
    wal.persist(vec![entry(3, 1)], None).unwrap();
    drop(wal);
    assert_eq!(
        held(&Wal::open(&path, vec![1], 16).unwrap()),
        vec![(1, 1), (2, 1), (3, 1)]
    );
}

#[test]
fn a_damaged_record_that_is_not_the_last_refuses_the_open() {
    let path = fresh("damaged");
    let mut wal = Wal::open(&path, vec![1], 16).unwrap();
    wal.persist(vec![entry(1, 1)], None).unwrap();
    wal.persist(vec![entry(2, 1)], None).unwrap();
    drop(wal);
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).unwrap();
    // The first record's last payload byte.
    let first_end = 8 + 1 + u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
    file.seek(SeekFrom::Start((first_end - 1) as u64)).unwrap();
    file.write_all(&[bytes[first_end - 1] ^ 0xff]).unwrap();
    drop(file);
    assert!(matches!(
        Wal::open(&path, vec![1], 16),
        Err(WalError::Corrupt(_))
    ));
}

#[test]
fn a_log_past_its_bound_is_refused() {
    let path = fresh("full");
    let mut wal = Wal::open(&path, vec![1], 2).unwrap();
    wal.persist(vec![entry(1, 1), entry(2, 1)], None).unwrap();
    assert!(matches!(
        wal.persist(vec![entry(3, 1)], None),
        Err(WalError::Full)
    ));
    assert_eq!(held(&wal), vec![(1, 1), (2, 1)]);
}
