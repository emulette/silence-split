//! Configuration errors and the messages the crate reports.

use core::fmt;

/// A configuration or input that cannot produce an analysis or a plan.
///
/// Errors are raised only while building configurations and analyses. Analysis and planning
/// themselves never fail: constraints that cannot be met are reported as flags on the result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ConfigError {
    /// The sample rate is zero.
    ZeroSampleRate,
    /// The channel count is zero.
    ZeroChannels,
    /// The sample rate leaves no room for the 80 Hz detection high-pass filter.
    SampleRateTooLow,
    /// The hop is zero, the window is shorter than the hop, or the hop is too long.
    InvalidFrames,
    /// The hop rounds to zero samples at this sample rate.
    HopTooShort,
    /// A threshold is not finite, or a relative threshold is negative.
    InvalidThreshold,
    /// The lengths are not `0 <= min <= target <= max` with a non-zero target.
    InvalidLengths,
    /// A placement ratio is outside `0.0..=1.0`.
    InvalidRatio,
    /// A cost weight is negative or not finite, the per-cut cost is not positive, or the pause
    /// reference is zero.
    InvalidWeights,
    /// The score rate or score hop is zero, or a score frame is shorter than one sample.
    InvalidScoreFrames,
    /// A score is outside `0.0..=1.0` or not finite, a level is not finite, or scores carry
    /// digital-zero marks.
    InvalidValue,
    /// The number of frames does not cover the stated audio length.
    FrameCountMismatch,
}

const ZERO_SAMPLE_RATE: &str = "sample rate must be greater than zero";
const ZERO_CHANNELS: &str = "channel count must be greater than zero";
const SAMPLE_RATE_TOO_LOW: &str = "sample rate must be above 160 Hz for the 80 Hz high-pass filter";
const INVALID_FRAMES: &str =
    "hop must be greater than zero, no longer than the window, and fit in u32 samples";
const HOP_TOO_SHORT: &str = "hop rounds to zero samples at this sample rate";
const INVALID_THRESHOLD: &str = "threshold must be finite, and a relative threshold non-negative";
const INVALID_LENGTHS: &str = "lengths must satisfy min <= target <= max with a non-zero target";
const INVALID_RATIO: &str = "placement ratio must be within 0.0..=1.0";
const INVALID_WEIGHTS: &str =
    "weights must be finite and non-negative, the per-cut cost and pause reference above zero";
const INVALID_SCORE_FRAMES: &str =
    "score rate and hop must be greater than zero, with each frame at least one sample long";
const INVALID_VALUE: &str =
    "scores must be within 0.0..=1.0 without zero marks, and levels must be finite";
const FRAME_COUNT_MISMATCH: &str = "frame count does not cover the audio length";

/// Panic message for planar input whose channel count differs from the layout.
pub(crate) const PLANAR_CHANNELS: &str = "planar input must have one slice per channel";
/// Panic message for planar input whose channel slices differ in length.
pub(crate) const PLANAR_LENGTHS: &str = "planar channel slices must have equal lengths";
/// Panic message for planar input after a partial interleaved frame.
pub(crate) const PLANAR_PENDING: &str = "planar input cannot follow a partial interleaved frame";

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::ZeroSampleRate => ZERO_SAMPLE_RATE,
            Self::ZeroChannels => ZERO_CHANNELS,
            Self::SampleRateTooLow => SAMPLE_RATE_TOO_LOW,
            Self::InvalidFrames => INVALID_FRAMES,
            Self::HopTooShort => HOP_TOO_SHORT,
            Self::InvalidThreshold => INVALID_THRESHOLD,
            Self::InvalidLengths => INVALID_LENGTHS,
            Self::InvalidRatio => INVALID_RATIO,
            Self::InvalidWeights => INVALID_WEIGHTS,
            Self::InvalidScoreFrames => INVALID_SCORE_FRAMES,
            Self::InvalidValue => INVALID_VALUE,
            Self::FrameCountMismatch => FRAME_COUNT_MISMATCH,
        })
    }
}

impl core::error::Error for ConfigError {}
