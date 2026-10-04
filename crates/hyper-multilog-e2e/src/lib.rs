//! hyper-multilog in real use (`docs/multilog.md` §11 step 5): each member an OS process of its
//! own ([`member`]), holding `n` logs over the same voters, each log a `hyper_raft` member on its
//! own fsynced file (`hyper_raft_e2e::wal`), speaking to the others over UDP with each Raft
//! datagram tagged with its log, and applying the logs' commands through the layer's merge to a
//! key-value store. The scenarios in `tests/cluster.rs` write and read keys across the logs, global
//! writes among them, kill a member with `SIGKILL` and start it again on its logs, and cut one off
//! and heal it, and assert what a client can observe: every write answered is read back, and every
//! member that is up reaches the same state.
//!
//! One thread per process, and one process per member: the harness never multiplies either.

pub mod member;
