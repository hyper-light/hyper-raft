//! scripts/check-recursion.py's fixture, the crate whose cycles it refuses: each kind of cycle the
//! checker must refuse, and a loop and others' cycles it must not.
#![allow(dead_code)]

/// Calls itself.
pub fn walk(depth: u64) -> u64 {
    if depth == 0 { 0 } else { walk(depth - 1).wrapping_add(1) }
}

/// Calls `odd`, which calls it.
pub fn even(n: u64) -> bool {
    if n == 0 { true } else { odd(n - 1) }
}

/// See `even`.
pub fn odd(n: u64) -> bool {
    if n == 0 { false } else { even(n - 1) }
}

/// A type holding itself: its drop glue runs its own.
pub struct Node {
    pub next: Option<Box<Node>>,
}

/// A type holding a channel of itself: dropping it can drop the values of it the channel holds.
pub struct Reply(pub Option<std::sync::mpsc::SyncSender<Reply>>);

/// This crate's trait, implemented on a foreign type: the impl's code is this crate's.
pub trait Visit {
    fn visit(&self, depth: u64) -> u64;
}

impl Visit for u64 {
    fn visit(&self, depth: u64) -> u64 {
        if depth == 0 { *self } else { self.visit(depth - 1) }
    }
}

/// A type std sorts: its sort's recursion is std's.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
pub struct Key(pub u64);

/// A loop, no recursion.
pub fn iterate(mut n: u64) -> u64 {
    let mut total = 0u64;
    while n > 0 {
        total = total.wrapping_add(step(n));
        n -= 1;
    }
    total
}

fn step(n: u64) -> u64 {
    n.wrapping_mul(2)
}

/// Compiles the generic code above where this crate instantiates it: each type's drop glue, std's
/// sort over `Key`, the other crate's generic recursion over `Key`.
pub fn instantiate(node: Node, reply: Reply, keys: &mut [Key]) -> u64 {
    drop(node);
    drop(reply);
    keys.sort();
    theirs_fixture::visit(Key(1), 3);
    theirs_fixture::walk(3)
}
