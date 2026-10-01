//! Dynamic programming over cut candidates.
//!
//! Nodes are in order of both their left and right positions. Node 0 starts the first piece and
//! the last node ends the last piece; a piece from node `i` to node `j` spans
//! `left[j] - right[i]` samples. A path costs the sum of its piece lengths' costs and of the cut
//! costs of its inner nodes.

use alloc::vec::Vec;

use crate::plan::Weights;

/// Piece length preferences in samples.
pub(crate) struct Lengths<'a> {
    pub(crate) min: f64,
    pub(crate) target: f64,
    pub(crate) max: u64,
    pub(crate) weights: &'a Weights,
}

impl Lengths<'_> {
    fn cost(&self, len: u64) -> f64 {
        let len = len as f64;
        let off = (len - self.target) / self.target;
        let mut cost = self.weights.target * off * off;
        if len < self.min {
            cost += self.weights.short * (self.min - len) / self.min;
        }
        cost
    }
}

/// A path's rank: first the number of pieces over the maximum, then the cost.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
struct Score {
    over: u32,
    cost: f64,
}

/// The best predecessor of node `j` among nodes `from..j`, given each node's score if reachable.
///
/// Predecessors within the maximum come first. When none is reachable, `nearest` is used, which
/// keeps the too-long piece as short as possible, and the piece is counted as over the maximum.
fn best_predecessor(
    j: usize,
    from: usize,
    nearest: usize,
    left: &[u64],
    right: &[u64],
    lengths: &Lengths<'_>,
    score: impl Fn(usize) -> Option<Score>,
) -> Option<(Score, usize)> {
    let mut best: Option<(Score, usize)> = None;
    // Walking back, `<=` keeps the earliest predecessor on ties: longer pieces, fewer cuts.
    for i in (from..j).rev() {
        let len = left[j] - right[i];
        if len > lengths.max {
            break;
        }
        if let Some(s) = score(i) {
            let v = Score {
                over: s.over,
                cost: s.cost + lengths.cost(len),
            };
            if best.is_none_or(|(b, _)| v <= b) {
                best = Some((v, i));
            }
        }
    }
    best.or_else(|| {
        let s = score(nearest)?;
        let cost = s.cost + lengths.cost(left[j] - right[nearest]);
        Some((
            Score {
                over: s.over + 1,
                cost,
            },
            nearest,
        ))
    })
}

/// The cheapest path from the first to the last node with every piece within `max`. Only when a
/// node has no predecessor within `max` does the path take a too-long piece from the node just
/// before it.
pub(crate) fn solve(left: &[u64], right: &[u64], cut: &[f64], lengths: &Lengths<'_>) -> Vec<usize> {
    let m = left.len() - 1;
    let mut score = alloc::vec![None; m + 1];
    let mut parent = alloc::vec![0; m + 1];
    score[0] = Some(Score { over: 0, cost: 0.0 });
    for j in 1..=m {
        if let Some((s, i)) = best_predecessor(j, 0, j - 1, left, right, lengths, |i| score[i]) {
            let cost = s.cost + if j < m { cut[j] } else { 0.0 };
            score[j] = Some(Score { over: s.over, cost });
            parent[j] = i;
        }
    }
    backtrack(m, |j| parent[j])
}

/// The cheapest path of exactly `count` pieces, or of as many as there are nodes to allow, with
/// the same rule for nodes without a predecessor within `max`.
///
/// Time is `O(count · nodes · nodes within max)`; memory is four bytes per count and node.
pub(crate) fn solve_count(
    left: &[u64],
    right: &[u64],
    cut: &[f64],
    lengths: &Lengths<'_>,
    count: usize,
) -> Vec<usize> {
    let m = left.len() - 1;
    let count = count.min(m);
    let mut previous = alloc::vec![None; m + 1];
    previous[0] = Some(Score { over: 0, cost: 0.0 });
    let mut parent: Vec<Vec<u32>> = Vec::with_capacity(count + 1);
    parent.push(Vec::new());
    for k in 1..=count {
        let mut current = alloc::vec![None; m + 1];
        let mut row = alloc::vec![0u32; m + 1];
        for j in k..=m {
            // After one piece only the start node is reachable; after more, every node from k - 1.
            let nearest = if k == 1 { 0 } else { j - 1 };
            let found = best_predecessor(j, k - 1, nearest, left, right, lengths, |i| previous[i]);
            if let Some((s, i)) = found {
                let cost = s.cost + if j < m { cut[j] } else { 0.0 };
                current[j] = Some(Score { over: s.over, cost });
                row[j] = i as u32;
            }
        }
        parent.push(row);
        previous = current;
    }
    let mut k = count;
    backtrack(m, |j| {
        let i = parent[k][j] as usize;
        k -= 1;
        i
    })
}

fn backtrack(last: usize, mut parent: impl FnMut(usize) -> usize) -> Vec<usize> {
    let mut path = alloc::vec![last];
    let mut j = last;
    while j > 0 {
        j = parent(j);
        path.push(j);
    }
    path.reverse();
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic xorshift for test inputs.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
    }

    fn path_cost(
        path: &[usize],
        left: &[u64],
        right: &[u64],
        cut: &[f64],
        l: &Lengths<'_>,
    ) -> Option<f64> {
        let mut total = 0.0;
        for w in path.windows(2) {
            let len = left[w[1]] - right[w[0]];
            if len > l.max {
                return None;
            }
            total += l.cost(len);
        }
        Some(total + path[1..path.len() - 1].iter().map(|j| cut[*j]).sum::<f64>())
    }

    /// Cheapest cost over every subset of inner nodes, optionally of a fixed piece count.
    fn exhaustive(
        left: &[u64],
        right: &[u64],
        cut: &[f64],
        l: &Lengths<'_>,
        count: Option<usize>,
    ) -> Option<f64> {
        let inner = left.len() - 2;
        (0u32..1 << inner)
            .filter(|mask| count.is_none_or(|c| mask.count_ones() as usize + 1 == c))
            .filter_map(|mask| {
                let mut path = alloc::vec![0];
                path.extend((0..inner).filter(|b| mask & (1 << b) != 0).map(|b| b + 1));
                path.push(inner + 1);
                path_cost(&path, left, right, cut, l)
            })
            .min_by(f64::total_cmp)
    }

    fn random_nodes(rng: &mut Rng) -> (Vec<u64>, Vec<u64>, Vec<f64>) {
        let nodes = 3 + (rng.next() % 10) as usize;
        let (mut left, mut right, mut cut) = (Vec::new(), Vec::new(), Vec::new());
        let mut at = 0;
        for i in 0..nodes {
            at += 1 + rng.next() % 40;
            let drop = if i == 0 || i + 1 == nodes {
                0
            } else {
                rng.next() % 3 * (rng.next() % 5)
            };
            left.push(at);
            right.push(at + drop);
            at += drop;
            cut.push((rng.next() % 1000) as f64 / 500.0 + 0.1);
        }
        (left, right, cut)
    }

    #[test]
    fn matches_exhaustive_search() {
        let weights = Weights::default();
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        for _ in 0..500 {
            let (left, right, cut) = random_nodes(&mut rng);
            let min = (rng.next() % 30) as f64;
            let target = min + 1.0 + (rng.next() % 40) as f64;
            let max = target as u64 + rng.next() % 60;
            let l = Lengths {
                min,
                target,
                max,
                weights: &weights,
            };

            if let Some(best) = exhaustive(&left, &right, &cut, &l, None) {
                let path = solve(&left, &right, &cut, &l);
                let got = path_cost(&path, &left, &right, &cut, &l).expect("within max");
                assert!((got - best).abs() < 1e-9, "dp {got} vs exhaustive {best}");
            }
            let count = 1 + (rng.next() % 4) as usize;
            if let Some(best) = exhaustive(&left, &right, &cut, &l, Some(count)) {
                let path = solve_count(&left, &right, &cut, &l, count);
                assert_eq!(path.len(), count + 1);
                let got = path_cost(&path, &left, &right, &cut, &l).expect("within max");
                assert!(
                    (got - best).abs() < 1e-9,
                    "count dp {got} vs exhaustive {best}"
                );
            }
        }
    }

    #[test]
    fn ties_keep_fewer_cuts() {
        let weights = Weights {
            target: 0.0,
            ..Weights::default()
        };
        let l = Lengths {
            min: 0.0,
            target: 10.0,
            max: 100,
            weights: &weights,
        };
        let path = solve(&[0, 50, 100], &[0, 50, 100], &[0.0, 0.0, 0.0], &l);
        assert_eq!(path, [0, 2]);
    }

    #[test]
    fn without_a_predecessor_within_max_the_nearest_node_is_used() {
        let weights = Weights::default();
        let l = Lengths {
            min: 0.0,
            target: 10.0,
            max: 10,
            weights: &weights,
        };
        let nodes = [0, 20, 40, 41];
        let cut = [0.0, 1.0, 1.0, 0.0];
        assert_eq!(solve(&nodes, &nodes, &cut, &l), [0, 1, 2, 3]);
        assert_eq!(solve_count(&nodes, &nodes, &cut, &l, 2), [0, 2, 3]);
        assert_eq!(solve_count(&nodes, &nodes, &cut, &l, 9), [0, 1, 2, 3]);
    }
}
