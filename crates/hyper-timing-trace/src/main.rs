//! Heartbeat traces between two processes on loopback, and their analysis: the measured inputs
//! `docs/timing.md` §3 asks for (the delay's distribution, loss, correlation time, stationarity,
//! timer lateness, the kernel-to-process gap).
//!
//! ```text
//! hyper-timing-trace timer <flush-file>
//! hyper-timing-trace run <dir> <interval-us> <seconds> [<flush-file>]
//! hyper-timing-trace analyse <dir> [<dir>...]
//! ```
//!
//! `timer` measures how late the two timed waits the recorder uses end (a sleep, and a socket wait
//! in `select` on macOS, `ppoll` on Linux), for asked durations on the 1-2-5 grid from 1 µs to
//! 10 ms, and how long one write and full flush of a log block takes.
//!
//! `run` binds a UDP socket on loopback with the kernel's receive timestamps on, starts a sender
//! process (`send`, the same binary), and records every heartbeat until the sender's count or the
//! run's end. The sender sends heartbeat `i` at its scheduled time `σ_i = start + iη`, sleeping
//! until then; with a flush file it first writes and fully flushes one block of it, as a heartbeat
//! that proves its log device took a write (`docs/timing.md` §2.1). Each heartbeat carries
//! `(i, σ_i, when the sender began to wait, when it woke, when it sent, the sender's realtime clock
//! at the send)`; the receiver adds the kernel's receive timestamp, and when it read the datagram
//! on the monotonic and realtime clocks. The receiver waits, as a detector does, until the next
//! scheduled heartbeat `σ_{last}+η`, and records each wait that ended on its timeout: when it began,
//! what it asked, when it woke. Both processes share the machine's monotonic clock, so every
//! difference is exact without clock synchronization.
//!
//! Records go to a fixed buffer written out a chunk at a time: memory is bounded whatever the run's
//! length. The load average is recorded with each chunk.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::disallowed_macros,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::needless_range_loop,
    // A measurement binary: it sleeps on the OS timer, opens sockets and writes files by design,
    // which the sans-io rule (CLAUDE.md §1) forbids the core crates.
    clippy::disallowed_methods,
    clippy::disallowed_types,
    missing_docs
)]

mod analyse;
mod sys;

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::net::UdpSocket;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sys::Clock;

/// Records a chunk holds before it is written out: 65,536 heartbeats of [`HEARTBEAT_BYTES`], 4.5 MiB,
/// a few seconds of the finest cadence.
const CHUNK_RECORDS: usize = 1 << 16;
/// A heartbeat datagram: six little-endian `u64`s.
const PACKET_BYTES: usize = 48;
/// A heartbeat record: the packet's six fields and the receiver's three.
pub(crate) const HEARTBEAT_BYTES: usize = 72;
/// A receiver wait record: began, asked deadline, woke.
pub(crate) const WAIT_BYTES: usize = 24;
/// The sequence number that ends a run.
const END: u64 = u64::MAX;
/// The asked durations of the timer sweep, nanoseconds: the 1-2-5 grid (IEC 60063's E3 series)
/// from 1 µs to 10 ms, which spans Linux's 50 µs default slack and Windows' 15.625 ms tick.
const SWEEP_NS: [u64; 13] = [
    1_000, 2_000, 5_000, 10_000, 20_000, 50_000, 100_000, 200_000, 500_000, 1_000_000, 2_000_000,
    5_000_000, 10_000_000,
];
/// Waits per asked duration in the sweep: 2,000, so the 99th percentile rests on 20 samples above
/// it.
const SWEEP_WAITS: usize = 2_000;

fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("timer") => timer(Path::new(&args[2])),
        Some("run") => run(
            Path::new(&args[2]),
            args[3].parse().map_err(io::Error::other)?,
            args[4].parse().map_err(io::Error::other)?,
            args.get(5).map(PathBuf::from),
        ),
        Some("send") => send(
            args[2].parse().map_err(io::Error::other)?,
            args[3].parse().map_err(io::Error::other)?,
            args[4].parse().map_err(io::Error::other)?,
            args.get(5).map(PathBuf::from),
        ),
        Some("analyse") => analyse::main(&args[2..]),
        _ => {
            eprintln!(
                "usage: hyper-timing-trace timer <flush-file> | run <dir> <interval-us> <seconds> [<flush-file>] | analyse <dir>..."
            );
            Ok(())
        }
    }
}

/// A file written a chunk of fixed-size records at a time, by a writer thread of its own so the
/// recording thread never blocks on the file: two buffers circulate between them over channels of
/// one, so memory is three chunks at most and a writer that falls behind stalls the recorder (which
/// the trace then shows) instead of growing.
struct Chunked {
    buf: Vec<u8>,
    record: usize,
    full: Option<std::sync::mpsc::SyncSender<Vec<u8>>>,
    empty: std::sync::mpsc::Receiver<Vec<u8>>,
    writer: Option<std::thread::JoinHandle<io::Result<()>>>,
    load: File,
    clock_start: u64,
}

impl Chunked {
    fn create(path: &Path, record: usize, load: &Path, clock_start: u64) -> io::Result<Self> {
        let mut file = File::create(path)?;
        let (full, filled) = std::sync::mpsc::sync_channel::<Vec<u8>>(1);
        let (give_back, empty) = std::sync::mpsc::sync_channel::<Vec<u8>>(1);
        give_back
            .send(Vec::with_capacity(CHUNK_RECORDS * record))
            .map_err(io::Error::other)?;
        let writer = std::thread::spawn(move || -> io::Result<()> {
            for mut chunk in filled {
                file.write_all(&chunk)?;
                chunk.clear();
                // The recorder may have finished; a buffer nobody takes back is dropped.
                let _ = give_back.send(chunk);
            }
            file.flush()
        });
        Ok(Self {
            buf: Vec::with_capacity(CHUNK_RECORDS * record),
            record,
            full: Some(full),
            empty,
            writer: Some(writer),
            load: OpenOptions::new().create(true).append(true).open(load)?,
            clock_start,
        })
    }
    fn push(&mut self, fields: &[u64], now: u64) -> io::Result<()> {
        for field in fields {
            self.buf.extend_from_slice(&field.to_le_bytes());
        }
        if self.buf.len() + self.record > self.buf.capacity() {
            self.hand_off(now)?;
        }
        Ok(())
    }
    fn hand_off(&mut self, now: u64) -> io::Result<()> {
        let next = self.empty.recv().map_err(io::Error::other)?;
        let chunk = std::mem::replace(&mut self.buf, next);
        self.full
            .as_ref()
            .ok_or_else(|| io::Error::other("writer closed"))?
            .send(chunk)
            .map_err(io::Error::other)?;
        let [one, five, fifteen] = sys::load_average();
        writeln!(
            self.load,
            "{:.3} {one:.2} {five:.2} {fifteen:.2}",
            (now.saturating_sub(self.clock_start)) as f64 / 1e9
        )
    }
    fn finish(mut self, now: u64) -> io::Result<()> {
        self.hand_off(now)?;
        drop(self.full.take());
        match self.writer.take().map(std::thread::JoinHandle::join) {
            Some(Ok(result)) => result,
            Some(Err(_)) => Err(io::Error::other("writer panicked")),
            None => Ok(()),
        }
    }
}

fn realtime_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
}

/// One block of the flush file, the file's preferred I/O size.
fn log_block(path: &Path) -> io::Result<(File, Vec<u8>)> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    let block = sys::preferred_block(&file)?;
    Ok((file, vec![0xa5; block]))
}

fn write_and_flush(file: &File, block: &mut [u8], i: u64) -> io::Result<()> {
    block[..8].copy_from_slice(&i.to_le_bytes());
    sys::write_and_flush(file, block)
}

fn quantile(sorted: &[u64], q: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let at = ((sorted.len() - 1) as f64 * q).round() as usize;
    sorted[at.min(sorted.len() - 1)]
}

fn timer(flush: &Path) -> io::Result<()> {
    let clock = Clock::new()?;
    let socket = UdpSocket::bind("127.0.0.1:0")?;
    let load = sys::load_average();
    println!(
        "# timer sweep, {} {}, load {:.2} {:.2} {:.2}",
        std::env::consts::OS,
        std::env::consts::ARCH,
        load[0],
        load[1],
        load[2]
    );
    println!("| wait | asked µs | n | late p50 µs | p90 | p99 | p99.9 | max | mean |");
    println!("|---|---|---|---|---|---|---|---|---|");
    let mut late = Vec::with_capacity(SWEEP_WAITS);
    for (name, select) in [("sleep", false), (sys::SOCKET_WAIT, true)] {
        for asked in SWEEP_NS {
            late.clear();
            for _ in 0..SWEEP_WAITS {
                let began = clock.now()?;
                if select {
                    sys::wait_readable(&socket, asked)?;
                } else {
                    std::thread::sleep(Duration::from_nanos(asked));
                }
                late.push(clock.now()?.saturating_sub(began).saturating_sub(asked));
            }
            late.sort_unstable();
            let mean = late.iter().sum::<u64>() as f64 / late.len() as f64;
            println!(
                "| {name} | {} | {} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} |",
                asked / 1_000,
                late.len(),
                quantile(&late, 0.5) as f64 / 1e3,
                quantile(&late, 0.9) as f64 / 1e3,
                quantile(&late, 0.99) as f64 / 1e3,
                quantile(&late, 0.999) as f64 / 1e3,
                late.last().copied().unwrap_or(0) as f64 / 1e3,
                mean / 1e3
            );
        }
    }
    let (file, mut block) = log_block(flush)?;
    late.clear();
    for i in 0..SWEEP_WAITS as u64 {
        let began = clock.now()?;
        write_and_flush(&file, &mut block, i)?;
        late.push(clock.now()?.saturating_sub(began));
    }
    late.sort_unstable();
    let mean = late.iter().sum::<u64>() as f64 / late.len() as f64;
    println!(
        "| write+flush {} B | — | {} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} |",
        block.len(),
        late.len(),
        quantile(&late, 0.5) as f64 / 1e3,
        quantile(&late, 0.9) as f64 / 1e3,
        quantile(&late, 0.99) as f64 / 1e3,
        quantile(&late, 0.999) as f64 / 1e3,
        late.last().copied().unwrap_or(0) as f64 / 1e3,
        mean / 1e3
    );
    Ok(())
}

fn word(packet: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(packet[at * 8..at * 8 + 8].try_into().unwrap_or([0; 8]))
}

fn run(dir: &Path, interval_us: u64, seconds: u64, flush: Option<PathBuf>) -> io::Result<()> {
    let clock = Clock::new()?;
    std::fs::create_dir_all(dir)?;
    let interval = interval_us * 1_000;
    let count = seconds * 1_000_000 / interval_us.max(1);
    let socket = UdpSocket::bind("127.0.0.1:0")?;
    sys::enable_receive_timestamps(&socket)?;
    socket.set_nonblocking(true)?;
    let port = socket.local_addr()?.port();
    let started = clock.now()?;
    let load_path = dir.join("load.txt");
    let _ = std::fs::remove_file(&load_path);
    let mut beats = Chunked::create(&dir.join("hb.bin"), HEARTBEAT_BYTES, &load_path, started)?;
    let mut waits = Chunked::create(
        &dir.join("wait.bin"),
        WAIT_BYTES,
        &dir.join("load-wait.txt"),
        started,
    )?;
    let load = sys::load_average();
    let mut meta = File::create(dir.join("meta.txt"))?;
    writeln!(meta, "os {}", std::env::consts::OS)?;
    writeln!(meta, "arch {}", std::env::consts::ARCH)?;
    writeln!(meta, "kernel_clock {}", sys::KERNEL_CLOCK)?;
    writeln!(meta, "interval_ns {interval}")?;
    writeln!(meta, "count {count}")?;
    writeln!(meta, "flush {}", flush.is_some())?;
    writeln!(
        meta,
        "load_start {:.2} {:.2} {:.2}",
        load[0], load[1], load[2]
    )?;
    writeln!(meta, "realtime_start_ns {}", realtime_ns())?;

    let mut child = Command::new(std::env::current_exe()?)
        .arg("send")
        .arg(port.to_string())
        .arg(interval_us.to_string())
        .arg(count.to_string())
        .args(flush.iter())
        .spawn()?;

    // The run ends at the sender's end marker, or a second past its last scheduled heartbeat.
    let end_by = started + (count + 1) * interval + 1_000_000_000;
    let mut deadline = started + interval;
    let mut packet = [0u8; 64];
    let mut received = 0u64;
    let mut stamped = 0u64;
    'run: loop {
        let began = clock.now()?;
        if began >= end_by {
            break;
        }
        let asked = deadline.saturating_sub(began);
        if !sys::wait_readable(&socket, asked)? {
            let woke = clock.now()?;
            if asked > 0 {
                waits.push(&[began, deadline, woke], woke)?;
            }
            deadline = deadline.saturating_add(interval);
            continue;
        }
        loop {
            match sys::recv_stamped(&socket, &mut packet) {
                Ok((PACKET_BYTES, kernel)) => {
                    let read = clock.now()?;
                    let read_real = realtime_ns();
                    let seq = word(&packet, 0);
                    if seq == END {
                        break 'run;
                    }
                    let kernel = kernel.map_or(0, |raw| clock.kernel_ns(raw));
                    stamped += u64::from(kernel != 0);
                    let sched = word(&packet, 1);
                    beats.push(
                        &[
                            seq,
                            sched,
                            word(&packet, 2),
                            word(&packet, 3),
                            word(&packet, 4),
                            word(&packet, 5),
                            kernel,
                            read,
                            read_real,
                        ],
                        read,
                    )?;
                    received += 1;
                    deadline = deadline.max(sched.saturating_add(interval));
                }
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(error),
            }
        }
    }
    let now = clock.now()?;
    beats.finish(now)?;
    waits.finish(now)?;
    let status = child.wait()?;
    let load = sys::load_average();
    writeln!(
        meta,
        "load_end {:.2} {:.2} {:.2}",
        load[0], load[1], load[2]
    )?;
    writeln!(meta, "received {received}")?;
    writeln!(meta, "stamped {stamped}")?;
    writeln!(meta, "seconds {:.3}", (now - started) as f64 / 1e9)?;
    writeln!(meta, "sender {status}")?;
    println!(
        "{}: {received} of {count} heartbeats, {stamped} kernel-stamped",
        dir.display()
    );
    Ok(())
}

fn send(port: u16, interval_us: u64, count: u64, flush: Option<PathBuf>) -> io::Result<()> {
    let clock = Clock::new()?;
    let socket = UdpSocket::bind("127.0.0.1:0")?;
    socket.connect(("127.0.0.1", port))?;
    let mut log = match flush {
        Some(path) => Some(log_block(&path)?),
        None => None,
    };
    let interval = interval_us * 1_000;
    let start = clock.now()? + interval;
    let mut packet = [0u8; PACKET_BYTES];
    for i in 0..=count {
        let seq = if i == count { END } else { i };
        let sched = start + i * interval;
        let began = clock.now()?;
        if sched > began {
            std::thread::sleep(Duration::from_nanos(sched - began));
        }
        let woke = clock.now()?;
        if let Some((file, block)) = log.as_mut()
            && seq != END
        {
            write_and_flush(file, block, i)?;
        }
        let sent = clock.now()?;
        for (at, value) in [seq, sched, began, woke, sent, realtime_ns()]
            .into_iter()
            .enumerate()
        {
            packet[at * 8..at * 8 + 8].copy_from_slice(&value.to_le_bytes());
        }
        // A full socket buffer on the receiver is a lost heartbeat, which the trace shows by its
        // sequence number; a local send error is the same loss.
        let _ = socket.send(&packet);
    }
    Ok(())
}
