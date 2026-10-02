//! What a trace says, with the uncertainty of each estimate (`docs/timing.md` §3).
//!
//! Per run: the delay a detector sees, `D_i = A_i − σ_i` from the sender's schedule (Chen, Toueg
//! and Aguilera's NFD-E measures exactly this, `A_i − iη`), with `A_i` the kernel's receive
//! timestamp and, beside it, the time the process read the datagram; the parts of `D` (the sender's
//! timer lateness, its write and flush, the network from send to the kernel, the kernel-to-process
//! gap); loss; the receiver's own timed waits; the autocorrelation of `D`; its Allan deviation over
//! windows of growing length; and the NFD-E detector replayed over the trace.
//!
//! Uncertainty: a mean's standard error is `sd · √(τ_int / n)` with `τ_int` the integrated
//! autocorrelation time, summed over Madras and Sokal's self-consistent window `M ≥ 6 τ_int(M)`
//! (Madras and Sokal, J. Stat. Phys. 50, 1988; Sokal, *Monte Carlo Methods in Statistical
//! Mechanics*, 1997, §3); a quantile's 95 % interval is the order statistics `n·q ± 1.96·√(n·q(1−q))`
//! ranks, widened by `√τ_int` for the correlation.

use std::fmt::Write as _;
use std::io;
use std::path::Path;
use std::time::Duration;

use hyper_timing::{
    Costs, Event, Floors, LinkBehaviour, LinkEstimator, Refusal, Schedule, Window, configure,
    election_span, mistake_bound,
};

use crate::{HEARTBEAT_BYTES, WAIT_BYTES};

/// The normal distribution's two-sided 95 % point.
const Z95: f64 = 1.959_963_984_540_054;
/// The lags the startup table reaches: 4,096 heartbeats.
const MAX_LAG: usize = 4_096;
/// The tail-dependence search reaches lags up to a sixteenth of the run, so each lag still has
/// fifteen sixteenths of the pairs.
const LAG_SHARE: usize = 16;
/// Madras and Sokal's window constant: sum the autocorrelation to the first `M ≥ 6 τ_int(M)`
/// (Sokal 1997, §3: `c ≈ 6` for a correlation that decays as an exponential).
const SOKAL_C: f64 = 6.0;
/// The normal-consistency factor of the median absolute deviation, `1 / Φ⁻¹(3/4)`: `1.4826 · MAD`
/// estimates `σ` for normal data (Rousseeuw and Croux, JASA 88, 1993).
const MAD_NORMAL: f64 = 1.482_602_218_505_602;

struct Meta {
    os: String,
    interval_ns: u64,
    count: u64,
    flush: bool,
    load_start: String,
    load_end: String,
    seconds: f64,
}

fn meta(dir: &Path) -> io::Result<Meta> {
    let text = std::fs::read_to_string(dir.join("meta.txt"))?;
    let get = |key: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(key).map(|v| v.trim().to_string()))
            .unwrap_or_default()
    };
    Ok(Meta {
        os: get("os "),
        interval_ns: get("interval_ns ").parse().unwrap_or(1),
        count: get("count ").parse().unwrap_or(0),
        flush: get("flush ") == "true",
        load_start: get("load_start "),
        load_end: get("load_end "),
        seconds: get("seconds ").parse().unwrap_or(0.0),
    })
}

fn words(bytes: &[u8], record: usize) -> impl Iterator<Item = Vec<u64>> + '_ {
    bytes.chunks_exact(record).map(|r| {
        r.as_chunks::<8>()
            .0
            .iter()
            .map(|w| u64::from_le_bytes(*w))
            .collect()
    })
}

/// One heartbeat as received.
#[derive(Clone, Copy)]
struct Beat {
    seq: u64,
    /// The sender's schedule `σ_i`, monotonic ns.
    sched: u64,
    /// `D` by the kernel's stamp, ns.
    kernel_delay: f64,
    /// `D` by the process's read, ns.
    read_delay: f64,
    /// The sender's timer lateness, ns, when it waited.
    sender_late: Option<f64>,
    /// The sender's write and flush (or nothing), ns.
    flush: f64,
    /// Send to kernel receipt, ns.
    network: f64,
    /// Kernel receipt to the process's read, ns.
    gap: f64,
}

fn beats(dir: &Path, linux: bool) -> io::Result<Vec<Beat>> {
    let bytes = std::fs::read(dir.join("hb.bin"))?;
    Ok(words(&bytes, HEARTBEAT_BYTES)
        .map(|w| {
            let (seq, sched, began, woke, sent, sent_real, kernel, read, read_real) =
                (w[0], w[1], w[2], w[3], w[4], w[5], w[6], w[7], w[8]);
            let d = |a: u64, b: u64| a as f64 - b as f64;
            // Linux stamps on CLOCK_REALTIME: the network and the gap are realtime differences,
            // and the kernel's delay from the schedule adds the sender's monotonic part.
            let (network, gap) = if linux {
                (d(kernel, sent_real), d(read_real, kernel))
            } else {
                (d(kernel, sent), d(read, kernel))
            };
            Beat {
                seq,
                sched,
                kernel_delay: d(sent, sched) + network,
                read_delay: d(read, sched),
                sender_late: (began < sched).then(|| d(woke, sched)),
                flush: d(sent, woke),
                network,
                gap,
            }
        })
        .collect())
}

/// The receiver's timed waits that ended on their timeout: (asked, late), ns.
fn waits(dir: &Path) -> io::Result<Vec<(f64, f64)>> {
    let bytes = std::fs::read(dir.join("wait.bin"))?;
    Ok(words(&bytes, WAIT_BYTES)
        .filter(|w| w[0] < w[1])
        .map(|w| ((w[1] - w[0]) as f64, w[2] as f64 - w[1] as f64))
        .collect())
}

/// A sample's summary with its uncertainty.
struct Stats {
    n: usize,
    mean: f64,
    sd: f64,
    median: f64,
    mad: f64,
    p99: f64,
    p999: f64,
    p9999: f64,
    max: f64,
    tau: f64,
    se_mean: f64,
    median_ci: (f64, f64),
    sd_ci: (f64, f64),
    mad_ci: (f64, f64),
}

fn quantile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let at = ((sorted.len() - 1) as f64 * q).round() as usize;
    sorted[at.min(sorted.len() - 1)]
}

/// The order statistics around quantile `q` of `sorted` that hold it with 95 % confidence, for a
/// sample whose effective size is `n / tau`.
fn quantile_ci(sorted: &[f64], q: f64, tau: f64) -> (f64, f64) {
    let n = sorted.len() as f64;
    let half = Z95 * (n * q * (1.0 - q) * tau.max(1.0)).sqrt();
    let at = |rank: f64| sorted[(rank.clamp(0.0, n - 1.0)) as usize];
    (at(n * q - half), at(n * q + half))
}

/// In-place radix-2 FFT of `(re, im)`, whose length is a power of two (Cooley and Tukey, Math.
/// Comp. 19, 1965); `inverse` conjugates the twiddles and leaves the result unscaled.
fn fft(re: &mut [f64], im: &mut [f64], inverse: bool) {
    let n = re.len();
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let angle = 2.0 * std::f64::consts::PI / len as f64 * if inverse { 1.0 } else { -1.0 };
        let (wr, wi) = (angle.cos(), angle.sin());
        for start in (0..n).step_by(len) {
            let (mut cr, mut ci) = (1.0f64, 0.0f64);
            for k in 0..len / 2 {
                let (a, b) = (start + k, start + k + len / 2);
                let (tr, ti) = (re[b] * cr - im[b] * ci, re[b] * ci + im[b] * cr);
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
                let next = cr * wr - ci * wi;
                ci = cr * wi + ci * wr;
                cr = next;
            }
        }
        len <<= 1;
    }
}

/// The autocorrelation of `x` at lags `0..=max_lag`, by the Wiener–Khinchin route: the inverse
/// transform of the zero-padded series' power spectrum.
fn autocorrelation(x: &[f64], max_lag: usize) -> Vec<f64> {
    let n = x.len();
    if n < 2 {
        return vec![1.0];
    }
    let mean = x.iter().sum::<f64>() / n as f64;
    let size = (2 * n).next_power_of_two();
    let mut re = vec![0.0; size];
    let mut im = vec![0.0; size];
    for (slot, v) in re.iter_mut().zip(x) {
        *slot = v - mean;
    }
    fft(&mut re, &mut im, false);
    for (r, i) in re.iter_mut().zip(im.iter_mut()) {
        *r = *r * *r + *i * *i;
        *i = 0.0;
    }
    fft(&mut re, &mut im, true);
    let c0 = re[0];
    (0..=max_lag.min(n - 1))
        .map(|k| if c0 > 0.0 { re[k] / c0 } else { 0.0 })
        .collect()
}

/// The integrated autocorrelation time over Madras and Sokal's self-consistent window.
fn tau_int(rho: &[f64]) -> f64 {
    let mut tau = 1.0;
    for (m, r) in rho.iter().enumerate().skip(1) {
        tau += 2.0 * r;
        if m as f64 >= SOKAL_C * tau {
            break;
        }
    }
    tau.max(1.0)
}

fn ranks(x: &[f64]) -> Vec<f64> {
    let mut order: Vec<usize> = (0..x.len()).collect();
    order.sort_by(|a, b| x[*a].total_cmp(&x[*b]));
    let mut r = vec![0.0; x.len()];
    for (rank, i) in order.into_iter().enumerate() {
        r[i] = rank as f64;
    }
    r
}

fn stats(x: &[f64], max_lag: usize) -> Stats {
    let n = x.len();
    let mean = x.iter().sum::<f64>() / n as f64;
    let var = x.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (n as f64 - 1.0).max(1.0);
    let m4 = x.iter().map(|v| (v - mean).powi(4)).sum::<f64>() / n as f64;
    let rho = autocorrelation(x, if max_lag == 0 { 0 } else { x.len() / 2 });
    let tau = tau_int(&rho);
    let mut sorted = x.to_vec();
    sorted.sort_by(f64::total_cmp);
    let median = quantile(&sorted, 0.5);
    let mut dev: Vec<f64> = x.iter().map(|v| (v - median).abs()).collect();
    dev.sort_by(f64::total_cmp);
    let n_eff = n as f64 / tau;
    // The variance's standard error from the fourth moment, `√((μ₄ − σ⁴) / n_eff)`, carried to
    // the deviation by the delta method.
    let se_var = ((m4 - var * var).max(0.0) / n_eff).sqrt();
    let sd = var.sqrt();
    let sd_ci = (
        (var - Z95 * se_var).max(0.0).sqrt(),
        (var + Z95 * se_var).sqrt(),
    );
    Stats {
        n,
        mean,
        sd,
        median,
        mad: quantile(&dev, 0.5),
        p99: quantile(&sorted, 0.99),
        p999: quantile(&sorted, 0.999),
        p9999: quantile(&sorted, 0.9999),
        max: *sorted.last().unwrap_or(&f64::NAN),
        tau,
        se_mean: sd / n_eff.sqrt(),
        median_ci: quantile_ci(&sorted, 0.5, tau),
        sd_ci,
        mad_ci: quantile_ci(&dev, 0.5, tau),
    }
}

fn us(ns: f64) -> String {
    format!("{:.1}", ns / 1e3)
}

fn row(name: &str, s: &Stats) -> String {
    format!(
        "| {name} | {} | {} ± {} | {} ({}–{}) | {} ({}–{}) | {} ({}–{}) | {} | {} | {} | {} | {:.1} |",
        s.n,
        us(s.mean),
        us(Z95 * s.se_mean),
        us(s.sd),
        us(s.sd_ci.0),
        us(s.sd_ci.1),
        us(s.median),
        us(s.median_ci.0),
        us(s.median_ci.1),
        us(s.mad),
        us(s.mad_ci.0),
        us(s.mad_ci.1),
        us(s.p99),
        us(s.p999),
        us(s.p9999),
        us(s.max),
        s.tau
    )
}

/// The summary table's header (format).
const HEADER: &str = "| series | n | mean ± 95 % | sd (95 %) | median (95 %) | MAD (95 %) | p99 | p99.9 | p99.99 | max | τ_int |\n|---|---|---|---|---|---|---|---|---|---|---|";

/// Non-overlapping Allan deviation of `x` at window lengths `1, 2, 4, …` (Allan, Proc. IEEE 54(2),
/// 1966; IEEE Std 1139): `σ_A(m)² = ½ E[(ȳ_{k+1} − ȳ_k)²]` over consecutive window means `ȳ`. For
/// uncorrelated, stationary samples it falls as `σ/√m`; where it stops falling, drift outweighs
/// averaging.
fn allan(x: &[f64], centre: impl Fn(&mut [f64]) -> f64) -> Vec<(usize, f64, usize)> {
    let mut out = Vec::new();
    let mut m = 1usize;
    let mut scratch = Vec::new();
    while x.len() / m >= 16 {
        let means: Vec<f64> = x
            .chunks_exact(m)
            .map(|c| {
                scratch.clear();
                scratch.extend_from_slice(c);
                centre(&mut scratch)
            })
            .collect();
        let sum: f64 = means.windows(2).map(|w| (w[1] - w[0]).powi(2)).sum();
        let pairs = means.len() - 1;
        out.push((m, (0.5 * sum / pairs as f64).sqrt(), means.len()));
        m *= 2;
    }
    out
}

fn mean_of(c: &mut [f64]) -> f64 {
    c.iter().sum::<f64>() / c.len() as f64
}

fn median_of(c: &mut [f64]) -> f64 {
    c.sort_by(f64::total_cmp);
    c[c.len() / 2]
}

/// The window that minimizes the Allan deviation: the averaging length past which the estimate
/// no longer improves. The deviation at a window of `K` means is itself uncertain by about
/// `1/√(2(K−1))` relative (Allan 1966; IEEE Std 1139 Annex), so the minimum is the shortest window
/// whose deviation is within that of the least.
fn allan_minimum(curve: &[(usize, f64, usize)]) -> (usize, f64) {
    let Some(&(_, least, k)) = curve.iter().min_by(|a, b| a.1.total_cmp(&b.1)) else {
        return (1, f64::NAN);
    };
    let tolerance = least * (1.0 + 1.0 / (2.0 * (k as f64 - 1.0)).sqrt());
    curve
        .iter()
        .find(|(_, dev, _)| *dev <= tolerance)
        .map_or((1, least), |(m, dev, _)| (*m, *dev))
}

/// The first lag at which the autocorrelation falls inside Bartlett's 95 % band for white noise,
/// `±1.96/√n` (Box, Jenkins, Reinsel, *Time Series Analysis*, §2.1.6), and stays there for the
/// next as many lags again.
fn decorrelation_lag(rho: &[f64], n: usize) -> Option<usize> {
    let band = Z95 / (n as f64).sqrt();
    (1..rho.len()).find(|&k| {
        let end = (2 * k).min(rho.len());
        rho[k..end].iter().all(|r| r.abs() < band)
    })
}

/// The trace by sequence number: each heartbeat's `D` by the kernel's stamp, NaN when lost, and
/// the schedule's origin `σ_0`.
struct Table {
    delay: Vec<f64>,
    origin: f64,
}

fn table(beats: &[Beat], count: u64, interval: u64) -> Table {
    let mut delay = vec![f64::NAN; count as usize];
    let mut origin = 0.0;
    for b in beats {
        if let Some(slot) = delay.get_mut(b.seq as usize) {
            *slot = b.kernel_delay;
        }
        origin = b.sched as f64 - (b.seq * interval) as f64;
    }
    Table { delay, origin }
}

/// Replays NFD-E at interval `stride · η` over every phase of the trace (the heartbeats `phase`
/// modulo `stride`, for each phase): freshness `τ_i = EA_i + α` with `EA_i = σ_i + centre(D)` over
/// the latest `window` received before heartbeat `i`, and the detector's trust per Chen et al.: at
/// `t ∈ [τ_i, τ_{i+1})` it trusts iff some `m_j`, `j ≥ i`, has arrived. Returns (mistakes, the
/// time suspected, the time replayed, the freshness points evaluated), ns.
fn replay(
    trace: &Table,
    interval: u64,
    stride: u64,
    window: usize,
    alpha: f64,
    robust: bool,
) -> (u64, f64, f64, u64) {
    let mut total = (0u64, 0.0f64, 0.0f64, 0u64);
    for phase in 0..stride {
        let (m, s, t, p) = replay_phase(trace, interval, stride, phase, window, alpha, robust);
        total = (total.0 + m, total.1 + s, total.2 + t, total.3 + p);
    }
    total
}

fn replay_phase(
    trace: &Table,
    interval: u64,
    stride: u64,
    phase: u64,
    window: usize,
    alpha: f64,
    robust: bool,
) -> (u64, f64, f64, u64) {
    let eta = (interval * stride) as f64;
    let delay: Vec<f64> = trace
        .delay
        .iter()
        .skip(phase as usize)
        .step_by(stride as usize)
        .copied()
        .collect();
    let span = delay.len();
    let sigma = |i: usize| trace.origin + phase as f64 * interval as f64 + i as f64 * eta;
    let arrival = |i: usize| sigma(i) + delay[i];
    let mut recent: std::collections::VecDeque<f64> = std::collections::VecDeque::new();
    let mut sorted: Vec<f64> = Vec::with_capacity(window + 1);
    let mut sum = 0.0;
    let (mut mistakes, mut suspected, mut points) = (0u64, 0.0f64, 0u64);
    let mut ea_offset: Option<f64> = None;
    for i in 0..span {
        if let Some(offset) = ea_offset {
            let tau = sigma(i) + offset + alpha;
            let next_tau = tau + eta;
            // The first arrival among m_j, j ≥ i, that can land before τ_{i+1}: a heartbeat
            // scheduled after it cannot.
            let mut first_arrival = f64::INFINITY;
            let mut j = i;
            while j < span && sigma(j) < next_tau {
                let a = arrival(j);
                if a < first_arrival {
                    first_arrival = a;
                }
                j += 1;
            }
            let suspected_at_tau = first_arrival > tau;
            // An S-transition happens only at a freshness point: trusted just before τ_i (some
            // m_j, j ≥ i−1, had arrived) and suspected at it.
            let trusted_before = i > 0 && arrival(i - 1) <= tau;
            if suspected_at_tau && trusted_before {
                mistakes += 1;
            }
            if suspected_at_tau {
                suspected += first_arrival.min(next_tau) - tau;
            }
            points += 1;
        }
        let d = delay[i];
        if !d.is_nan() {
            recent.push_back(d);
            sum += d;
            let at = sorted.partition_point(|v| *v < d);
            sorted.insert(at, d);
            if recent.len() > window {
                let old = recent.pop_front().unwrap_or(0.0);
                sum -= old;
                let at = sorted.partition_point(|v| *v < old);
                sorted.remove(at);
            }
            ea_offset = Some(if robust {
                sorted[sorted.len() / 2]
            } else {
                sum / recent.len() as f64
            });
        }
    }
    (mistakes, suspected, span as f64 * eta, points)
}

/// The joint exceedance of `threshold` at lags `0..=max_lag` against independence,
/// `#{D_i > x, D_{i+k} > x} / ((n − k) p²)`, by sequence number, and the first lag from which the
/// count stays within a Poisson 95 % band of independence's (`|c − e| ≤ 1.96√e`) for as many lags
/// again.
fn exceedance(beats: &[Beat], threshold: f64, max_lag: usize) -> (Vec<f64>, Option<usize>) {
    let mut positions: Vec<u64> = beats
        .iter()
        .filter(|b| b.kernel_delay > threshold)
        .map(|b| b.seq)
        .collect();
    positions.sort_unstable();
    let n = beats.len() as f64;
    let p = positions.len() as f64 / n;
    let mut counts = vec![0u64; max_lag + 1];
    for (i, a) in positions.iter().enumerate() {
        for b in &positions[i + 1..] {
            let k = (b - a) as usize;
            if k > max_lag {
                break;
            }
            counts[k] += 1;
        }
    }
    let expected = |k: usize| (n - k as f64).max(0.0) * p * p;
    let ratio: Vec<f64> = (0..=max_lag)
        .map(|k| {
            let e = expected(k);
            if e > 0.0 {
                counts[k] as f64 / e
            } else {
                f64::NAN
            }
        })
        .collect();
    let inside = |k: usize| {
        let e = expected(k);
        (counts[k] as f64 - e).abs() <= Z95 * e.sqrt()
    };
    let lag = (1..=max_lag).find(|&k| (k..=(2 * k).min(max_lag)).all(inside));
    (ratio, lag)
}

/// hyper-timing's estimator over the heartbeats `phase` modulo `stride`, at interval `stride · η`,
/// with the receiver's granularity `g` (ns): every received heartbeat fed by its kernel stamp, on
/// the sender's schedule, which the two processes share a clock for. The window, the loss and the
/// estimates are the estimator's, so the analysis and the detector cannot compute them apart.
fn estimator(
    trace: &Table,
    interval: u64,
    stride: u64,
    phase: u64,
    g: f64,
) -> Option<LinkEstimator> {
    let eta = interval * stride;
    let origin = trace.origin + (phase * interval) as f64;
    let mut link = LinkEstimator::new(
        Duration::from_nanos(eta),
        Duration::from_nanos(g.max(1.0) as u64),
        Some(Schedule {
            seq: 0,
            at_ns: origin.max(0.0) as u64,
        }),
    )
    .ok()?;
    for (i, d) in trace
        .delay
        .iter()
        .skip(phase as usize)
        .step_by(stride as usize)
        .enumerate()
    {
        if !d.is_nan() {
            let arrival = origin + i as f64 * eta as f64 + d;
            let _ = link.on_heartbeat(i as u64, arrival.max(0.0) as u64);
        }
    }
    Some(link)
}

/// NFD-E's window at interval `stride · η`, the estimator's: `min(n_G, n_A)` under its drift bound,
/// over the heartbeats one in `stride`.
fn window_parts(trace: &Table, interval: u64, stride: u64, g: f64) -> Window {
    estimator(trace, interval, stride, 0, g).map_or(
        Window {
            length: 1,
            granularity: None,
            allan: None,
            drift: 1,
        },
        |link| link.estimates().window,
    )
}

fn window_for(trace: &Table, interval: u64, stride: u64, g: f64) -> usize {
    window_parts(trace, interval, stride, g).length as usize
}

/// The estimator run as the detector over every phase of the trace at interval `stride · η`, as a
/// sans-io driver runs it: deadlines polled before each arrival, the arrival fed, and the detector
/// configured from its own estimates whenever they have renewed. The sender never failed, so every
/// suspicion is a mistake. Returns (mistakes, freshness points, the mistakes Theorem 7 allows summed
/// over the points at the configuration then in force, configurations).
fn online(
    trace: &Table,
    interval: u64,
    stride: u64,
    g: f64,
    costs: &Costs,
    floors: &Floors,
) -> (u64, u64, f64, u64) {
    let eta = interval * stride;
    let mut total = (0u64, 0u64, 0.0f64, 0u64);
    for phase in 0..stride {
        let origin = trace.origin + (phase * interval) as f64;
        let Ok(mut link) = LinkEstimator::new(
            Duration::from_nanos(eta),
            Duration::from_nanos(g.max(1.0) as u64),
            Some(Schedule {
                seq: 0,
                at_ns: origin.max(0.0) as u64,
            }),
        ) else {
            continue;
        };
        let mut beta = None;
        for (i, d) in trace
            .delay
            .iter()
            .skip(phase as usize)
            .step_by(stride as usize)
            .enumerate()
        {
            if d.is_nan() {
                continue;
            }
            let arrival = (origin + i as f64 * eta as f64 + d).max(0.0) as u64;
            while let Some(deadline) = link.deadline().filter(|t| *t <= arrival) {
                if link.poll(deadline) == Some(Event::Suspected) {
                    total.0 += 1;
                }
            }
            let _ = link.on_heartbeat(i as u64, arrival);
            if let Some(beta) = beta {
                total.1 += 1;
                total.2 += beta;
            }
            if link.reconfigure_due() {
                match link.configure(costs, floors) {
                    Ok(configured) => {
                        let current = configured.current;
                        beta = Some(
                            current.interval.as_secs_f64()
                                / current.mistake_recurrence.as_secs_f64(),
                        );
                        total.3 += 1;
                    }
                    Err(Refusal::TooFewHeartbeats | Refusal::CorrelationUnmeasured) => {}
                    Err(Refusal::Unconfigurable) => break,
                }
            }
        }
    }
    total
}

/// Ferro and Segers' intervals estimator of the extremal index `θ` of the exceedances of
/// `threshold` (Ferro and Segers, "Inference for clusters of extreme values", JRSS B 65(2), 2003,
/// §3–4): with inter-exceedance times `T_i` (in heartbeats),
/// `θ̂ = min(1, 2(ΣT_i)² / ((N−1)ΣT_i²))` when every `T_i ≤ 2`, else
/// `θ̂ = min(1, 2(Σ(T_i−1))² / ((N−1)Σ(T_i−1)(T_i−2)))`. Exceedances come in clusters of mean size
/// `1/θ`; the `C − 1` largest gaps, `C = ⌊θ̂N⌋`, separate clusters, and the smallest of them is the
/// run length that declusters the series: exceedances further apart belong to different
/// clusters, nearer ones to the same. Returns `(θ̂, run length, N)`.
fn extremal(trace: &Table, threshold: f64) -> (f64, u64, usize) {
    let times: Vec<u64> = trace
        .delay
        .iter()
        .enumerate()
        .filter(|(_, d)| **d > threshold)
        .map(|(i, _)| i as u64)
        .collect();
    let n = times.len();
    if n < 3 {
        return (1.0, 1, n);
    }
    let mut gaps: Vec<u64> = times.windows(2).map(|w| w[1] - w[0]).collect();
    let (sum, sum_sq): (f64, f64) = gaps.iter().fold((0.0, 0.0), |(s, q), &t| {
        (s + t as f64, q + (t as f64) * (t as f64))
    });
    let m = (n - 1) as f64;
    let theta = if gaps.iter().all(|&t| t <= 2) {
        2.0 * sum * sum / (m * sum_sq)
    } else {
        let (a, b): (f64, f64) = gaps.iter().fold((0.0, 0.0), |(a, b), &t| {
            let t = t as f64;
            (a + t - 1.0, b + (t - 1.0) * (t - 2.0))
        });
        if b > 0.0 { 2.0 * a * a / (m * b) } else { 1.0 }
    }
    .min(1.0);
    let clusters = ((theta * n as f64).floor() as usize).max(1);
    gaps.sort_unstable_by(|a, b| b.cmp(a));
    // The C − 1 largest gaps are between clusters; the run length is the smallest of them.
    let run = if clusters >= 2 {
        gaps.get(clusters - 2).copied().unwrap_or(1)
    } else {
        gaps.first().copied().unwrap_or(1) + 1
    };
    (theta, run, n)
}

/// The 95 % score interval of a Poisson count `k`: `k + z²/2 ∓ z√(k + z²/4)`.
fn poisson95(k: u64) -> (f64, f64) {
    let k = k as f64;
    let centre = k + Z95 * Z95 / 2.0;
    let half = Z95 * (k + Z95 * Z95 / 4.0).sqrt();
    ((centre - half).max(0.0), centre + half)
}

fn secs(d: Duration) -> String {
    let s = d.as_secs_f64();
    if s >= 86_400.0 {
        format!("{:.1} d", s / 86_400.0)
    } else if s >= 1.0 {
        format!("{s:.3} s")
    } else {
        format!("{:.1} µs", s * 1e6)
    }
}

pub fn main(dirs: &[String]) -> io::Result<()> {
    for dir in dirs {
        let path = Path::new(dir);
        let meta = meta(path)?;
        let linux = meta.os == "linux";
        let beats = beats(path, linux)?;
        let waits = waits(path)?;
        let mut out = String::new();
        let _ = writeln!(
            out,
            "\n## {dir}\n\n{} {}, η = {} µs, flush {}, {} s, load {} → {}\n",
            meta.os,
            std::env::consts::ARCH,
            meta.interval_ns / 1_000,
            meta.flush,
            meta.seconds,
            meta.load_start,
            meta.load_end
        );
        // Loss and order.
        let received = beats.len() as u64;
        let mut reordered = 0u64;
        let mut last = None;
        for b in &beats {
            if let Some(prev) = last
                && b.seq < prev
            {
                reordered += 1;
            }
            last = Some(b.seq);
        }
        let lost = meta.count.saturating_sub(received);
        // G, the mean lateness of the receiver's own waits, and the estimator over the whole
        // trace at its interval: its loss is the Jeffreys posterior mean `(k + ½)/(m + 1)` over the
        // sequence numbers from the first received to the latest.
        let g = waits.iter().map(|w| w.1).sum::<f64>() / waits.len().max(1) as f64;
        let trace = table(&beats, meta.count, meta.interval_ns);
        let full = estimator(&trace, meta.interval_ns, 1, 0, g);
        let loss = full.as_ref().map_or(0.5, |link| link.estimates().loss);
        let _ = writeln!(
            out,
            "sent {}, received {received}, lost {lost}, reordered {reordered}; p_L (Jeffreys mean) {loss:.3e}\n",
            meta.count
        );
        let _ = writeln!(out, "{HEADER}");
        let kd: Vec<f64> = beats.iter().map(|b| b.kernel_delay).collect();
        let rd: Vec<f64> = beats.iter().map(|b| b.read_delay).collect();
        let net: Vec<f64> = beats.iter().map(|b| b.network).collect();
        let gap: Vec<f64> = beats.iter().map(|b| b.gap).collect();
        let late: Vec<f64> = beats.iter().filter_map(|b| b.sender_late).collect();
        let flush: Vec<f64> = beats.iter().map(|b| b.flush).collect();
        let behind = beats.iter().filter(|b| b.sender_late.is_none()).count();
        let s_kd = stats(&kd, MAX_LAG);
        let s_rd = stats(&rd, MAX_LAG);
        for (name, series) in [
            ("D (kernel stamp − σ)", &s_kd),
            ("D (process read − σ)", &s_rd),
        ] {
            let _ = writeln!(out, "{}", row(name, series));
        }
        let s_net = stats(&net, MAX_LAG);
        let s_gap = stats(&gap, MAX_LAG);
        let s_late = stats(&late, MAX_LAG);
        let s_flush = stats(&flush, MAX_LAG);
        let _ = writeln!(out, "{}", row("send → kernel", &s_net));
        let _ = writeln!(out, "{}", row("kernel → read (gap)", &s_gap));
        let _ = writeln!(out, "{}", row("sender timer lateness", &s_late));
        let _ = writeln!(out, "{}", row("sender write+flush", &s_flush));
        let wl: Vec<f64> = waits.iter().map(|w| w.1).collect();
        let s_wait = stats(&wl, 0);
        let _ = writeln!(out, "{}", row("receiver wait lateness", &s_wait));
        let asked: Vec<f64> = waits.iter().map(|w| w.0).collect();
        let s_asked = stats(&asked, 0);
        let ratio: Vec<f64> = waits.iter().map(|w| w.1 / w.0).collect();
        let s_ratio = stats(&ratio, 0);
        let _ = writeln!(
            out,
            "\nsender behind its schedule (no wait) {behind}; receiver waits asked median {} µs, lateness/asked median {:.3}\n",
            us(s_asked.median),
            s_ratio.median
        );

        // Correlation: of the values, of their ranks, and of the tail exceedances, which is what
        // Theorem 7's product over the heartbeats inside the margin takes as independent.
        let eta = meta.interval_ns as f64;
        let reach = kd.len() / LAG_SHARE;
        let rho = autocorrelation(&kd, reach);
        let rank_rho = autocorrelation(&ranks(&kd), reach);
        let lags: Vec<usize> = (0..12)
            .flat_map(|e| [1usize, 2, 5].map(|f| f * 10usize.pow(e)))
            .take_while(|&k| k <= reach)
            .collect();
        let show = |v: &[f64]| {
            lags.iter()
                .map(|&k| v.get(k).map_or("—".into(), |r| format!("{r:.3}")))
                .collect::<Vec<_>>()
                .join(" | ")
        };
        let _ = writeln!(
            out,
            "\n| lag (heartbeats) | {} |\n|---|{}",
            lags.iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(" | "),
            "---|".repeat(lags.len())
        );
        let _ = writeln!(out, "| ρ of D | {} |", show(&rho));
        let _ = writeln!(out, "| ρ of ranks | {} |", show(&rank_rho));
        let mut sorted_kd = kd.clone();
        sorted_kd.sort_by(f64::total_cmp);
        let mut tail_lag = 1u64;
        for q in [0.99, 0.999] {
            let x = quantile(&sorted_kd, q);
            let (ratio, _) = exceedance(&beats, x, reach);
            let _ = writeln!(
                out,
                "| P(both > p{}) / P(>p{})² | {} |",
                q * 100.0,
                q * 100.0,
                show(&ratio)
            );
            let (theta, run, count) = extremal(&trace, x);
            let _ = writeln!(
                out,
                "\nexceedances of p{} ({} µs): {count}, extremal index θ {theta:.4} (mean cluster {:.1} heartbeats), clusters separated by ≥ {run} heartbeats = {} µs",
                q * 100.0,
                us(x),
                1.0 / theta,
                us(run as f64 * eta)
            );
            tail_lag = tail_lag.max(run);
        }
        let _ = tail_lag;
        let _ = writeln!(
            out,
            "τ_int {:.1} heartbeats = {} µs; Bartlett band ±{:.4}, values inside from lag {:?}, ranks from {:?}\n",
            s_kd.tau,
            us(s_kd.tau * eta),
            Z95 / (kd.len() as f64).sqrt(),
            decorrelation_lag(&rho, kd.len()),
            decorrelation_lag(&rank_rho, kd.len()),
        );

        // Stationarity.
        let a_mean = allan(&kd, mean_of);
        let a_median = allan(&kd, median_of);
        let _ = writeln!(
            out,
            "| window m | windows | Allan dev of mean, µs | white-noise σ/√m, µs | Allan dev of median, µs |\n|---|---|---|---|---|"
        );
        for ((m, dev, k), (_, dmed, _)) in a_mean.iter().zip(&a_median) {
            let _ = writeln!(
                out,
                "| {m} ({} ms) | {k} | {:.2} | {:.2} | {:.2} |",
                *m as f64 * eta / 1e6,
                dev / 1e3,
                s_kd.sd / (*m as f64).sqrt() / 1e3,
                dmed / 1e3
            );
        }
        let (n_allan, dev_allan) = allan_minimum(&a_mean);
        let (n_allan_med, _) = allan_minimum(&a_median);
        let t_stat = n_allan as f64 * eta;
        let n_g = (s_kd.tau * s_kd.sd.powi(2) / (g * g)).ceil().max(1.0);
        let _ = writeln!(
            out,
            "\nAllan minimum (mean): window {n_allan} = {} ms, deviation {:.2} µs; (median): window {n_allan_med}\nG (mean lateness of the receiver's own waits) {} µs; n_G = τ_int·V/G² = {n_g}\n",
            t_stat / 1e6,
            dev_allan / 1e3,
            us(g)
        );

        // Startup: how far the deviation of the first m heartbeats falls short.
        let _ = writeln!(
            out,
            "| m | blocks | median sd_m/sd | p05 sd_m/sd |\n|---|---|---|---|"
        );
        let mut m = 2usize;
        while kd.len() / m >= 16 && m <= MAX_LAG {
            let mut r: Vec<f64> = kd
                .chunks_exact(m)
                .map(|c| {
                    let mu = c.iter().sum::<f64>() / m as f64;
                    (c.iter().map(|v| (v - mu).powi(2)).sum::<f64>() / (m as f64 - 1.0)).sqrt()
                        / s_kd.sd
                })
                .collect();
            r.sort_by(f64::total_cmp);
            let _ = writeln!(
                out,
                "| {m} | {} | {:.3} | {:.3} |",
                r.len(),
                quantile(&r, 0.5),
                quantile(&r, 0.05)
            );
            m *= 4;
        }

        // The correlation time: the least spacing from which Theorem 7's product, which takes the
        // heartbeats inside the margin as independent, is not refuted by the replay at any margin
        // that holds two heartbeats or more.
        let mut sweep_rows = String::new();
        let mut t_c_stride = 1u64;
        let strides: Vec<u64> = (0..12)
            .flat_map(|e| [1u64, 2, 5].map(|f| f * 10u64.pow(e)))
            .take_while(|&k| k <= (kd.len() / LAG_SHARE) as u64)
            .collect();
        for &stride in &strides {
            let spacing = eta * stride as f64;
            let window = window_for(&trace, meta.interval_ns, stride, g);
            let mut refuted = false;
            let mut tests = 0;
            // The margins tested: the trace's own tail quantiles, and margins holding 2, 3, 5, 10
            // and 20 heartbeats, so a dependence the quantiles miss is still exercised.
            let margins: Vec<(String, f64)> = [0.99, 0.999, 0.9999]
                .iter()
                .map(|q| {
                    (
                        format!("p{}", q * 100.0),
                        quantile(&sorted_kd, *q) - s_kd.mean,
                    )
                })
                .chain(
                    [2.0, 3.0, 5.0, 10.0, 20.0]
                        .iter()
                        .map(|k: &f64| (format!("{k} beats"), (k - 0.5) * spacing)),
                )
                .collect();
            for (label, alpha) in margins {
                if alpha < spacing || alpha > sorted_kd.last().copied().unwrap_or(0.0) {
                    continue;
                }
                tests += 1;
                let (mistakes, _, _, points) =
                    replay(&trace, meta.interval_ns, stride, window, alpha, false);
                let bound = mistake_bound(loss, s_kd.sd * s_kd.sd, spacing, alpha);
                let (lower, _) = poisson95(mistakes);
                let no = lower / points.max(1) as f64 > bound;
                refuted |= no;
                let _ = writeln!(
                    sweep_rows,
                    "| {} | {label} {} | {:.0} | {window} | {bound:.2e} | {mistakes} / {points} | {} |",
                    secs(Duration::from_secs_f64(spacing / 1e9)),
                    secs(Duration::from_secs_f64(alpha / 1e9)),
                    (alpha / spacing).floor() + 1.0,
                    if no { "**refuted**" } else { "holds" }
                );
            }
            if refuted {
                t_c_stride = stride.saturating_mul(2).max(
                    strides
                        .iter()
                        .copied()
                        .find(|&k| k > stride)
                        .unwrap_or(stride),
                );
            }
            if tests == 0 {
                break;
            }
        }
        let t_c = t_c_stride as f64 * eta;
        let _ = writeln!(
            out,
            "\n| spacing η | α (quantile) | heartbeats in margin | window | Theorem 7 β (mean/sd) | replayed mistakes / points | verdict |\n|---|---|---|---|---|---|---|\n{sweep_rows}\ncorrelation time (least spacing past every refutation) {} µs\n",
            us(t_c)
        );

        // The floors on η.
        let lindley = if meta.flush { s_flush.mean + g } else { 0.0 };
        let floor_ns = g.max(t_c).max(lindley);
        let floor = Duration::from_secs_f64(floor_ns / 1e9);
        let base_floor = Duration::from_secs_f64(g.max(lindley) / 1e9);
        let resolution = Duration::from_secs_f64(g / 1e9);
        let floors = Floors {
            granularity: resolution,
            sender: base_floor,
            correlation: Duration::from_secs_f64(t_c / 1e9),
        };
        // A vote travels as a heartbeat does from send to read, and its voter persists the vote
        // before answering (Raft §3.4 / Figure 2): one way, and a round of two ways and a flush.
        let deliver = s_net.mean + s_gap.mean;
        let latency = Duration::from_secs_f64(deliver.max(0.0) / 1e9);
        let round = Duration::from_secs_f64((2.0 * deliver + s_flush.mean).max(0.0) / 1e9);
        let _ = writeln!(
            out,
            "\nfloors: G {} µs, correlation time {} µs, flush stability E[flush]+G {} µs → floor {}; election inputs: l = {}, vote round = {}\n",
            us(g),
            us(t_c),
            us(lindley),
            secs(floor),
            secs(latency),
            secs(round)
        );

        // Theorem 7 against the replay: the bound on the probability that a freshness point is a
        // mistake, from each estimate pair, and the rate the replayed detector made.
        let robust_sd = MAD_NORMAL * s_kd.mad;
        let window_at = |stride: u64| window_for(&trace, meta.interval_ns, stride, g);
        let _ = writeln!(
            out,
            "| estimate | η | α (at quantile) | window | Theorem 7 β | replayed mistakes / points | replayed rate (95 %) | bound holds |\n|---|---|---|---|---|---|---|---|"
        );
        for (name, centre, sd, robust) in [
            ("mean/sd", s_kd.mean, s_kd.sd, false),
            ("median/1.4826·MAD", s_kd.median, robust_sd, true),
        ] {
            for stride in [1u64, t_c_stride] {
                for q in [0.9, 0.99, 0.999, 0.9999] {
                    let alpha = quantile(&sorted_kd, q) - centre;
                    if alpha <= 0.0 {
                        continue;
                    }
                    let window = window_at(stride);
                    let (mistakes, _, _, points) =
                        replay(&trace, meta.interval_ns, stride, window, alpha, robust);
                    let bound = mistake_bound(loss, sd * sd, eta * stride as f64, alpha);
                    let rate = mistakes as f64 / points.max(1) as f64;
                    let (lower, upper) = poisson95(mistakes);
                    let (lower, upper) =
                        (lower / points.max(1) as f64, upper / points.max(1) as f64);
                    let _ = writeln!(
                        out,
                        "| {name} | {} | {} (p{}) | {window} | {bound:.2e} | {mistakes} / {points} | {rate:.2e} ({lower:.2e}–{upper:.2e}) | {} |",
                        secs(Duration::from_secs_f64(eta * stride as f64 / 1e9)),
                        secs(Duration::from_secs_f64(alpha / 1e9)),
                        q * 100.0,
                        if lower <= bound { "yes" } else { "**no**" }
                    );
                }
            }
        }

        // The configurator on the measured inputs.
        let _ = writeln!(
            out,
            "\n| voters/up | span W | T_E | split |\n|---|---|---|---|"
        );
        let mut t_e = None;
        for (v, a) in [(3u32, 3u32), (3, 2), (5, 5), (5, 4)] {
            if let Some(span) = election_span(v, a, latency, round, resolution) {
                let _ = writeln!(
                    out,
                    "| {v}/{a} | {} | {} | {:.3} |",
                    secs(span.span),
                    secs(span.election),
                    span.split
                );
                if (v, a) == (3, 2) {
                    t_e = Some(span.election);
                }
            }
        }
        let _ = writeln!(
            out,
            "\n| estimate | MTBF | η | α | window | detection bound | T_MR bound | U | replayed mistakes / points | replayed T_MR | bound holds | suspected share |\n|---|---|---|---|---|---|---|---|---|---|---|---|"
        );
        for (name, mean, sd, robust) in [
            ("mean/sd", s_kd.mean, s_kd.sd, false),
            ("median/1.4826·MAD", s_kd.median, robust_sd, true),
        ] {
            let link = LinkBehaviour {
                loss,
                mean_delay: Duration::from_secs_f64(mean.max(0.0) / 1e9),
                delay_deviation: Duration::from_secs_f64(sd.max(0.0) / 1e9),
            };
            for mtbf in [3_600u64, 86_400, 30 * 86_400, 365 * 86_400] {
                let Some(election) = t_e else { continue };
                let costs = Costs {
                    election,
                    mtbf: Duration::from_secs(mtbf),
                };
                let Some(det) = configure(&link, &costs, &floors) else {
                    continue;
                };
                let stride = ((det.interval.as_nanos() as f64 / eta).round() as u64).max(1);
                let parts = window_parts(&trace, meta.interval_ns, stride, g);
                let window = parts.length;
                let (n_g_at, n_allan_at, drift) = (
                    parts.granularity.map_or("—".into(), |n| n.to_string()),
                    parts.allan.map_or("—".into(), |n| n.to_string()),
                    parts.drift,
                );
                let (mistakes, suspected, replayed, points) = replay(
                    &trace,
                    meta.interval_ns,
                    stride,
                    window as usize,
                    det.margin.as_nanos() as f64,
                    robust,
                );
                let _ = writeln!(
                    out,
                    "| {name} | {} | {} | {} | {window} (n_G {n_g_at}, n_Allan {n_allan_at}, drift {drift}) | {} | {} | {:.3e} | {mistakes} / {points} | {} | {} | {:.2e} |",
                    secs(Duration::from_secs(mtbf)),
                    secs(det.interval),
                    secs(det.margin),
                    secs(det.detection),
                    secs(det.mistake_recurrence),
                    det.unavailability,
                    if mistakes > 0 {
                        secs(Duration::from_secs_f64(replayed / mistakes as f64 / 1e9))
                    } else {
                        format!("> {}", secs(Duration::from_secs_f64(replayed / 1e9)))
                    },
                    {
                        // The bound is on the share of freshness points that start a mistake,
                        // `β = η / E(T_MR)`.
                        let beta = det.interval.as_secs_f64()
                            / det.mistake_recurrence.as_secs_f64().max(f64::MIN_POSITIVE);
                        let (lower, _) = poisson95(mistakes);
                        if lower / points.max(1) as f64 <= beta {
                            "yes"
                        } else {
                            "**no**"
                        }
                    },
                    suspected / replayed.max(1.0)
                );
            }
        }

        // The estimator itself as the detector, online: its own window, estimates and
        // configurations as the heartbeats come, at each interval the configurator chose above.
        let _ = writeln!(
            out,
            "\n| online, MTBF | η | configurations | mistakes / points | Theorem 7 allows (Σβ) | bound holds |\n|---|---|---|---|---|---|"
        );
        let mean_link = LinkBehaviour {
            loss,
            mean_delay: Duration::from_secs_f64(s_kd.mean.max(0.0) / 1e9),
            delay_deviation: Duration::from_secs_f64(s_kd.sd.max(0.0) / 1e9),
        };
        for mtbf in [3_600u64, 86_400, 30 * 86_400, 365 * 86_400] {
            let Some(election) = t_e else { continue };
            let costs = Costs {
                election,
                mtbf: Duration::from_secs(mtbf),
            };
            let Some(det) = configure(&mean_link, &costs, &floors) else {
                continue;
            };
            let stride = ((det.interval.as_nanos() as f64 / eta).round() as u64).max(1);
            let (mistakes, points, allowed, configurations) =
                online(&trace, meta.interval_ns, stride, g, &costs, &floors);
            let (lower, _) = poisson95(mistakes);
            let _ = writeln!(
                out,
                "| {} | {} | {configurations} | {mistakes} / {points} | {allowed:.2} | {} |",
                secs(Duration::from_secs(mtbf)),
                secs(Duration::from_secs_f64(eta * stride as f64 / 1e9)),
                if lower <= allowed { "yes" } else { "**no**" }
            );
        }
        print!("{out}");
    }
    Ok(())
}
