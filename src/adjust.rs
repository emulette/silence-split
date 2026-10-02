//! Adjust caller-supplied cut points while preserving their correspondence to the result.

use alloc::vec::Vec;
use core::time::Duration;

use crate::analyze::Analysis;
use crate::candidates::{Candidate, Context, Grid};
use crate::errors::ConfigError;
use crate::plan::{Cut, Gap, Placement, Plan, Weights};
use crate::segment::SegmentConfig;

/// Search window and cut preferences for [`adjust`].
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct AdjustConfig {
    window: Duration,
    weights: Weights,
    placement: Placement,
}

impl AdjustConfig {
    /// Searches within `window` of each point, using default weights and the quietest placement.
    /// A zero window snaps each point to the nearest frame boundary when no candidate is exact.
    #[must_use]
    pub fn new(window: Duration) -> Self {
        Self {
            window,
            weights: Weights::default(),
            placement: Placement::Quietest,
        }
    }

    /// Sets the cut cost weights. Length weights are unused; distance from the point always has
    /// weight one. The fixed per-cut cost is identical for all choices of one point.
    ///
    /// # Errors
    ///
    /// [`ConfigError::InvalidWeights`] for invalid weights, as with [`crate::PlanConfig::weights`].
    pub fn weights(mut self, weights: Weights) -> Result<Self, ConfigError> {
        weights.validate()?;
        self.weights = weights;
        Ok(self)
    }

    /// Sets the representative cut position inside each silence.
    ///
    /// # Errors
    ///
    /// [`ConfigError::InvalidRatio`] if a ratio is outside `0.0..=1.0`.
    pub fn placement(mut self, placement: Placement) -> Result<Self, ConfigError> {
        placement.validate()?;
        self.placement = placement;
        Ok(self)
    }
}

/// The adjusted plan and the result for every original input point, in the caller's order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct AdjustResult {
    /// Pieces in timeline order, keeping all audio. Piece length flags are always false.
    pub plan: Plan,
    /// One entry per input point. `None` means the point was skipped: it was at or beyond an
    /// endpoint, duplicated an earlier input point, or adjusted onto an endpoint or an already
    /// chosen boundary. For duplicate input positions, the first input owns the result.
    pub point_cuts: Vec<Option<Cut>>,
}

/// Moves cut points to nearby candidates, keeping all audio and returning each point's result.
///
/// Points are processed in sorted, deduplicated order. Each point picks a candidate within the
/// configured window and before the midpoints to its neighbours, minimizing cut cost plus
/// `|cut - point| / window`. If no candidate exists, it snaps to the nearest frame boundary as a
/// [`crate::CutReason::Forced`] cut. The distance weight is always one. Min/max lengths do not
/// apply. [`AdjustResult::point_cuts`] preserves the original order, including skipped points.
#[must_use]
pub fn adjust(
    analysis: &Analysis,
    segment: &SegmentConfig,
    points: &[u64],
    config: &AdjustConfig,
) -> AdjustResult {
    let mut result = AdjustResult {
        plan: Plan::default(),
        point_cuts: alloc::vec![None; points.len()],
    };
    let ctx = Context::new(analysis, segment, &config.weights, usize::MAX);
    let n = analysis.frames();
    if n == 0 {
        return result;
    }
    let window = config.window;
    let grid = Grid {
        block: analysis
            .frames_floor(window.min(Duration::from_secs(1)))
            .max(1),
        edge_limit: Some(0),
    };
    let candidates = ctx.candidates(Gap::Keep, config.placement, &grid, 0, n);
    let inner = &candidates[1..candidates.len() - 1];
    let positions: Vec<u64> = inner.iter().map(|c| analysis.position(c.left)).collect();
    let len = analysis.sample_count();
    let mut points: Vec<(usize, u64)> = points
        .iter()
        .copied()
        .enumerate()
        .filter(|(_, p)| *p > 0 && *p < len)
        .collect();
    points.sort_unstable_by_key(|&(index, point)| (point, index));
    points.dedup_by_key(|entry| entry.1);

    let reach = analysis.samples_in(window);
    let mut chosen: Vec<Candidate> = Vec::with_capacity(points.len() + 2);
    let mut accepted = Vec::with_capacity(points.len());
    chosen.push(Candidate::node(0));
    for (k, &(index, p)) in points.iter().enumerate() {
        let lo = k.checked_sub(1).map_or(0, |i| points[i].1.midpoint(p));
        let hi = points.get(k + 1).map_or(len, |next| p.midpoint(next.1));
        let from = positions.partition_point(|x| (*x as f64) < p as f64 - reach || *x < lo);
        let to = positions.partition_point(|x| *x < hi && *x as f64 <= p as f64 + reach);
        let best = (from..to.max(from))
            .map(|i| {
                let distance = positions[i].abs_diff(p) as f64;
                let pull = if reach > 0.0 { distance / reach } else { 0.0 };
                (inner[i].cost + pull, &inner[i])
            })
            .fold(None, |best: Option<(f64, &Candidate)>, (v, c)| match best {
                Some((b, _)) if b <= v => best,
                _ => Some((v, c)),
            });
        let pick = match best {
            Some((_, c)) => c.clone(),
            None => ctx.forced_at(p),
        };
        let after_previous = chosen.last().is_some_and(|c| c.left < pick.left);
        if pick.left < n && after_previous {
            chosen.push(pick);
            accepted.push(index);
        }
    }
    chosen.push(Candidate::node(n));
    let path: Vec<usize> = (0..chosen.len()).collect();
    result.plan = ctx.to_plan(&chosen, &path, 0.0, u64::MAX);
    for (index, cut) in accepted.into_iter().zip(&result.plan.cuts) {
        result.point_cuts[index] = Some(*cut);
    }
    result
}
