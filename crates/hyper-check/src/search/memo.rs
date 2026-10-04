//! Lowe's memo (§3.1): every configuration the search reached, so none is searched twice.
//!
//! A configuration is the set of operations linearized and the object's state. Two forms hold it:
//!
//! - **Fingerprints** ([`Prints`]): 128 bits a configuration, as slates' exhaustive search holds its
//!   states (`docs/research/sim.md` §7, where whole keys took twice the memory). The set's part is
//!   kept by XOR of a key per operation (Zobrist's hashing; Horn and Kroening §5.1: "the bitwise XOR
//!   operator over fixed-size bit vectors forms an abelian group"), so linearizing or undoing an
//!   operation updates it in constant time, and the fingerprint hashes that with the state. Two
//!   configurations share a fingerprint with probability 2⁻¹²⁸, so some pair of `n` does with
//!   probability below `n²/2¹²⁹`. A shared one can only make the search skip a configuration it
//!   had not searched, and so refuse a history that has an order: a refusal is confirmed on whole
//!   keys before it is reported, and a pass is never in doubt (it exhibits its order).
//! - **Whole keys** ([`Whole`]): the configuration itself, compact. The linearized set is every
//!   operation whose return precedes the first return still in the list, and those of the operations
//!   pending there that were linearized ahead of it (Lowe §4's configuration: the position, the
//!   operations linearized and not yet returned, and the state), so the key is that position, that
//!   short list and the state, not a bit for every operation of the history.
//!
//! Each holds at most the bytes its [`Budget`] states, a resize's two tables counted together, and
//! refuses past them ([`Spent`]): the search then says `Unknown`.

use std::collections::BTreeSet;
use std::hash::{DefaultHasher, Hash, Hasher};

use crate::model::Model;

/// The bytes the memory a search may hold, by default: GitHub's smallest runner (the macOS image,
/// 7 GB) holds a search of 4 GiB beside the test binary and the runner's own processes, the
/// ceiling slates derived for its exhaustive search (`crates/cluster/tests/support/exhaustive.rs`,
/// `MEMORY_CEILING_BYTES`; `docs/sim.md` §7).
pub const MEMORY_CEILING: usize = 4 << 30;

/// The memory a search may hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budget {
    /// Bytes of memo, a resize's old and new tables counted together.
    pub memory: usize,
}

impl Budget {
    /// [`MEMORY_CEILING`].
    pub const CEILING: Self = Self {
        memory: MEMORY_CEILING,
    };
}

impl Default for Budget {
    fn default() -> Self {
        Self::CEILING
    }
}

/// Why a memo took no more.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Spent {
    /// The budget's bytes were reached at this many configurations.
    Budget {
        /// Configurations held when the next table would not fit.
        configurations: u64,
    },
    /// The host refused an allocation the budget allowed.
    Memory,
}

/// A fingerprint function for configurations: the Zobrist hash of the linearized set and the
/// state, to 128 bits.
pub trait Print {
    /// The fingerprint of the configuration whose linearized set hashes to `zobrist` and whose
    /// state is `state`.
    fn print<S: Hash>(&self, zobrist: u128, state: &S) -> u128;
}

/// Two lanes of SipHash-1-3 (std's `DefaultHasher` under its fixed keys), each over a lane's
/// byte, the set's hash and the state: slates' `fingerprint`, its 64-bit lanes joined.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sip;

impl Print for Sip {
    fn print<S: Hash>(&self, zobrist: u128, state: &S) -> u128 {
        let lane = |which: u8| {
            let mut hasher = DefaultHasher::new();
            which.hash(&mut hasher);
            zobrist.hash(&mut hasher);
            state.hash(&mut hasher);
            hasher.finish()
        };
        (u128::from(lane(0)) << 64) | u128::from(lane(1))
    }
}

/// What the search tells a memo and asks of it.
pub(crate) trait Memo<S> {
    /// `op`, whose return is at `ret` in the history, was linearized.
    fn linearized(&mut self, op: u32, ret: u32) -> Result<(), Spent>;
    /// `op`, whose return is at `ret`, was undone.
    fn undone(&mut self, op: u32, ret: u32);
    /// Whether the configuration reached, its set hashing to `zobrist`, the first return left in
    /// the list at `first`, and its state `state`, is new; it is held from now on.
    fn fresh(&mut self, zobrist: u128, first: u32, state: &S) -> Result<bool, Spent>;
    /// Configurations held.
    fn held(&self) -> u64;
}

/// The bytes of `slots` slots of `width` bytes, or `None` past `usize`.
fn bytes_of(slots: usize, width: usize) -> Option<usize> {
    slots.checked_mul(width)
}

/// A table of slots, empty or full, over a capacity that is a power of two, growing by doubling
/// within a budget of bytes that counts the old table and the new one together while it grows.
struct Slots<T: Copy + PartialEq> {
    slots: Vec<T>,
    len: usize,
    empty: T,
    memory: usize,
}

/// The fill at which a table doubles, as a fraction: `LOAD_NUMERATOR / LOAD_DENOMINATOR`. Cited:
/// linear probing at load α takes about `½(1 + 1/(1−α)²)` probes for an unsuccessful search
/// (Knuth, TAOCP Vol. 3, §6.4), which every new configuration's insertion is: 8.5 at ¾, against
/// 32.5 at ⅞ and 2.5 at ½. At ¾ a table is between ⅜ and ¾ full, so a configuration's 16 bytes cost
/// 21⅓ to 42⅔ of table (`docs/sim.md` §14.3).
const LOAD_NUMERATOR: usize = 3;
/// See [`LOAD_NUMERATOR`].
const LOAD_DENOMINATOR: usize = 4;

impl<T: Copy + PartialEq> Slots<T> {
    fn new(capacity: usize, empty: T, memory: usize) -> Result<Self, Spent> {
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

    fn mask(&self) -> usize {
        self.slots.len().saturating_sub(1)
    }

    /// The slot `hash` probes to first, then each after it in turn, wrapping.
    fn find(&self, hash: u64, mut same: impl FnMut(T) -> bool) -> Probe {
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
    fn has_room(&self) -> bool {
        let after = self.len.saturating_add(1);
        after.saturating_mul(LOAD_DENOMINATOR) <= self.slots.len().saturating_mul(LOAD_NUMERATOR)
    }

    /// The table doubled, its slots placed again by `hash`; refused when the old and the new
    /// table would not fit the budget together.
    fn double(&mut self, hash: impl Fn(T) -> u64) -> Result<(), Spent> {
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

    fn put(&mut self, at: usize, value: T) {
        if let Some(slot) = self.slots.get_mut(at) {
            *slot = value;
            self.len = self.len.saturating_add(1);
        }
    }

    fn bytes(&self) -> usize {
        bytes_of(self.slots.len(), size_of::<T>()).unwrap_or(usize::MAX)
    }
}

enum Probe {
    Empty(usize),
    Found,
    Full,
}

/// The low 64 bits of a fingerprint, which place it in a table: a fingerprint's bits are uniform.
fn low(print: u128) -> u64 {
    u64::try_from(print & u128::from(u64::MAX)).unwrap_or(0)
}

/// The fingerprint memo: a table of 128-bit fingerprints. Zero marks an empty slot, so a
/// configuration that prints to zero is held as one: that merges it with the configuration that
/// prints to one, with probability 2⁻¹²⁸, which a refusal's confirmation covers like any shared
/// fingerprint.
pub(crate) struct Prints<'p, P> {
    table: Slots<u128>,
    print: &'p P,
}

impl<'p, P: Print> Prints<'p, P> {
    /// A memo within `memory` bytes, its table first sized for `expected` configurations.
    pub(crate) fn new(print: &'p P, memory: usize, expected: usize) -> Result<Self, Spent> {
        let capacity = expected
            .saturating_mul(LOAD_DENOMINATOR)
            .checked_div(LOAD_NUMERATOR)
            .unwrap_or(expected)
            .saturating_add(1);
        Ok(Self {
            table: Slots::new(capacity, 0, memory)?,
            print,
        })
    }
}

impl<P> Prints<'_, P> {
    /// Configurations held.
    pub(crate) fn count(&self) -> u64 {
        u64::try_from(self.table.len).unwrap_or(u64::MAX)
    }
}

impl<S: Hash, P: Print> Memo<S> for Prints<'_, P> {
    fn linearized(&mut self, _op: u32, _ret: u32) -> Result<(), Spent> {
        Ok(())
    }

    fn undone(&mut self, _op: u32, _ret: u32) {}

    fn fresh(&mut self, zobrist: u128, _first: u32, state: &S) -> Result<bool, Spent> {
        let print = self.print.print(zobrist, state).max(1);
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
                configurations: self.count(),
            }),
        }
    }

    fn held(&self) -> u64 {
        self.count()
    }
}

/// A configuration held whole: the first return left in the list, the operations linearized
/// whose returns lie past it (their places in `pending`), the state, and its fingerprint, which
/// places it.
struct Entry<S> {
    print: u128,
    first: u32,
    pending: (u32, u32),
    state: S,
}

/// The whole-key memo, for the confirmation of a refusal.
pub(crate) struct Whole<'m, M: Model> {
    model: &'m M,
    /// The configurations, in the order reached; a slot of the table names one by its place
    /// plus one, zero marking an empty slot.
    entries: Vec<Entry<M::State>>,
    pending: Vec<u32>,
    table: Slots<u32>,
    /// Every operation linearized, by the place of its return: the pending part of a
    /// configuration is those past its first return.
    linearized: BTreeSet<(u32, u32)>,
    /// Bytes held by `entries`, `pending` and the states' heaps.
    bytes: usize,
    memory: usize,
    scratch: Vec<u32>,
}

impl<'m, M: Model> Whole<'m, M> {
    /// A memo within `memory` bytes, first sized for `expected` configurations.
    pub(crate) fn new(model: &'m M, memory: usize, expected: usize) -> Result<Self, Spent> {
        let capacity = expected
            .saturating_mul(LOAD_DENOMINATOR)
            .checked_div(LOAD_NUMERATOR)
            .unwrap_or(expected)
            .saturating_add(1);
        Ok(Self {
            model,
            entries: Vec::new(),
            pending: Vec::new(),
            table: Slots::new(capacity, 0, memory)?,
            linearized: BTreeSet::new(),
            bytes: 0,
            memory,
            scratch: Vec::new(),
        })
    }

    fn spent(&self) -> Spent {
        Spent::Budget {
            configurations: u64::try_from(self.entries.len()).unwrap_or(u64::MAX),
        }
    }

    /// Room for one more entry and `more` pending places, within the budget: each vector doubles
    /// when full, its old and new buffers counted together while it grows.
    fn reserve(&mut self, more: usize, state_heap: usize) -> Result<(), Spent> {
        let spent = self.spent();
        let entry = size_of::<Entry<M::State>>();
        let entries = grow_by(&mut self.entries, 1, entry, self.bytes, self.memory, spent)?;
        let pending = grow_by(&mut self.pending, more, 4, entries, self.memory, spent)?;
        let total = pending.checked_add(state_heap).ok_or(spent)?;
        let with_table = total.checked_add(self.table.bytes()).ok_or(spent)?;
        if with_table > self.memory {
            return Err(spent);
        }
        self.bytes = total;
        Ok(())
    }

    fn same(&self, at: u32, print: u128, first: u32, state: &M::State) -> bool {
        let Some(entry) = usize::try_from(at)
            .ok()
            .and_then(|at| at.checked_sub(1))
            .and_then(|at| self.entries.get(at))
        else {
            return false;
        };
        let (from, to) = entry.pending;
        let held = usize::try_from(from)
            .ok()
            .zip(usize::try_from(to).ok())
            .and_then(|(from, to)| self.pending.get(from..to));
        entry.print == print
            && entry.first == first
            && held == Some(self.scratch.as_slice())
            && entry.state == *state
    }
}

/// `vec` given room for `more` elements of `width` bytes, doubling when it must, with `bytes` the
/// bytes held before; the bytes held after, refused with `spent` when the old and new buffers
/// would not fit `memory` together.
fn grow_by<T>(
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

impl<M: Model> Memo<M::State> for Whole<'_, M> {
    fn linearized(&mut self, op: u32, ret: u32) -> Result<(), Spent> {
        self.linearized.insert((ret, op));
        Ok(())
    }

    fn undone(&mut self, op: u32, ret: u32) {
        self.linearized.remove(&(ret, op));
    }

    fn fresh(&mut self, zobrist: u128, first: u32, state: &M::State) -> Result<bool, Spent> {
        let print = Sip.print(zobrist, state);
        self.scratch.clear();
        let past = first.saturating_add(1);
        let ops = self.linearized.range((past, 0)..).map(|(_, op)| *op);
        self.scratch.extend(ops);
        self.scratch.sort_unstable();
        let seen = self
            .table
            .find(low(print), |slot| self.same(slot, print, first, state));
        if matches!(seen, Probe::Found) {
            return Ok(false);
        }
        if !self.table.has_room() {
            let entries = &self.entries;
            let place = |slot: u32| {
                usize::try_from(slot)
                    .ok()
                    .and_then(|slot| slot.checked_sub(1))
                    .and_then(|at| entries.get(at))
                    .map_or(0, |entry| low(entry.print))
            };
            self.table.double(place)?;
        }
        let probe = self
            .table
            .find(low(print), |slot| self.same(slot, print, first, state));
        let Probe::Empty(at) = probe else {
            return match probe {
                Probe::Found => Ok(false),
                _ => Err(self.spent()),
            };
        };
        let heap = self
            .model
            .bytes(state)
            .saturating_sub(size_of::<M::State>());
        self.reserve(self.scratch.len(), heap)?;
        let spent = self.spent();
        let from = u32::try_from(self.pending.len()).map_err(|_| spent)?;
        self.pending.extend_from_slice(&self.scratch);
        let to = u32::try_from(self.pending.len()).map_err(|_| spent)?;
        self.entries.push(Entry {
            print,
            first,
            pending: (from, to),
            state: state.clone(),
        });
        let slot = u32::try_from(self.entries.len()).map_err(|_| spent)?;
        self.table.put(at, slot);
        Ok(true)
    }

    fn held(&self) -> u64 {
        u64::try_from(self.entries.len()).unwrap_or(u64::MAX)
    }
}
