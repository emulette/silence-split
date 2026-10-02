//! Exact fixed-count DP with lower-bound pruning of predecessor ranges.
//!
//! Floating-point rounding can break Monge argmin ordering, even with convex real-valued costs.
//! This search prunes only ranges whose cost lower bound cannot improve the existing result.

use alloc::vec::Vec;
use core::ops::Range;

use super::{Lengths, Score, backtrack};

/// The cheapest path of exactly `count` pieces, limited by the candidate boundaries.
/// Keeps two score rows and four bytes per count and node for backtracking.
pub(crate) fn solve_count(
    left: &[u64],
    right: &[u64],
    cut: &[f64],
    lengths: &Lengths<'_>,
    count: usize,
) -> Vec<usize> {
    let m = left.len() - 1;
    let count = count.min(m);
    let first: Vec<usize> = left
        .iter()
        .enumerate()
        .map(|(j, &at)| {
            right[..j]
                .partition_point(|r| at - *r > lengths.max)
                .min(j.saturating_sub(1))
        })
        .collect();
    let mut previous = alloc::vec![Score { over: 0, cost: 0.0 }; m + 1];
    let mut parent = Vec::with_capacity(count + 1);
    parent.push(Vec::new());
    parent.push(alloc::vec![0u32; m + 1]);
    // On the first row only node 0 is reachable, including the legacy over-max exception.
    for j in 1..=m {
        let len = left[j] - right[0];
        previous[j] = Score {
            over: u32::from(len > lengths.max),
            cost: lengths.cost(len) + if j < m { cut[j] } else { 0.0 },
        };
    }
    let mut current = previous.clone();
    let mut tree = MinTree::new(m + 1);
    for k in 2..=count {
        tree.fill(&previous);
        let search = Search {
            right,
            previous: &previous,
            lengths,
            tree: &tree,
        };
        let mut row = alloc::vec![0u32; m + 1];
        for j in k..=m {
            let from = first[j].max(k - 1);
            let (mut score, mut best) = (search.score(left[j], j - 1), j - 1);
            search.visit(1, 0..tree.leaves, from..j, left[j], &mut score, &mut best);
            current[j] = Score {
                over: score.over,
                cost: score.cost + if j < m { cut[j] } else { 0.0 },
            };
            row[j] = best as u32;
        }
        parent.push(row);
        core::mem::swap(&mut previous, &mut current);
    }
    let mut k = count;
    backtrack(m, |j| {
        let i = parent[k][j] as usize;
        k -= 1;
        i
    })
}

/// Minimum predecessor score per interval, rebuilt in linear time for each DP row.
struct MinTree {
    leaves: usize,
    scores: Vec<Score>,
}

impl MinTree {
    fn new(n: usize) -> Self {
        let leaves = n.next_power_of_two();
        Self {
            leaves,
            scores: alloc::vec![Score { over: u32::MAX, cost: f64::INFINITY }; 2 * leaves],
        }
    }

    fn fill(&mut self, previous: &[Score]) {
        self.scores[self.leaves..self.leaves + previous.len()].copy_from_slice(previous);
        for i in (1..self.leaves).rev() {
            let (a, b) = (self.scores[2 * i], self.scores[2 * i + 1]);
            self.scores[i] = if a <= b { a } else { b };
        }
    }
}

struct Search<'a, 'w> {
    right: &'a [u64],
    previous: &'a [Score],
    lengths: &'a Lengths<'w>,
    tree: &'a MinTree,
}

impl Search<'_, '_> {
    fn score(&self, end: u64, i: usize) -> Score {
        let len = end - self.right[i];
        Score {
            over: self.previous[i].over + u32::from(len > self.lengths.max),
            cost: self.previous[i].cost + self.lengths.cost(len),
        }
    }

    /// Independent lower bounds on predecessor score, squared target distance and shortfall.
    /// Every operation on the nonnegative terms is monotone, including IEEE rounding. Using the
    /// same operation order as `Lengths::cost` makes this safe even at exact floating-point ties.
    fn bound(&self, node: usize, range: &Range<usize>, end: u64) -> Score {
        let min = end - self.right[range.end - 1];
        let max = end - self.right[range.start];
        let l = self.lengths;
        let closest = l.target.clamp(min as f64, max as f64);
        let off = (closest - l.target) / l.target;
        let mut cost = l.weights.target * off * off;
        if (max as f64) < l.min {
            cost += l.weights.short * (l.min - max as f64) / l.min;
        }
        let prior = self.tree.scores[node];
        Score {
            over: prior.over + u32::from(min > l.max),
            cost: prior.cost + cost,
        }
    }

    fn visit(
        &self,
        node: usize,
        range: Range<usize>,
        query: Range<usize>,
        end: u64,
        best: &mut Score,
        parent: &mut usize,
    ) {
        let active = range.start.max(query.start)..range.end.min(query.end);
        if active.is_empty() {
            return;
        }
        let lower = self.bound(node, &active, end);
        if lower > *best || (lower == *best && active.start >= *parent) {
            return;
        }
        if range.len() == 1 {
            let s = self.score(end, range.start);
            if s < *best || (s == *best && range.start < *parent) {
                *best = s;
                *parent = range.start;
            }
            return;
        }
        let mid = range.start + range.len() / 2;
        let a = range.start.max(query.start)..mid.min(query.end);
        let b = mid.max(query.start)..range.end.min(query.end);
        let right_first = !a.is_empty()
            && !b.is_empty()
            && self.bound(node * 2 + 1, &b, end) < self.bound(node * 2, &a, end);
        if right_first {
            self.visit(
                node * 2 + 1,
                mid..range.end,
                query.clone(),
                end,
                best,
                parent,
            );
            self.visit(node * 2, range.start..mid, query, end, best, parent);
        } else {
            self.visit(node * 2, range.start..mid, query.clone(), end, best, parent);
            self.visit(node * 2 + 1, mid..range.end, query, end, best, parent);
        }
    }
}
