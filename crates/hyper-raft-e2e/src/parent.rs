//! A member's parent, the test that started it, as both harnesses' members watch it: the test
//! holds the other end of the member's standard input for as long as it lives, so a member never
//! outlives the test however the test ends (a test killed outright runs no clean-up of its own),
//! and a byte on the pipe lets go a member the test holds outside any write of its log
//! (`stream::put_hold`), as a member deadlocked is held, on a platform with no signal that stops a
//! process (Windows).
use std::io::Read;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::Thread;

/// Set once the process's standard input ends: its parent is gone, and the member's loop returns.
pub static PARENT_GONE: AtomicBool = AtomicBool::new(false);
/// Set when a byte comes on the process's standard input: a member held by the test goes on.
pub static RELEASED: AtomicBool = AtomicBool::new(false);

/// Holds the calling thread, the member's, until the test releases it or goes ([`RELEASED`],
/// [`PARENT_GONE`], each set by the watcher [`watch`] starts, which unparks this thread): it reads
/// nothing and answers nothing meanwhile, as a member deadlocked outside a write of its log.
pub fn hold_until_released() {
    while !RELEASED.swap(false, Ordering::AcqRel) && !PARENT_GONE.load(Ordering::Acquire) {
        std::thread::park();
    }
}

/// Holds the calling thread, the member's, until the test goes: a member stopped where the test
/// armed it waits to be killed, or for the test to go.
pub fn wait_gone() {
    while !PARENT_GONE.load(Ordering::Acquire) {
        std::thread::park();
    }
}

/// Watches standard input until it ends, on one thread of its own, blocked on the pipe. A byte on
/// it releases a held member ([`RELEASED`]); at its end it sets [`PARENT_GONE`] and wakes the
/// member: it unparks `member`, its thread, if held, and sends `wake`, a sealed datagram, to `me`,
/// its socket, on which the member waits for as long as nothing is due.
#[allow(
    clippy::disallowed_methods,
    reason = "real processes on the host's clock, threads and environment (CLAUDE.md §1a, end to end)"
)]
pub fn watch(me: SocketAddr, member: Thread, wake: Vec<u8>) -> std::io::Result<()> {
    let out = UdpSocket::bind(SocketAddr::new(me.ip(), 0))?;
    std::thread::Builder::new()
        .name("parent".to_owned())
        .spawn(move || {
            let mut stdin = std::io::stdin().lock();
            let mut byte = [0u8; 1];
            while let Ok(1) = stdin.read(&mut byte) {
                RELEASED.store(true, Ordering::Release);
                member.unpark();
            }
            PARENT_GONE.store(true, Ordering::Release);
            member.unpark();
            let _ = out.send_to(&wake, me);
        })
        .map(drop)
}
