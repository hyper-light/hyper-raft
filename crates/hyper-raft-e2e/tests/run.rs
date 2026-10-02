//! The member's run, kept beside its log, rises at every start and is refused when its record is
//! not one count.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_macros)]

use std::path::PathBuf;

use hyper_raft_e2e::run::{self, RunError};

#[expect(
    clippy::disallowed_methods,
    reason = "a test removes the files it made in its own target directory"
)]
fn fresh(name: &str) -> PathBuf {
    let wal = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("run-{}-{name}.wal", std::process::id()));
    let _ = std::fs::remove_file(run::path(&wal));
    wal
}

#[test]
fn each_start_raises_the_run_kept_beside_the_log() {
    let wal = fresh("raise");
    let at = run::path(&wal);
    assert_eq!(at, PathBuf::from(format!("{}.run", wal.display())));
    assert_eq!(run::raise(&at).unwrap(), 1);
    assert_eq!(run::raise(&at).unwrap(), 2);
    assert_eq!(run::raise(&at).unwrap(), 3);
}

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "a test damages the record it made in its own target directory"
)]
fn a_record_that_is_not_one_count_is_refused() {
    let wal = fresh("damaged");
    let at = run::path(&wal);
    run::raise(&at).unwrap();
    let mut bytes = std::fs::read(&at).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    std::fs::write(&at, &bytes).unwrap();
    assert!(matches!(run::raise(&at), Err(RunError::Disk(_))));
}
