//! hyper-log on the workload, as `crates/hyper-log/benches/log.rs` drives it.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::sync_channel;
use std::time::{Duration, Instant};

use hyper_block::buf::Alignment;
use hyper_block::file::{CachingRequest, DeviceFile};
use hyper_block::scratch::Scratch;
use hyper_block::threads;
use hyper_log::{Class, Config, Entries, Entry, Log, LogError, Pending, Start, Update, Waits};

use crate::{KEEP, Point, counted, drivers, quantile};

fn config(replicas: usize) -> Config {
    Config {
        segment_bytes: 16 << 20,
        max_segments: 64,
        max_groups: replicas,
        group_entries: 1 << 20,
        group_bytes: 1 << 30,
        group_cache: 1 << 16,
        queue_submissions: replicas * 2,
        waits: Waits::Measured,
    }
}

fn open(path: &Path) -> DeviceFile {
    DeviceFile::open(
        path,
        false,
        CachingRequest::PreferDirect,
        Alignment::new(4096).unwrap(),
    )
    .unwrap()
}

struct Replica {
    group: u128,
    last: u64,
    started: Instant,
    pending: Option<Pending>,
}

fn update(last: u64, size: usize) -> Update {
    let next = last + 1;
    Update {
        start: (next > KEEP && next % KEEP == 0).then(|| Start {
            index: next - KEEP,
            term: 1,
        }),
        entries: Some(Entries {
            first: next,
            entries: vec![Entry {
                term: 1,
                bytes: vec![0x5a; size],
            }],
        }),
        ..Update::default()
    }
}

fn drive(
    log: &Log<DeviceFile>,
    driver: usize,
    count: usize,
    size: usize,
    deadline: Instant,
    peak: &AtomicUsize,
) -> Result<Vec<u64>, LogError> {
    let n = drivers(count);
    let mut replicas: Vec<Replica> = (driver..count)
        .step_by(n)
        .map(|r| Replica {
            group: r as u128,
            last: 0,
            started: Instant::now(),
            pending: None,
        })
        .collect();
    let (ready, woken) = sync_channel(replicas.len());
    let wakers: Vec<_> = (0..replicas.len())
        .map(|i| hyper_measure::wake::waker(i, ready.clone()).0)
        .collect();
    drop(ready);
    let submit = |r: &mut Replica, waker: &std::task::Waker| -> Result<(), LogError> {
        let u = update(r.last, size);
        r.started = Instant::now();
        r.pending = Some(log.submit_waking(r.group, Class::Normal, u, waker.clone())?);
        Ok(())
    };
    let mut latencies = Vec::new();
    let mut out = 0usize;
    for (r, w) in replicas.iter_mut().zip(&wakers) {
        submit(r, w)?;
        out += 1;
    }
    peak.fetch_max(threads::count().unwrap(), Ordering::Relaxed);
    while out > 0 {
        let Ok(i) = woken.recv() else { break };
        let Some(pending) = replicas[i].pending.take() else {
            continue;
        };
        let Some(answer) = pending.poll() else {
            replicas[i].pending = Some(pending);
            continue;
        };
        out -= 1;
        answer?;
        latencies.push(replicas[i].started.elapsed().as_nanos() as u64);
        replicas[i].last += 1;
        if Instant::now() < deadline {
            submit(&mut replicas[i], &wakers[i])?;
            out += 1;
        }
    }
    Ok(latencies)
}

pub fn point(dir: &Path, size: usize, count: usize, step: Duration, count_allocs: bool) -> Point {
    let scratch = Scratch::create(dir, ".hyper-log-compare").unwrap();
    let log = Log::create(open(scratch.path()), config(count), 1).unwrap();
    let peak = AtomicUsize::new(0);
    let mut latencies = Vec::new();
    let mut elapsed = Duration::ZERO;
    let allocs = counted(count_allocs, || {
        let started = Instant::now();
        let deadline = started + step;
        let n = drivers(count);
        let results: Vec<Vec<u64>> = std::thread::scope(|s| {
            let handles: Vec<_> = (0..n)
                .map(|d| {
                    let (log, peak) = (&log, &peak);
                    s.spawn(move || drive(log, d, count, size, deadline, peak).unwrap())
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        elapsed = started.elapsed();
        latencies = results.concat();
        latencies.len() as u64
    });
    latencies.sort_unstable();
    let (frames, updates) = log.flushed();
    drop(log.close().unwrap());
    let reopening = Instant::now();
    let (log, _) = Log::open(open(scratch.path()), config(count), 1).unwrap();
    let reopen_ns = reopening.elapsed().as_nanos() as u64;
    let mut read = 0u64;
    for g in 0..count {
        if let Some(v) = log.view(g as u128).unwrap()
            && v.last > v.start.index
        {
            read += log
                .entries(g as u128, v.start.index + 1, v.last + 1, u64::MAX)
                .unwrap()
                .len() as u64;
        }
    }
    Point {
        appends_per_s: latencies.len() as f64 / elapsed.as_secs_f64(),
        p50_ns: quantile(&latencies, 0.5),
        p99_ns: quantile(&latencies, 0.99),
        p999_ns: quantile(&latencies, 0.999),
        per_flush: updates as f64 / frames.max(1) as f64,
        reopen_ns,
        read,
        threads: peak.load(Ordering::Relaxed),
        allocs,
    }
}
