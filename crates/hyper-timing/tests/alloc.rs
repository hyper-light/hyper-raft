//! The allocation law for the detector's estimator (`CLAUDE.md` §1a): once built, a heartbeat, a
//! poll, a read of its estimates and a configuration allocate nothing, and neither do the folds.
#![allow(
    clippy::unwrap_used,
    clippy::disallowed_macros,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    missing_docs
)]

use std::time::Duration;

use hyper_measure::alloc;
use hyper_timing::{Costs, Exposure, Floors, Flushes, Lateness, LinkEstimator, Schedule};

#[global_allocator]
static ALLOCATOR: alloc::Counting = alloc::Counting;

const MS: u64 = 1_000_000;

#[test]
fn a_heartbeat_a_poll_and_a_configuration_allocate_nothing() {
    assert!(alloc::installed());
    let interval = Duration::from_millis(50);
    let granularity = Duration::from_micros(1_000);
    let floors = Floors {
        granularity,
        sender: granularity,
        correlation: interval,
    };
    let costs = Costs {
        election: Duration::from_micros(400),
        mtbf: Duration::from_secs(3_600),
    };
    let mut link =
        LinkEstimator::new(interval, granularity, Some(Schedule { seq: 0, at_ns: 0 })).unwrap();
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let mut delay = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        // A millisecond of jitter, and one heartbeat in a thousand stalled 40 ms.
        MS + state % MS
            + if state.is_multiple_of(1_000) {
                40 * MS
            } else {
                0
            }
    };
    let mut seq = 0u64;
    // Fill the window to the drift bound before counting.
    while seq < 4_000 {
        link.on_heartbeat(seq, seq * 50 * MS + delay()).unwrap();
        seq += 1;
    }
    link.configure(&costs, &floors).unwrap();
    let mut late = Lateness::new();
    let mut flushes = Flushes::new();
    let mut fleet = Exposure::new();
    alloc::begin();
    let mut configurations = 0;
    for _ in 0..100_000 {
        let arrival = seq * 50 * MS + delay();
        if let Some(deadline) = link.deadline().filter(|d| *d <= arrival) {
            link.poll(deadline);
        }
        link.on_heartbeat(seq, arrival).unwrap();
        std::hint::black_box(link.estimates());
        if link.reconfigure_due() {
            link.configure(&costs, &floors).unwrap();
            configurations += 1;
        }
        late.on_wait(arrival, arrival + 1_000).unwrap();
        flushes.on_flush(arrival, arrival + 4 * MS).unwrap();
        fleet.on_exposure(interval);
        seq += 1;
    }
    let counts = alloc::end();
    assert!(configurations > 100, "{configurations} configurations");
    assert_eq!(
        (counts.allocations, counts.reallocations),
        (0, 0),
        "{counts:?}"
    );
}
