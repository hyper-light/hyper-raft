//! One node pair: the stream this node sends the peer and the detector it runs on the peer's.

use std::time::Duration;

use hyper_timing::{
    Configuration, Costs, Event, ExchangeRtt, Floors, LinkEstimator, Trust, mistake_bound,
};

use crate::bound::Sums;
use crate::codec::{Echo, Heartbeat, MAX_BYTES};
use crate::{Change, Last, PairReport, PeerId, Refusal, Suspicion};

/// `duration` in nanoseconds, saturating at `u64::MAX` (584 years).
fn nanos(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

/// What the node gives every pair's sender at a poll.
pub(crate) struct Sender {
    pub(crate) local_boot: u64,
    pub(crate) floor: Option<Duration>,
    pub(crate) granularity: Option<Duration>,
    pub(crate) durable_count: u64,
    pub(crate) durable_ns: Option<u64>,
}

/// What the node gives a pair's receiver with a heartbeat.
pub(crate) struct Context {
    pub(crate) granularity: Option<Duration>,
    pub(crate) mtbf: Option<Duration>,
}

/// What a poll's send did.
pub(crate) enum Sent {
    /// A heartbeat of this many bytes is in the node's buffer.
    Message(usize),
    /// A heartbeat is due and no flush proves it.
    NeedsFlush,
    /// Nothing due.
    Nothing,
}

/// This node's stream to the peer.
#[derive(Debug, Default)]
struct Stream {
    /// The next heartbeat's number and when it is due; `None` before the first.
    next: Option<(u64, u64)>,
    /// The interval the next heartbeat is due at, after the one before it; zero before the first.
    interval_ns: u64,
    /// The interval the peer asked for.
    asked_ns: u64,
    /// The durable count the latest heartbeat carried: the next must carry more.
    proof: u64,
}

/// The estimator of the peer's stream and the ring of its delay sums, boxed together: the
/// estimator's Allan levels are a kilobyte, and the pair's other fields are read every poll.
#[derive(Debug)]
struct Link {
    estimator: LinkEstimator,
    sums: Sums,
    granularity_ns: u64,
}

/// What this node holds of the peer's stream. The link's history outlives the peer's runs: the
/// delays it measures are the hosts' and the path's (`docs/timing.md` §2.6), so a restarted peer is
/// judged at once by the detector in force, its heartbeats numbered on from the last run's
/// (`base`).
#[derive(Debug, Default)]
struct Received {
    boot: Option<u64>,
    link: Option<Box<Link>>,
    /// What the run's numbers are offset by in the estimator: one past the last run's latest.
    base: u64,
    /// The durable count of the latest heartbeat taken from the run.
    flushes: u64,
    /// The peer's stability floor, as it last said.
    floor_ns: u64,
    last: Option<Last>,
    /// The estimator's number of the latest heartbeat taken.
    last_mapped: Option<u64>,
    /// The next heartbeat begins a new run's schedule: the estimator is anchored anew.
    reanchor: bool,
    /// The latest heartbeat to echo back: its send and lateness on the peer's clock, and its
    /// arrival on this node's.
    echo: Option<(u64, u64, u64)>,
    configuration: Option<Configuration>,
    /// The latest heartbeat number whose freshness point the allowance counts.
    accounted: Option<u64>,
    /// When the detector was last configured: the heartbeats taken then, and `β` at its margin.
    renewed: Option<(u64, f64)>,
    round_trip: ExchangeRtt,
}

/// One pair.
#[derive(Debug)]
pub(crate) struct Pair {
    pub(crate) groups: u32,
    pub(crate) election: Option<Duration>,
    stream: Stream,
    received: Received,
    report: PairReport,
}

impl Pair {
    pub(crate) fn new() -> Self {
        Self {
            groups: 0,
            election: None,
            stream: Stream::default(),
            received: Received::default(),
            report: PairReport::default(),
        }
    }

    pub(crate) fn trust(&self) -> Trust {
        self.received
            .link
            .as_ref()
            .map_or(Trust::Unconfigured, |link| link.estimator.trust())
    }

    pub(crate) fn configuration(&self) -> Option<Configuration> {
        self.received.configuration
    }

    pub(crate) fn round_trip(&self) -> &ExchangeRtt {
        &self.received.round_trip
    }

    pub(crate) fn report(&self) -> PairReport {
        PairReport {
            groups: self.groups,
            configured: self.received.configuration.is_some(),
            ..self.report
        }
    }

    /// When the next heartbeat to the peer is due.
    pub(crate) fn next_due(&self) -> Option<u64> {
        self.stream.next.map(|(_, due)| due)
    }

    /// The peer's freshness point, while trusted.
    pub(crate) fn deadline(&self) -> Option<u64> {
        self.received
            .link
            .as_ref()
            .and_then(|link| link.estimator.deadline())
    }

    /// The interval to send at: the one the peer asked, never below this node's floor; the floor
    /// until the peer asks. A change smaller than `G`, the configurator's resolution, is none.
    fn interval(&self, sender: &Sender) -> Option<u64> {
        let floor = nanos(sender.floor?);
        let wanted = self.stream.asked_ns.max(floor);
        if wanted == 0 {
            return None;
        }
        let current = self.stream.interval_ns;
        let resolution = sender.granularity.map_or(0, nanos);
        Some(if current != 0 && wanted.abs_diff(current) <= resolution {
            current
        } else {
            wanted
        })
    }

    /// Sends the heartbeat due, if a flush proves it: one made durable after the previous
    /// heartbeat was due, and newer than the one the previous heartbeat carried. A sender behind
    /// its schedule sends the latest heartbeat due; the ones it skipped are lost to the peer, which
    /// is what they are.
    pub(crate) fn send(&mut self, sender: &Sender, now_ns: u64, out: &mut [u8; MAX_BYTES]) -> Sent {
        if self.groups == 0 {
            return Sent::Nothing;
        }
        let Some(interval) = self.interval(sender) else {
            // No flush measured yet: the first proves the first heartbeat and gives the floor.
            return Sent::NeedsFlush;
        };
        let (seq, due) = match self.stream.next {
            None => (0, now_ns),
            Some((seq, due)) if due <= now_ns => {
                let step = self.stream.interval_ns.max(1);
                let behind = now_ns.saturating_sub(due).checked_div(step).unwrap_or(0);
                (
                    seq.saturating_add(behind),
                    due.saturating_add(behind.saturating_mul(step)),
                )
            }
            Some(_) => return Sent::Nothing,
        };
        let spacing = if self.stream.next.is_none() {
            interval
        } else {
            self.stream.interval_ns
        };
        let previous_due = due.saturating_sub(spacing);
        let proven = sender.durable_count > self.stream.proof
            && sender
                .durable_ns
                .is_some_and(|at| at > previous_due && at <= now_ns);
        let Some(durable_ns) = sender.durable_ns.filter(|_| proven) else {
            self.stream.next = Some((seq, due));
            if self.stream.interval_ns == 0 {
                self.stream.interval_ns = spacing;
            }
            return Sent::NeedsFlush;
        };
        let echo = self
            .received
            .echo
            .map(|(sent_ns, late_ns, arrival_ns)| Echo {
                sent_ns,
                late_ns,
                hold_ns: now_ns.saturating_sub(arrival_ns),
            });
        let beat = Heartbeat {
            boot: sender.local_boot,
            seq,
            interval_ns: spacing,
            floor_ns: sender.floor.map_or(0, nanos),
            ask_ns: self
                .received
                .configuration
                .map_or(0, |configured| nanos(configured.best.interval)),
            sent_ns: now_ns,
            late_ns: now_ns.saturating_sub(due),
            flushes: sender.durable_count,
            flush_age_ns: now_ns.saturating_sub(durable_ns),
            echo,
        };
        let length = beat.encode(out).len();
        self.stream.proof = sender.durable_count;
        self.stream.interval_ns = interval;
        self.stream.next = Some((seq.saturating_add(1), due.saturating_add(interval)));
        self.report.sent = self.report.sent.saturating_add(1);
        Sent::Message(length)
    }

    /// The peer's freshness at `now_ns`: a suspicion when it passed.
    pub(crate) fn judge(&mut self, peer: PeerId, now_ns: u64) -> Option<Change> {
        let link = self.received.link.as_mut()?;
        let until = link.estimator.deadline()?;
        if link.estimator.poll(now_ns) != Some(Event::Suspected) {
            return None;
        }
        // The freshness point of the heartbeat after the latest passed: one more point judged.
        if let Some(next) = self.received.last_mapped.map(|seq| seq.saturating_add(1)) {
            let beta = self.beta_now();
            self.account(next, beta);
        }
        self.report.suspicions = self.report.suspicions.saturating_add(1);
        Some(Change::Suspected(self.suspicion(peer, until, now_ns)))
    }

    fn suspicion(&self, peer: PeerId, at_ns: u64, noticed_ns: u64) -> Suspicion {
        let detection = self.received.link.as_ref().and_then(|link| {
            let margin = link.estimator.margin()?;
            link.sums.detection(
                link.estimator.estimates().window.length,
                link.estimator.interval(),
                margin,
            )
        });
        Suspicion {
            peer,
            at_ns,
            noticed_ns,
            last: self.received.last,
            detection,
            detector: self
                .received
                .configuration
                .map(|configured| configured.current),
        }
    }

    /// Theorem 7's `β` at the margin and interval in force, from the estimates as they stand: the
    /// bound on a mistake at each freshness point now, whatever the configuration assumed. `None`
    /// before a margin is configured.
    fn beta_now(&self) -> Option<f64> {
        let link = self.received.link.as_ref()?;
        let margin = link.estimator.margin()?;
        let behaviour = link.estimator.behaviour().ok().or(self
            .received
            .configuration
            .map(|configured| configured.link))?;
        let variance = behaviour.delay_deviation.as_secs_f64().powi(2);
        Some(mistake_bound(
            behaviour.loss,
            variance,
            link.estimator.interval().as_secs_f64(),
            margin.as_secs_f64(),
        ))
    }

    /// Counts the freshness points through the estimator's heartbeat `seq` into the allowance, each
    /// at `β` (`beta_now`), while a margin is in force: the allowance is the bound the detector
    /// keeps as it runs, so a configuration older than the estimates does not understate it.
    fn account(&mut self, seq: u64, beta: Option<f64>) {
        let points = match self.received.accounted {
            Some(accounted) if seq > accounted => seq.saturating_sub(accounted),
            Some(_) => return,
            None => 1,
        };
        if let Some(beta) = beta {
            // u64 → f64 rounds only past 2⁵³ heartbeats.
            self.report.allowance += beta.clamp(0.0, 1.0) * points as f64;
        }
        self.received.accounted = Some(seq);
    }

    /// A heartbeat of a run not seen before: the peer restarted, or this is its first. Its
    /// numbers, proofs and echo start again; the link's history stays, its schedule anchored anew
    /// at the run's first heartbeat. Whether the peer restarted.
    fn begin_run(&mut self, boot: u64) -> bool {
        if self.received.boot == Some(boot) {
            return false;
        }
        let restarted = self.received.boot.is_some();
        self.received.boot = Some(boot);
        self.received.base = self
            .received
            .last_mapped
            .map_or(0, |seq| seq.saturating_add(1));
        self.received.flushes = 0;
        self.received.last = None;
        self.received.echo = None;
        self.received.reanchor = restarted;
        restarted
    }

    /// Takes heartbeat `beat` from `peer`, received at `arrival_ns`, after judging the peer at
    /// that arrival: a freshness point that passed before the heartbeat came is a suspicion
    /// whatever order the owner fed them in.
    pub(crate) fn take(
        &mut self,
        peer: PeerId,
        beat: &Heartbeat,
        arrival_ns: u64,
        context: &Context,
        changes: &mut [Option<Change>; 2],
    ) -> Result<bool, Refusal> {
        changes[0] = self.judge(peer, arrival_ns);
        let restarted = self.begin_run(beat.boot);
        if self.received.last.is_some_and(|last| beat.seq <= last.seq) {
            return Err(Refusal::Stale);
        }
        let fresh_flush = beat.flush_age_ns <= beat.late_ns.saturating_add(beat.interval_ns);
        if beat.flushes <= self.received.flushes || !fresh_flush {
            self.report.unproven = self.report.unproven.saturating_add(1);
            return Err(Refusal::Unproven);
        }
        self.received.flushes = beat.flushes;
        self.received.floor_ns = beat.floor_ns;
        self.stream.asked_ns = beat.ask_ns;
        self.received.echo = Some((beat.sent_ns, beat.late_ns, arrival_ns));
        let sum = self.round_trip_sum(beat, arrival_ns);
        let granularity = context.granularity.ok_or(Refusal::Unmeasured)?;
        let mapped = self
            .received
            .base
            .checked_add(beat.seq)
            .ok_or(Refusal::OutOfRange)?;
        let link = self.link(beat.interval_ns, granularity)?;
        let event = link
            .estimator
            .on_heartbeat(mapped, arrival_ns)
            .map_err(|_| Refusal::OutOfRange)?;
        link.sums.push(sum);
        self.received.last = Some(Last {
            seq: beat.seq,
            arrival_ns,
            due_ns: beat.sent_ns.saturating_sub(beat.late_ns),
            sent_ns: beat.sent_ns,
        });
        self.received.last_mapped = Some(mapped);
        self.report.taken = self.report.taken.saturating_add(1);
        let beta = self.beta_now();
        self.account(mapped, beta);
        if self.renewal_due(beta) {
            self.configure(context, granularity);
        }
        changes[1] = match event {
            Some(Event::Trusted) => Some(Change::Trusted {
                peer,
                at_ns: arrival_ns,
            }),
            Some(Event::Suspected) => {
                self.report.suspicions = self.report.suspicions.saturating_add(1);
                Some(Change::Suspected(
                    self.suspicion(peer, arrival_ns, arrival_ns),
                ))
            }
            None => None,
        };
        Ok(restarted)
    }

    /// The delay sum of `beat`'s echo (the `bound` module): the round trip on this node's clock,
    /// with each side's lateness past its schedule added back. The network round trip is a sample
    /// of the pair's path. `None` without an echo, or when the echo says the heartbeat arrived
    /// before this node sent the one it echoes, which no clock can make true.
    fn round_trip_sum(&mut self, beat: &Heartbeat, arrival_ns: u64) -> Option<u64> {
        let echo = beat.echo?;
        let network = arrival_ns
            .checked_sub(echo.sent_ns)?
            .checked_sub(echo.hold_ns)?;
        self.received.round_trip.on_sample(network);
        network.checked_add(echo.late_ns)?.checked_add(beat.late_ns)
    }

    /// The link at the peer's `interval`, built on the first heartbeat and started again at a new
    /// interval or a new run's schedule: one allocation each.
    fn link(&mut self, interval_ns: u64, granularity: Duration) -> Result<&mut Link, Refusal> {
        let interval = Duration::from_nanos(interval_ns);
        let reanchor = std::mem::take(&mut self.received.reanchor);
        match self.received.link.as_mut() {
            None => {
                let estimator = LinkEstimator::new(interval, granularity, None)
                    .map_err(|_| Refusal::Malformed)?;
                let sums = Sums::new(estimator.estimates().window.drift);
                self.received.link = Some(Box::new(Link {
                    estimator,
                    sums,
                    granularity_ns: nanos(granularity),
                }));
            }
            Some(link) => {
                if link.granularity_ns != nanos(granularity) {
                    link.granularity_ns = nanos(granularity);
                    link.estimator.set_granularity(granularity);
                }
                if reanchor || link.estimator.interval() != interval {
                    link.estimator
                        .retime(interval, None)
                        .map_err(|_| Refusal::Malformed)?;
                    link.sums.restart(link.estimator.estimates().window.drift);
                }
            }
        }
        self.received.link.as_deref_mut().ok_or(Refusal::Malformed)
    }

    /// Whether the detector is to be configured again (Chen et al.'s adaptive detector, which
    /// reconfigures as its estimates move, §6), on a doubling schedule: never configured; at an
    /// interval the configuration was not made for (the peer moved to the one asked); once the
    /// heartbeats taken have doubled since the last configuration, as the history the inputs are
    /// estimated over, and the exposure the MTBF is, have; or once `β` at the margin in force has
    /// doubled past the configured one, the link having got worse. Each doubling halves the
    /// estimates' remaining weight of evidence at most once, so a link is configured
    /// `O(log heartbeats)` times, not once a window: at a window of a few heartbeats, the window's
    /// cadence spent five times the stream's own work on the configurator
    /// (`docs/benchmarks.md`, "hyper-liveness").
    fn renewal_due(&self, beta: Option<f64>) -> bool {
        let Some(configured) = self.received.configuration else {
            return true;
        };
        let interval = self
            .received
            .link
            .as_ref()
            .map(|link| link.estimator.interval());
        let Some((taken, renewed_beta)) = self.received.renewed else {
            return true;
        };
        interval != Some(configured.current.interval)
            || self.report.taken >= taken.saturating_mul(2)
            || beta.is_some_and(|now| now >= 2.0 * renewed_beta)
    }

    /// Configures the detector: the configurator over this node's `G`, the peer's floor and the
    /// costs, with one heartbeat in the margin (`α < η`), whose Theorem 7 bound is a single
    /// Cantelli factor and assumes no independence between heartbeats. The traces' correlation
    /// time `T_c` is the spacing past the longest stall a run saw (`docs/timing.md` §2.6, item 6),
    /// which a young link has not seen: taking it as `τ_int·η` from the link's own history let the
    /// product of a many-heartbeat margin promise a mistake rate that six runs in ten broke under a
    /// one-CPU throttle, whose stalls the history had not yet held (`docs/benchmarks.md`,
    /// "hyper-liveness"). hyper-swim judges each probe on its own for the same reason (§2.7). A
    /// refusal leaves the detector in force (`LinkEstimator::configure`).
    fn configure(&mut self, context: &Context, granularity: Duration) {
        let (Some(election), Some(mtbf)) = (self.election, context.mtbf) else {
            return;
        };
        let floor_ns = self.received.floor_ns;
        let Some(link) = self.received.link.as_mut() else {
            return;
        };
        let estimator = &mut link.estimator;
        let floors = Floors {
            granularity,
            sender: Duration::from_nanos(floor_ns).max(granularity),
            correlation: Duration::MAX,
        };
        let costs = Costs { election, mtbf };
        if let Ok(configured) = estimator.configure(&costs, &floors) {
            self.received.configuration = Some(configured);
            self.report.configurations = self.report.configurations.saturating_add(1);
            let beta = self.beta_now().unwrap_or(1.0);
            self.received.renewed = Some((self.report.taken, beta));
        }
    }
}
