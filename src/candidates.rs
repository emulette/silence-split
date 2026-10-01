//! Cut candidates and their costs.

use alloc::vec::Vec;
use core::ops::Range;
use core::time::Duration;

use crate::analyze::{Analysis, Domain};
use crate::math;
use crate::plan::{Cut, CutReason, Gap, Piece, Placement, Plan, Weights};
use crate::segment::{self, Levels, SegmentConfig};

mod feasible;

/// Grid candidates are the quietest point of each block, on a level smoothed over at least this
/// long so a pause between words beats a stop closure in the same block.
const SMOOTHING: Duration = Duration::from_millis(150);

/// Grid candidates at or below this normalized level are [`CutReason::Quiet`].
const QUIET_LEVEL: f64 = 0.25;

/// A possible cut: the earlier piece ends at frame boundary `left`, the later starts at `right`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Candidate {
    pub(crate) left: usize,
    pub(crate) right: usize,
    pub(crate) cost: f64,
    pub(crate) reason: CutReason,
}

impl Candidate {
    /// A start or end node at boundary `k`.
    pub(crate) fn node(k: usize) -> Self {
        Self {
            left: k,
            right: k,
            cost: 0.0,
            reason: CutReason::Forced,
        }
    }
}

/// Where grid candidates go.
pub(crate) struct Grid {
    /// Block size in frames, at least one.
    pub(crate) block: usize,
    /// With [`Gap::Keep`], a silence longer than this many samples also gets grid candidates
    /// outside its padding. `None` never allows them.
    pub(crate) edge_limit: Option<u64>,
}

/// Per-boundary cut costs of one analysis.
pub(crate) struct Context<'a> {
    analysis: &'a Analysis,
    weights: &'a Weights,
    silences: Vec<Range<usize>>,
    /// Normalized smoothed level at each frame boundary `0..=frames`.
    level: Vec<f64>,
    /// Length in seconds of the below-`off` stretch at each frame boundary.
    quiet: Vec<f64>,
    pre: usize,
    tail: usize,
}

impl<'a> Context<'a> {
    /// Costs for `analysis`, with the padding limited to `padding_cap` frames.
    pub(crate) fn new(
        analysis: &'a Analysis,
        segment: &SegmentConfig,
        weights: &'a Weights,
        padding_cap: usize,
    ) -> Self {
        let levels = Levels::new(analysis, segment.threshold);
        let frame_level = smoothed(analysis, analysis.frames_ceil(SMOOTHING).max(1))
            .into_iter()
            .map(|v| levels.normalize(analysis.domain(), v))
            .collect::<Vec<_>>();
        let mut quiet_run = alloc::vec![0; analysis.frames()];
        for (below, run) in segment::runs(&levels.below_off(analysis)) {
            if below {
                quiet_run[run.clone()].fill(run.len());
            }
        }
        let n = analysis.frames();
        let around = |k: usize| k.saturating_sub(1)..(k + 1).min(n);
        let level = (0..=n)
            .map(|k| {
                let r = around(k);
                let count = r.len().max(1) as f64;
                frame_level[r].iter().sum::<f64>() / count
            })
            .collect();
        let quiet = (0..=n)
            .map(|k| {
                let frames = quiet_run[around(k)].iter().copied().max().unwrap_or(0);
                frames as f64 * analysis.frame_secs()
            })
            .collect();
        Self {
            analysis,
            weights,
            silences: segment::silence_runs(analysis, &levels, segment),
            level,
            quiet,
            pre: analysis.frames_in(segment.pre).min(padding_cap),
            tail: analysis.frames_in(segment.tail).min(padding_cap),
        }
    }

    fn leading(&self) -> Option<&Range<usize>> {
        self.silences.first().filter(|s| s.start == 0)
    }

    fn trailing(&self) -> Option<&Range<usize>> {
        self.silences
            .last()
            .filter(|s| s.end == self.analysis.frames())
    }

    /// First and last frame boundary of the plan, or `None` when there is nothing to keep.
    ///
    /// [`Gap::Drop`] moves them inward to the padding of the first and last sound.
    pub(crate) fn ends(&self, gap: Gap) -> Option<(usize, usize)> {
        let n = self.analysis.frames();
        if n == 0 {
            return None;
        }
        match gap {
            Gap::Keep => Some((0, n)),
            Gap::Drop => {
                if self.silences.first().is_some_and(|s| s.len() == n) {
                    return None;
                }
                let first = self.leading().map_or(0, |s| s.end.saturating_sub(self.pre));
                let last = self.trailing().map_or(n, |s| (s.start + self.tail).min(n));
                Some((first, last))
            }
        }
    }

    fn cut_cost(&self, level: f64, quiet_secs: f64) -> f64 {
        let w = self.weights;
        let pause = (quiet_secs / w.pause_ref.as_secs_f64()).min(1.0);
        w.cut + w.level * level + w.pause * (1.0 - pause)
    }

    /// A supplemental point boundary, with the same costs as the grid candidates.
    fn point(&self, k: usize) -> Candidate {
        let silence = self.silences.partition_point(|s| s.end < k);
        let reason = if self.silences.get(silence).is_some_and(|s| s.start <= k) {
            CutReason::Silence
        } else if self.level[k] <= QUIET_LEVEL {
            CutReason::Quiet
        } else {
            CutReason::Forced
        };
        Candidate {
            left: k,
            right: k,
            cost: self.cut_cost(self.level[k], self.quiet[k]),
            reason,
        }
    }

    /// Start node, candidates strictly between `first` and `last` in order, and end node.
    pub(crate) fn candidates(
        &self,
        gap: Gap,
        placement: Placement,
        grid: &Grid,
        first: usize,
        last: usize,
    ) -> Vec<Candidate> {
        let n = self.analysis.frames();
        // A long silence gets grid candidates when silence is kept and no plan within the maximum
        // could step over it; otherwise its only candidate is its own. Grid candidates stay out of
        // the padding, so a piece of nothing but edge silence or padding never comes from them.
        let gets_grid = |s: &Range<usize>| {
            let len = self.analysis.position(s.end) - self.analysis.position(s.start);
            gap == Gap::Keep && grid.edge_limit.is_some_and(|limit| len > limit)
        };
        let mut blocked = alloc::vec![false; n + 1];
        let mut in_silence = alloc::vec![false; n + 1];
        let mut out = Vec::new();
        for s in &self.silences {
            let lo = if s.start == 0 {
                0
            } else {
                (s.start + self.tail).min(n)
            };
            let hi = if s.end == n {
                n
            } else {
                s.end.saturating_sub(self.pre)
            };
            blocked[s.start..=s.end].fill(true);
            if lo <= hi && gets_grid(s) {
                blocked[lo..=hi].fill(false);
                in_silence[lo..=hi].fill(true);
            }
            if s.start == 0 || s.end == n {
                continue;
            }
            let secs = s.len() as f64 * self.analysis.frame_secs();
            let (left, right, level) = if lo > hi {
                let k = segment::split_point(hi, lo, s);
                (k, k, self.level[k])
            } else if gap == Gap::Drop {
                let level = self.level[s.start..=s.end]
                    .iter()
                    .copied()
                    .fold(f64::INFINITY, f64::min);
                (lo, hi, level)
            } else {
                let k = self.place(placement, lo, hi);
                (k, k, self.level[k])
            };
            out.push(Candidate {
                left,
                right,
                cost: self.cut_cost(level, secs),
                reason: CutReason::Silence,
            });
        }

        // Blocks restart after every blocked stretch, so a stretch longer than a block, such as
        // long padding, never leaves the candidates on either side of it more than a block away.
        let mut blocks = Vec::new();
        let mut k = first + 1;
        while k < last {
            let open = (k..last).find(|j| blocked[*j]).unwrap_or(last);
            blocks.extend(
                (k..open)
                    .step_by(grid.block)
                    .map(|b| b..(b + grid.block).min(open)),
            );
            k = (open..last).find(|j| !blocked[*j]).unwrap_or(last);
        }
        for block in blocks {
            let quietest = block.fold(None, |best: Option<usize>, k| match best {
                Some(b) if self.level[b] <= self.level[k] => best,
                _ => Some(k),
            });
            if let Some(k) = quietest {
                let level = self.level[k];
                let reason = if in_silence[k] {
                    CutReason::Silence
                } else if level <= QUIET_LEVEL {
                    CutReason::Quiet
                } else {
                    CutReason::Forced
                };
                out.push(Candidate {
                    left: k,
                    right: k,
                    cost: self.cut_cost(level, self.quiet[k]),
                    reason,
                });
            }
        }

        out.retain(|c| c.left > first && c.right < last);
        out.sort_by(|a, b| a.left.cmp(&b.left).then(a.cost.total_cmp(&b.cost)));
        out.dedup_by_key(|c| c.left);
        out.insert(0, Candidate::node(first));
        out.push(Candidate::node(last));
        out
    }

    /// The boundary within `lo..=hi` chosen by `placement`.
    fn place(&self, placement: Placement, lo: usize, hi: usize) -> usize {
        match placement {
            Placement::Quietest => (lo..=hi).fold(lo, |best, k| {
                if self.level[k] < self.level[best] {
                    k
                } else {
                    best
                }
            }),
            Placement::Center => lo.midpoint(hi),
            Placement::Ratio(r) => lo + math::round(f64::from(r) * (hi - lo) as f64) as usize,
        }
    }

    /// A forced cut at the frame boundary nearest to original sample `p`.
    pub(crate) fn forced_at(&self, p: u64) -> Candidate {
        let pos = |k: usize| self.analysis.position(k);
        let (mut lo, mut hi) = (0, self.analysis.frames());
        while lo < hi {
            let mid = lo.midpoint(hi);
            if pos(mid) < p { lo = mid + 1 } else { hi = mid }
        }
        let k = match lo.checked_sub(1) {
            Some(before) if p - pos(before) <= pos(lo).saturating_sub(p) => before,
            _ => lo,
        };
        Candidate::node(k)
    }

    /// The plan along `path` of node indices into `nodes`.
    pub(crate) fn to_plan(&self, nodes: &[Candidate], path: &[usize], min: f64, max: u64) -> Plan {
        let pos = |k: usize| self.analysis.position(k);
        let pieces = path
            .windows(2)
            .map(|w| {
                let (start, end) = (pos(nodes[w[0]].right), pos(nodes[w[1]].left));
                let len = end - start;
                Piece {
                    start,
                    end,
                    too_short: (len as f64) < min,
                    too_long: len > max,
                }
            })
            .collect();
        let cuts = path[1..path.len().saturating_sub(1)]
            .iter()
            .map(|&j| Cut {
                end: pos(nodes[j].left),
                start: pos(nodes[j].right),
                reason: nodes[j].reason,
            })
            .collect();
        Plan { pieces, cuts }
    }
}

/// Frame values averaged over `width` frames centered on each frame, in power for dB values.
fn smoothed(analysis: &Analysis, width: usize) -> Vec<f64> {
    let db = analysis.domain() == Domain::Decibels;
    let linear: Vec<f64> = analysis
        .values()
        .iter()
        .map(|v| {
            if db {
                math::db_to_power(f64::from(*v))
            } else {
                f64::from(*v)
            }
        })
        .collect();
    let n = linear.len();
    let half = (width - 1) / 2;
    (0..n)
        .map(|i| {
            let r = i.saturating_sub(half)..(i + width - half).min(n);
            let mean = linear[r.clone()].iter().sum::<f64>() / r.len() as f64;
            if db { math::power_to_db(mean) } else { mean }
        })
        .collect()
}
