//! A global allocator that counts: allocations, reallocations, frees, the
//! bytes asked for, and the bytes held now and at most since the count began.
//!
//! It forwards every call to [`System`] unchanged and counts on the calling
//! thread only, in thread-local cells: no lock, no atomic, no allocation, and
//! a thread that is not counting pays one thread-local read. A measurement
//! runs on one thread and turns counting on around what it measures
//! ([`begin`], [`pause`], [`resume`], [`end`]).
//!
//! Within a count, work that is the harness's own and not the code under
//! measurement (an owner's storage, a simulated network) is set [`aside`]: it
//! is counted in the total and also apart, so one run gives both what the
//! whole loop did and what the measured calls did ([`Counts::less`]).
//!
//! Counts are per thread: a block one thread allocates and another frees is
//! a free on the second. The measurements in this repository allocate and
//! free on one thread.
#![allow(unsafe_code)]
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
};

/// What the allocator was asked to do while counting was on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    /// Blocks allocated, zeroed or not.
    pub allocations: u64,
    /// Blocks reallocated, whether or not the block moved.
    pub reallocations: u64,
    /// Reallocations that moved the block: its bytes were copied.
    pub moved: u64,
    /// Blocks freed.
    pub frees: u64,
    /// Bytes asked for: every allocation's size, and what each reallocation
    /// grew a block by.
    pub bytes: u64,
    /// Bytes held now less bytes held when counting began. Negative when
    /// more was freed than allocated.
    pub live: i64,
    /// The most `live` reached since counting began.
    pub peak: i64,
}

impl Counts {
    /// No event at all.
    pub const ZERO: Self = Self {
        allocations: 0,
        reallocations: 0,
        moved: 0,
        frees: 0,
        bytes: 0,
        live: 0,
        peak: 0,
    };
    /// Allocations and reallocations together: every call that may have
    /// gone to the system for memory.
    pub fn calls(&self) -> u64 {
        self.allocations.saturating_add(self.reallocations)
    }
    /// The events of `self` that are not in `aside`: what the measured calls
    /// did when `aside` is what was set aside within the same count. Live
    /// and peak bytes are not divided (a block may be allocated by one and
    /// freed by the other), so they are zero here; the total's say them.
    pub fn less(&self, aside: &Self) -> Self {
        Self {
            allocations: self.allocations.saturating_sub(aside.allocations),
            reallocations: self.reallocations.saturating_sub(aside.reallocations),
            moved: self.moved.saturating_sub(aside.moved),
            frees: self.frees.saturating_sub(aside.frees),
            bytes: self.bytes.saturating_sub(aside.bytes),
            live: 0,
            peak: 0,
        }
    }
}

thread_local! {
    // Const-initialized `Cell`s of `Copy` values register no destructor, so
    // they are readable for the whole life of the thread, its teardown
    // included, and reading them never allocates.
    static COUNTS: Cell<Counts> = const { Cell::new(Counts::ZERO) };
    static ASIDE: Cell<Counts> = const { Cell::new(Counts::ZERO) };
    static ON: Cell<bool> = const { Cell::new(false) };
    static SET_ASIDE: Cell<bool> = const { Cell::new(false) };
}

/// Bytes as a signed count. A block is at most `isize::MAX` bytes
/// (`Layout`'s rule), so the conversion never saturates for one block.
fn signed(bytes: usize) -> i64 {
    i64::try_from(bytes).unwrap_or(i64::MAX)
}

/// Records one event on this thread when counting is on. Every counter wraps
/// at its width, which no run of this repository approaches (2^64 events);
/// wrapping is stated so that the allocator can never unwind.
fn note(record: impl Fn(&mut Counts)) {
    if !ON.try_with(Cell::get).unwrap_or(false) {
        return;
    }
    let apply = |cell: &Cell<Counts>| {
        let mut counts = cell.get();
        record(&mut counts);
        counts.peak = counts.peak.max(counts.live);
        cell.set(counts);
    };
    // A thread whose cells are gone records nothing; it cannot be counting.
    let _ = COUNTS.try_with(apply);
    if SET_ASIDE.try_with(Cell::get).unwrap_or(false) {
        let _ = ASIDE.try_with(apply);
    }
}

fn allocated(size: usize) {
    note(|counts| {
        counts.allocations = counts.allocations.wrapping_add(1);
        counts.bytes = counts
            .bytes
            .wrapping_add(u64::try_from(size).unwrap_or(u64::MAX));
        counts.live = counts.live.wrapping_add(signed(size));
    });
}

fn freed(size: usize) {
    note(|counts| {
        counts.frees = counts.frees.wrapping_add(1);
        counts.live = counts.live.wrapping_sub(signed(size));
    });
}

fn reallocated(old: usize, new: usize, moved: bool) {
    note(|counts| {
        counts.reallocations = counts.reallocations.wrapping_add(1);
        if moved {
            counts.moved = counts.moved.wrapping_add(1);
        }
        let grown = new.saturating_sub(old);
        counts.bytes = counts
            .bytes
            .wrapping_add(u64::try_from(grown).unwrap_or(u64::MAX));
        counts.live = counts
            .live
            .wrapping_add(signed(new))
            .wrapping_sub(signed(old));
    });
}

/// Zeroes this thread's counts and turns counting on.
pub fn begin() {
    let _ = COUNTS.try_with(|cell| cell.set(Counts::ZERO));
    let _ = ASIDE.try_with(|cell| cell.set(Counts::ZERO));
    let _ = SET_ASIDE.try_with(|aside| aside.set(false));
    let _ = ON.try_with(|on| on.set(true));
}
/// What follows is the harness's own work: counted in the total and apart,
/// until [`back`].
pub fn aside() {
    let _ = SET_ASIDE.try_with(|aside| aside.set(true));
}
/// What follows is the measured code's again.
pub fn back() {
    let _ = SET_ASIDE.try_with(|aside| aside.set(false));
}
/// What was set aside so far in this count.
pub fn read_aside() -> Counts {
    ASIDE.try_with(Cell::get).unwrap_or(Counts::ZERO)
}
/// Turns counting off on this thread, keeping the counts.
pub fn pause() {
    let _ = ON.try_with(|on| on.set(false));
}
/// Turns counting back on, adding to the counts kept.
pub fn resume() {
    let _ = ON.try_with(|on| on.set(true));
}
/// This thread's counts so far, counting left as it is.
pub fn read() -> Counts {
    COUNTS.try_with(Cell::get).unwrap_or(Counts::ZERO)
}
/// Turns counting off and gives this thread's counts.
pub fn end() -> Counts {
    pause();
    read()
}
/// Whether the allocator is counting on this thread: false when a binary did
/// not install [`Counting`], which a measurement checks before it trusts a
/// zero.
pub fn installed() -> bool {
    begin();
    // Kept from the optimizer, which may elide an allocation nothing reads.
    let probe: Vec<u8> = std::hint::black_box(Vec::with_capacity(1));
    let counts = end();
    drop(probe);
    counts.allocations > 0
}

/// The counting allocator. A binary installs it with
/// `#[global_allocator] static ALLOCATOR: Counting = Counting;`.
#[derive(Clone, Copy, Debug, Default)]
pub struct Counting;

// SAFETY: every method forwards to `System` with its arguments unchanged and
// returns what `System` returned, so the allocator keeps each guarantee
// `System` gives. What it adds only reads and writes this thread's `Cell`s,
// which never allocates, never reenters the allocator and never unwinds.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller keeps `GlobalAlloc::alloc`'s contract for
        // `layout` (a size that is not zero), which is `System::alloc`'s.
        let block = unsafe { System.alloc(layout) };
        if !block.is_null() {
            allocated(layout.size());
        }
        block
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: as `alloc`: the caller's contract for `layout` is
        // `System::alloc_zeroed`'s.
        let block = unsafe { System.alloc_zeroed(layout) };
        if !block.is_null() {
            allocated(layout.size());
        }
        block
    }
    unsafe fn dealloc(&self, block: *mut u8, layout: Layout) {
        // SAFETY: the caller gives a block this allocator returned for
        // `layout`, and every block this allocator returns is `System`'s,
        // allocated for that same layout.
        unsafe { System.dealloc(block, layout) };
        freed(layout.size());
    }
    unsafe fn realloc(&self, block: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: the caller gives a block this allocator, and so `System`,
        // returned for `layout`, and a `new_size` that is not zero and does
        // not overflow `isize` when rounded to `layout`'s alignment:
        // `System::realloc`'s contract.
        let moved = unsafe { System.realloc(block, layout, new_size) };
        if !moved.is_null() {
            reallocated(layout.size(), new_size, moved != block);
        }
        moved
    }
}
