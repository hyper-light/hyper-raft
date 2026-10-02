//! focal-log's shared WAL on the workload: each replica a lease of its own group, appending
//! with `append_async_notified` and hearing of each answer through the notification, which
//! wakes its driver as a waker does; every 64th append a checkpoint of the 64 entries the
//! replica keeps, focal's way of keeping a window (`rewrite_checkpoint_async_notified`).
//!
//! focal refuses an append with `Capacity` when its writer's memory budget is spent, its
//! back-pressure, where mantle and hyper-log hold a submitter until there is room. A refused
//! replica tries again after the next answer, which gives budget back; the refusals are counted.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::sync_channel;
use std::time::{Duration, Instant};

use focal_log::{
    LogicalLogId, Record, RecordKind, SharedWal, WalAppend, WalIdentity, WalLease, WalOptions,
};
use focal_memory::BudgetLane;
use hyper_block::threads;

use crate::{KEEP, Point, counted, drivers, quantile};

fn options() -> WalOptions {
    WalOptions::new(WalIdentity {
        cluster: [7; 16],
        node: 1,
        stream: 0,
    })
}

fn log_id(group: usize) -> LogicalLogId {
    let mut id = [0u8; 16];
    id[..8].copy_from_slice(&(group as u64 + 1).to_le_bytes());
    LogicalLogId(id)
}

fn record(group: usize, index: u64, size: usize) -> Record {
    Record {
        log: log_id(group),
        kind: RecordKind::Entry,
        index,
        term: 1,
        payload: vec![0x5a; size],
    }
}

struct Replica {
    group: usize,
    lease: WalLease,
    last: u64,
    started: Instant,
    pending: Option<WalAppend>,
}

/// A directory this process made under `dir`, removed when dropped.
struct Directory(PathBuf);

impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn drive(
    leases: Vec<(usize, WalLease)>,
    size: usize,
    deadline: Instant,
    peak: &AtomicUsize,
) -> Vec<u64> {
    let mut replicas: Vec<Replica> = leases
        .into_iter()
        .map(|(group, lease)| Replica {
            group,
            lease,
            last: 0,
            started: Instant::now(),
            pending: None,
        })
        .collect();
    let (ready, woken) = sync_channel::<usize>(replicas.len());
    let submit = |r: &mut Replica, i: usize| -> bool {
        let next = r.last + 1;
        let tell = ready.clone();
        let notify: focal_log::Persisted = Box::new(move || {
            let _ = tell.try_send(i);
        });
        r.started = Instant::now();
        let sent = if next > KEEP && next % KEEP == 0 {
            let kept: Vec<Record> = (next - KEEP + 1..=next)
                .map(|index| record(r.group, index, size))
                .collect();
            r.lease
                .rewrite_checkpoint_async_notified(&kept, BudgetLane::Ordinary, Some(notify))
        } else {
            r.lease.append_async_notified(
                &[record(r.group, next, size)],
                BudgetLane::Ordinary,
                Some(notify),
            )
        };
        match sent {
            Ok(pending) => {
                r.pending = Some(pending);
                true
            }
            Err(focal_log::LogError::Capacity) => false,
            Err(e) => panic!("{e}"),
        }
    };
    let mut latencies = Vec::new();
    let mut out = 0usize;
    let mut refused = std::collections::VecDeque::new();
    for (i, r) in replicas.iter_mut().enumerate() {
        if submit(r, i) {
            out += 1;
        } else {
            refused.push_back(i);
        }
    }
    peak.fetch_max(threads::count().unwrap(), Ordering::Relaxed);
    while out > 0 {
        let Ok(i) = woken.recv() else { break };
        let Some(mut pending) = replicas[i].pending.take() else {
            continue;
        };
        let Some(answer) = pending.try_complete() else {
            replicas[i].pending = Some(pending);
            continue;
        };
        out -= 1;
        answer.unwrap();
        latencies.push(replicas[i].started.elapsed().as_nanos() as u64);
        replicas[i].last += 1;
        if Instant::now() < deadline {
            refused.push_back(i);
        }
        // The answer gave budget back: the refused try again, in the order refused.
        while let Some(&j) = refused.front() {
            if !submit(&mut replicas[j], j) {
                break;
            }
            refused.pop_front();
            out += 1;
        }
    }
    latencies
}

pub fn point(dir: &Path, size: usize, count: usize, step: Duration, count_allocs: bool) -> Point {
    let mut bits = [0u8; 8];
    for (i, b) in std::process::id().to_le_bytes().iter().enumerate() {
        bits[i] = *b;
    }
    let directory = Directory(dir.join(format!(".focal-log-compare-{}", u64::from_le_bytes(bits))));
    std::fs::create_dir_all(&directory.0).unwrap();
    let wal = SharedWal::open(&directory.0, options()).unwrap();
    let peak = AtomicUsize::new(0);
    let mut latencies = Vec::new();
    let mut elapsed = Duration::ZERO;
    // Every replica's lease is taken before any appends, as a node holds its groups' leases
    // before it serves: a lease is admitted against the writer's budget, which appends in
    // flight spend.
    let n = drivers(count);
    let mut shares: Vec<Vec<(usize, WalLease)>> = (0..n).map(|_| Vec::new()).collect();
    for g in 0..count {
        shares[g % n].push((g, wal.lease(log_id(g)).unwrap()));
    }
    let allocs = counted(count_allocs, || {
        let started = Instant::now();
        let deadline = started + step;
        let results: Vec<Vec<u64>> = std::thread::scope(|s| {
            let handles: Vec<_> = shares
                .into_iter()
                .map(|leases| {
                    let peak = &peak;
                    s.spawn(move || drive(leases, size, deadline, peak))
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        elapsed = started.elapsed();
        latencies = results.concat();
        latencies.len() as u64
    });
    latencies.sort_unstable();
    let stats = wal.stats().unwrap();
    drop(wal);
    let reopening = Instant::now();
    let wal = SharedWal::open(&directory.0, options()).unwrap();
    let reopen_ns = reopening.elapsed().as_nanos() as u64;
    let mut read = 0u64;
    for g in 0..count {
        let lease = wal.lease(log_id(g)).unwrap();
        lease
            .replay(|_| {
                read += 1;
                Ok(())
            })
            .unwrap();
    }
    Point {
        appends_per_s: latencies.len() as f64 / elapsed.as_secs_f64(),
        p50_ns: quantile(&latencies, 0.5),
        p99_ns: quantile(&latencies, 0.99),
        p999_ns: quantile(&latencies, 0.999),
        per_flush: stats.appended_records as f64 / stats.group_commits.max(1) as f64,
        reopen_ns,
        read,
        threads: peak.load(Ordering::Relaxed),
        allocs,
        syncs_per_s: f64::NAN,
        busy: f64::NAN,
    }
}
