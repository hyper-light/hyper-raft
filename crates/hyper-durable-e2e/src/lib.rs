//! hyper-durable in real use: each member an OS process of its own ([`node`]), a `Replica` over
//! hyper-log on a real file it flushes with the platform's full flush ([`file`]), speaking to the
//! others over UDP on the loopback interface in hyper-raft-e2e's datagrams. The scenarios in
//! `tests/kill.rs` start a group, write to it and read from it as a client, and kill members with
//! `SIGKILL` at named durability points ([`control::Point`]) and at random, fail a flush, stall a
//! disk, change
//! the configuration, and hold what a client can observe and what a restarted member reopens with
//! to the shell's invariants: every write a member answered is read back, every member applies the
//! same history, a member acted on nothing it reopens below, and a founder that removed its only
//! peer elects itself.
//!
//! Each member's failure detectors are its own: the node-pair liveness stream (`hyper_liveness`),
//! wired to its replica as hyper-durable's owner wires it ([`node`]).
//!
//! A process runs five threads, whatever it holds: its own, the log's two, the relay that turns its
//! waker into a datagram to its own socket, and the one that watches for its test to go.

pub mod control;
pub mod file;
pub mod machine;
pub mod node;
