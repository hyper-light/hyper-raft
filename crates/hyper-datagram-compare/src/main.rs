//! hyper-datagram measured against the plane it replaces, on one workload, the same hardware and
//! recorded commands (hyper-raft `CLAUDE.md` §1a, `docs/benchmarks.md`):
//!
//! ```text
//! hyper-datagram-compare one <hyper|slates> <message bytes> <rounds>
//! hyper-datagram-compare table <runs> <rounds> [sizes]
//! ```
//!
//! The workload is a consensus round's control traffic to one peer: as many messages of one size
//! as a path of 1,232 bytes carries (the IPv6 minimum MTU less its headers), sealed at one node and
//! opened at the other, in one thread, with no socket between them so the cost is the plane's.
//!
//! - **hyper**: hyper-datagram's `Plane`, which packs the round's messages into one datagram.
//! - **slates**: slates' `ControlDatagram::encode_sealed` and `decode_sealed` at `5cce86a`, the
//!   seal hyper-datagram was built after; it carries one message per datagram, under its own
//!   envelope.
//!
//! The messages are built before the clock starts, as a caller holds them. `one` runs one point
//! in this process and prints it as one line: nanoseconds per message, allocations per message,
//! bytes on the wire per message and datagrams per round. `table` runs every point of each plane
//! in a fresh process of its own, the planes in a rotated order each run, and prints a Markdown
//! table of each row's medians, with the least and the most.
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
    missing_docs
)]

use std::hint::black_box;
use std::process::Command;
use std::time::Instant;

use hyper_datagram::{
    AdmitAll, ExporterSecret, MAX_DATAGRAM_BYTES, OVERHEAD_BYTES, Plane, PlaneLimits, Role,
    SECRET_BYTES,
};
use hyper_measure::alloc::{self, Counting};
use hyper_measure::stats;
use slates_transport::seal::{KEY_BYTES, Opener, Sealer};
use slates_transport::{ControlDatagram, Envelope};

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// The path every point packs for: the IPv6 minimum MTU less the IP and UDP headers
/// (RFC 8200 §5: 1,280; 40 + 8 bytes of headers).
const PATH: usize = 1_280 - 48;
/// hyper-datagram's per-message length prefix.
const LENGTH_BYTES: usize = 2;
/// The planes, by the name the command line gives them.
const PLANES: [&str; 2] = ["hyper", "slates"];

/// What one point measured, per message.
#[derive(Clone, Copy, Debug, Default)]
struct Point {
    ns: f64,
    allocs: f64,
    wire: f64,
    datagrams: f64,
}

impl Point {
    fn line(&self) -> String {
        format!(
            "{} {} {} {}",
            self.ns, self.allocs, self.wire, self.datagrams
        )
    }

    fn parse(line: &str) -> Option<Self> {
        let f: Vec<&str> = line.split_whitespace().collect();
        Some(Self {
            ns: f.first()?.parse().ok()?,
            allocs: f.get(1)?.parse().ok()?,
            wire: f.get(2)?.parse().ok()?,
            datagrams: f.get(3)?.parse().ok()?,
        })
    }
}

/// The messages of one round: as many of `size` as hyper-datagram packs into the path, at least one.
fn messages(size: usize) -> usize {
    ((PATH - OVERHEAD_BYTES) / (size + LENGTH_BYTES)).max(1)
}

/// Runs `rounds` of `round` twice, once timed and once counted, and gives the cost per message.
fn measure(rounds: u64, per_round: usize, mut round: impl FnMut() -> usize) -> Point {
    // Warm: keys expanded, buffers grown, caches filled.
    for _ in 0..rounds / 10 + 1 {
        black_box(round());
    }
    let start = Instant::now();
    let mut wire = 0usize;
    for _ in 0..rounds {
        wire += black_box(round());
    }
    let elapsed = start.elapsed().as_nanos() as f64;
    alloc::begin();
    for _ in 0..rounds {
        black_box(round());
    }
    let counts = alloc::end();
    let n = (rounds * per_round as u64) as f64;
    Point {
        ns: elapsed / n,
        allocs: counts.allocations as f64 / n,
        wire: wire as f64 / n,
        datagrams: 0.0,
    }
}

fn hyper(size: usize, rounds: u64) -> Point {
    let limits = PlaneLimits {
        max_peers: 1,
        epochs_per_peer: 2,
        window_limit: 1_024,
    };
    let secret = || ExporterSecret::new([7; SECRET_BYTES]);
    let mut sender = Plane::new(1, limits).unwrap();
    sender
        .install_epoch(2, 1, &secret(), Role::Initiator)
        .unwrap();
    sender.set_path(2, PATH).unwrap();
    let mut receiver = Plane::new(2, limits).unwrap();
    receiver
        .install_epoch(1, 1, &secret(), Role::Acceptor)
        .unwrap();
    let per_round = messages(size);
    let message = vec![0x5a; size];
    let mut wire = vec![0u8; MAX_DATAGRAM_BYTES];
    let mut point = measure(rounds, per_round, || {
        for _ in 0..per_round {
            sender.queue(2, &message).unwrap();
        }
        let mut length = 0;
        sender.flush(|_, datagram| {
            let datagram = datagram.unwrap();
            wire[..datagram.len()].copy_from_slice(datagram);
            length = datagram.len();
        });
        let opened = receiver.open(&mut wire[..length], &AdmitAll).unwrap();
        assert_eq!(opened.messages().count(), per_round);
        length
    });
    point.datagrams = 1.0;
    point
}

fn slates(size: usize, rounds: u64) -> Point {
    let key = [7u8; KEY_BYTES];
    let mut sealer = Sealer::from_key(&key, 0);
    let mut opener = Opener::from_key(&key, 0);
    let per_round = messages(size);
    let datagram = ControlDatagram {
        sender: 1,
        key_epoch: 1,
        envelope: Envelope {
            kind: 1,
            class: 0,
            flags: 0,
            epoch: 1,
            hlc: 0,
            request_id: 0,
        },
        body: vec![0x5a; size],
    };
    let mut point = measure(rounds, per_round, || {
        let mut length = 0;
        for _ in 0..per_round {
            let sealed = datagram.encode_sealed(&mut sealer).unwrap();
            length += sealed.len();
            let opened = ControlDatagram::decode_sealed(&sealed, &mut opener).unwrap();
            assert_eq!(opened.body.len(), size);
        }
        length
    });
    point.datagrams = per_round as f64;
    point
}

fn one(args: &[String]) {
    let size: usize = args[1].parse().unwrap();
    let rounds: u64 = args[2].parse().unwrap();
    assert!(alloc::installed(), "the counting allocator is installed");
    let point = match args[0].as_str() {
        "hyper" => hyper(size, rounds),
        "slates" => slates(size, rounds),
        other => panic!("no plane named {other}"),
    };
    println!("{}", point.line());
}

fn run_one(plane: &str, size: usize, rounds: u64) -> Point {
    let out = Command::new(std::env::current_exe().unwrap())
        .args(["one", plane, &size.to_string(), &rounds.to_string()])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    Point::parse(text.lines().last().unwrap_or("")).unwrap_or_else(|| {
        panic!(
            "{plane} printed {text} {}",
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

/// A column's median with its least and most, as `median (least–most)`.
fn cell(samples: &[f64], digits: usize) -> String {
    let summary = stats::Summary::of(samples).unwrap();
    format!(
        "{:.digits$} ({:.digits$}–{:.digits$})",
        summary.median, summary.min, summary.max
    )
}

fn table(args: &[String]) {
    let runs: usize = args[0].parse().unwrap();
    let rounds: u64 = args[1].parse().unwrap();
    let sizes: Vec<usize> = args.get(2).map_or_else(
        || vec![16, 128, 1_024],
        |list| list.split(',').map(|s| s.parse().unwrap()).collect(),
    );
    println!(
        "{runs} runs of {rounds} rounds, each in a fresh process; medians (least–most). \
         A round is as many messages as a {PATH}-byte path packs."
    );
    println!();
    println!(
        "| Message | Plane | Messages a round | Datagrams a round | ns a message | Allocations a message | Wire bytes a message |"
    );
    println!("|---|---|---|---|---|---|---|");
    for size in sizes {
        let mut points: Vec<Vec<Point>> = vec![Vec::new(); PLANES.len()];
        for run in 0..runs {
            for turn in 0..PLANES.len() {
                let index = (run + turn) % PLANES.len();
                points[index].push(run_one(PLANES[index], size, rounds));
            }
        }
        for (index, plane) in PLANES.iter().enumerate() {
            let column = |f: fn(&Point) -> f64| points[index].iter().map(f).collect::<Vec<_>>();
            println!(
                "| {size} B | {plane} | {} | {} | {} | {} | {} |",
                messages(size),
                column(|p| p.datagrams)[0],
                cell(&column(|p| p.ns), 1),
                cell(&column(|p| p.allocs), 2),
                cell(&column(|p| p.wire), 1),
            );
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("one") => one(&args[1..]),
        Some("table") => table(&args[1..]),
        _ => panic!("usage: one <hyper|slates> <bytes> <rounds> | table <runs> <rounds> [sizes]"),
    }
}
