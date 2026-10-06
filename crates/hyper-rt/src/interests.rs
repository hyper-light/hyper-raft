//! Who waits on which handle, in which direction (docs/runtime.md §3.7): the loop's table between the
//! tasks' readiness waits and the driver's one registration per handle.
//!
//! A driver keeps one registration per handle: epoll's `EPOLL_CTL_MOD` replaces the mask and the user word
//! together, kqueue's `EV_ADD` replaces a filter's `udata`, and an AFD poll is one request per socket. So
//! the loop keeps the waiters itself and arms the driver once per handle with the union of the directions
//! waited for; a completion says which directions fired, and each waiter of those is woken. A reader and a
//! writer of one full-duplex stream, or two readers of one socket, each get their wake (mantle's review of
//! hyper-rt, finding 2: a writable registration used to erase the readable one, and the reader slept for
//! good).
//!
//! **Bounded, and allocation-free after the build**: at most `bound` waiters (the shard's
//! `interests_per_shard`), in an arena of that many nodes with a free list, indexed by a map reserved for
//! that many handles; past the bound, `Capacity`. A word waits at most once per handle and direction (a
//! task re-arming its wait does not add itself twice).

use std::collections::HashMap;

use crate::error::RtError;

/// Format: the bit that marks a completion's `user_data` as a handle's tag, not a task's word. A task word
/// is `shard:16 | slot:24 | generation:24` with the shard below `registry::MAX_SHARDS` (1,024), so its top
/// bit is never set.
pub const HANDLE_TAG: u64 = 1 << 63;

/// The tag a handle's readiness completions carry.
pub fn tag_of(raw: i32) -> u64 {
    HANDLE_TAG | u64::from(u32::from_ne_bytes(raw.to_ne_bytes()))
}

/// The handle a completion's tag names, if it is one.
pub fn handle_of(user_data: u64) -> Option<i32> {
    if user_data & HANDLE_TAG == 0 {
        return None;
    }
    let low = u32::try_from(user_data & u64::from(u32::MAX)).ok()?;
    Some(i32::from_ne_bytes(low.to_ne_bytes()))
}

/// Directions of readiness, as a set.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Readiness(u8);

impl Readiness {
    /// Nothing.
    pub const NONE: Readiness = Readiness(0);
    /// Readable: data, a connection to accept, end of stream, an error.
    pub const READ: Readiness = Readiness(1);
    /// Writable: send-buffer space, a connect's result, an error.
    pub const WRITE: Readiness = Readiness(2);

    /// One direction.
    pub const fn of(writable: bool) -> Readiness {
        if writable { Self::WRITE } else { Self::READ }
    }

    /// Whether `other`'s directions are all in this set.
    pub const fn contains(self, other: Readiness) -> bool {
        self.0 & other.0 == other.0 && other.0 != 0
    }

    /// Whether the set is empty.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Both sets' directions.
    #[must_use]
    pub const fn union(self, other: Readiness) -> Readiness {
        Readiness(self.0 | other.0)
    }

    /// The set as bits, for a completion's `result`.
    pub fn bits(self) -> i32 {
        i32::from(self.0)
    }

    /// The set a completion's `result` names.
    pub fn from_bits(bits: i32) -> Readiness {
        Readiness(u8::try_from(bits & 0b11).unwrap_or(0))
    }
}

/// Format: the end of a list.
const END: u32 = u32::MAX;

/// One waiter: its task word and the next waiter of the same handle and direction.
#[derive(Clone, Copy, Debug)]
struct Node {
    word: u64,
    next: u32,
}

/// One handle's waiters: a list per direction.
#[derive(Clone, Copy, Debug)]
struct Lists {
    read: u32,
    write: u32,
}

impl Lists {
    fn head(&mut self, writable: bool) -> &mut u32 {
        if writable {
            &mut self.write
        } else {
            &mut self.read
        }
    }

    /// The directions with waiters.
    fn wanted(self) -> Readiness {
        let read = if self.read == END {
            Readiness::NONE
        } else {
            Readiness::READ
        };
        let write = if self.write == END {
            Readiness::NONE
        } else {
            Readiness::WRITE
        };
        read.union(write)
    }
}

/// The table.
#[derive(Debug)]
pub(crate) struct Interests {
    nodes: Vec<Node>,
    free: Vec<u32>,
    handles: HashMap<i32, Lists>,
    bound: usize,
}

impl Interests {
    /// A table for at most `bound` waiters, reserved now. `Capacity` when the reservation fails.
    pub(crate) fn new(bound: usize) -> Result<Interests, RtError> {
        let refused = || RtError::Capacity {
            what: "readiness waiters",
            bound,
        };
        let mut nodes = Vec::new();
        nodes.try_reserve_exact(bound).map_err(|_| refused())?;
        let mut free = Vec::new();
        free.try_reserve_exact(bound).map_err(|_| refused())?;
        let mut handles = HashMap::new();
        handles.try_reserve(bound).map_err(|_| refused())?;
        Ok(Interests {
            nodes,
            free,
            handles,
            bound,
        })
    }

    /// Waiters now.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.nodes.len().saturating_sub(self.free.len())
    }

    /// A free node holding `word`, or `Capacity`.
    fn node(&mut self, word: u64) -> Result<u32, RtError> {
        if let Some(index) = self.free.pop()
            && let Some(node) = self
                .nodes
                .get_mut(usize::try_from(index).unwrap_or(usize::MAX))
        {
            *node = Node { word, next: END };
            return Ok(index);
        }
        if self.nodes.len() >= self.bound {
            return Err(RtError::Capacity {
                what: "readiness waiters",
                bound: self.bound,
            });
        }
        let index = u32::try_from(self.nodes.len()).map_err(|_| RtError::Capacity {
            what: "readiness waiters",
            bound: self.bound,
        })?;
        // Within the reservation: no allocation.
        self.nodes.push(Node { word, next: END });
        Ok(index)
    }

    fn get(&self, index: u32) -> Option<Node> {
        self.nodes.get(usize::try_from(index).ok()?).copied()
    }

    /// Adds `word` as a waiter of `raw` in one direction; the directions now wanted of `raw`, which the loop
    /// arms. A word already waiting there is not added again.
    pub(crate) fn add(
        &mut self,
        raw: i32,
        writable: bool,
        word: u64,
    ) -> Result<Readiness, RtError> {
        let mut lists = self.handles.get(&raw).copied().unwrap_or(Lists {
            read: END,
            write: END,
        });
        let head = *lists.head(writable);
        let mut at = head;
        // A list is as long as the waiters of one handle and direction, bounded by `bound`.
        for _ in 0..self.bound {
            let Some(node) = self.get(at) else {
                break;
            };
            if node.word == word {
                return Ok(lists.wanted());
            }
            at = node.next;
        }
        let index = self.node(word)?;
        if let Some(node) = self
            .nodes
            .get_mut(usize::try_from(index).unwrap_or(usize::MAX))
        {
            node.next = head;
        }
        *lists.head(writable) = index;
        if self.handles.len() >= self.bound && !self.handles.contains_key(&raw) {
            self.free.push(index);
            return Err(RtError::Capacity {
                what: "readiness handles",
                bound: self.bound,
            });
        }
        // Reserved for `bound` handles, and there are fewer: no allocation.
        self.handles.insert(raw, lists);
        Ok(lists.wanted())
    }

    /// Removes `word` from `raw`'s waiters in one direction (a wait dropped before it fired); the directions
    /// still wanted.
    pub(crate) fn remove(&mut self, raw: i32, writable: bool, word: u64) -> Readiness {
        let Some(mut lists) = self.handles.get(&raw).copied() else {
            return Readiness::NONE;
        };
        let mut previous = END;
        let mut at = *lists.head(writable);
        for _ in 0..self.bound {
            let Some(node) = self.get(at) else {
                break;
            };
            if node.word == word {
                if previous == END {
                    *lists.head(writable) = node.next;
                } else if let Some(before) = self
                    .nodes
                    .get_mut(usize::try_from(previous).unwrap_or(usize::MAX))
                {
                    before.next = node.next;
                }
                self.free.push(at);
                break;
            }
            previous = at;
            at = node.next;
        }
        self.settle(raw, lists)
    }

    /// Wakes every waiter of `raw` in the directions `fired`, through `wake`; the directions still wanted,
    /// which the loop arms again.
    pub(crate) fn fire(
        &mut self,
        raw: i32,
        fired: Readiness,
        mut wake: impl FnMut(u64),
    ) -> Readiness {
        let Some(mut lists) = self.handles.get(&raw).copied() else {
            return Readiness::NONE;
        };
        for writable in [false, true] {
            if !fired.contains(Readiness::of(writable)) {
                continue;
            }
            let mut at = std::mem::replace(lists.head(writable), END);
            for _ in 0..self.bound {
                let Some(node) = self.get(at) else {
                    break;
                };
                wake(node.word);
                self.free.push(at);
                at = node.next;
            }
        }
        self.settle(raw, lists)
    }

    /// Stores `raw`'s lists, or forgets the handle when none waits; the directions wanted.
    fn settle(&mut self, raw: i32, lists: Lists) -> Readiness {
        let wanted = lists.wanted();
        if wanted.is_empty() {
            self.handles.remove(&raw);
        } else {
            self.handles.insert(raw, lists);
        }
        wanted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reader_and_a_writer_of_one_handle_each_get_their_wake() {
        let mut table = Interests::new(8).unwrap();
        assert_eq!(table.add(5, false, 100).unwrap(), Readiness::READ);
        assert_eq!(
            table.add(5, true, 200).unwrap(),
            Readiness::READ.union(Readiness::WRITE)
        );
        let mut woken = Vec::new();
        let left = table.fire(5, Readiness::WRITE, |word| woken.push(word));
        assert_eq!(woken, vec![200]);
        assert_eq!(left, Readiness::READ, "the reader still waits, armed again");
        let left = table.fire(5, Readiness::READ, |word| woken.push(word));
        assert_eq!(woken, vec![200, 100]);
        assert!(left.is_empty());
        assert_eq!(table.len(), 0);
    }

    #[test]
    fn two_readers_both_wake_and_a_repeat_is_not_added_twice() {
        let mut table = Interests::new(8).unwrap();
        table.add(3, false, 1).unwrap();
        table.add(3, false, 2).unwrap();
        table.add(3, false, 1).unwrap();
        assert_eq!(table.len(), 2);
        let mut woken = Vec::new();
        table.fire(3, Readiness::READ, |word| woken.push(word));
        woken.sort_unstable();
        assert_eq!(woken, vec![1, 2]);
    }

    #[test]
    fn a_dropped_wait_leaves_and_the_bound_holds() {
        let mut table = Interests::new(2).unwrap();
        table.add(1, false, 10).unwrap();
        table.add(2, true, 20).unwrap();
        assert!(matches!(
            table.add(3, false, 30),
            Err(RtError::Capacity { .. })
        ));
        assert_eq!(table.remove(1, false, 10), Readiness::NONE);
        assert_eq!(
            table.add(3, false, 30).unwrap(),
            Readiness::READ,
            "the freed node is reused"
        );
        let mut woken = Vec::new();
        table.fire(1, Readiness::READ, |word| woken.push(word));
        assert!(woken.is_empty(), "the dropped wait is not woken");
    }
}
