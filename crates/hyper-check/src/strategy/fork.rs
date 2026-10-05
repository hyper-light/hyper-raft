//! Crash enumeration by fork (`docs/sim.md` §3.7): a schedule runs once, and at each point a crash
//! after it would cut into, the world is cloned and the clone runs the crash's branch to its end.
//! The replay of the prefix for every crash point, which made crash at every event quadratic in
//! the run (§1.5 item 4), is gone: the steps saved are the sum of the points' positions.
//!
//! **Bounded children.** One branch is alive at a time: the clone is made, run to its end and
//! dropped before the schedule takes its next step (§7, "Forks alive: 1 + depth of nesting; crash
//! enumeration needs one"). Nothing is shared: the world is a value, its clone a second owner of
//! its own copy, so a branch cannot reach the trunk.

/// What a crash enumeration did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Forked<O> {
    /// Each branch's outcome, in the order of its point.
    pub branches: Vec<O>,
    /// Steps the trunk took.
    pub steps: u64,
}

/// `world` driven by `step` for at most `steps` steps (fewer when `step` returns `None`, the
/// schedule ended); after each step that returns a point, a clone of the world is given with the
/// point to `branch`, run to its end there, and dropped before the trunk goes on.
pub fn each_point<W: Clone, P, O>(
    world: &mut W,
    steps: u64,
    mut step: impl FnMut(&mut W) -> Option<Option<P>>,
    mut branch: impl FnMut(W, P) -> O,
) -> Forked<O> {
    let mut forked = Forked {
        branches: Vec::new(),
        steps: 0,
    };
    for _ in 0..steps {
        let Some(point) = step(world) else {
            break;
        };
        forked.steps = forked.steps.saturating_add(1);
        if let Some(point) = point {
            let child = world.clone();
            forked.branches.push(branch(child, point));
        }
    }
    forked
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A world whose run is a sum; a crash zeroes it.
    #[derive(Clone)]
    struct Sum {
        total: u64,
        at: u64,
    }

    fn advance(world: &mut Sum) -> Option<Option<u64>> {
        if world.at >= 10 {
            return None;
        }
        world.at += 1;
        world.total += world.at;
        Some(world.at.is_multiple_of(3).then_some(world.at))
    }

    #[test]
    fn each_branch_gives_what_a_replay_to_its_point_gives() {
        let mut trunk = Sum { total: 0, at: 0 };
        let forked = each_point(&mut trunk, 100, advance, |mut child, point| {
            child.total = 0;
            while advance(&mut child).is_some() {}
            (point, child.total)
        });
        for (point, total) in &forked.branches {
            let mut replay = Sum { total: 0, at: 0 };
            while replay.at < *point {
                advance(&mut replay);
            }
            replay.total = 0;
            while advance(&mut replay).is_some() {}
            assert_eq!(replay.total, *total);
        }
        assert_eq!(forked.branches.len(), 3);
        assert_eq!(forked.steps, 10);
    }
}
