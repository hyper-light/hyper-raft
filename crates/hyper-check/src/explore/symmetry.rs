//! A class's representative under renaming members (`docs/sim.md` §4.5; slates'
//! `least_over_ties`; FlyMC §4.1's state symmetry): members are sorted by a signature that
//! renaming leaves unchanged, and among the orders that permute only members of equal signature,
//! the one whose packed state is least is the representative. Every renaming of a state reaches
//! the same sorted signatures and the same set of tie orders, so the same least key.

/// Every order of `order[at..end]`, and for each the orders of the runs after.
fn permute_run(
    order: &mut [usize],
    at: usize,
    end: usize,
    runs: &[(usize, usize)],
    visit: &mut dyn FnMut(&[usize]),
) {
    if at.saturating_add(1) >= end {
        each_tie_order(order, runs, visit);
        return;
    }
    for swap in at..end {
        order.swap(at, swap);
        permute_run(order, at.saturating_add(1), end, runs, visit);
        order.swap(at, swap);
    }
}

/// Every order that permutes `order` only within `runs`, half-open ranges of equal signatures.
fn each_tie_order(order: &mut [usize], runs: &[(usize, usize)], visit: &mut dyn FnMut(&[usize])) {
    match runs.split_first() {
        None => visit(order),
        Some((&(start, end), rest)) => permute_run(order, start, end, rest, visit),
    }
}

/// The least key `arrange` gives over the orders of the members sorted by `signatures` that permute
/// only members of equal signature: `arrange` places the member at `order[k]` as member `k`. `None`
/// only for no members.
pub fn least_over_ties<K: Ord + Copy, S: Ord>(
    signatures: &[S],
    arrange: &mut dyn FnMut(&[usize]) -> K,
) -> Option<K> {
    let mut order: Vec<usize> = (0..signatures.len()).collect();
    order.sort_by(|left, right| signatures.get(*left).cmp(&signatures.get(*right)));
    let mut runs = Vec::new();
    let mut start = 0;
    for end in 1..=order.len() {
        let differs = end == order.len()
            || order.get(end).and_then(|at| signatures.get(*at))
                != order.get(start).and_then(|at| signatures.get(*at));
        if differs {
            if end.saturating_sub(start) > 1 {
                runs.push((start, end));
            }
            start = end;
        }
    }
    let mut least: Option<K> = None;
    each_tie_order(&mut order, &runs, &mut |candidate| {
        let key = arrange(candidate);
        if least.is_none_or(|least| key < least) {
            least = Some(key);
        }
    });
    least
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_renaming_reaches_one_representative() {
        // Three members, two alike: the representative of each renaming of the values is the same.
        let values = [[5u8, 1, 1], [1, 5, 1], [1, 1, 5]];
        let keys: Vec<Vec<u8>> = values
            .iter()
            .map(|state| {
                let signatures: Vec<u8> = state.to_vec();
                let packed = least_over_ties(&signatures, &mut |order| {
                    let mut out = [0u8; 3];
                    for (new, old) in order.iter().enumerate() {
                        out[new] = state[*old];
                    }
                    out
                });
                packed.unwrap().to_vec()
            })
            .collect();
        assert!(keys.windows(2).all(|pair| pair[0] == pair[1]), "{keys:?}");
    }
}
