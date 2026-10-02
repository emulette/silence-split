//! Dynamic programming over cut candidates.
//!
//! Nodes are in order of both their left and right positions. Node 0 starts the first piece and
//! the last node ends the last piece; a piece from node `i` to node `j` spans
//! `left[j] - right[i]` samples. A path costs the sum of its piece lengths' costs and of the cut
//! costs of its inner nodes.

use alloc::vec::Vec;

use crate::plan::Weights;

mod count;
pub(crate) use count::solve_count;

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
mod tests;
