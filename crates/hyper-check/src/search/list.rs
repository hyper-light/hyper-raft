//! Wing and Gong's linked list of a history's events (§4.1), as Horn and Kroening lift and unlift
//! them (Algorithm 2): each call and each return is an entry, in time order; linearizing an
//! operation lifts its two entries out in constant time, and undoing it puts them back in constant
//! time, for an entry lifted keeps its own links.

use super::SearchError;

/// The history's entries, in time order: a call before a return at the same time, for an
/// operation is a closed interval (Porcupine's rule: two stamps of one clock may be equal), so two
/// operations whose ends share a stamp are concurrent.
pub(crate) struct List {
    /// Each entry's successor; the head is the entry past the last.
    next: Vec<u32>,
    /// Each entry's predecessor.
    prev: Vec<u32>,
    /// Each entry's operation.
    op: Vec<u32>,
    /// Whether each entry is a return.
    returns: Vec<bool>,
    /// Each operation's call entry.
    call_at: Vec<u32>,
    /// Each operation's return entry.
    ret_at: Vec<u32>,
    head: u32,
}

/// The value an absent index reads as: the head, which ends every walk.
fn read(vec: &[u32], at: u32, or: u32) -> u32 {
    usize::try_from(at)
        .ok()
        .and_then(|at| vec.get(at))
        .copied()
        .unwrap_or(or)
}

fn write(vec: &mut [u32], at: u32, value: u32) {
    if let Some(slot) = usize::try_from(at).ok().and_then(|at| vec.get_mut(at)) {
        *slot = value;
    }
}

impl List {
    /// The list of operations called at `calls[i]` and returning at `rets[i]` (`None`: never,
    /// the end of time).
    pub(crate) fn new(calls: &[u64], rets: &[Option<u64>]) -> Result<Self, SearchError> {
        let ops = calls.len();
        let entries = ops.checked_mul(2).ok_or(SearchError::TooLong)?;
        let head = u32::try_from(entries).map_err(|_| SearchError::TooLong)?;
        let mut events: Vec<(u64, bool, u32)> = Vec::new();
        events
            .try_reserve_exact(entries)
            .map_err(|_| SearchError::Memory)?;
        for (op, (call, ret)) in calls.iter().zip(rets).enumerate() {
            let op = u32::try_from(op).map_err(|_| SearchError::TooLong)?;
            events.push((*call, false, op));
            events.push((ret.unwrap_or(u64::MAX), true, op));
        }
        events.sort_unstable();
        let mut list = Self {
            next: Vec::new(),
            prev: Vec::new(),
            op: Vec::new(),
            returns: Vec::new(),
            call_at: vec_of(ops, 0)?,
            ret_at: vec_of(ops, 0)?,
            head,
        };
        list.link(&events)?;
        Ok(list)
    }

    fn link(&mut self, events: &[(u64, bool, u32)]) -> Result<(), SearchError> {
        let slots = usize::try_from(self.head)
            .ok()
            .and_then(|head| head.checked_add(1))
            .ok_or(SearchError::TooLong)?;
        for vec in [&mut self.next, &mut self.prev, &mut self.op] {
            vec.try_reserve_exact(slots)
                .map_err(|_| SearchError::Memory)?;
        }
        self.returns
            .try_reserve_exact(slots)
            .map_err(|_| SearchError::Memory)?;
        for (at, (_, is_return, op)) in events.iter().enumerate() {
            let at = u32::try_from(at).map_err(|_| SearchError::TooLong)?;
            self.next
                .push(at.checked_add(1).ok_or(SearchError::TooLong)?);
            self.prev.push(at.checked_sub(1).unwrap_or(self.head));
            self.op.push(*op);
            self.returns.push(*is_return);
            let ends = if *is_return {
                &mut self.ret_at
            } else {
                &mut self.call_at
            };
            write(ends, *op, at);
        }
        // The head, before the first entry and after the last.
        let last = self.head.checked_sub(1).unwrap_or(self.head);
        self.next
            .push(if events.is_empty() { self.head } else { 0 });
        self.prev.push(last);
        self.op.push(u32::MAX);
        self.returns.push(true);
        Ok(())
    }

    /// Whether every operation is lifted.
    pub(crate) fn is_empty(&self) -> bool {
        self.next_of(self.head) == self.head
    }

    /// The first entry after the head.
    pub(crate) fn first(&self) -> u32 {
        self.next_of(self.head)
    }

    /// The entry after `entry`.
    pub(crate) fn next_of(&self, entry: u32) -> u32 {
        read(&self.next, entry, self.head)
    }

    /// `entry`'s operation.
    pub(crate) fn op_of(&self, entry: u32) -> u32 {
        read(&self.op, entry, u32::MAX)
    }

    /// Whether `entry` is a return (the head counts as one: it ends a walk).
    pub(crate) fn is_return(&self, entry: u32) -> bool {
        usize::try_from(entry)
            .ok()
            .and_then(|at| self.returns.get(at))
            .copied()
            .unwrap_or(true)
    }

    /// The first return left in the list: every call before it is of an operation that may be
    /// linearized next (Wing and Gong's minimal operations), and its own operation must be
    /// linearized before anything after it. The head when the list is empty.
    pub(crate) fn first_return(&self) -> u32 {
        let mut entry = self.first();
        while !self.is_return(entry) {
            entry = self.next_of(entry);
        }
        entry
    }

    /// Where `op` returns, as a place in the history.
    pub(crate) fn ret_of(&self, op: u32) -> u32 {
        read(&self.ret_at, op, self.head)
    }

    /// Where `op` is called.
    pub(crate) fn call_of(&self, op: u32) -> u32 {
        read(&self.call_at, op, self.head)
    }

    fn unlink(&mut self, entry: u32) {
        let (before, after) = (read(&self.prev, entry, self.head), self.next_of(entry));
        write(&mut self.next, before, after);
        write(&mut self.prev, after, before);
    }

    fn relink(&mut self, entry: u32) {
        let (before, after) = (read(&self.prev, entry, self.head), self.next_of(entry));
        write(&mut self.next, before, entry);
        write(&mut self.prev, after, entry);
    }

    /// `op` lifted out: its call, then its return (Horn and Kroening's LIFT).
    pub(crate) fn lift(&mut self, op: u32) {
        self.unlink(self.call_of(op));
        self.unlink(self.ret_of(op));
    }

    /// `op` put back, the reverse of [`List::lift`]: its return, then its call (UNLIFT).
    pub(crate) fn unlift(&mut self, op: u32) {
        self.relink(self.ret_of(op));
        self.relink(self.call_of(op));
    }
}

fn vec_of(len: usize, value: u32) -> Result<Vec<u32>, SearchError> {
    let mut vec = Vec::new();
    vec.try_reserve_exact(len)
        .map_err(|_| SearchError::Memory)?;
    vec.resize(len, value);
    Ok(vec)
}
