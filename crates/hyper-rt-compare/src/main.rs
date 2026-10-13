//! hyper-rt's loop under load against the runtimes it replaces, on one host: slates' runtime
//! (`slates-rt` at slates `8a18d90`) and tokio 1.53.1 (focal's), current-thread and multi-thread.
//! The cases are mantle's (its `docs/research/42-event-loop-under-load.md` §7–§8, measured there on
//! its vendored hyper-rt against tokio; `docs/benchmarks.md`, "hyper-rt: the loop under load"), run
//! here with slates' runtime beside the two. One case of one arm a process, so each runtime starts
//! as its consumer starts it: hyper-rt calibrated (`Calibration::measure`,
//! `RuntimeConfig::from_calibration`, mantle's ranges' path), slates' runtime from a measured machine
//! profile (`MachineProfile::measure`, `RuntimeConfig::from_profile`, its daemon's path), tokio with
//! its defaults.
//!
//! ```text
//! hyper-rt-compare <arm> <case> [key=value ...]
//!   arm:  hyper | hyper-mpsc | slates | tokio-ct | tokio-mt
//!   case: yield | spawn | pingpong | rtt | timer
//! ```
//!
//! **Channels.** hyper-rt has its own (`hyper_rt::sync`), tokio its `mpsc`. slates' runtime has none,
//! so its arm carries values on tokio's `mpsc`, which wakes any runtime's waker, and `hyper-mpsc` runs
//! hyper-rt on it too: the same channel under three runtimes, beside each runtime's own.
//!
//! **What is counted.** Every number is printed as `key=value` on a labelled line. The process's own
//! account (`hyper_measure::usage`) over the measured window gives CPU and, where the OS counts them,
//! instructions an operation; latencies are timed on the host's clock into a log-linear histogram of
//! bounded size (exact below 32 ns, within about 3% above), its percentiles a bucket's lower bound.
#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::disallowed_macros,
    clippy::disallowed_methods,
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::indexing_slicing,
    clippy::cognitive_complexity,
    clippy::too_many_lines
)]

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::sync_channel;
use std::time::{Duration, Instant};

use hyper_measure::usage::Usage;

static EPOCH: OnceLock<Instant> = OnceLock::new();

/// The harness's clock: nanoseconds since its first read.
fn now_ns() -> u64 {
    EPOCH.get_or_init(Instant::now).elapsed().as_nanos() as u64
}

/// Shape: slates' daemon's latency budget for its batch bound (slates `crates/server/src/config.rs`
/// `LATENCY_BUDGET_NS`, the provisioning target of its R9), which its runtime configuration is
/// calibrated against.
const SLATES_LATENCY_BUDGET_NS: u64 = 50_000;

struct Args(BTreeMap<String, String>);

impl Args {
    fn parse(rest: &[String]) -> Self {
        let mut map = BTreeMap::new();
        for arg in rest {
            let (k, v) = arg.split_once('=').expect("key=value");
            map.insert(k.to_owned(), v.to_owned());
        }
        Self(map)
    }

    fn get(&self, key: &str, default: u64) -> u64 {
        self.0
            .get(key)
            .map_or(default, |v| v.parse().expect("a number"))
    }
}

fn account() -> Usage {
    hyper_measure::usage::this().expect("the process's account")
}

/// The window's cost a operation, from the process's account before and after it.
fn print_cost(ops: u64, before: Usage, after: Usage, wall_ns: u64) {
    let spent = after.since(&before);
    let per = |v: u64| v as f64 / ops.max(1) as f64;
    let optional = |v: Option<u64>| v.map_or_else(|| "-".to_owned(), |v| format!("{:.1}", per(v)));
    println!(
        "cost ops={ops} elapsed_ns={wall_ns} ops_per_s={:.0} cpu_ns_per_op={:.1} instructions_per_op={} cycles_per_op={} wakeups_per_op={} cores_busy={:.3}",
        ops as f64 / (wall_ns.max(1) as f64 / 1e9),
        per(spent.cpu_ns()),
        optional(spent.instructions),
        optional(spent.cycles),
        optional(spent.wakeups),
        spent.cpu_ns() as f64 / wall_ns.max(1) as f64,
    );
}

/// A bounded latency histogram: log-linear, exact below 32 ns and 32 sub-buckets an octave above (within
/// about 3%), 1,920 buckets whatever the operation count, nothing kept an operation (mantle's rtbench).
struct Hist {
    buckets: Box<[u64]>,
    count: u64,
    sum: u128,
    max: u64,
}

/// Format: sub-buckets an octave (2^5).
const SUB: u64 = 32;
/// Format: 32 exact buckets, then 59 octaves of 32.
const BUCKETS: usize = 32 + 59 * 32;

impl Hist {
    fn new() -> Self {
        Self {
            buckets: vec![0; BUCKETS].into_boxed_slice(),
            count: 0,
            sum: 0,
            max: 0,
        }
    }

    fn index(v: u64) -> usize {
        if v < SUB {
            return v as usize;
        }
        let e = 63 - u64::from(v.leading_zeros());
        let sub = (v >> (e - 5)) - SUB;
        (SUB + (e - 5) * SUB + sub) as usize
    }

    fn lower(i: usize) -> u64 {
        let i = i as u64;
        if i < SUB {
            return i;
        }
        let e = (i - SUB) / SUB + 5;
        let sub = (i - SUB) % SUB;
        (SUB + sub) << (e - 5)
    }

    fn record(&mut self, v: u64) {
        self.buckets[Self::index(v).min(BUCKETS - 1)] += 1;
        self.count += 1;
        self.sum += u128::from(v);
        self.max = self.max.max(v);
    }

    fn merge(&mut self, other: &Self) {
        for (a, b) in self.buckets.iter_mut().zip(other.buckets.iter()) {
            *a += *b;
        }
        self.count += other.count;
        self.sum += other.sum;
        self.max = self.max.max(other.max);
    }

    fn at(&self, p: f64) -> u64 {
        let rank = ((self.count as f64 * p) as u64).min(self.count.saturating_sub(1));
        let mut seen = 0u64;
        for (i, c) in self.buckets.iter().enumerate() {
            seen += c;
            if seen > rank {
                return Self::lower(i);
            }
        }
        self.max
    }
}

fn print_lat(name: &str, h: &Hist) {
    if h.count == 0 {
        println!("lat.{name} n=0");
        return;
    }
    println!(
        "lat.{name} n={} mean_ns={:.0} p50_ns={} p90_ns={} p99_ns={} p999_ns={} max_ns={}",
        h.count,
        h.sum as f64 / h.count as f64,
        h.at(0.50),
        h.at(0.90),
        h.at(0.99),
        h.at(0.999),
        h.max
    );
}

/// A one-value mailbox a plain client thread waits on: the value in an atomic, the client parked
/// (std's parker) and unparked by the replier only if it parked (mantle's rtbench): the reply leg
/// costs the same under every runtime.
struct Mailbox {
    v: AtomicU64,
    waiter: OnceLock<std::thread::Thread>,
}

impl Mailbox {
    fn leaked() -> &'static Self {
        Box::leak(Box::new(Self {
            v: AtomicU64::new(0),
            waiter: OnceLock::new(),
        }))
    }

    fn put(&self, x: u64) {
        self.v.store(x + 1, Ordering::Release);
        if let Some(t) = self.waiter.get() {
            t.unpark();
        }
    }

    fn take(&self) -> u64 {
        let _ = self.waiter.get_or_init(std::thread::current);
        loop {
            let x = self.v.swap(0, Ordering::AcqRel);
            if x != 0 {
                return x - 1;
            }
            std::thread::park();
        }
    }
}

// ---------------------------------------------------------------------------------- the runtimes

mod hyper {
    use super::Duration;
    use hyper_rt::machine::calibration::{Calibration, Policy};
    use hyper_rt::{Runtime, RuntimeConfig, ShardId};

    /// mantle's path: the machine calibrated (40 ms a probe), the configuration from it, the shards
    /// left to the OS.
    pub fn runtime(shards: u16, tasks: usize, timers: usize) -> Runtime {
        let calibration = Calibration::measure(Duration::from_millis(40), shards)
            .expect("the machine calibrates");
        let constants = calibration
            .constants(&Policy {
                reserved_cores: shards,
                lateness_tolerance_ns: None,
                latency_objective_ns: None,
            })
            .expect("constants");
        let mut config = RuntimeConfig::from_calibration(&calibration, &constants, tasks, timers);
        config.shards = shards;
        config.pin = false;
        config.cores.clear();
        println!(
            "hyper.config shards={} wake_mean_ns={} step_budget_ns={} timer_tick_ns={} batch={} spin_ns={} ring_entries={}",
            config.shards,
            calibration.wake.mean_ns,
            config.step_budget_ns,
            config.timer_tick_ns,
            config.batch,
            config.spin_ns,
            config.ring_entries
        );
        Runtime::start(&config).expect("start")
    }

    pub fn shard(rt: &Runtime, i: usize) -> ShardId {
        rt.shard_ids()[i]
    }

    /// Stops the runtime and prints each shard's counters.
    pub fn finish(rt: Runtime) {
        let counters = rt.shutdown().expect("shutdown");
        for (i, c) in counters.iter().enumerate() {
            println!(
                "hyper.shard{i} steps={} polls={} long_steps={} preempted_steps={} unattributed_steps={} thread_accounts={} longest_step_ns={} waits={} harvests={} wakes_foreign={} timers_fired={} controls={}",
                c.steps,
                c.polls,
                c.long_steps,
                c.preempted_steps,
                c.unattributed_steps,
                c.thread_accounts,
                c.longest_step_ns,
                c.waits,
                c.harvests,
                c.wakes_foreign,
                c.timers_fired,
                c.controls
            );
        }
    }

    /// A busy client keeps the shard spinning between requests rather than parking (mantle's ranges).
    pub fn activity() {
        hyper_rt::registry::with_current(|context| context.note_activity());
    }
}

mod slates {
    use super::SLATES_LATENCY_BUDGET_NS;
    use slates_rt::{Runtime, RuntimeConfig, ShardId};

    /// The daemon's path: the machine profiled (its probes at their wall budget, no codec or core
    /// matrix, which the runtime's configuration reads nothing of), the configuration from it, the
    /// shards left to the OS as the daemon leaves an explicit count.
    pub fn runtime(shards: u16, tasks: usize, timers: usize) -> Runtime {
        let profile = slates_machine::MachineProfile::measure(slates_machine::ProfileOptions {
            codecs: false,
            core_matrix: false,
            ..slates_machine::ProfileOptions::default()
        })
        .expect("the machine profile measures");
        let mut config =
            RuntimeConfig::from_profile(&profile, tasks, timers, SLATES_LATENCY_BUDGET_NS);
        config.shards = shards;
        config.pin = false;
        config.cores.clear();
        println!(
            "slates.config shards={} wake_mean_ns={} step_budget_ns={} timer_tick_ns={} batch={} spin_ns={} ring_entries={}",
            config.shards,
            profile.wake.mean_ns,
            config.step_budget_ns,
            config.timer_tick_ns,
            config.batch,
            config.spin_ns,
            config.ring_entries
        );
        Runtime::start(&config).expect("start")
    }

    pub fn shard(rt: &Runtime, i: usize) -> ShardId {
        rt.shard_ids()[i]
    }
}

mod tok {
    pub fn current() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime")
    }

    pub fn multi(workers: usize) -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(workers.max(1))
            .enable_all()
            .build()
            .expect("tokio runtime")
    }
}

/// Runs the future `make` gives as the root task on shard 0 (thread 0) of `arm`'s runtime of one shard,
/// and returns its output.
fn root<T: Send + 'static, F: Future<Output = T> + Send + 'static>(
    arm: &str,
    tasks: usize,
    timers: usize,
    make: impl FnOnce() -> F,
) -> T {
    let (tx, rx) = sync_channel(1);
    let fut = make();
    let body = async move {
        let _ = tx.send(fut.await);
    };
    match arm {
        "hyper" | "hyper-mpsc" => {
            let rt = hyper::runtime(1, tasks, timers);
            rt.spawn_on(hyper::shard(&rt, 0), body).expect("spawn root");
            let out = rx.recv().expect("root result");
            let _ = rt.shutdown();
            out
        }
        "slates" => {
            let rt = slates::runtime(1, tasks, timers);
            rt.spawn_on(slates::shard(&rt, 0), body)
                .expect("spawn root");
            let out = rx.recv().expect("root result");
            let _ = rt.shutdown();
            out
        }
        "tokio-ct" => {
            tok::current().block_on(body);
            rx.recv().expect("root result")
        }
        "tokio-mt" => {
            let rt = tok::multi(1);
            rt.block_on(async { rt.spawn(body).await.expect("join") });
            rt.shutdown_background();
            rx.recv().expect("root result")
        }
        other => panic!("unknown arm {other}"),
    }
}

// ---------------------------------------------------------------------------------- yield, spawn

/// `tasks` tasks on one shard (thread) each yield `n / tasks` times: a poll that re-queues itself.
fn case_yield(arm: &str, args: &Args) {
    let n = args.get("n", 1_000_000);
    let tasks = args.get("tasks", 16);
    let per = n / tasks;
    println!("case=yield arm={arm} n={n} tasks={tasks}");
    macro_rules! body {
        ($spawn:expr, $join:expr, $yield_now:expr) => {
            root(arm, tasks as usize + 2, 1, move || async move {
                let before = account();
                let t0 = now_ns();
                let ids: Vec<_> = (0..tasks)
                    .map(|_| {
                        $spawn(async move {
                            for _ in 0..per {
                                $yield_now.await;
                            }
                        })
                    })
                    .collect();
                for id in ids {
                    $join(id).await;
                }
                (before, account(), now_ns() - t0)
            })
        };
    }
    let (before, after, wall) = match arm {
        "hyper" | "hyper-mpsc" => body!(
            |f| hyper_rt::futures::spawn(f).expect("spawn"),
            |id| async move {
                let _ = hyper_rt::futures::join(id).await;
            },
            hyper_rt::futures::yield_now()
        ),
        "slates" => body!(
            |f| slates_rt::futures::spawn(f).expect("spawn"),
            |id| async move {
                let _ = slates_rt::futures::join(id).await;
            },
            slates_rt::futures::yield_now()
        ),
        _ => body!(
            tokio::spawn,
            |id: tokio::task::JoinHandle<()>| async move {
                let _ = id.await;
            },
            tokio::task::yield_now()
        ),
    };
    print_cost(per * tasks, before, after, wall);
}

/// A root task spawns `batch` children that end at their first poll, joins them, and repeats until `n`
/// ran: one spawn, one poll, one completion and one join an operation.
fn case_spawn(arm: &str, args: &Args) {
    let n = args.get("n", 200_000);
    let batch = args.get("batch", 1_000);
    let rounds = n / batch;
    println!("case=spawn arm={arm} n={n} batch={batch}");
    macro_rules! body {
        ($spawn:expr, $join:expr) => {
            root(arm, 2 * batch as usize + 2, 1, move || async move {
                let mut ids = Vec::with_capacity(batch as usize);
                let before = account();
                let t0 = now_ns();
                for _ in 0..rounds {
                    for _ in 0..batch {
                        ids.push($spawn(async {}));
                    }
                    for id in ids.drain(..) {
                        $join(id).await;
                    }
                }
                (before, account(), now_ns() - t0)
            })
        };
    }
    let (before, after, wall) = match arm {
        "hyper" | "hyper-mpsc" => body!(
            |f| hyper_rt::futures::spawn(f).expect("spawn"),
            |id| async move {
                let _ = hyper_rt::futures::join(id).await;
            }
        ),
        "slates" => body!(
            |f| slates_rt::futures::spawn(f).expect("spawn"),
            |id| async move {
                let _ = slates_rt::futures::join(id).await;
            }
        ),
        _ => body!(tokio::spawn, |id: tokio::task::JoinHandle<()>| async move {
            let _ = id.await;
        }),
    };
    print_cost(rounds * batch, before, after, wall);
}

// ---------------------------------------------------------------------------------- pingpong

/// Two tasks on two shards (threads) bounce a value through two channels of capacity one: every hop
/// a wake across threads. The round trip is timed on the initiator.
fn case_pingpong(arm: &str, args: &Args) {
    let n = args.get("n", 20_000);
    let warm = args.get("warm", 2_000);
    let act = args.get("activity", 1) != 0;
    // Both ends on one shard (thread): the local request/reply handoff.
    let local = args.get("local", 0) != 0;
    // What each end computes per message, nanoseconds: a handoff whose polls do work.
    let work_ns = args.get("work_ns", 0);
    println!(
        "case=pingpong arm={arm} n={n} warm={warm} activity={act} local={local} work_ns={work_ns}"
    );
    let far = usize::from(!local);
    let shards: u16 = if local { 1 } else { 2 };
    let (done_tx, done_rx) = sync_channel::<(Hist, Usage, Usage, u64)>(1);
    // The initiator and the echo, over any channel: `send` and `recv` are the channel's, `active` the
    // runtime's mark of a busy client
    macro_rules! initiator {
        ($to_b:ident, $a_rx:ident, $send:expr, $recv:expr, $active:expr) => {
            async move {
                let mut lat = Hist::new();
                let mut start = None;
                for i in 0..warm + n {
                    if i == warm {
                        start = Some((account(), now_ns()));
                    }
                    let t0 = now_ns();
                    if !$send(&$to_b, t0).await {
                        break;
                    }
                    if !$recv(&mut $a_rx).await {
                        break;
                    }
                    spin(work_ns);
                    $active;
                    if i >= warm {
                        lat.record(now_ns() - t0);
                    }
                }
                let (before, t0) = start.expect("measured");
                let _ = done_tx.send((lat, before, account(), now_ns() - t0));
            }
        };
    }
    macro_rules! echo {
        ($to_a:ident, $b_rx:ident, $send:expr, $recv:expr, $active:expr) => {
            async move {
                while $recv(&mut $b_rx).await {
                    spin(work_ns);
                    $active;
                    if !$send(&$to_a, 0).await {
                        break;
                    }
                }
            }
        };
    }
    async fn hyper_send(tx: &hyper_rt::sync::Sender<u64>, v: u64) -> bool {
        tx.send(v).await.is_ok()
    }
    async fn hyper_recv(rx: &mut hyper_rt::sync::ChannelReceiver<u64>) -> bool {
        rx.recv().await.is_ok()
    }
    async fn mpsc_send(tx: &tokio::sync::mpsc::Sender<u64>, v: u64) -> bool {
        tx.send(v).await.is_ok()
    }
    async fn mpsc_recv(rx: &mut tokio::sync::mpsc::Receiver<u64>) -> bool {
        rx.recv().await.is_some()
    }
    let (lat, before, after, wall) = match arm {
        "hyper" => {
            let rt = hyper::runtime(shards, 4, 1);
            let (to_b, mut b_rx) = hyper_rt::sync::channel::<u64>(1).expect("channel");
            let (to_a, mut a_rx) = hyper_rt::sync::channel::<u64>(1).expect("channel");
            let active = |on: bool| {
                if on {
                    hyper::activity()
                }
            };
            rt.spawn_on(
                hyper::shard(&rt, far),
                echo!(to_a, b_rx, hyper_send, hyper_recv, active(act)),
            )
            .expect("spawn echo");
            rt.spawn_on(
                hyper::shard(&rt, 0),
                initiator!(to_b, a_rx, hyper_send, hyper_recv, active(act)),
            )
            .expect("spawn initiator");
            let out = done_rx.recv().expect("done");
            let _ = rt.shutdown();
            out
        }
        "hyper-mpsc" => {
            let rt = hyper::runtime(shards, 4, 1);
            let (to_b, mut b_rx) = tokio::sync::mpsc::channel::<u64>(1);
            let (to_a, mut a_rx) = tokio::sync::mpsc::channel::<u64>(1);
            let active = |on: bool| {
                if on {
                    hyper::activity()
                }
            };
            rt.spawn_on(
                hyper::shard(&rt, far),
                echo!(to_a, b_rx, mpsc_send, mpsc_recv, active(act)),
            )
            .expect("spawn echo");
            rt.spawn_on(
                hyper::shard(&rt, 0),
                initiator!(to_b, a_rx, mpsc_send, mpsc_recv, active(act)),
            )
            .expect("spawn initiator");
            let out = done_rx.recv().expect("done");
            let _ = rt.shutdown();
            out
        }
        "slates" => {
            let rt = slates::runtime(shards, 4, 1);
            let (to_b, mut b_rx) = tokio::sync::mpsc::channel::<u64>(1);
            let (to_a, mut a_rx) = tokio::sync::mpsc::channel::<u64>(1);
            rt.spawn_on(
                slates::shard(&rt, far),
                echo!(to_a, b_rx, mpsc_send, mpsc_recv, ()),
            )
            .expect("spawn echo");
            rt.spawn_on(
                slates::shard(&rt, 0),
                initiator!(to_b, a_rx, mpsc_send, mpsc_recv, ()),
            )
            .expect("spawn initiator");
            let out = done_rx.recv().expect("done");
            let _ = rt.shutdown();
            out
        }
        "tokio-mt" => {
            let rt = tok::multi(1 + far);
            let (to_b, mut b_rx) = tokio::sync::mpsc::channel::<u64>(1);
            let (to_a, mut a_rx) = tokio::sync::mpsc::channel::<u64>(1);
            rt.spawn(echo!(to_a, b_rx, mpsc_send, mpsc_recv, ()));
            rt.spawn(initiator!(to_b, a_rx, mpsc_send, mpsc_recv, ()));
            let out = done_rx.recv().expect("done");
            rt.shutdown_background();
            out
        }
        "tokio-ct" if local => {
            let (to_b, mut b_rx) = tokio::sync::mpsc::channel::<u64>(1);
            let (to_a, mut a_rx) = tokio::sync::mpsc::channel::<u64>(1);
            let rt = tok::current();
            rt.block_on(async {
                tokio::spawn(echo!(to_a, b_rx, mpsc_send, mpsc_recv, ()));
                initiator!(to_b, a_rx, mpsc_send, mpsc_recv, ()).await;
            });
            done_rx.recv().expect("done")
        }
        "tokio-ct" => {
            let (to_b, mut b_rx) = tokio::sync::mpsc::channel::<u64>(1);
            let (to_a, mut a_rx) = tokio::sync::mpsc::channel::<u64>(1);
            let (rt_a, rt_b) = (tok::current(), tok::current());
            std::thread::scope(|scope| {
                scope.spawn(move || rt_b.block_on(echo!(to_a, b_rx, mpsc_send, mpsc_recv, ())));
                scope
                    .spawn(move || rt_a.block_on(initiator!(to_b, a_rx, mpsc_send, mpsc_recv, ())));
                done_rx.recv().expect("done")
            })
        }
        other => panic!("unknown arm {other}"),
    };
    print_cost(lat.count, before, after, wall);
    print_lat("rtt", &lat);
}

/// Computes for `ns` nanoseconds on the harness's clock.
fn spin(ns: u64) {
    if ns == 0 {
        return;
    }
    let t = now_ns();
    while now_ns().saturating_sub(t) < ns {
        std::hint::spin_loop();
    }
}

// ---------------------------------------------------------------------------------- rtt

/// Whether the busy task of [`case_rtt`] should stop.
static BUSY_STOP: AtomicBool = AtomicBool::new(false);
/// The busy task's slices so far: its share of the shard over the measured window, so a runtime that
/// starves it does not read as one that serves the clients sooner.
static BUSY_SLICES: AtomicU64 = AtomicU64::new(0);

/// A self-wake, the same under every runtime: what a future that has more to do and lets others run
/// first returns, where the runtime's own `yield_now` may do more.
struct SelfWake(bool);

impl Future for SelfWake {
    type Output = ();

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<()> {
        if self.0 {
            return std::task::Poll::Ready(());
        }
        self.0 = true;
        cx.waker().wake_by_ref();
        std::task::Poll::Pending
    }
}

/// A cooperative task computing in slices of `busy_ns`, yielding between them through `yield_now`.
async fn busy<Y: Future<Output = ()>>(busy_ns: u64, yield_now: impl Fn() -> Y) {
    while !BUSY_STOP.load(Ordering::Relaxed) {
        let t = now_ns();
        while now_ns().saturating_sub(t) < busy_ns {
            std::hint::spin_loop();
        }
        BUSY_SLICES.fetch_add(1, Ordering::Relaxed);
        yield_now().await;
    }
}

/// Plain client threads against server tasks on one shard (thread): each request carries the
/// client's send stamp, its server stamps the receipt (the wake from a foreign thread into the
/// runtime, one way) and answers through the client's mailbox. With `gap_us` a client sleeps between
/// requests, so the server has gone idle when the next arrives; `clients` threads, each with its own
/// server task (fan-in); with `busy_us` a cooperative task beside the servers computes in slices of
/// that length, yielding between them: through its runtime's `yield_now`, or with `yield=wake` a
/// self-wake, the same under every runtime.
fn case_rtt(arm: &str, args: &Args) {
    let n = args.get("n", 20_000);
    let warm = args.get("warm", 2_000);
    let gap_us = args.get("gap_us", 0);
    let clients = args.get("clients", 1).max(1) as usize;
    let act = args.get("activity", 1) != 0;
    let busy_ns = args.get("busy_us", 0) * 1_000;
    let self_wake = args.0.get("yield").is_some_and(|y| y == "wake");
    let per = n / clients as u64;
    println!(
        "case=rtt arm={arm} n={n} warm={warm} gap_us={gap_us} clients={clients} activity={act} busy_us={} yield={}",
        busy_ns / 1_000,
        if self_wake { "wake" } else { "own" }
    );
    let mailboxes: Vec<&'static Mailbox> = (0..clients).map(|_| Mailbox::leaked()).collect();

    /// One client: `count` requests, each one's one-way and round-trip times recorded.
    fn client(count: u64, gap_us: u64, send: &dyn Fn(u64), mailbox: &Mailbox) -> (Hist, Hist) {
        let mut oneway = Hist::new();
        let mut rtt = Hist::new();
        for _ in 0..count {
            if gap_us > 0 {
                std::thread::sleep(Duration::from_micros(gap_us));
            }
            let t0 = now_ns();
            send(t0);
            let one = mailbox.take();
            oneway.record(one);
            rtt.record(now_ns() - t0);
        }
        (oneway, rtt)
    }

    /// The clients: a warm-up, then the measured requests between two barriers, the last client to
    /// finish reading the account on its own thread.
    fn clients_run(
        warm: u64,
        per: u64,
        gap_us: u64,
        senders: Vec<Box<dyn Fn(u64) + Send>>,
        mailboxes: &[&'static Mailbox],
    ) -> (Hist, Hist, Usage, Usage, u64, u64) {
        let barrier = std::sync::Barrier::new(senders.len() + 1);
        let remaining = std::sync::atomic::AtomicUsize::new(senders.len());
        let (done_tx, done_rx) = sync_channel(senders.len());
        let (end_tx, end_rx) = sync_channel(1);
        let mut start = None;
        std::thread::scope(|scope| {
            for (send, mailbox) in senders.into_iter().zip(mailboxes) {
                let (barrier, remaining) = (&barrier, &remaining);
                let (done_tx, end_tx) = (done_tx.clone(), end_tx.clone());
                scope.spawn(move || {
                    let _ = client(warm, gap_us, &*send, mailbox);
                    barrier.wait();
                    barrier.wait();
                    let out = client(per, gap_us, &*send, mailbox);
                    if remaining.fetch_sub(1, Ordering::AcqRel) == 1 {
                        let _ =
                            end_tx.send((account(), now_ns(), BUSY_SLICES.load(Ordering::Relaxed)));
                    }
                    drop(send);
                    let _ = done_tx.send(out);
                });
            }
            barrier.wait();
            start = Some((account(), now_ns(), BUSY_SLICES.load(Ordering::Relaxed)));
            barrier.wait();
        });
        let (after, t1, s1) = end_rx.recv().expect("end");
        drop(done_tx);
        let (mut oneway, mut rtt) = (Hist::new(), Hist::new());
        for (o, r) in done_rx.iter() {
            oneway.merge(&o);
            rtt.merge(&r);
        }
        let (before, t0, s0) = start.expect("start");
        (oneway, rtt, before, after, t1 - t0, s1 - s0)
    }

    // The busy task under each runtime: its own yield, or the self-wake.
    macro_rules! busy_with {
        ($own:expr) => {
            async move {
                if self_wake {
                    busy(busy_ns, || SelfWake(false)).await;
                } else {
                    busy(busy_ns, $own).await;
                }
            }
        };
    }

    // A server over tokio's mpsc, under any runtime
    let mpsc_server = |mailbox: &'static Mailbox, active: fn()| {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<u64>(1);
        let server = async move {
            while let Some(stamp) = rx.recv().await {
                let got = now_ns();
                active();
                mailbox.put(got.saturating_sub(stamp));
            }
        };
        let send: Box<dyn Fn(u64) + Send> = Box::new(move |t| tx.try_send(t).expect("room"));
        (server, send)
    };
    fn hyper_active() {
        hyper::activity();
    }
    fn none() {}

    let (oneway, rtt, before, after, wall, slices) = match arm {
        "hyper" => {
            let rt = hyper::runtime(1, clients + 2, 1);
            if busy_ns > 0 {
                rt.spawn_on(
                    hyper::shard(&rt, 0),
                    busy_with!(hyper_rt::futures::yield_now),
                )
                .expect("spawn busy");
            }
            let mut senders: Vec<Box<dyn Fn(u64) + Send>> = Vec::new();
            for &mailbox in &mailboxes {
                let (tx, mut rx) = hyper_rt::sync::channel::<u64>(1).expect("channel");
                rt.spawn_on(hyper::shard(&rt, 0), async move {
                    while let Ok(stamp) = rx.recv().await {
                        let got = now_ns();
                        if act {
                            hyper::activity();
                        }
                        mailbox.put(got.saturating_sub(stamp));
                    }
                })
                .expect("spawn server");
                senders.push(Box::new(move |t| tx.try_send(t).expect("room")));
            }
            let out = clients_run(warm, per, gap_us, senders, &mailboxes);
            BUSY_STOP.store(true, Ordering::Relaxed);
            hyper::finish(rt);
            out
        }
        "hyper-mpsc" => {
            let rt = hyper::runtime(1, clients + 2, 1);
            if busy_ns > 0 {
                rt.spawn_on(
                    hyper::shard(&rt, 0),
                    busy_with!(hyper_rt::futures::yield_now),
                )
                .expect("spawn busy");
            }
            let mut senders = Vec::new();
            for &mailbox in &mailboxes {
                let active: fn() = if act { hyper_active } else { none };
                let (server, send) = mpsc_server(mailbox, active);
                rt.spawn_on(hyper::shard(&rt, 0), server)
                    .expect("spawn server");
                senders.push(send);
            }
            let out = clients_run(warm, per, gap_us, senders, &mailboxes);
            BUSY_STOP.store(true, Ordering::Relaxed);
            let _ = rt.shutdown();
            out
        }
        "slates" => {
            let rt = slates::runtime(1, clients + 2, 1);
            if busy_ns > 0 {
                rt.spawn_on(
                    slates::shard(&rt, 0),
                    busy_with!(slates_rt::futures::yield_now),
                )
                .expect("spawn busy");
            }
            let mut senders = Vec::new();
            for &mailbox in &mailboxes {
                let (server, send) = mpsc_server(mailbox, none);
                rt.spawn_on(slates::shard(&rt, 0), server)
                    .expect("spawn server");
                senders.push(send);
            }
            let out = clients_run(warm, per, gap_us, senders, &mailboxes);
            BUSY_STOP.store(true, Ordering::Relaxed);
            let _ = rt.shutdown();
            out
        }
        "tokio-mt" => {
            let rt = tok::multi(1);
            if busy_ns > 0 {
                rt.spawn(busy_with!(tokio::task::yield_now));
            }
            let mut senders = Vec::new();
            for &mailbox in &mailboxes {
                let (server, send) = mpsc_server(mailbox, none);
                rt.spawn(server);
                senders.push(send);
            }
            let out = clients_run(warm, per, gap_us, senders, &mailboxes);
            BUSY_STOP.store(true, Ordering::Relaxed);
            rt.shutdown_background();
            out
        }
        "tokio-ct" => {
            let rt = tok::current();
            let mut servers = Vec::new();
            let mut senders = Vec::new();
            for &mailbox in &mailboxes {
                let (server, send) = mpsc_server(mailbox, none);
                servers.push(server);
                senders.push(send);
            }
            std::thread::scope(|scope| {
                let handle = scope.spawn(|| {
                    rt.block_on(async {
                        let busy =
                            (busy_ns > 0).then(|| tokio::spawn(busy_with!(tokio::task::yield_now)));
                        let joins: Vec<_> = servers.into_iter().map(tokio::spawn).collect();
                        for j in joins {
                            let _ = j.await;
                        }
                        if let Some(busy) = busy {
                            let _ = busy.await;
                        }
                    });
                });
                let out = clients_run(warm, per, gap_us, senders, &mailboxes);
                BUSY_STOP.store(true, Ordering::Relaxed);
                handle.join().expect("tokio thread");
                out
            })
        }
        other => panic!("unknown arm {other}"),
    };
    print_cost(rtt.count, before, after, wall);
    print_lat("oneway", &oneway);
    print_lat("rtt", &rtt);
    if busy_ns > 0 {
        println!(
            "busy slices={slices} share={:.3}",
            (slices * busy_ns) as f64 / wall.max(1) as f64
        );
    }
}

// ---------------------------------------------------------------------------------- timer

/// One task sleeps `d_us` `n` times; each sleep's lateness (what it took less what it asked) is
/// recorded.
fn case_timer(arm: &str, args: &Args) {
    let n = args.get("n", 2_000);
    let d_us = args.get("d_us", 1_000);
    let d_ns = d_us * 1_000;
    println!("case=timer arm={arm} n={n} d_us={d_us}");
    macro_rules! body {
        ($sleep:expr) => {
            root(arm, 2, 2, move || async move {
                let mut late = Hist::new();
                let before = account();
                let t0 = now_ns();
                for _ in 0..n {
                    let t = now_ns();
                    $sleep(d_ns).await;
                    late.record((now_ns() - t).saturating_sub(d_ns));
                }
                (late, before, account(), now_ns() - t0)
            })
        };
    }
    let (late, before, after, wall) = match arm {
        "hyper" | "hyper-mpsc" => body!(|ns| async move {
            hyper_rt::futures::sleep(ns).await.expect("sleep");
        }),
        "slates" => body!(|ns| async move {
            slates_rt::futures::sleep(ns).await.expect("sleep");
        }),
        _ => body!(|ns| tokio::time::sleep(Duration::from_nanos(ns))),
    };
    print_cost(n, before, after, wall);
    print_lat("late", &late);
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let (Some(arm), Some(case)) = (argv.first(), argv.get(1)) else {
        eprintln!(
            "usage: hyper-rt-compare <hyper|hyper-mpsc|slates|tokio-ct|tokio-mt> <yield|spawn|pingpong|rtt|timer> [key=value ...]"
        );
        std::process::exit(2);
    };
    let _ = now_ns();
    let args = Args::parse(&argv[2..]);
    match case.as_str() {
        "yield" => case_yield(arm, &args),
        "spawn" => case_spawn(arm, &args),
        "pingpong" => case_pingpong(arm, &args),
        "rtt" => case_rtt(arm, &args),
        "timer" => case_timer(arm, &args),
        other => panic!("unknown case {other}"),
    }
}
