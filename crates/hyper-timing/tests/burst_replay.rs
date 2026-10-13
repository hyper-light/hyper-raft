//! MEASUREMENT ONLY (branch burst-evidence-arms, never landed): the heartbeats four links of the
//! windows-2025 processes test took in round 6 of diag run 38317208076 (`tests/data/round6-*`),
//! replayed through the estimator one stretch at a time, each stretch a run of the sender's schedule
//! at one interval. For each: the latenesses, the first heartbeat at which the current rule asks a
//! longer interval for the link's evidence (`independent_interval`), and the first at which the
//! arrivals are measured (`arrivals`), where a link that waits at its interval would configure.
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
    clippy::cast_possible_wrap
)]

use std::time::Duration;

use hyper_timing::{LinkEstimator, Schedule};

/// The receiver's timer granularity on the Windows runners: a timed wait ends on the next 15.625 ms
/// clock interrupt (Microsoft, `timeBeginPeriod`). It sizes the window, not the Allan levels.
const G: Duration = Duration::from_micros(15_625);

fn series(name: &str) -> Vec<(u64, u64, u64)> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name);
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .filter(|l| !l.starts_with('#'))
        .map(|l| {
            let f: Vec<u64> = l.split(' ').map(|x| x.parse().unwrap()).collect();
            (f[0], f[1], f[2])
        })
        .collect()
}

#[test]
#[ignore = "measurement"]
fn measure_the_recorded_bursts() {
    for name in [
        "round6-a_stoppe-3from2.txt",
        "round6-a_stoppe-1from2.txt",
        "round6-a_stalle-2from4.txt",
        "round6-a_node_k-2from1.txt",
    ] {
        let beats = series(name);
        println!("REPLAY {name}: {} heartbeats", beats.len());
        // Stretches of one interval: each heartbeat's due against its predecessor's, per sequence step.
        let mut start = 0;
        while start + 1 < beats.len() {
            let step = |i: usize| {
                let (s0, _, d0) = beats[i - 1];
                let (s1, _, d1) = beats[i];
                (d1 - d0) / (s1 - s0)
            };
            let interval = step(start + 1);
            let mut end = start + 1;
            while end + 1 < beats.len() && step(end + 1) == interval {
                end += 1;
            }
            let stretch = &beats[start..=end];
            let (seq0, _, due0) = stretch[0];
            let mut link = LinkEstimator::new(
                Duration::from_nanos(interval),
                G,
                Some(Schedule {
                    seq: seq0,
                    at_ns: due0,
                }),
            )
            .unwrap();
            let (mut asked, mut measured) = (None, None);
            let mut late = Vec::new();
            for (i, &(seq, arrival, due)) in stretch.iter().enumerate() {
                late.push((arrival as f64 - due as f64) / 1e6);
                let _ = link.on_heartbeat(seq, arrival);
                if asked.is_none()
                    && let Some(next) = link.independent_interval()
                {
                    asked = Some((i + 1, next.as_millis()));
                }
                if measured.is_none() && link.arrivals().is_ok() {
                    measured = Some((i + 1, link.estimates().correlation.unwrap_or(0.0)));
                }
            }
            let most = late.iter().copied().fold(f64::MIN, f64::max);
            let mut sorted = late.clone();
            sorted.sort_by(f64::total_cmp);
            let median = sorted[sorted.len() / 2];
            println!(
                "  stretch at {} ms, {} heartbeats: lateness median {median:.1} ms, most {most:.1} ms; \
                 the current rule asks {} ; measured (a waiting link configures) {}",
                interval / 1_000_000,
                stretch.len(),
                asked.map_or("nothing".into(), |(i, ms)| format!(
                    "{ms} ms at heartbeat {i}"
                )),
                measured.map_or("not within the stretch".into(), |(i, tau)| format!(
                    "at heartbeat {i}, tau_int {tau:.2}"
                )),
            );
            start = end;
        }
    }
}

/// As the live pair runs it: one estimator for the whole series, retimed at each change of the
/// sender's interval with no schedule (the next heartbeat anchors it, as `hyper-liveness`'s pair
/// does), so what a retime carries over (the loss, the prediction errors' and latenesses' moments)
/// is carried here too.
#[test]
#[ignore = "measurement"]
fn measure_the_recorded_bursts_as_the_live_pair_runs() {
    for name in [
        "round6-a_stoppe-3from2.txt",
        "round6-a_stoppe-1from2.txt",
        "round6-a_stalle-2from4.txt",
        "round6-a_node_k-2from1.txt",
    ] {
        let beats = series(name);
        println!("LIVE {name}");
        let step = |i: usize| {
            let (s0, _, d0) = beats[i - 1];
            let (s1, _, d1) = beats[i];
            (d1 - d0) / (s1 - s0)
        };
        let mut interval = step(1);
        let mut link = LinkEstimator::new(Duration::from_nanos(interval), G, None).unwrap();
        let (mut since, mut asked_here) = (0usize, false);
        for i in 0..beats.len() {
            if i > 0 && step(i) != interval {
                interval = step(i);
                link.retime(Duration::from_nanos(interval), None).unwrap();
                since = 0;
                asked_here = false;
            }
            let (seq, arrival, _) = beats[i];
            let _ = link.on_heartbeat(seq, arrival);
            since += 1;
            if !asked_here && let Some(next) = link.independent_interval() {
                println!(
                    "  at {} ms, heartbeat {since} of the stretch ({} in all): asks {} ms",
                    interval / 1_000_000,
                    i + 1,
                    next.as_millis()
                );
                asked_here = true;
            }
            if link.arrivals().is_ok() {
                println!(
                    "  at {} ms, heartbeat {since} of the stretch ({} in all): measured, tau_int {:.2}",
                    interval / 1_000_000,
                    i + 1,
                    link.estimates().correlation.unwrap_or(0.0)
                );
                break;
            }
        }
    }
}
