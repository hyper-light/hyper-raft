//! A bounded table of values under generational identifiers (CLAUDE.md §1: arenas with
//! generational handles). An identifier names one value for ever: a freed slot's generation moves
//! on, so a stale identifier finds nothing, and a slot whose generation is exhausted is retired
//! instead of reused (mantle note 32 T4: identifiers that cannot wrap into an acknowledged one).

use crate::Refusal;

/// The bits of an identifier that name the slot; the rest are its generation.
const SLOT_BITS: u32 = 32;

#[derive(Debug)]
struct Slot<T> {
    generation: u32,
    value: Option<T>,
}

/// A table of at most `capacity` values.
#[derive(Debug)]
pub(crate) struct Arena<T> {
    slots: Vec<Slot<T>>,
    free: Vec<u32>,
    capacity: usize,
    live: usize,
}

impl<T> Arena<T> {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            slots: Vec::new(),
            free: Vec::new(),
            capacity,
            live: 0,
        }
    }
    /// Store `value`, or refuse: [`Refusal::Exchanges`] at the bound, [`Refusal::Exhausted`]
    /// when every slot's generations are spent.
    pub(crate) fn insert(&mut self, value: T) -> Result<u64, Refusal> {
        if self.live >= self.capacity {
            return Err(Refusal::Exchanges);
        }
        let index = match self.free.pop() {
            Some(index) => index,
            None => {
                let index = u32::try_from(self.slots.len()).map_err(|_| Refusal::Exhausted)?;
                self.slots.push(Slot {
                    generation: 0,
                    value: None,
                });
                index
            }
        };
        let slot = self
            .slots
            .get_mut(usize::try_from(index).map_err(|_| Refusal::Exhausted)?);
        let slot = slot.ok_or(Refusal::Exhausted)?;
        slot.value = Some(value);
        self.live = self.live.saturating_add(1);
        Ok((u64::from(slot.generation) << SLOT_BITS) | u64::from(index))
    }
    fn split(id: u64) -> Option<(usize, u32)> {
        let index = usize::try_from(id & u64::from(u32::MAX)).ok()?;
        let generation = u32::try_from(id >> SLOT_BITS).ok()?;
        Some((index, generation))
    }
    pub(crate) fn get(&self, id: u64) -> Option<&T> {
        let (index, generation) = Self::split(id)?;
        let slot = self.slots.get(index)?;
        (slot.generation == generation)
            .then_some(slot.value.as_ref())
            .flatten()
    }
    pub(crate) fn get_mut(&mut self, id: u64) -> Option<&mut T> {
        let (index, generation) = Self::split(id)?;
        let slot = self.slots.get_mut(index)?;
        if slot.generation != generation {
            return None;
        }
        slot.value.as_mut()
    }
    /// Take the value out; its identifier names nothing from now on.
    pub(crate) fn remove(&mut self, id: u64) -> Option<T> {
        let (index, generation) = Self::split(id)?;
        let slot = self.slots.get_mut(index)?;
        if slot.generation != generation {
            return None;
        }
        let value = slot.value.take()?;
        self.live = self.live.saturating_sub(1);
        if let Some(next) = slot.generation.checked_add(1) {
            slot.generation = next;
            if let Ok(index) = u32::try_from(index) {
                self.free.push(index);
            }
        }
        Some(value)
    }
    pub(crate) fn values(&self) -> impl Iterator<Item = &T> {
        self.slots.iter().filter_map(|slot| slot.value.as_ref())
    }
    pub(crate) fn len(&self) -> usize {
        self.live
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stale_identifier_names_nothing_and_the_bound_refuses() {
        let mut arena = Arena::new(2);
        let first = arena.insert("a").unwrap();
        let second = arena.insert("b").unwrap();
        assert_eq!(arena.insert("c"), Err(Refusal::Exchanges));
        assert_eq!(arena.remove(first), Some("a"));
        assert_eq!(arena.get(first), None);
        let third = arena.insert("c").unwrap();
        assert_ne!(third, first, "the slot is reused under a new generation");
        assert_eq!(
            (arena.get(third), arena.get(second)),
            (Some(&"c"), Some(&"b"))
        );
        assert_eq!(arena.remove(first), None);
        assert_eq!((arena.len(), arena.values().count()), (2, 2));
    }

    #[test]
    fn an_exhausted_slot_is_retired() {
        let mut arena = Arena::new(1);
        let id = arena.insert(1).unwrap();
        arena.slots[0].generation = u32::MAX;
        let last = (u64::from(u32::MAX) << SLOT_BITS) | (id & u64::from(u32::MAX));
        assert_eq!(arena.remove(last), Some(1));
        assert!(
            arena.free.is_empty(),
            "a slot past its last generation is not reused"
        );
        let next = arena.insert(2).unwrap();
        assert_eq!(next & u64::from(u32::MAX), 1);
    }
}
