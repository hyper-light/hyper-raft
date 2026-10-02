//! hyper-raft in real use: each member an OS process of its own ([`node`]), speaking to the
//! others over UDP on the loopback interface and keeping its log in a file it flushes with the
//! platform's full flush ([`wal`]). The scenarios in `tests/cluster.rs` start such a group,
//! write to it and read from it as a client, kill members with `SIGKILL`, restart them on their
//! logs, and cut them off from the others with a drop filter inside the process, and assert what
//! a client can observe: every write a member answered is read back, from whichever member
//! leads, and every member that is up applies the same history.
//!
//! One thread per process, and one process per member: the harness never multiplies either.

pub mod node;
pub mod run;
pub mod stream;
pub mod wal;
pub mod wire;
