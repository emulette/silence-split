//! Supplement the acoustic candidates with paths that satisfy the length and count limits.

use alloc::vec::Vec;

use super::{Candidate, Context};

impl Context<'_> {
    /// Adds only the boundaries needed to bridge candidate gaps larger than `max`.
    /// Padding may yield to the hard maximum, but no point is inserted inside a dropped range.
    pub(crate) fn complete_max(&self, nodes: &mut Vec<Candidate>, max: u64) {
        let mut added = Vec::new();
        for pair in nodes.windows(2) {
            let (mut at, end) = (pair[0].right, pair[1].left);
            while self.analysis.position(end) - self.analysis.position(at) > max {
                // A frame longer than max cannot be split; retain it as a flagged piece.
                let next = self.reachable(at, end, max).max(at + 1);
                if next == end {
                    break;
                }
                added.push(self.point(next));
                at = next;
            }
        }
        merge(nodes, added);
    }

    /// Adds a feasible count path on the full frame grid. Only if that grid cannot meet `max`
    /// is the bound raised to the smallest attainable maximum piece length.
    pub(crate) fn complete_count(&self, nodes: &mut Vec<Candidate>, max: u64, count: usize) -> u64 {
        let first = nodes[0].right;
        let last = nodes[nodes.len() - 1].left;
        let gaps: Vec<&Candidate> = nodes.iter().filter(|c| c.left < c.right).collect();
        let kept_frames = last - first - gaps.iter().map(|c| c.right - c.left).sum::<usize>();
        let count = count.min(kept_frames);

        let (reach, path) = match self.count_path(first, last, &gaps, max, count) {
            Some(path) => (max, path),
            None => {
                let mut low = max;
                let mut high = self.analysis.position(last) - self.analysis.position(first);
                while low < high {
                    let mid = low.midpoint(high);
                    if self.count_path(first, last, &gaps, mid, count).is_some() {
                        high = mid;
                    } else {
                        low = mid + 1;
                    }
                }
                // At the upper bound, the entire span fits in one piece.
                (
                    low,
                    self.count_path(first, last, &gaps, low, count).unwrap(),
                )
            }
        };

        let mut added: Vec<Candidate> = path.into_iter().map(|k| self.boundary(k, &gaps)).collect();
        // The greedy path may use fewer pieces. Equal shares of the retained frame grid supply
        // enough distinct positions to refine it to exactly count pieces. Refinement can only
        // shorten pieces, since both ends of every candidate are monotonically ordered.
        let (mut gap, mut removed) = (0, 0);
        for i in 1..count {
            let offset = (i as u128 * kept_frames as u128 / count as u128) as usize;
            let mut k = first + offset + removed;
            while gap < gaps.len() && gaps[gap].left < k {
                let width = gaps[gap].right - gaps[gap].left;
                removed += width;
                k += width;
                gap += 1;
            }
            added.push(self.boundary(k, &gaps));
        }
        merge(nodes, added);
        reach
    }

    /// Farthest reachable node at every step gives the minimum piece count: all candidate left
    /// and right endpoints are ordered, so advancing farther cannot reduce future reach.
    /// The dense grid is implicit; only the selected boundaries are allocated.
    fn count_path(
        &self,
        first: usize,
        last: usize,
        gaps: &[&Candidate],
        max: u64,
        count: usize,
    ) -> Option<Vec<usize>> {
        let mut at = first;
        let mut path = Vec::new();
        for _ in 0..count {
            let k = self.reachable(at, last, max);
            if k == last {
                return Some(path);
            }
            if k == at {
                return None;
            }
            let gap = gaps.partition_point(|c| c.left <= k);
            if let Some(c) = gap.checked_sub(1).map(|i| gaps[i]).filter(|c| k <= c.right) {
                path.push(c.left);
                at = c.right;
            } else {
                path.push(k);
                at = k;
            }
        }
        None
    }

    /// Last frame boundary in `from..=end` within max original samples of `from`.
    fn reachable(&self, from: usize, end: usize, max: u64) -> usize {
        let start = self.analysis.position(from);
        let (mut low, mut high) = (from, end);
        while low < high {
            let mid = low + (high - low).div_ceil(2);
            if self.analysis.position(mid) - start <= max {
                low = mid;
            } else {
                high = mid - 1;
            }
        }
        low
    }

    fn boundary(&self, k: usize, gaps: &[&Candidate]) -> Candidate {
        let gap = gaps.partition_point(|c| c.left < k);
        match gaps.get(gap).filter(|c| c.left == k) {
            Some(c) => (*c).clone(),
            None => self.point(k),
        }
    }
}

fn merge(nodes: &mut Vec<Candidate>, added: Vec<Candidate>) {
    nodes.extend(added);
    nodes.sort_by(|a, b| a.left.cmp(&b.left).then(a.cost.total_cmp(&b.cost)));
    nodes.dedup_by_key(|c| c.left);
}
