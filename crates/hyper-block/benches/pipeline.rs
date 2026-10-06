//! Durable frames a second, and each frame's latency to durable, with `K` frames in flight:
//! the curve docs/research/log-pipeline.md §5 asks for before hyper-log picks `K`.
//! `cargo bench -p hyper-block --bench pipeline -- DIR [FRAMES] [DEPTHS] [BYTES] [MODE]`, depths
//! comma-separated (1,2,4,8,16 by default), 2000 frames of 4096 bytes a depth by default, mode
//! `flush` (the default) or `dsync`.
//!
//! `flush` is the portable pipeline of research/log-pipeline.md §3, the only one macOS has:
//! writers put frames at increasing offsets of a preallocated, zero-written file, and one flusher
//! runs the platform's full flush (`F_FULLFSYNC` on macOS) back to back. A flush covers the
//! writes that had returned when it started, so a frame is durable once a flush that started
//! after its write returned has returned, and every earlier frame is too. Frame `n` is written
//! only after frame `n − K` is durable: the issue rule of §1. At `K = 1` this is today's log,
//! one frame then one flush.
//!
//! `dsync` is §2's Linux pipeline: each writer's file is opened `O_DIRECT | O_DSYNC`, so a write
//! returns once its frame is durable (FUA on a device that has it, a flush per write otherwise)
//! and no flusher runs. The prefix moves as writes return in order. Elsewhere it is refused.
//!
//! Writers are `K` threads, one a frame in flight, as the issuer's pool on macOS runs them. The
//! flusher is one more and also coordinates: it hands each writer a frame over its own channel,
//! hears each write return on one bounded channel, and flushes once a prefix has returned. A
//! frame is handed out only within the window, so all a flush can cover is issued before it
//! starts. Each line reports the depth, the frames, the seconds, frames a second,
//! the flushes run, and the latency from a frame's submission to its durability at p50, p99,
//! p99.9 and the maximum, in microseconds.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::disallowed_macros,
    clippy::disallowed_methods,
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::indexing_slicing,
    reason = "a benchmark measures real time on a real file and reports it"
)]

use std::path::{Path, PathBuf};
use std::sync::mpsc::{SyncSender, sync_channel};
use std::time::Instant;

use hyper_block::buf::{AlignedBuf, Alignment};
use hyper_block::file::{CachingRequest, DeviceFile};

fn main() {
    let args: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| a != "--bench")
        .collect();
    let dir = PathBuf::from(args.first().expect("DIR"));
    let frames: u64 = args.get(1).map_or(2000, |s| s.parse().expect("FRAMES"));
    let depths: Vec<u64> = args.get(2).map_or_else(
        || vec![1, 2, 4, 8, 16],
        |s| s.split(',').map(|d| d.parse().expect("DEPTHS")).collect(),
    );
    let bytes: usize = args.get(3).map_or(4096, |s| s.parse().expect("BYTES"));
    let dsync = match args.get(4).map_or("flush", String::as_str) {
        "flush" => Ok(false),
        "dsync" if cfg!(target_os = "linux") => Ok(true),
        _ => Err("MODE: `flush`, or `dsync` on Linux"),
    }
    .expect("MODE");
    let align = Alignment::new(4096).unwrap();
    println!("depth frames seconds frames_per_s flushes p50_us p99_us p999_us max_us");
    for &k in &depths {
        run(&dir, frames, k, bytes, align, dsync);
    }
}

fn filled(bytes: usize, align: Alignment, byte: u8) -> AlignedBuf {
    let mut buf = AlignedBuf::zeroed(bytes, align).unwrap();
    buf.extend_zeros(bytes).unwrap();
    buf.as_mut_slice().fill(byte);
    buf
}

/// A writer's handle: the bench's file, or on Linux a second open with `O_DSYNC`.
enum Writer {
    Flushed(DeviceFile),
    #[cfg(target_os = "linux")]
    Durable(std::fs::File),
}

impl Writer {
    fn write(&self, buf: &[u8], offset: u64) {
        match self {
            Self::Flushed(file) => file.write_all_at(buf, offset).unwrap(),
            #[cfg(target_os = "linux")]
            Self::Durable(file) => {
                std::os::unix::fs::FileExt::write_all_at(file, buf, offset).unwrap()
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn durable_open(path: &Path) -> Option<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let flags = rustix::fs::OFlags::DIRECT | rustix::fs::OFlags::DSYNC;
    let file = std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(i32::try_from(flags.bits()).unwrap())
        .open(path)
        .expect("O_DIRECT | O_DSYNC: a file system that takes direct I/O");
    Some(file)
}

/// Off Linux there is no durable open: `main` refuses `dsync` there.
#[cfg(not(target_os = "linux"))]
fn durable_open(_path: &Path) -> Option<std::fs::File> {
    None
}

fn writer(file: &DeviceFile, durable: Option<&std::fs::File>) -> Writer {
    match durable {
        #[cfg(target_os = "linux")]
        Some(d) => Writer::Durable(d.try_clone().unwrap()),
        _ => Writer::Flushed(file.try_clone().unwrap()),
    }
}

fn run(dir: &Path, frames: u64, k: u64, bytes: usize, align: Alignment, dsync: bool) {
    let path = dir.join(format!("pipeline-{k}.dat"));
    let _ = std::fs::remove_file(&path);
    let file = DeviceFile::open(&path, true, CachingRequest::PreferDirect, align).unwrap();
    let zeros = filled(bytes, file.alignment(), 0);
    for n in 0..frames {
        file.write_all_at(zeros.as_slice(), n * bytes as u64)
            .unwrap();
    }
    file.sync_data().unwrap();
    assert_eq!(file.len().unwrap(), frames * bytes as u64);
    let frame = filled(bytes, file.alignment(), 0xa5);
    let durable = if dsync { durable_open(&path) } else { None };

    // Each frame's submission and durable times, in nanoseconds from the start.
    let mut submitted = vec![0u64; frames as usize];
    let mut durable_at = vec![0u64; frames as usize];
    let mut flushes = 0u64;
    let start = Instant::now();

    std::thread::scope(|s| {
        let (done, returns) = sync_channel::<u64>(k as usize);
        let mut idle: Vec<SyncSender<u64>> = Vec::new();
        for _ in 0..k {
            let writer = writer(&file, durable.as_ref());
            let (give, work) = sync_channel::<u64>(1);
            let (done, frame) = (done.clone(), &frame);
            s.spawn(move || {
                for n in work {
                    writer.write(frame.as_slice(), n * bytes as u64);
                    done.send(n).unwrap();
                }
            });
            idle.push(give);
        }
        drop(done);
        // The writer that took each frame in flight, returned to `idle` when it comes back.
        let mut holder: Vec<Option<SyncSender<u64>>> = (0..frames).map(|_| None).collect();
        let mut returned = vec![false; frames as usize];
        let (mut issued, mut covered) = (0u64, 0u64);
        while covered < frames {
            while issued < frames && issued < covered + k {
                let give = idle.pop().expect("a writer for each frame in the window");
                submitted[issued as usize] = start.elapsed().as_nanos() as u64;
                give.send(issued).unwrap();
                holder[issued as usize] = Some(give);
                issued += 1;
            }
            let mut back = Some(returns.recv().unwrap());
            while let Some(n) = back {
                returned[n as usize] = true;
                if dsync {
                    durable_at[n as usize] = start.elapsed().as_nanos() as u64;
                }
                idle.push(holder[n as usize].take().unwrap());
                back = returns.try_recv().ok();
            }
            let mut upto = covered;
            while upto < issued && returned[upto as usize] {
                upto += 1;
            }
            if upto == covered {
                continue;
            }
            if dsync {
                covered = upto;
                continue;
            }
            file.sync_data().unwrap();
            flushes += 1;
            let now = start.elapsed().as_nanos() as u64;
            durable_at[covered as usize..upto as usize].fill(now);
            covered = upto;
        }
        // Dropping the senders ends each writer's loop.
        drop(idle);
    });

    let seconds = start.elapsed().as_secs_f64();
    let mut lat: Vec<u64> = durable_at
        .iter()
        .zip(&submitted)
        .map(|(d, s)| (d - s) / 1000)
        .collect();
    lat.sort_unstable();
    let at = |q: f64| lat[((lat.len() as f64 * q) as usize).min(lat.len() - 1)];
    println!(
        "{k} {frames} {seconds:.3} {:.0} {flushes} {} {} {} {}",
        frames as f64 / seconds,
        at(0.50),
        at(0.99),
        at(0.999),
        lat[lat.len() - 1],
    );
    drop((durable, file));
    std::fs::remove_file(&path).unwrap();
}
