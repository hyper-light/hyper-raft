//! The estimator run as the detector over synthetic traces of the recorded shapes, checked against
//! Theorem 7's bound the way the trace analyser checks a recorded trace (`docs/timing.md` §2.6;
//! `docs/benchmarks.md`, "Heartbeat traces"). No raw trace is kept, so each shape is a model fitted
//! to a recorded run's table, and the test first checks the model still has that run's shape.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::disallowed_macros,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::cognitive_complexity,
    missing_docs
)]

use std::time::Duration;

use hyper_timing::{Costs, Event, Floors, LinkEstimator, Refusal, Schedule};

/// The normal distribution's two-sided 95 % point, for the Poisson score interval the analyser
/// uses (Brown, Cai and DasGupta 2003).
const Z95: f64 = 1.959_963_984_540_054;

/// A delay model: a body (a floor plus an exponential), occasional hiccups, and stalls. A stall
/// starts at Poisson times, lasts a Pareto time capped at the run's longest, and delays every
/// heartbeat scheduled inside it to its end, as a sender blocked on its scheduler or its disk does.
/// Microseconds and seconds.
#[derive(Clone, Copy)]
struct Shape {
    floor: f64,
    body: f64,
    hiccup: f64,
    hiccup_mean: f64,
    stalls_per_second: f64,
    stall_least: f64,
    stall_tail: f64,
    stall_most: f64,
}

/// macOS at 100 µs (`docs/benchmarks.md`, "macOS, 100 µs"): median 51.7 µs, MAD 14.8, mean 95.7,
/// deviation 521.5, `sd / (1.4826·MAD)` 24, longest 42 ms.
const MACOS: Shape = Shape {
    floor: 33.0,
    body: 25.0,
    hiccup: 0.02,
    hiccup_mean: 400.0,
    stalls_per_second: 0.15,
    stall_least: 2_000.0,
    stall_tail: 1.0,
    stall_most: 42_000.0,
};

/// macOS with a write and `F_FULLFSYNC` before each heartbeat at 10 ms ("macOS, write and
/// F_FULLFSYNC, 10 ms"): median 5.9 ms, MAD 0.58, mean 7.0, deviation 10.8, `sd / (1.4826·MAD)` 13,
/// p99.9 189 ms, longest 246 ms.
const FLUSH: Shape = Shape {
    floor: 5_000.0,
    body: 1_200.0,
    hiccup: 0.015,
    hiccup_mean: 6_000.0,
    stalls_per_second: 0.3,
    stall_least: 10_000.0,
    stall_tail: 1.0,
    stall_most: 250_000.0,
};

/// xorshift64* (Vigna 2016): a fixed seed gives the same trace on every machine.
struct Noise(u64);

impl Noise {
    fn uniform(&mut self) -> f64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        let word = self.0.wrapping_mul(0x2545_F491_4F6C_DD1D);
        ((word >> 11) as f64 + 0.5) / (1u64 << 53) as f64
    }
    fn exponential(&mut self, mean: f64) -> f64 {
        -mean * self.uniform().ln()
    }
}

/// Arrival times, nanoseconds on the sender's clock, of `count` heartbeats every `interval_us`
/// scheduled from zero, over a FIFO link (a heartbeat never passes the one before it).
fn trace(shape: Shape, interval_us: f64, count: usize, seed: u64) -> Vec<u64> {
    let mut noise = Noise(seed);
    let mut next_stall = noise.exponential(1e6 / shape.stalls_per_second);
    let mut stall_end = f64::NEG_INFINITY;
    let mut last = 0u64;
    (0..count)
        .map(|i| {
            let sigma = i as f64 * interval_us;
            while next_stall <= sigma {
                let length = (shape.stall_least / noise.uniform().powf(1.0 / shape.stall_tail))
                    .min(shape.stall_most);
                stall_end = stall_end.max(next_stall + length);
                next_stall += noise.exponential(1e6 / shape.stalls_per_second);
            }
            let mut delay = shape.floor + noise.exponential(shape.body);
            if noise.uniform() < shape.hiccup {
                delay += noise.exponential(shape.hiccup_mean);
            }
            delay += (stall_end - sigma).max(0.0);
            let arrival = ((sigma + delay) * 1e3) as u64;
            last = last.max(arrival);
            last
        })
        .collect()
}

/// `(median, MAD, mean, deviation)` of the delays, microseconds.
fn summary(arrivals: &[u64], interval_us: f64) -> (f64, f64, f64, f64) {
    let delays: Vec<f64> = arrivals
        .iter()
        .enumerate()
        .map(|(i, a)| *a as f64 / 1e3 - i as f64 * interval_us)
        .collect();
    let n = delays.len() as f64;
    let mean = delays.iter().sum::<f64>() / n;
    let sd = (delays.iter().map(|d| (d - mean).powi(2)).sum::<f64>() / (n - 1.0)).sqrt();
    let mut sorted = delays.clone();
    sorted.sort_by(f64::total_cmp);
    let median = sorted[sorted.len() / 2];
    let mut deviations: Vec<f64> = delays.iter().map(|d| (d - median).abs()).collect();
    deviations.sort_by(f64::total_cmp);
    (median, deviations[deviations.len() / 2], mean, sd)
}

/// The 95 % score interval's lower end for a Poisson count.
fn poisson_lower(k: u64) -> f64 {
    let k = k as f64;
    (k + Z95 * Z95 / 2.0 - Z95 * (k + Z95 * Z95 / 4.0).sqrt()).max(0.0)
}

struct Replay {
    mistakes: u64,
    points: u64,
    /// Theorem 7's bound summed over the freshness points, each at the configuration then in
    /// force: the mistakes the bound allows.
    allowed: f64,
    configurations: u64,
}

/// Runs the estimator as the detector over `arrivals`, as a sans-io driver runs it: every deadline
/// before the next arrival is polled, then the arrival is fed, and the detector is reconfigured
/// whenever its estimates have renewed. The sender never fails, so every suspicion is a mistake.
fn replay(
    arrivals: &[u64],
    interval: Duration,
    granularity: Duration,
    costs: &Costs,
    floors: &Floors,
) -> (Replay, LinkEstimator) {
    let mut link =
        LinkEstimator::new(interval, granularity, Some(Schedule { seq: 0, at_ns: 0 })).unwrap();
    let mut run = Replay {
        mistakes: 0,
        points: 0,
        allowed: 0.0,
        configurations: 0,
    };
    let mut beta = None;
    for (seq, &arrival) in arrivals.iter().enumerate() {
        while let Some(deadline) = link.deadline().filter(|d| *d <= arrival) {
            if link.poll(deadline) == Some(Event::Suspected) {
                run.mistakes += 1;
            }
        }
        link.on_heartbeat(seq as u64, arrival).unwrap();
        if let Some(beta) = beta {
            run.points += 1;
            run.allowed += beta;
        }
        if link.reconfigure_due() {
            match link.configure(costs, floors) {
                Ok(configured) => {
                    let current = configured.current;
                    beta = Some(
                        current.interval.as_secs_f64() / current.mistake_recurrence.as_secs_f64(),
                    );
                    run.configurations += 1;
                }
                Err(Refusal::TooFewHeartbeats | Refusal::CorrelationUnmeasured) => {}
                Err(Refusal::Unconfigurable) => panic!("unconfigurable at {seq}"),
            }
        }
    }
    (run, link)
}

struct Case {
    name: &'static str,
    shape: Shape,
    /// The recorded run's interval, microseconds, and its heartbeats: the shape is checked on a
    /// run as long.
    recorded_us: f64,
    recorded: usize,
    /// The recorded run's `sd / (1.4826·MAD)`, and its mean over its median.
    heavy: f64,
    skew: f64,
    /// The detector's interval: the model's correlation time, by §2.6's rule the spacing on the
    /// 1-2-5 grid past its longest stall, since a stall delays every heartbeat it covers.
    interval: Duration,
    granularity: Duration,
    sender: Duration,
    election: Duration,
}

fn check(case: &Case) {
    // The model has the recorded run's shape at the recorded interval.
    let recorded = trace(
        case.shape,
        case.recorded_us,
        case.recorded,
        0x9E37_79B9_7F4A_7C15,
    );
    let (median, mad, mean, sd) = summary(&recorded, case.recorded_us);
    let heavy = sd / (1.482_602_218_505_602 * mad);
    println!(
        "{}: at the recorded interval median {median:.1} µs, MAD {mad:.1}, mean {mean:.1}, sd {sd:.1}, sd/(1.4826·MAD) {heavy:.1} (recorded {}), mean/median {:.2} (recorded {})",
        case.name,
        case.heavy,
        mean / median,
        case.skew
    );
    assert!(
        heavy > case.heavy / 2.0 && heavy < case.heavy * 2.0,
        "tail weight"
    );
    assert!(
        mean / median > 1.0 + (case.skew - 1.0) / 2.0 && mean / median < case.skew * 1.5,
        "skew"
    );
    // The detector at its interval, over four simulated hours, for nodes failing every hour (tight
    // margins, so mistakes happen) and every month.
    let interval_us = case.interval.as_secs_f64() * 1e6;
    let count = (4.0 * 3_600.0 * 1e6 / interval_us) as usize;
    let arrivals = trace(case.shape, interval_us, count, 0x2545_F491_4F6C_DD1D);
    let floors = Floors {
        granularity: case.granularity,
        sender: case.sender,
        correlation: case.interval,
    };
    for mtbf in [3_600u64, 30 * 86_400] {
        let costs = Costs {
            election: case.election,
            mtbf: Duration::from_secs(mtbf),
        };
        let (run, link) = replay(&arrivals, case.interval, case.granularity, &costs, &floors);
        let estimates = link.estimates();
        println!(
            "{} at {:?}, MTBF {mtbf} s: {} mistakes in {} freshness points, Theorem 7 allows {:.1}; {} configurations, window {:?}, τ_int {:?}, unseen {:?}, deviation {:?}",
            case.name,
            case.interval,
            run.mistakes,
            run.points,
            run.allowed,
            run.configurations,
            estimates.window,
            estimates.correlation,
            estimates.unseen,
            estimates.delay_deviation
        );
        assert!(run.configurations > 0 && run.points > count as u64 / 2);
        assert!(
            poisson_lower(run.mistakes) <= run.allowed,
            "{}: {} mistakes against a bound of {:.2}",
            case.name,
            run.mistakes,
            run.allowed
        );
    }
}

#[test]
fn the_detector_keeps_theorem_7s_bound_on_the_macos_shape() {
    check(&Case {
        name: "macOS 100 µs",
        shape: MACOS,
        recorded_us: 100.0,
        recorded: 3_000_000,
        heavy: 24.0,
        skew: 95.7 / 51.7,
        interval: Duration::from_millis(50),
        granularity: Duration::from_nanos(45_400),
        sender: Duration::from_nanos(45_400),
        election: Duration::from_nanos(378_200),
    });
}

#[test]
fn the_detector_keeps_theorem_7s_bound_on_the_flush_shape() {
    check(&Case {
        name: "macOS F_FULLFSYNC 10 ms",
        shape: FLUSH,
        recorded_us: 10_000.0,
        recorded: 60_000,
        heavy: 13.0,
        skew: 7_002.5 / 5_889.7,
        // The recorded run's correlation time was 200 ms: its stalls were a flushing sender's
        // backlog, which drained while it kept sending. The model's stalls block the sender for up
        // to 250 ms, so its correlation time is 500 ms; at 200 ms two heartbeats in a margin are
        // late together, and the replay breaks the bound (19 mistakes against 11.5 at an hour's
        // MTBF), as the independence rule says it must.
        interval: Duration::from_millis(500),
        granularity: Duration::from_nanos(1_627_400),
        sender: Duration::from_nanos(4_535_800 + 1_627_400),
        election: Duration::from_nanos(5_670_000),
    });
}

/// A sender that stops is suspected within the detection bound the configurator promised.
#[test]
fn a_stopped_sender_is_suspected_within_the_detection_bound() {
    let interval = Duration::from_millis(50);
    let granularity = Duration::from_nanos(45_400);
    let arrivals = trace(MACOS, 50_000.0, 20_000, 7);
    let floors = Floors {
        granularity,
        sender: granularity,
        correlation: interval,
    };
    let costs = Costs {
        election: Duration::from_nanos(378_200),
        mtbf: Duration::from_secs(30 * 86_400),
    };
    let (_, mut link) = replay(&arrivals, interval, granularity, &costs, &floors);
    let configured = link.configure(&costs, &floors).unwrap();
    // The sender stops right after its last heartbeat was scheduled: no heartbeat after.
    let last_scheduled = (arrivals.len() as u64 - 1) * 50_000_000;
    let Some(deadline) = link.deadline() else {
        // The last heartbeat came late: the sender is already suspected.
        assert_eq!(link.trust(), hyper_timing::Trust::Suspected);
        return;
    };
    assert_eq!(link.poll(deadline), Some(Event::Suspected));
    let detected = Duration::from_nanos(deadline - last_scheduled);
    assert!(
        detected <= configured.current.detection + Duration::from_nanos(1),
        "detected after {detected:?}, bound {:?}",
        configured.current.detection
    );
}
