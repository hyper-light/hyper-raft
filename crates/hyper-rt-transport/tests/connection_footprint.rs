//! What a server holds for each connection it holds, as the operating system charges it, beside what
//! its Rust code asked the allocator for (ignored: a measurement, run in release with `--ignored
//! --nocapture`). `connection_memory` counts the bytes the server's thread asks Rust's allocator for;
//! that misses what a connection holds outside it: the C library's allocations (aws-lc's AEAD and
//! header-protection contexts go to `malloc`, not through Rust's allocator), the allocator's own
//! rounding and bookkeeping, and the pages either keeps. The OS's account of the process holds them
//! all (`hyper_measure::usage`: macOS's physical footprint, Linux's resident set, Windows's working
//! set), so this test's process is the server alone: its clients run in a process of their own (this
//! binary again, `footprint_clients`), so the process's footprint is the server's.
//!
//! Each row makes a server admitting `n`, has `n` clients connect and go quiet, and reports, per
//! connection, what the server thread's Rust allocations and the process's footprint grew by while
//! they connected and settled, and their ratio: what a charge made of Rust's requests alone leaves
//! out on this host. A cost learned on the host a node runs on is to be learned from the second.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::disallowed_macros,
    clippy::disallowed_methods,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::too_many_lines,
    dead_code,
    missing_docs,
    unsafe_code,
    reason = "a measurement harness: aws-lc's allocator hooks are C functions it calls"
)]

#[path = "../../hyper-transport/tests/common/mod.rs"]
mod common;

use std::alloc::{GlobalAlloc as _, Layout, System};
use std::ffi::{c_char, c_int, c_void};
use std::io::Read as _;
use std::net::SocketAddr;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use common::*;
use hyper_measure::{alloc, usage};
use hyper_rt::runtime::{LocalRuntime, RuntimeConfig};
use hyper_rt_transport::{Driver, Io};
use hyper_transport::{AdmissionLimits, Config, Endpoint, Fixed, Limits};

/// The counting allocator, and a histogram of the sizes asked for while a row's histogram is on:
/// what a burst of handshakes allocates, by size, to tell a transient's makeup.
struct Sized;
/// Allocations by log2 of their size (bucket b: sizes below 2^(b+1)), and their bytes.
static SIZES: [AtomicI64; 40] = [const { AtomicI64::new(0) }; 40];
static SIZE_BYTES: [AtomicI64; 40] = [const { AtomicI64::new(0) }; 40];
static LARGEST: AtomicI64 = AtomicI64::new(0);
static HISTOGRAM: AtomicBool = AtomicBool::new(false);

fn note_size(size: usize) {
    if !HISTOGRAM.load(Ordering::Relaxed) {
        return;
    }
    let bucket = (usize::BITS - size.max(1).leading_zeros()).saturating_sub(1) as usize;
    let bucket = bucket.min(SIZES.len() - 1);
    SIZES[bucket].fetch_add(1, Ordering::Relaxed);
    SIZE_BYTES[bucket].fetch_add(size as i64, Ordering::Relaxed);
    LARGEST.fetch_max(size as i64, Ordering::Relaxed);
    // Where a block of 16 MiB or more is asked for, once a thread: capturing allocates, so the
    // thread's guard keeps the capture from noting itself.
    thread_local! {
        static CAPTURING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    if size >= 16 << 20 && !CAPTURING.with(std::cell::Cell::get) {
        CAPTURING.with(|capturing| capturing.set(true));
        eprintln!(
            "LARGE {size} B at\n{}",
            std::backtrace::Backtrace::force_capture()
        );
        CAPTURING.with(|capturing| capturing.set(false));
    }
}

// SAFETY: every call is forwarded unchanged to the counting allocator, which forwards to `System`.
unsafe impl std::alloc::GlobalAlloc for Sized {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note_size(layout.size());
        // SAFETY: as the caller's.
        unsafe { alloc::Counting.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        note_size(layout.size());
        // SAFETY: as the caller's.
        unsafe { alloc::Counting.alloc_zeroed(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: as the caller's.
        unsafe { alloc::Counting.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note_size(new_size);
        // SAFETY: as the caller's.
        unsafe { alloc::Counting.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Sized = Sized;

type Drv = Driver<Mantle, Fixed, Book>;

/// Datagrams a system call carries: one QUIC initial window (RFC 9002 §7.2).
const IO: Io = Io { batch: 10 };
/// The server's peer id; the clients are 1 to `n`.
const SERVER: u64 = 0;
/// The failure guard of a wait.
const GUARD: Duration = Duration::from_secs(300);
/// How long the server must receive nothing for its connections to count as settled.
const QUIET: Duration = Duration::from_millis(500);
/// Set in the clients' process once its parent closed its standard input: the clients' tasks end.
static STOP: AtomicBool = AtomicBool::new(false);
/// The clients that saw their connection made, in the clients' process.
static CONNECTED: AtomicUsize = AtomicUsize::new(0);

/// aws-lc's live bytes and their highest since the hooks were set, in the server's process.
static C_LIVE: AtomicI64 = AtomicI64::new(0);
static C_PEAK: AtomicI64 = AtomicI64::new(0);
/// Each of aws-lc's blocks carries its size in a prefix of this many bytes, kept aligned.
const PREFIX: usize = 16;

fn c_note(delta: i64) {
    let live = C_LIVE.fetch_add(delta, Ordering::Relaxed) + delta;
    C_PEAK.fetch_max(live, Ordering::Relaxed);
}

/// aws-lc's `malloc`, counted: the system's, the block's size before it.
unsafe extern "C" fn c_malloc(size: usize, _: *const c_char, _: c_int) -> *mut c_void {
    let Ok(layout) = Layout::from_size_align(size + PREFIX, PREFIX) else {
        return std::ptr::null_mut();
    };
    // SAFETY: the layout is non-zero (at least the prefix) and valid.
    let base = unsafe { System.alloc(layout) };
    if base.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: the block holds the prefix, aligned for a usize.
    unsafe { base.cast::<usize>().write(size) };
    c_note(size as i64);
    // SAFETY: within the block.
    unsafe { base.add(PREFIX).cast() }
}

/// aws-lc's `free`, counted, the block wiped first as aws-lc's own `free` wipes it.
unsafe extern "C" fn c_free(ptr: *mut c_void, _: *const c_char, _: c_int) {
    if ptr.is_null() {
        return;
    }
    // SAFETY: `ptr` came from `c_malloc` or `c_realloc`: its prefix is before it.
    let base = unsafe { ptr.cast::<u8>().sub(PREFIX) };
    // SAFETY: the prefix holds the size `c_malloc` wrote.
    let size = unsafe { base.cast::<usize>().read() };
    // SAFETY: the block is `size + PREFIX` bytes, ours to write.
    unsafe { std::ptr::write_bytes(base, 0, size + PREFIX) };
    c_note(-(size as i64));
    // SAFETY: allocated by `System` with this layout.
    unsafe {
        System.dealloc(
            base,
            Layout::from_size_align_unchecked(size + PREFIX, PREFIX),
        )
    };
}

/// aws-lc's `realloc`, counted: a new block, the old one's bytes copied, the old one freed.
unsafe extern "C" fn c_realloc(
    ptr: *mut c_void,
    size: usize,
    file: *const c_char,
    line: c_int,
) -> *mut c_void {
    // SAFETY: as `c_malloc` and `c_free`.
    unsafe {
        let new = c_malloc(size, file, line);
        if !ptr.is_null() && !new.is_null() {
            let old = ptr.cast::<u8>().sub(PREFIX).cast::<usize>().read();
            std::ptr::copy_nonoverlapping(ptr.cast::<u8>(), new.cast::<u8>(), old.min(size));
            c_free(ptr, file, line);
        }
        new
    }
}

/// aws-lc's allocations go through the counting functions from now on; set before aws-lc allocates.
fn count_aws_lc() {
    // SAFETY: called once, first in the server's process, before anything uses aws-lc.
    let set = unsafe {
        aws_lc_sys::CRYPTO_set_mem_functions(Some(c_malloc), Some(c_realloc), Some(c_free))
    };
    assert_eq!(set, 1, "aws-lc's memory functions were set already");
}

/// A shard with room for `n` clients' tasks, sockets and timers.
fn runtime(n: usize) -> RuntimeConfig {
    let room = 4 * n + 64;
    RuntimeConfig {
        shards: 1,
        tasks_per_shard: room,
        timers_per_shard: room,
        interests_per_shard: room,
        ring_entries: 256,
        step_budget_ns: 1_000_000_000,
        timer_tick_ns: 100_000,
        batch: 64,
        pin: false,
        cores: Vec::new(),
        page_bytes: 4096,
        spin_ns: 0,
        wake_tracking: None,
        latency_objective_ns: None,
    }
}

/// Admission for `n` identities a connection each; no keep-alive and an idle bound past any row, so
/// a settled connection sends nothing.
fn admitting(n: usize) -> Limits {
    Limits {
        admission: AdmissionLimits {
            pending: n + 8,
            identities: n + 8,
            connections: n + 8,
            per_identity: 2,
        },
        keep_alive: None,
        idle_timeout: Duration::from_secs(600),
        ..limits()
    }
}

fn endpoint(
    root: &[u8],
    own: &(Vec<u8>, Vec<u8>),
    peers: &[(&[u8], u64)],
    listen: bool,
    limits: Limits,
) -> Endpoint<Mantle, Fixed, Book> {
    let mut book = Book::default();
    for (certificate, peer) in peers {
        book.add(certificate, *peer, Role::Node);
    }
    let config = Config {
        tls: hyper_transport::tls::Tls::mutual(&credentials(root, &own.0, &own.1), listen).unwrap(),
        role: Role::Node,
        limits,
        congestion: Default::default(),
    };
    Endpoint::new(config, Fixed::new(512 << 20, 64), book, Instant::now()).unwrap()
}

/// The driver's next event, or none after 5 ms, whichever first.
async fn turn(driver: &mut Drv) -> Option<hyper_transport::Event<Mantle>> {
    use std::future::Future;
    let mut event = std::pin::pin!(driver.event());
    let mut tick = std::pin::pin!(hyper_rt::futures::sleep(5_000_000));
    std::future::poll_fn(|context| {
        if let std::task::Poll::Ready(event) = event.as_mut().poll(context) {
            return std::task::Poll::Ready(event.ok());
        }
        tick.as_mut().poll(context).map(|_| None)
    })
    .await
}

/// The footprint the OS charges this process now, bytes.
fn footprint() -> i64 {
    usage::this()
        .unwrap()
        .footprint
        .map_or(0, |bytes| bytes as i64)
}

/// The socket memory the kernel charges this process's control group, bytes, where it is read
/// (Linux, cgroup v2's `memory.stat` `sock`): a connection's socket buffers are charged to a
/// container's limit though no process's footprint holds them.
fn cgroup_sock() -> Option<i64> {
    let relative = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    let path = relative.lines().find_map(|line| line.strip_prefix("0::"))?;
    let stat = std::fs::read_to_string(format!("/sys/fs/cgroup{path}/memory.stat")).ok()?;
    stat.lines()
        .find_map(|line| line.strip_prefix("sock "))
        .and_then(|value| value.trim().parse().ok())
}

/// What one row measured: the server thread's Rust bytes and the process's footprint once the
/// server was made and once its `n` connections settled.
struct Row {
    n: usize,
    rust_made: i64,
    rust_settled: i64,
    rust_peak: i64,
    c_made: i64,
    c_settled: i64,
    c_peak: i64,
    os_made: i64,
    os_settled: i64,
    sock_made: Option<i64>,
    sock_settled: Option<i64>,
}

/// The clients' credentials, written for the clients' process: the root, the server's certificate,
/// and each client's certificate and key.
fn write_credentials(dir: &Path, root: &[u8], server: &[u8], clients: &[(Vec<u8>, Vec<u8>)]) {
    std::fs::write(dir.join("root.der"), root).unwrap();
    std::fs::write(dir.join("server.der"), server).unwrap();
    for (index, (certificate, key)) in clients.iter().enumerate() {
        std::fs::write(dir.join(format!("{index}.der")), certificate).unwrap();
        std::fs::write(dir.join(format!("{index}.key")), key).unwrap();
    }
}

/// One row: a server admitting `n` measured as `n` clients, in a process of their own, connect to it
/// and go quiet.
fn row(n: usize) -> Row {
    let pki = Pki::new();
    let server = pki.issue(&name(SERVER));
    let clients: Vec<(Vec<u8>, Vec<u8>)> =
        (1..=n as u64).map(|peer| pki.issue(&name(peer))).collect();
    // The row's credentials for its clients' process, in a directory of its own, removed after.
    let dir = std::env::temp_dir().join(format!("footprint-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    write_credentials(&dir, &pki.root, &server.0, &clients);
    let peers: Vec<(Vec<u8>, u64)> = clients
        .iter()
        .enumerate()
        .map(|(index, client)| (client.0.clone(), index as u64 + 1))
        .collect();
    let root = pki.root.clone();
    // Room for every connection's socket, timer and task: each connection has a socket of its own
    // where the platform shares ports.
    let mut rt = LocalRuntime::new(&runtime(n)).unwrap();
    rt.block_on(async move {
        let peers: Vec<(&[u8], u64)> = peers
            .iter()
            .map(|(certificate, peer)| (certificate.as_slice(), *peer))
            .collect();
        let loopback: SocketAddr = "127.0.0.1:0".parse().unwrap();
        alloc::begin();
        let mut driver: Drv = Driver::bind(
            endpoint(&root, &server, &peers, true, admitting(n)),
            loopback,
            IO,
        )
        .unwrap();
        let rust_made = alloc::read().live;
        HISTOGRAM.store(true, Ordering::Relaxed);
        let c_made = C_LIVE.load(Ordering::Relaxed);
        C_PEAK.store(c_made, Ordering::Relaxed);
        let os_made = footprint();
        let sock_made = cgroup_sock();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["footprint_clients", "--exact", "--ignored", "--nocapture"])
            .env("FOOTPRINT_DIR", &dir)
            .env("FOOTPRINT_N", n.to_string())
            .env("FOOTPRINT_SERVER", driver.local_addr().unwrap().to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let began = Instant::now();
        let mut last_received = 0;
        let mut quiet_since = Instant::now();
        loop {
            turn(&mut driver).await;
            while driver.endpoint().poll_event().is_some() {}
            let held = driver.endpoint().stats().connections;
            let received = driver.stats().received + driver.peer_stats().received;
            if received != last_received {
                last_received = received;
                quiet_since = Instant::now();
            }
            let sockets_ready = driver.peer_sockets() >= n || !cfg!(unix);
            if held >= n && sockets_ready && quiet_since.elapsed() >= QUIET {
                break;
            }
            assert!(
                began.elapsed() < GUARD,
                "{held} of {n} connections held; endpoint {:?}",
                driver.endpoint().stats()
            );
        }
        let counts = alloc::read();
        HISTOGRAM.store(false, Ordering::Relaxed);
        let (rust_settled, rust_peak) = (counts.live, counts.peak);
        let (c_settled, c_peak) = (
            C_LIVE.load(Ordering::Relaxed),
            C_PEAK.load(Ordering::Relaxed),
        );
        let os_settled = footprint();
        let sock_settled = cgroup_sock();
        alloc::end();
        drop(child.stdin.take());
        child.wait().unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        Row {
            n,
            rust_made,
            rust_settled,
            rust_peak,
            c_made,
            c_settled,
            c_peak,
            os_made,
            os_settled,
            sock_made,
            sock_settled,
        }
    })
    .unwrap()
}

/// Repetitions of each row, each in a process of its own.
const REPEATS: usize = 3;

#[test]
#[ignore = "a measurement: cargo test --release -p hyper-rt-transport --test connection_footprint -- --ignored --nocapture what_the_os"]
fn what_the_os_charges_a_server_for_each_connection() {
    println!("{} {}", std::env::consts::OS, std::env::consts::ARCH);
    println!(
        "| connections | Rust each | C each | Rust + C each | OS footprint each | footprint / (Rust + C) | handshake peak each (Rust + C) | cgroup socket each |"
    );
    println!("|---|---|---|---|---|---|---|---|");
    for n in [64usize, 256, 1024] {
        for _ in 0..REPEATS {
            // Each row in a process of its own: what an earlier row freed and its allocator kept
            // is not reused by this one's.
            let output = Command::new(std::env::current_exe().unwrap())
                .args(["footprint_server", "--exact", "--ignored", "--nocapture"])
                .env("FOOTPRINT_ROW", n.to_string())
                .stderr(Stdio::inherit())
                .output()
                .unwrap();
            assert!(output.status.success(), "the row's server failed");
            let text = String::from_utf8_lossy(&output.stdout);
            let line = text
                .lines()
                .find_map(|line| line.strip_prefix("ROW "))
                .unwrap_or_else(|| panic!("no row in {text}"));
            let v: Vec<i64> = line.split(' ').map(|f| f.parse().unwrap_or(-1)).collect();
            let [
                n,
                rust_made,
                rust_settled,
                rust_peak,
                c_made,
                c_settled,
                c_peak,
                os_made,
                os_settled,
                sock_made,
                sock_settled,
            ] = v[..]
            else {
                panic!("a row of other fields: {line}");
            };
            let each = |made: i64, settled: i64| (settled - made) / n;
            let rust = each(rust_made, rust_settled);
            let c = each(c_made, c_settled);
            let os = each(os_made, os_settled);
            let peak = each(rust_made + c_made, rust_peak + c_peak);
            let sock = if sock_made < 0 || sock_settled < 0 {
                "-".to_owned()
            } else {
                format!("{} B", each(sock_made, sock_settled))
            };
            println!(
                "| {n} | {rust} B | {c} B | {} B | {os} B | {:.2} | {peak} B | {sock} |",
                rust + c,
                os as f64 / (rust + c).max(1) as f64
            );
        }
    }
}

/// One row's server, in a process of its own (run by the measurement): aws-lc's allocations counted
/// from its start, the row made, its figures printed on one line.
#[test]
#[ignore = "the server of a footprint row, run by it in a process of its own"]
fn footprint_server() {
    let Ok(n) = std::env::var("FOOTPRINT_ROW") else {
        return;
    };
    count_aws_lc();
    let row = row(n.parse().unwrap());
    // What the burst asked for, by size: the buckets holding the most bytes.
    let mut buckets: Vec<(usize, i64, i64)> = (0..SIZES.len())
        .map(|b| {
            (
                b,
                SIZES[b].load(Ordering::Relaxed),
                SIZE_BYTES[b].load(Ordering::Relaxed),
            )
        })
        .filter(|(_, count, _)| *count > 0)
        .collect();
    buckets.sort_by_key(|(_, _, bytes)| -bytes);
    for (b, count, bytes) in buckets.iter().take(6) {
        eprintln!(
            "SIZES [{}, {}) B: {count} allocations, {bytes} B",
            1u64 << b,
            1u64 << (b + 1)
        );
    }
    eprintln!("SIZES largest {} B", LARGEST.load(Ordering::Relaxed));
    println!(
        "ROW {} {} {} {} {} {} {} {} {} {} {}",
        row.n,
        row.rust_made,
        row.rust_settled,
        row.rust_peak,
        row.c_made,
        row.c_settled,
        row.c_peak,
        row.os_made,
        row.os_settled,
        row.sock_made.unwrap_or(-1),
        row.sock_settled.unwrap_or(-1)
    );
}

/// The clients of a row, in a process of their own (run by [`row`]): `FOOTPRINT_N` clients connect
/// to `FOOTPRINT_SERVER` and stay until this process's standard input closes.
#[test]
#[ignore = "the clients of a footprint row, run by it in a process of their own"]
fn footprint_clients() {
    let Ok(dir) = std::env::var("FOOTPRINT_DIR") else {
        return;
    };
    let dir = Path::new(&dir).to_path_buf();
    let n: usize = std::env::var("FOOTPRINT_N").unwrap().parse().unwrap();
    let server_addr: SocketAddr = std::env::var("FOOTPRINT_SERVER").unwrap().parse().unwrap();
    let root = std::fs::read(dir.join("root.der")).unwrap();
    let server_cert = std::fs::read(dir.join("server.der")).unwrap();
    let clients: Vec<(Vec<u8>, Vec<u8>)> = (0..n)
        .map(|index| {
            (
                std::fs::read(dir.join(format!("{index}.der"))).unwrap(),
                std::fs::read(dir.join(format!("{index}.key"))).unwrap(),
            )
        })
        .collect();
    std::thread::spawn(|| {
        let mut sink = Vec::new();
        let _ = std::io::stdin().read_to_end(&mut sink);
        STOP.store(true, Ordering::Relaxed);
    });
    let mut rt = LocalRuntime::new(&runtime(n)).unwrap();
    rt.block_on(async move {
        let loopback: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let to_server = [(server_cert.as_slice(), SERVER)];
        for client in &clients {
            let mut driver: Drv = Driver::bind(
                endpoint(&root, client, &to_server, false, admitting(1)),
                loopback,
                IO,
            )
            .unwrap();
            let now = driver.clock().unwrap();
            driver.endpoint().connect(now, SERVER, server_addr).unwrap();
            hyper_rt::futures::spawn_detached(async move {
                while !STOP.load(Ordering::Relaxed) {
                    if let Some(event) = turn(&mut driver).await
                        && matches!(event, hyper_transport::Event::Connected { .. })
                    {
                        CONNECTED.fetch_add(1, Ordering::Relaxed);
                    }
                    while let Some(event) = driver.endpoint().poll_event() {
                        if matches!(event, hyper_transport::Event::Connected { .. }) {
                            CONNECTED.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
            })
            .unwrap();
        }
        while !STOP.load(Ordering::Relaxed) {
            hyper_rt::futures::sleep(5_000_000).await.unwrap();
        }
    })
    .unwrap();
}
