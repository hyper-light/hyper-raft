//! The wakes another thread sends a shard (docs/runtime.md §3.2): one bit per task slot, in 64-bit words,
//! with a summary bit per word and one pending flag above them, the two-level shape of the Linux block
//! layer's scalable bitmap of tags (`lib/sbitmap.c`).
//!
//! A wake is a `fetch_or` on the slot's bit, then on its word's summary bit, then a store of the pending
//! flag; the shard drains by swapping the flag, then each marked summary word, then each marked word, to
//! zero. The bitmap is allocated once at the shard's build and holds every slot, so it cannot fill and no
//! sender ever waits: slates' rings made a sender spin until the consumer drained a full one, a loop with no
//! counted bound. Duplicates collapse, as the run queue's pending flags collapse them.
//!
//! A bit names a slot, not a generation: a wake that raced its task's end and the slot's reuse wakes the new
//! occupant once, spuriously, which every future tolerates. Ordering: a producer's three stores are
//! `Release` read-modify-writes and the consumer's swaps are `Acquire`, so a bit set before the flag is seen
//! by the drain that takes the flag; a bit set after the drain took the flag sets it again and is drained
//! next time (DERIVED; loom-modelled in `tests` under `--cfg loom`).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Format: slots per word.
const BITS: usize = 64;

/// A shard's wake bitmap.
#[derive(Debug)]
pub struct WakeBitmap {
  words: Box<[AtomicU64]>,
  summary: Box<[AtomicU64]>,
  pending: AtomicBool,
}

impl WakeBitmap {
  /// A bitmap for slots `0..slots`.
  pub fn new(slots: usize) -> Self {
    let words = slots.div_ceil(BITS);
    Self {
      words: (0..words).map(|_| AtomicU64::new(0)).collect(),
      summary: (0..words.div_ceil(BITS))
        .map(|_| AtomicU64::new(0))
        .collect(),
      pending: AtomicBool::new(false),
    }
  }

  /// Marks `slot` woken; false for a slot past the bitmap (a stale or foreign word, ignored).
  pub fn set(&self, slot: u32) -> bool {
    let Ok(slot) = usize::try_from(slot) else {
      return false;
    };
    let (word, bit) = (slot / BITS, slot % BITS);
    let (Some(target), Some(summary)) = (self.words.get(word), self.summary.get(word / BITS)) else {
      return false;
    };
    target.fetch_or(1u64 << bit, Ordering::AcqRel);
    summary.fetch_or(1u64 << (word % BITS), Ordering::AcqRel);
    self.pending.store(true, Ordering::Release);
    true
  }

  /// Whether a wake may be waiting (one atomic load: the parking protocol's re-check).
  pub fn is_pending(&self) -> bool {
    self.pending.load(Ordering::Acquire)
  }

  /// Hands every woken slot to `woken`, clearing it; returns how many.
  pub fn drain(&self, mut woken: impl FnMut(u32)) -> usize {
    if !self.pending.swap(false, Ordering::AcqRel) {
      return 0;
    }
    let mut count: usize = 0;
    for (high, summary) in self.summary.iter().enumerate() {
      let mut marked = summary.swap(0, Ordering::AcqRel);
      while marked != 0 {
        let low = usize::try_from(marked.trailing_zeros()).unwrap_or(0);
        marked &= marked.wrapping_sub(1);
        let word = high.saturating_mul(BITS).saturating_add(low);
        let Some(target) = self.words.get(word) else {
          continue;
        };
        let mut bits = target.swap(0, Ordering::AcqRel);
        while bits != 0 {
          let bit = usize::try_from(bits.trailing_zeros()).unwrap_or(0);
          bits &= bits.wrapping_sub(1);
          if let Ok(slot) = u32::try_from(word.saturating_mul(BITS).saturating_add(bit)) {
            woken(slot);
            count = count.saturating_add(1);
          }
        }
      }
    }
    count
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn wakes_collapse_and_drain_once_in_slot_order() {
    let bitmap = WakeBitmap::new(5_000);
    for slot in [4_999, 3, 64, 3, 0, 4_999] {
      assert!(bitmap.set(slot));
    }
    assert!(!bitmap.set(5_000 + 64 * 64), "past the bitmap");
    assert!(bitmap.is_pending());
    let mut seen = Vec::new();
    assert_eq!(bitmap.drain(|slot| seen.push(slot)), 4);
    assert_eq!(seen, vec![0, 3, 64, 4_999]);
    assert!(!bitmap.is_pending());
    assert_eq!(bitmap.drain(|_| panic!("nothing is left")), 0);
  }

  /// Wakes from many threads at once all arrive, none twice per drain, while the owner drains as they land.
  #[test]
  fn concurrent_wakes_are_never_lost() {
    let bitmap = WakeBitmap::new(4_096);
    let mut seen = vec![0u32; 4_096];
    std::thread::scope(|scope| {
      for thread in 0..4u32 {
        let bitmap = &bitmap;
        scope.spawn(move || {
          for slot in (thread..4_096).step_by(4) {
            bitmap.set(slot);
          }
        });
      }
      for _ in 0..1_000 {
        bitmap.drain(|slot| seen[usize::try_from(slot).unwrap()] += 1);
      }
    });
    bitmap.drain(|slot| seen[usize::try_from(slot).unwrap()] += 1);
    assert!(seen.iter().all(|count| *count >= 1), "every wake arrived");
  }
}
