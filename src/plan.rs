//! Cut planning: where to split the audio.

use alloc::vec::Vec;
use core::num::NonZeroUsize;
use core::time::Duration;

use crate::analyze::Analysis;
use crate::candidates::{Candidate, Context, Grid};
use crate::dp;
use crate::errors::ConfigError;
use crate::segment::SegmentConfig;

/// Where to cut inside a silence that is longer than its padding.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub enum Placement {
    /// At the lowest smoothed level.
    Quietest,
    /// In the middle.
    Center,
    /// At this fraction of the way through, from `0.0` (start) to `1.0` (end).
    Ratio(f32),
}

/// What happens to the silence at a cut.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Gap {
    /// Nothing is dropped; the pieces joined in order are the original audio.
    Keep,
    /// The middle of a silence chosen for a cut is dropped, keeping the padding on both sides.
    /// Leading and trailing silence beyond the padding is dropped too.
    Drop,
}

/// Cost weights of the planner.
///
/// A cut at `x` costs `cut + level · norm(x) + pause · (1 − min(1, q(x) / pause_ref))`, where
/// `norm` is the level smoothed over 150 ms and scaled between the noise floor and the reference
/// (at least 20 dB apart), and `q` is the
/// length of the quiet stretch around `x`. A piece of length `ℓ` costs
/// `target · ((ℓ − target_len) / target_len)² + short · (min − ℓ) / min` when `ℓ < min`.
/// [`adjust`] adds `|x − p| / window` for the distance from the requested point.
///
/// The defaults are starting values that have not been tuned on a corpus.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Weights {
    /// Fixed cost of every cut. Keeps free cuts in digital silence from multiplying.
    pub cut: f64,
    /// Weight of the level at the cut.
    pub level: f64,
    /// Weight of cutting outside a long pause.
    pub pause: f64,
    /// Pause length that earns the full pause credit.
    pub pause_ref: Duration,
    /// Weight of the squared relative distance from the target length. Zero cuts only to stay
    /// within the maximum.
    pub target: f64,
    /// Weight of pieces shorter than the minimum.
    pub short: f64,
}

impl Default for Weights {
    fn default() -> Self {
        Self {
            cut: 0.1,
            level: 1.0,
            pause: 1.0,
            pause_ref: Duration::from_millis(500),
            target: 1.0,
            short: 4.0,
        }
    }
}

/// Piece length limits and cut preferences for [`plan`].
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct PlanConfig {
    pub(crate) min: Duration,
    pub(crate) target: Duration,
    pub(crate) max: Duration,
    pub(crate) count: Option<NonZeroUsize>,
    pub(crate) gap: Gap,
    pub(crate) placement: Placement,
    pub(crate) weights: Weights,
}

impl PlanConfig {
    /// Pieces never longer than `max`, preferably `target` long and not shorter than `min`.
    ///
    /// `max` is a hard limit that includes padding. `min` is soft: a shorter piece is penalized
    /// ([`Weights::short`]) and flagged [`Piece::too_short`]. Silence is kept ([`Gap::Keep`]) and cuts inside
    /// silence go to the quietest point ([`Placement::Quietest`]).
    ///
    /// # Errors
    ///
    /// [`ConfigError::InvalidLengths`] unless `min <= target <= max` and `target` is non-zero.
    pub fn new(min: Duration, target: Duration, max: Duration) -> Result<Self, ConfigError> {
        if target.is_zero() || min > target || target > max {
            return Err(ConfigError::InvalidLengths);
        }
        Ok(Self {
            min,
            target,
            max,
            count: None,
            gap: Gap::Keep,
            placement: Placement::Quietest,
            weights: Weights::default(),
        })
    }

    /// Asks for exactly `count` pieces, each targeting an equal share of the audio. `min` and
    /// `max` still apply; fewer pieces are returned only when there are not enough cut positions.
    #[must_use]
    pub fn count(mut self, count: NonZeroUsize) -> Self {
        self.count = Some(count);
        self
    }

    /// Sets what happens to the silence at a cut.
    #[must_use]
    pub fn gap(mut self, gap: Gap) -> Self {
        self.gap = gap;
        self
    }

    /// Sets where a cut goes inside a long silence when silence is kept.
    ///
    /// # Errors
    ///
    /// [`ConfigError::InvalidRatio`] if a [`Placement::Ratio`] is outside `0.0..=1.0`.
    pub fn placement(mut self, placement: Placement) -> Result<Self, ConfigError> {
        if let Placement::Ratio(r) = placement {
            if !(0.0..=1.0).contains(&r) {
                return Err(ConfigError::InvalidRatio);
            }
        }
        self.placement = placement;
        Ok(self)
    }

    /// Sets the cost weights.
    ///
    /// # Errors
    ///
    /// [`ConfigError::InvalidWeights`] if a weight is negative or not finite, the per-cut cost is
    /// zero, or `pause_ref` is zero.
    pub fn weights(mut self, weights: Weights) -> Result<Self, ConfigError> {
        let w = &weights;
        let valid = [w.level, w.pause, w.target, w.short]
            .iter()
            .all(|v| v.is_finite() && *v >= 0.0)
            && w.cut.is_finite()
            && w.cut > 0.0
            && !w.pause_ref.is_zero();
        if !valid {
            return Err(ConfigError::InvalidWeights);
        }
        self.weights = weights;
        Ok(self)
    }
}

/// A boundary between two consecutive pieces.
///
/// The earlier piece ends at `end` and the later one starts at `start`. With [`Gap::Keep`] they
/// are equal; with [`Gap::Drop`] the samples `[end, start)` are dropped.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct Cut {
    /// End of the earlier piece.
    pub end: u64,
    /// Start of the later piece.
    pub start: u64,
    /// What the cut is in.
    pub reason: CutReason,
}

/// What a cut falls in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum CutReason {
    /// A silence span.
    Silence,
    /// Not silence, but within the quietest quarter of the level range.
    Quiet,
    /// Anywhere else: the length limits left no quiet choice.
    Forced,
}

/// A piece of the original audio, `[start, end)` in samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct Piece {
    /// First sample.
    pub start: u64,
    /// One past the last sample.
    pub end: u64,
    /// Shorter than the minimum length.
    pub too_short: bool,
    /// Longer than the maximum length. Only when the limits cannot be met on the analysis frame
    /// grid, such as a maximum shorter than a frame or a count too small for the maximum.
    pub too_long: bool,
}

/// Pieces in order and the cuts between them: `cuts[i]` separates `pieces[i]` and
/// `pieces[i + 1]`.
///
/// Samples before the first piece and after the last are dropped leading and trailing silence
/// ([`Gap::Drop`] only).
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct Plan {
    /// The pieces.
    pub pieces: Vec<Piece>,
    /// The cuts between pieces.
    pub cuts: Vec<Cut>,
}

/// Chooses cuts over the whole audio that keep every piece within `max`, minimizing the total
/// cost of all cuts and piece lengths (see [`Weights`]).
///
/// Candidates are the silence spans and the quietest point of every block of
/// `min(1 s, max((max − min) / 2, max / 8))` outside them. Additional frame boundaries are inserted
/// when needed to satisfy the maximum or the piece count, including within padding. Padding that
/// would leave less than two blocks of the maximum is shortened to fit. Silence wins over quiet
/// sound through the cut cost. Cuts fall on analysis frame boundaries. The result is deterministic.
///
/// With [`PlanConfig::count`], feasibility includes the removable silence ranges. Only when no
/// frame-grid plan can meet both limits is the maximum relaxed to the smallest attainable longest
/// piece; pieces exceeding the requested maximum are flagged [`Piece::too_long`].
#[must_use]
pub fn plan(analysis: &Analysis, segment: &SegmentConfig, config: &PlanConfig) -> Plan {
    let max = analysis.samples_floor(config.max);
    // Blocks follow max − min, but not below max / 8: a tight soft minimum must not shrink the
    // grid to every frame.
    let block_time = (config.max.saturating_sub(config.min) / 2)
        .max(config.max / 8)
        .min(Duration::from_secs(1));
    let block = analysis.frames_floor(block_time).max(1);
    let block_samples = analysis.position(block.min(analysis.frames()));
    // A piece must hold the padding plus a block to reach the next grid candidate.
    let padding_cap = analysis.frames_floor(config.max).saturating_sub(2 * block);
    let ctx = Context::new(analysis, segment, &config.weights, padding_cap);
    let Some((first, last)) = ctx.ends(config.gap) else {
        return Plan::default();
    };
    let span = analysis.position(last) - analysis.position(first);
    // The nearest grid candidates outside a silence may be a block or two from it, so a silence
    // longer than the maximum less two blocks gets grid candidates of its own.
    let grid = Grid {
        block,
        edge_limit: Some(max.saturating_sub(2 * block_samples)),
    };
    let mut nodes = ctx.candidates(config.gap, config.placement, &grid, first, last);
    let target = match config.count {
        Some(n) => span as f64 / n.get() as f64,
        None => {
            ctx.complete_max(&mut nodes, max);
            analysis.samples_in(config.target)
        }
    };
    let solve = |nodes: &[Candidate], reach: u64| -> Plan {
        let lengths = dp::Lengths {
            min: analysis.samples_in(config.min),
            target,
            max: reach,
            weights: &config.weights,
        };
        let left: Vec<u64> = nodes.iter().map(|c| analysis.position(c.left)).collect();
        let right: Vec<u64> = nodes.iter().map(|c| analysis.position(c.right)).collect();
        let cost: Vec<f64> = nodes.iter().map(|c| c.cost).collect();
        let path = match config.count {
            Some(n) => dp::solve_count(&left, &right, &cost, &lengths, n.get()),
            None => dp::solve(&left, &right, &cost, &lengths),
        };
        ctx.to_plan(nodes, &path, lengths.min, max)
    };
    let result = solve(&nodes, max);
    if let Some(n) = config.count {
        if result.pieces.len() < n.get() || result.pieces.iter().any(|p| p.too_long) {
            let reach = ctx.complete_count(&mut nodes, max, n.get());
            return solve(&nodes, reach);
        }
    }
    result
}

/// Moves each requested cut point to the best candidate within `window` of it, keeping the order
/// of the points and all audio ([`Gap::Keep`]).
///
/// Each point is adjusted on its own; the pieces' lengths are not constrained. Candidates are the
/// silence spans, points inside silence clear of its padding, and the quietest point of every
/// block of `min(1 s, window)`. Each point takes the candidate with the lowest cut cost (default
/// [`Weights`], [`Placement::Quietest`]) plus `|x − p| / window`, among those before the midpoints
/// to its neighbouring points. A point with no candidate in its window stays at the nearest frame
/// boundary as a [`CutReason::Forced`] cut. Points are sorted first; points at or beyond either
/// end of the audio are ignored.
#[must_use]
pub fn adjust(
    analysis: &Analysis,
    segment: &SegmentConfig,
    points: &[u64],
    window: Duration,
) -> Plan {
    let weights = Weights::default();
    let ctx = Context::new(analysis, segment, &weights, usize::MAX);
    let n = analysis.frames();
    if n == 0 {
        return Plan::default();
    }
    let grid = Grid {
        block: analysis
            .frames_floor(window.min(Duration::from_secs(1)))
            .max(1),
        edge_limit: Some(0),
    };
    let candidates = ctx.candidates(Gap::Keep, Placement::Quietest, &grid, 0, n);
    let inner = &candidates[1..candidates.len() - 1];
    let positions: Vec<u64> = inner.iter().map(|c| analysis.position(c.left)).collect();
    let len = analysis.sample_count();
    let mut points: Vec<u64> = points
        .iter()
        .copied()
        .filter(|p| *p > 0 && *p < len)
        .collect();
    points.sort_unstable();
    points.dedup();

    let reach = analysis.samples_in(window);
    let mut chosen: Vec<Candidate> = Vec::with_capacity(points.len() + 2);
    chosen.push(Candidate::node(0));
    for (k, &p) in points.iter().enumerate() {
        let lo = k.checked_sub(1).map_or(0, |i| points[i].midpoint(p));
        let hi = points.get(k + 1).map_or(len, |next| p.midpoint(*next));
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
        }
    }
    chosen.push(Candidate::node(n));
    let path: Vec<usize> = (0..chosen.len()).collect();
    ctx.to_plan(&chosen, &path, 0.0, u64::MAX)
}
