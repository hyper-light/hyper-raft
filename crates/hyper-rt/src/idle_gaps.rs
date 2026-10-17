#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::arithmetic_side_effects, clippy::indexing_slicing, missing_docs)]
//! PROTOTYPE (docs/runtime.md §15 item 8, branch `proto-spin-rule`): the spin an idle shard makes before it
//! parks, chosen from the idle gaps it has seen rather than fixed at what blocking costs.
//!
//! Spin-then-block with the spin as long as blocking costs `B` is 2-competitive against any gap [KARLIN].
//! Against gaps that are all longer than `B` (a client that pauses 200 µs, `B` a few µs) it pays `2B` an idle
//! period where parking at once pays `B`. Two rules are measured against it:
//!
//! - **Learned**: the spin that minimizes the expected cost of an idle period over the empirical distribution
//!   of the last `WINDOW` gaps, `cost(t) = E[min(G, t)] + B · P(G > t)` (spin up to `t`, then park), taken over
//!   `t = 0` and every gap seen. `WINDOW` is the Dvoretzky–Kiefer–Wolfowitz sample size (Massart's constant)
//!   for the empirical distribution to lie within `ε` of the true one everywhere with probability `1 − α`:
//!   `n = ⌈ln(2/α) / (2ε²)⌉`, `α = 0.05` and `ε = 0.05`, the stopping rule's half width
//!   (`CONVERGED_WIDTH_PERMILLE`): 738.
//! - **Randomized**: Karlin et al.'s `e/(e−1)`-competitive spin, drawn each idle period from the density
//!   `e^{t/B} / (B (e − 1))` on `[0, B]`, `t = B · ln(1 + u (e − 1))` for `u` uniform.

/// Prototype: the gaps the learned rule keeps (the module's DKW size).
pub(crate) const WINDOW: usize = 738;

/// Which rule chooses the spin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Rule {
    /// The spin lasts what blocking costs (the runtime's rule).
    Karlin,
    /// The spin that minimizes the expected cost over the gaps seen.
    Learned,
    /// Karlin et al.'s randomized spin.
    Random,
    /// Karlin's spin or none: park at once when the gaps seen make that cheaper than spinning `B`.
    KarlinOrPark,
}

/// The gaps seen, and the spin they make cheapest.
#[derive(Debug)]
pub(crate) struct IdleGaps {
    pub(crate) rule: Rule,
    ring: Vec<u64>,
    next: usize,
    scratch: Vec<u64>,
    threshold: Option<u64>,
    draws: crate::machine::stats::Xorshift,
}

impl IdleGaps {
    pub(crate) fn new(rule: Rule) -> Self {
        Self {
            rule,
            ring: Vec::with_capacity(WINDOW),
            next: 0,
            scratch: Vec::with_capacity(WINDOW),
            threshold: None,
            draws: crate::machine::stats::Xorshift::new(0x9e37_79b9_7f4a_7c15),
        }
    }

    /// An idle period ended `gap_ns` after it began, by work that came; `blocking_ns` what a park costs now.
    pub(crate) fn record(&mut self, gap_ns: u64, blocking_ns: Option<u64>) {
        if !matches!(self.rule, Rule::Learned | Rule::KarlinOrPark) {
            return;
        }
        if self.ring.len() < WINDOW {
            self.ring.push(gap_ns);
        } else if let Some(slot) = self.ring.get_mut(self.next) {
            *slot = gap_ns;
        }
        self.next = (self.next + 1) % WINDOW;
        if self.ring.len() == WINDOW
            && self.next == 0
            && let Some(blocking) = blocking_ns
        {
            self.scratch.clear();
            self.scratch.extend_from_slice(&self.ring);
            self.scratch.sort_unstable();
            self.threshold = Some(match self.rule {
                Rule::KarlinOrPark => karlin_or_park(&self.scratch, blocking),
                _ => optimal_threshold(&self.scratch, blocking),
            });
        }
    }

    /// The spin for this idle period, given what blocking costs; `None` to keep the runtime's rule.
    pub(crate) fn spin_ns(&mut self, blocking_ns: u64) -> Option<u64> {
        match self.rule {
            Rule::Karlin => None,
            Rule::Learned => self.threshold,
            Rule::KarlinOrPark => self.threshold.map(|t| if t == 0 { 0 } else { blocking_ns }),
            Rule::Random => {
                let u = (self.draws.next_u64() >> 11) as f64 / (1u64 << 53) as f64;
                let t = blocking_ns as f64 * (1.0 + u * (std::f64::consts::E - 1.0)).ln();
                Some(t as u64)
            }
        }
    }
}

/// 0 when parking at once costs less over the gaps seen than spinning `B` and then parking,
/// `B < E[min(G, B)] + B · P(G > B)`; else 1 (spin `B`, Karlin's rule).
fn karlin_or_park(sorted: &[u64], blocking: u64) -> u64 {
    let n = sorted.len() as u128;
    let b = u128::from(blocking);
    let spun: u128 = sorted.iter().map(|&g| u128::from(g).min(b)).sum::<u128>()
        + b * sorted.iter().filter(|&&g| u128::from(g) > b).count() as u128;
    if b * n < spun { 0 } else { 1 }
}

/// The `t` minimizing `Σ min(g, t) + (t > g ? 0 : …)`: over `t = 0` and each gap of `sorted`, the expected cost of
/// spinning up to `t` and then parking, `Σ_{g ≤ t} g + (t + B) · #{g > t}`, all over `n`.
fn optimal_threshold(sorted: &[u64], blocking: u64) -> u64 {
    let n = sorted.len() as u128;
    let blocking = u128::from(blocking);
    // t = 0: every period parks.
    let mut best = (blocking * n, 0u64);
    let mut below: u128 = 0;
    for (index, &gap) in sorted.iter().enumerate() {
        below += u128::from(gap);
        let above = n - (index as u128 + 1);
        let cost = below + (u128::from(gap) + blocking) * above;
        if cost < best.0 {
            best = (cost, gap);
        }
    }
    best.1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_gaps_park_at_once_and_short_ones_spin_through() {
        let long: Vec<u64> = (0..100).map(|i| 200_000 + i).collect();
        assert_eq!(optimal_threshold(&long, 5_000), 0);
        let short: Vec<u64> = (0..100).map(|i| 1_000 + i).collect();
        assert_eq!(optimal_threshold(&short, 5_000), 1_099);
        // Half short, half long: spin the short ones out.
        let mut mixed: Vec<u64> = (0..50).map(|i| 500 + i).chain((0..50).map(|i| 300_000 + i)).collect();
        mixed.sort_unstable();
        assert_eq!(optimal_threshold(&mixed, 5_000), 549);
    }
}
