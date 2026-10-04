//! Where a command goes (`docs/multilog.md` §2.1): a global command to log 0, a keyed one to the
//! log its key hashes to.

/// SplitMix64's increment, the odd integer nearest `2^64 / φ` (Steele, Lea and Flood, *Fast
/// splittable pseudorandom number generators*, OOPSLA 2014): added to the key before the finalizer,
/// so that key 0 does not hash to 0.
const GAMMA: u64 = 0x9e37_79b9_7f4a_7c15;
/// The finalizer's first multiplier (SplitMix64, Steele, Lea and Flood, OOPSLA 2014: Stafford's
/// variant 13 of MurmurHash3's 64-bit finalizer).
const MIX_ONE: u64 = 0xbf58_476d_1ce4_e5b9;
/// The finalizer's second multiplier (as [`MIX_ONE`]).
const MIX_TWO: u64 = 0x94d0_49bb_1331_11eb;
/// The finalizer's shifts, in the order applied (as [`MIX_ONE`]).
const SHIFTS: [u32; 3] = [30, 27, 31];

/// Where a command goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    /// Log 0: a command that may read and write any of the group's state.
    Global,
    /// The log `key` hashes to: a command that reads and writes `key`'s state alone, and reads
    /// the global state.
    Key(u64),
}

impl Route {
    /// The log this route names among `logs` logs (at least one; none is no group and goes to
    /// log 0).
    pub fn log(self, logs: usize) -> usize {
        match self {
            Self::Global => 0,
            Self::Key(key) => log_of(key, logs),
        }
    }
}

/// SplitMix64's finalizer of `key + γ`: a bijection on 64-bit words with full avalanche, so that
/// consecutive keys spread over the logs.
pub fn mix(key: u64) -> u64 {
    let [first, second, third] = SHIFTS;
    let mut z = key.wrapping_add(GAMMA);
    z = (z ^ (z >> first)).wrapping_mul(MIX_ONE);
    z = (z ^ (z >> second)).wrapping_mul(MIX_TWO);
    z ^ (z >> third)
}

/// The log a keyed command with `key` goes to among `logs` logs: its hash modulo the count, whose
/// bias toward the low logs is under `logs / 2^64`. A function of the key and the count alone, the
/// same on every member and every version: it is part of the format.
pub fn log_of(key: u64, logs: usize) -> usize {
    let Ok(count) = u64::try_from(logs) else {
        return 0;
    };
    mix(key)
        .checked_rem(count)
        .and_then(|log| usize::try_from(log).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The finalizer is SplitMix64's: its first outputs from seed 0 are the reference
    /// implementation's (Vigna's `splitmix64.c`, which the paper's generator is), drawn by adding
    /// γ before each finalization.
    #[test]
    fn the_hash_is_splitmix64s_finalizer() {
        // splitmix64.c from state 0: 0xe220a8397b1dcdaf, 0x6e789e6aa1b965f4, 0x06c45d188009454f.
        assert_eq!(mix(0), 0xe220_a839_7b1d_cdaf);
        assert_eq!(mix(GAMMA), 0x6e78_9e6a_a1b9_65f4);
        assert_eq!(mix(GAMMA.wrapping_mul(2)), 0x06c4_5d18_8009_454f);
    }

    /// One log takes every key; none is no group, and routes to log 0.
    #[test]
    fn one_log_takes_every_key() {
        for key in [0, 1, u64::MAX, 12_345] {
            assert_eq!(log_of(key, 1), 0);
            assert_eq!(log_of(key, 0), 0);
            assert_eq!(Route::Key(key).log(1), 0);
        }
        assert_eq!(Route::Global.log(5), 0);
    }
}
