//! Thresholds and silence/sound segmentation.

use alloc::vec::Vec;
use core::ops::Range;
use core::time::Duration;

use crate::analyze::{Analysis, Domain};
use crate::errors::ConfigError;

/// Where a frame turns from silence to sound.
///
/// For PCM analyses the values are in dB; for external scores they are probabilities.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub enum Threshold {
    /// Derived from the file itself, with hysteresis.
    ///
    /// For PCM, signal-free frames are left out, `floor` is the 10th percentile of frame levels
    /// and `ref` the 99th. Sound starts at `max(floor + 4, min(floor + 6, ref − 20))` dB and ends
    /// 3 dB below that. If that onset exceeds `ref`, it is lowered to `floor` to retain steady
    /// sound. This also retains steady background noise; use [`Self::Abs`] to remove it at a
    /// known level. For scores, sound starts at 0.5 and ends below 0.35.
    Auto,
    /// A fixed level: dBFS for PCM, probability for scores.
    Abs(f32),
    /// This far below the 99th percentile frame value: dB for PCM, probability for scores.
    BelowRef(f32),
}

/// How frame values become silence and sound spans.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct SegmentConfig {
    pub(crate) threshold: Threshold,
    pub(crate) min_silence: Duration,
    pub(crate) min_sound: Duration,
    pub(crate) pre: Duration,
    pub(crate) tail: Duration,
}

impl Default for SegmentConfig {
    /// Automatic threshold, 250 ms minimum silence, 100 ms minimum sound, 100 ms kept before
    /// sound and 200 ms after it.
    fn default() -> Self {
        Self {
            threshold: Threshold::Auto,
            min_silence: Duration::from_millis(250),
            min_sound: Duration::from_millis(100),
            pre: Duration::from_millis(100),
            tail: Duration::from_millis(200),
        }
    }
}

impl SegmentConfig {
    /// Sets the threshold.
    ///
    /// # Errors
    ///
    /// [`ConfigError::InvalidThreshold`] if the value is not finite or a
    /// [`Threshold::BelowRef`] distance is negative.
    pub fn threshold(mut self, threshold: Threshold) -> Result<Self, ConfigError> {
        let valid = match threshold {
            Threshold::Auto => true,
            Threshold::Abs(v) => v.is_finite(),
            Threshold::BelowRef(v) => v.is_finite() && v >= 0.0,
        };
        if !valid {
            return Err(ConfigError::InvalidThreshold);
        }
        self.threshold = threshold;
        Ok(self)
    }

    /// Silence shorter than this counts as sound, so stop closures inside words stay whole. This
    /// also applies to silence at either end, but never fills silence covering the entire audio.
    #[must_use]
    pub fn min_silence(mut self, d: Duration) -> Self {
        self.min_silence = d;
        self
    }

    /// Sound shorter than this counts as silence, which removes clicks.
    #[must_use]
    pub fn min_sound(mut self, d: Duration) -> Self {
        self.min_sound = d;
        self
    }

    /// Silence kept before sound (`pre`) and after it (`tail`), for onsets and reverb tails.
    ///
    /// When the padding of two sounds overlaps, the overlap is split in the middle.
    #[must_use]
    pub fn padding(mut self, pre: Duration, tail: Duration) -> Self {
        self.pre = pre;
        self.tail = tail;
        self
    }
}

/// A half-open range `[start, end)` of original samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct Span {
    /// First sample.
    pub start: u64,
    /// One past the last sample.
    pub end: u64,
    /// Whether the range is silence or sound.
    pub kind: Kind,
}

/// The content of a [`Span`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum Kind {
    /// Below the threshold.
    Silence,
    /// Sound, including its padding.
    Sound,
}

/// Smallest dB range between the floor and the reference when normalizing levels.
const MIN_RANGE_DB: f64 = 20.0;

/// Levels resolved against one analysis and a segmentation threshold.
///
/// PCM values are in dBFS; external scores are probabilities. Signal-free PCM frames are excluded
/// from the percentiles. Obtain these values with [`Analysis::levels`].
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct Levels {
    /// 10th percentile of signal-bearing frame values.
    pub floor: f32,
    /// 99th percentile of signal-bearing frame values.
    pub reference: f32,
    /// Level at which silence becomes sound.
    pub on: f32,
    /// Sound ends below this level.
    pub off: f32,
}

impl Levels {
    pub(crate) fn new(analysis: &Analysis, threshold: Threshold) -> Self {
        let mut values: Vec<f32> = analysis
            .values()
            .iter()
            .zip(analysis.no_signal())
            .filter(|(_, no_signal)| !**no_signal)
            .map(|(v, _)| *v)
            .collect();
        if values.is_empty() {
            return Self {
                floor: 0.0,
                reference: 0.0,
                on: f32::INFINITY,
                off: f32::INFINITY,
            };
        }
        values.sort_unstable_by(f32::total_cmp);
        let percentile = |p: usize| values[percentile_index(values.len(), p)];
        let floor = percentile(10);
        let reference = percentile(99);
        let (on, off) = match (analysis.domain(), threshold) {
            (Domain::Decibels, Threshold::Auto) => {
                let on = (floor + 4.0).max((floor + 6.0).min(reference - 20.0));
                // A threshold above the reference cannot distinguish a steady sound from noise.
                // Keep steady input; an absolute threshold can reject noise at a known level.
                let on = if on > reference { floor } else { on };
                (on, on - 3.0)
            }
            (Domain::Scores, Threshold::Auto) => (0.5, 0.35),
            (_, Threshold::Abs(v)) => (v, v),
            (_, Threshold::BelowRef(v)) => (reference - v, reference - v),
        };
        Self {
            floor,
            reference,
            on,
            off,
        }
    }

    /// Frame value mapped to `0.0..=1.0` between the floor and the reference.
    ///
    /// The dB range is at least [`MIN_RANGE_DB`], so level ripple in flat audio does not stretch
    /// to the full range and outweigh the length costs.
    pub(crate) fn normalize(&self, domain: Domain, value: f64) -> f64 {
        match domain {
            Domain::Scores => value.clamp(0.0, 1.0),
            Domain::Decibels => {
                let range = f64::from(self.reference - self.floor).max(MIN_RANGE_DB);
                ((value - f64::from(self.floor)) / range).clamp(0.0, 1.0)
            }
        }
    }

    /// Whether each frame is below the `off` threshold.
    pub(crate) fn below_off(&self, analysis: &Analysis) -> Vec<bool> {
        analysis
            .values()
            .iter()
            .zip(analysis.no_signal())
            .map(|(v, no_signal)| *no_signal || *v < self.off)
            .collect()
    }
}

/// Rounded percentile index, without overflowing a 32-bit `usize` for long recordings.
fn percentile_index(len: usize, percentile: usize) -> usize {
    ((percentile as u64 * (len - 1) as u64 + 50) / 100) as usize
}

/// Silence runs in frames after hysteresis and the minimum lengths, without padding.
pub(crate) fn silence_runs(
    analysis: &Analysis,
    levels: &Levels,
    config: &SegmentConfig,
) -> Vec<Range<usize>> {
    let mut sound = Vec::with_capacity(analysis.frames());
    let mut state = false;
    for (v, no_signal) in analysis.values().iter().zip(analysis.no_signal()) {
        state = !*no_signal
            && if state {
                *v >= levels.off
            } else {
                *v >= levels.on
            };
        sound.push(state);
    }
    fill_short_runs(&mut sound, true, analysis.frames_in(config.min_sound));
    if sound.iter().any(|s| *s) {
        fill_short_runs(&mut sound, false, analysis.frames_in(config.min_silence));
    }
    runs(&sound)
        .filter(|(loud, _)| !loud)
        .map(|(_, r)| r)
        .collect()
}

/// Flips runs of `kind` shorter than `min` frames to the other kind.
fn fill_short_runs(flags: &mut [bool], kind: bool, min: usize) {
    let short: Vec<Range<usize>> = runs(flags)
        .filter(|(k, r)| *k == kind && r.len() < min)
        .map(|(_, r)| r)
        .collect();
    for r in short {
        flags[r].fill(!kind);
    }
}

/// Maximal runs of equal flags.
pub(crate) fn runs(flags: &[bool]) -> impl Iterator<Item = (bool, Range<usize>)> + '_ {
    let mut start = 0;
    core::iter::from_fn(move || {
        let kind = *flags.get(start)?;
        let len = flags[start..].iter().take_while(|f| **f == kind).count();
        let run = start..start + len;
        start += len;
        Some((kind, run))
    })
}

/// The middle of the overlap of two paddings, kept within the silence between the sounds.
///
/// `next_start` is the padded start of the later sound and `prev_end` the padded end of the
/// earlier one, so the overlap is `[next_start, prev_end)`.
pub(crate) fn split_point(next_start: usize, prev_end: usize, silence: &Range<usize>) -> usize {
    next_start
        .midpoint(prev_end)
        .clamp(silence.start, silence.end)
}

impl Analysis {
    /// The floor, reference and thresholds used for segmentation, or `None` when the analysis
    /// is empty or every PCM frame is signal-free. Such an analysis is entirely silent.
    #[must_use]
    pub fn levels(&self, config: &SegmentConfig) -> Option<Levels> {
        if self.no_signal().iter().all(|no_signal| *no_signal) {
            None
        } else {
            Some(Levels::new(self, config.threshold))
        }
    }

    /// Silence and sound spans covering the whole audio in order.
    ///
    /// Sound spans include their padding. Two sound spans are adjacent without silence between
    /// them when their paddings overlapped.
    #[must_use]
    pub fn segments(&self, config: &SegmentConfig) -> Vec<Span> {
        let sounds = self.padded_sounds(config);
        let mut spans = Vec::with_capacity(2 * sounds.len() + 1);
        let mut at = 0;
        for r in sounds {
            if r.start > at {
                spans.push(self.span(at, r.start, Kind::Silence));
            }
            spans.push(self.span(r.start, r.end, Kind::Sound));
            at = r.end;
        }
        if at < self.frames() {
            spans.push(self.span(at, self.frames(), Kind::Silence));
        }
        spans
    }

    /// The samples from the start of the first sound to the end of the last, padding included,
    /// or `None` if there is no sound.
    #[must_use]
    pub fn trim(&self, config: &SegmentConfig) -> Option<Range<u64>> {
        let sounds = self.padded_sounds(config);
        let first = sounds.first()?;
        let last = sounds.last()?;
        Some(self.position(first.start)..self.position(last.end))
    }

    fn span(&self, start: usize, end: usize, kind: Kind) -> Span {
        Span {
            start: self.position(start),
            end: self.position(end),
            kind,
        }
    }

    /// Sound runs in frames with padding applied.
    fn padded_sounds(&self, config: &SegmentConfig) -> Vec<Range<usize>> {
        let levels = Levels::new(self, config.threshold);
        let silences = silence_runs(self, &levels, config);
        let (pre, tail) = (self.frames_in(config.pre), self.frames_in(config.tail));
        let n = self.frames();
        let mut sounds: Vec<Range<usize>> = Vec::new();
        let mut at = 0;
        let mut gap = 0..0;
        for silence in silences.iter().chain(core::iter::once(&(n..n))) {
            if silence.start > at {
                let mut start = at.saturating_sub(pre);
                let end = silence.start.saturating_add(tail).min(n);
                if let Some(prev) = sounds.last_mut() {
                    if start < prev.end {
                        let mid = split_point(start, prev.end, &gap);
                        prev.end = mid;
                        start = mid;
                    }
                }
                sounds.push(start..end);
            }
            at = silence.end;
            gap = silence.clone();
        }
        sounds
    }
}

#[cfg(test)]
mod tests {
    use super::percentile_index;

    #[test]
    fn percentile_arithmetic_covers_long_32_bit_recordings() {
        assert_eq!(percentile_index(44_000_001, 99), 43_560_000);
        assert_eq!(percentile_index(44_000_001, 10), 4_400_000);
        assert_eq!(percentile_index(1, 99), 0);
        assert_eq!(percentile_index(6, 10), 1);
    }
}
