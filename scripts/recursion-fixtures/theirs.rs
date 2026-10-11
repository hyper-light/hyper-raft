//! scripts/check-recursion.py's fixture, a crate standing for a registry crate: its cycles are
//! listed, never refused, even where the crate checked instantiates them.

/// Calls itself.
pub fn walk(depth: u64) -> u64 {
    if depth == 0 { 0 } else { walk(depth - 1).wrapping_add(1) }
}

/// Calls itself, for any `T`.
pub fn visit<T>(value: T, depth: u64) -> T {
    if depth == 0 { value } else { visit(value, depth - 1) }
}
