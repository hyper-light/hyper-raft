//! Open-addressing tables within a budget of bytes: the search checker's memo (`search::memo`)
//! and the exhaustive searches' visited sets (`explore`) hold their keys in them, each growth
//! counted with the old table and the new one together, and a key that would pass the budget
//! refused ([`Spent`]).

use crate::search::Spent;

/// The bytes of `slots` slots of `width` bytes, or `None` past `usize`.
pub(crate) fn bytes_of(slots: usize, width: usize) -> Option<usize> {
    slots.checked_mul(width)
}

/// A table of slots, empty or full, over a capacity that is a power of two, growing by doubling
/// within a budget of bytes that counts the old table and the new one together while it grows.
pub(crate) struct Slots<T: Copy + PartialEq> {
    slots: Vec<T>,
    pub(crate) len: usize,
    empty: T,
    memory: usize,
}

/// The fill at which a table doubles, as a fraction: `LOAD_NUMERATOR / LOAD_DENOMINATOR`. Cited:
/// linear probing at load α takes about `½(1 + 1/(1−α)²)` probes for an unsuccessful search
/// (Knuth, TAOCP Vol. 3, §6.4), which every new configuration's insertion is: 8.5 at ¾, against
/// 32.5 at ⅞ and 2.5 at ½. At ¾ a table is between ⅜ and ¾ full, so a configuration's 16 bytes cost
/// 21⅓ to 42⅔ of table (`docs/sim.md` §14.3).
pub(crate) const LOAD_NUMERATOR: usize = 3;
/// See [`LOAD_NUMERATOR`].
pub(crate) const LOAD_DENOMINATOR: usize = 4;

impl<T: Copy + PartialEq> Slots<T> {
    pub(crate) fn new(capacity: usize, empty: T, memory: usize) -> Result<Self, Spent> {
        let capacity = capacity
            .max(1)
            .checked_next_power_of_two()
            .ok_or(Spent::Memory)?;
        let fits = bytes_of(capacity, size_of::<T>()).is_some_and(|bytes| bytes <= memory);
        if !fits {
            return Err(Spent::Budget { configurations: 0 });
        }
        let mut slots = Vec::new();
        slots
            .try_reserve_exact(capacity)
            .map_err(|_| Spent::Memory)?;
        slots.resize(capacity, empty);
        Ok(Self {
            slots,
            len: 0,
            empty,
            memory,
        })
    }

    /// Slots in the table.
    pub(crate) fn slots_len(&self) -> usize {
        self.slots.len()
    }

    pub(crate) fn mask(&self) -> usize {
        self.slots.len().saturating_sub(1)
    }

    /// The slot `hash` probes to first, then each after it in turn, wrapping.
    pub(crate) fn find(&self, hash: u64, mut same: impl FnMut(T) -> bool) -> Probe {
        let mask = self.mask();
        let mut at = usize::try_from(hash).unwrap_or(usize::MAX) & mask;
        // A table is never full (it doubles at its load), so the probe meets an empty slot within
        // its length.
        for _ in 0..self.slots.len() {
            match self.slots.get(at) {
                Some(slot) if *slot == self.empty => return Probe::Empty(at),
                Some(slot) if same(*slot) => return Probe::Found,
                _ => at = at.wrapping_add(1) & mask,
            }
        }
        Probe::Full
    }

    /// Whether one more fits before the table doubles.
    pub(crate) fn has_room(&self) -> bool {
        let after = self.len.saturating_add(1);
        after.saturating_mul(LOAD_DENOMINATOR) <= self.slots.len().saturating_mul(LOAD_NUMERATOR)
    }

    /// The table doubled, its slots placed again by `hash`; refused when the old and the new
    /// table would not fit the budget together.
    pub(crate) fn double(&mut self, hash: impl Fn(T) -> u64) -> Result<(), Spent> {
        let held = u64::try_from(self.len).unwrap_or(u64::MAX);
        let spent = Spent::Budget {
            configurations: held,
        };
        let capacity = self.slots.len().checked_mul(2).ok_or(spent)?;
        let peak = capacity
            .checked_add(self.slots.len())
            .and_then(|slots| bytes_of(slots, size_of::<T>()))
            .ok_or(spent)?;
        if peak > self.memory {
            return Err(spent);
        }
        let mut grown = Vec::new();
        grown
            .try_reserve_exact(capacity)
            .map_err(|_| Spent::Memory)?;
        grown.resize(capacity, self.empty);
        let old = std::mem::replace(&mut self.slots, grown);
        let mask = self.mask();
        for slot in old.into_iter().filter(|slot| *slot != self.empty) {
            let mut at = usize::try_from(hash(slot)).unwrap_or(usize::MAX) & mask;
            while self.slots.get(at).is_some_and(|held| *held != self.empty) {
                at = at.wrapping_add(1) & mask;
            }
            if let Some(free) = self.slots.get_mut(at) {
                *free = slot;
            }
        }
        Ok(())
    }

    pub(crate) fn put(&mut self, at: usize, value: T) {
        if let Some(slot) = self.slots.get_mut(at) {
            *slot = value;
            self.len = self.len.saturating_add(1);
        }
    }

    pub(crate) fn bytes(&self) -> usize {
        bytes_of(self.slots.len(), size_of::<T>()).unwrap_or(usize::MAX)
    }
}

pub(crate) enum Probe {
    Empty(usize),
    Found,
    Full,
}

/// The low 64 bits of a fingerprint, which place it in a table: a fingerprint's bits are uniform.
pub(crate) fn low(print: u128) -> u64 {
    u64::try_from(print & u128::from(u64::MAX)).unwrap_or(0)
}

/// `vec` given room for `more` elements of `width` bytes, doubling when it must, with `bytes` the
/// bytes held before; the bytes held after, refused with `spent` when the old and new buffers
/// would not fit `memory` together.
pub(crate) fn grow_by<T>(
    vec: &mut Vec<T>,
    more: usize,
    width: usize,
    bytes: usize,
    memory: usize,
    spent: Spent,
) -> Result<usize, Spent> {
    let needed = vec.len().checked_add(more).ok_or(spent)?;
    if needed <= vec.capacity() {
        return Ok(bytes);
    }
    let capacity = needed.max(vec.capacity().saturating_mul(2)).max(4);
    let old = bytes_of(vec.capacity(), width).ok_or(spent)?;
    let new = bytes_of(capacity, width).ok_or(spent)?;
    let peak = bytes.checked_add(new).ok_or(spent)?;
    if peak > memory {
        return Err(spent);
    }
    vec.try_reserve_exact(capacity.saturating_sub(vec.len()))
        .map_err(|_| Spent::Memory)?;
    Ok(bytes.saturating_sub(old).saturating_add(new))
}

/// A set of 128-bit fingerprints within a budget of bytes: the exhaustive searches' visited
/// classes (`explore`) and the coverage strategies' points (`strategy::guided`). Zero marks an
/// empty slot, so a key that prints to zero is held as one: it then merges with the key that
/// prints to one, with probability 2⁻¹²⁸, as any two keys sharing a fingerprint do.
pub(crate) struct PrintSet {
    table: Slots<u128>,
}

impl PrintSet {
    /// An empty set within `memory` bytes, its table first sized for `expected` fingerprints.
    pub(crate) fn new(memory: usize, expected: usize) -> Result<Self, Spent> {
        let capacity = expected
            .saturating_mul(LOAD_DENOMINATOR)
            .checked_div(LOAD_NUMERATOR)
            .unwrap_or(expected)
            .saturating_add(1);
        Ok(Self {
            table: Slots::new(capacity, 0, memory)?,
        })
    }

    /// Whether `print` is held.
    pub(crate) fn contains(&self, print: u128) -> bool {
        let print = print.max(1);
        matches!(
            self.table.find(low(print), |held| held == print),
            Probe::Found
        )
    }

    /// Holds `print`: true when it was not held before. Refused when the table would have to
    /// grow past its budget.
    pub(crate) fn insert(&mut self, print: u128) -> Result<bool, Spent> {
        let print = print.max(1);
        if matches!(
            self.table.find(low(print), |held| held == print),
            Probe::Found
        ) {
            return Ok(false);
        }
        if !self.table.has_room() {
            self.table.double(low)?;
        }
        match self.table.find(low(print), |held| held == print) {
            Probe::Found => Ok(false),
            Probe::Empty(at) => {
                self.table.put(at, print);
                Ok(true)
            }
            Probe::Full => Err(Spent::Budget {
                configurations: self.len(),
            }),
        }
    }

    /// Fingerprints held.
    pub(crate) fn len(&self) -> u64 {
        u64::try_from(self.table.len).unwrap_or(u64::MAX)
    }

    /// Bytes the table holds.
    pub(crate) fn bytes(&self) -> usize {
        self.table.bytes()
    }

    /// The bytes the table would hold at its peak to take `more` fingerprints: its present table
    /// and, if it must double to hold them at its load, every doubled table it passes through
    /// beside the one before it.
    pub(crate) fn peak_for(&self, more: usize) -> Option<usize> {
        let wanted = self.table.len.checked_add(more)?;
        let mut capacity = self.table.slots_len();
        let mut peak = bytes_of(capacity, size_of::<u128>())?;
        while wanted.checked_mul(LOAD_DENOMINATOR)? > capacity.checked_mul(LOAD_NUMERATOR)? {
            let doubled = capacity.checked_mul(2)?;
            let both = bytes_of(capacity.checked_add(doubled)?, size_of::<u128>())?;
            peak = peak.max(both);
            capacity = doubled;
        }
        Some(peak)
    }
}
