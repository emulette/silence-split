//! Frame analysis: PCM levels or external scores on the original sample timeline.

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::time::Duration;

use crate::errors::{ConfigError, PLANAR_CHANNELS, PLANAR_LENGTHS, PLANAR_PENDING};
use crate::filter::{CUTOFF_HZ, HighPass};
use crate::math;
use crate::sample::{Layout, Sample};

const NANOS_PER_SEC: u128 = 1_000_000_000;

/// `d` in whole samples at `rate`, rounded to nearest, in exact integer arithmetic.
fn round_samples(d: Duration, rate: u32) -> u128 {
    (d.as_nanos() * u128::from(rate) + NANOS_PER_SEC / 2) / NANOS_PER_SEC
}

fn saturating_usize(v: u128) -> usize {
    usize::try_from(v).unwrap_or(usize::MAX)
}

/// Window and hop of the PCM analyzer.
///
/// Both are given in time and rounded once to whole samples when an [`Analyzer`] is created, so
/// frame positions never drift at sample rates where the hop is not a whole number of samples.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct AnalyzeConfig {
    window: Duration,
    hop: Duration,
}

impl Default for AnalyzeConfig {
    /// A 30 ms window every 10 ms.
    fn default() -> Self {
        Self {
            window: Duration::from_millis(30),
            hop: Duration::from_millis(10),
        }
    }
}

impl AnalyzeConfig {
    /// Sets the level window and the hop between frames. The window is centered on its hop.
    ///
    /// # Errors
    ///
    /// [`ConfigError::InvalidFrames`] if the hop is zero or longer than the window.
    pub fn frames(mut self, window: Duration, hop: Duration) -> Result<Self, ConfigError> {
        if hop.is_zero() || window < hop {
            return Err(ConfigError::InvalidFrames);
        }
        self.window = window;
        self.hop = hop;
        Ok(self)
    }
}

/// The detector's sample rate and hop for [`Analysis::from_scores`].
///
/// Construct with named fields; both are validated when the analysis is created.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScoreFrames {
    /// Detector sample rate in Hz, which may differ from the original audio's rate.
    pub sample_rate: u32,
    /// Samples per score frame at the detector's sample rate.
    pub hop: u32,
}

/// What the frame values of an [`Analysis`] measure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub(crate) enum Domain {
    /// Frame level in dBFS of the high-passed signal.
    Decibels,
    /// External speech probability in `0.0..=1.0`.
    Scores,
}

/// Incremental PCM analyzer. Push decoded audio in any block size, then [`finish`](Self::finish).
///
/// Each channel is high-passed at 80 Hz for detection only, the channel powers are averaged, and
/// every hop produces one frame level in dBFS over a window centered on that hop. Only the frame
/// levels and signal-free flags are kept, about 1.8 MB per hour at the default 10 ms hop. Samples
/// that are not finite (NaN or infinite) are read as zero. A frame is signal-free when all raw
/// samples are zero or its filtered mean power is at most `1e-20` (-200 dBFS).
#[derive(Clone, Debug)]
pub struct Analyzer {
    layout: Layout,
    hop: u64,
    window: u64,
    filters: Vec<HighPass>,
    /// Channel samples of an interleaved frame split across pushes.
    pending: Vec<f32>,
    /// Per-sample channel-mean filtered power and whether any raw channel sample was non-zero.
    buffer: VecDeque<(f64, bool)>,
    /// Sample index of `buffer[0]`.
    buffer_start: u64,
    received: u64,
    values: Vec<f32>,
    no_signal: Vec<bool>,
}

impl Analyzer {
    /// An analyzer for audio with `layout`.
    ///
    /// # Errors
    ///
    /// [`ConfigError::ZeroSampleRate`], [`ConfigError::ZeroChannels`],
    /// [`ConfigError::SampleRateTooLow`] if the rate is at or below 160 Hz,
    /// [`ConfigError::HopTooShort`] if the hop rounds to zero samples, or
    /// [`ConfigError::InvalidFrames`] if the hop exceeds `u32::MAX` samples or the window exceeds
    /// `u64::MAX` samples.
    pub fn new(layout: Layout, config: &AnalyzeConfig) -> Result<Self, ConfigError> {
        validate_layout(layout)?;
        if f64::from(layout.sample_rate) <= 2.0 * CUTOFF_HZ {
            return Err(ConfigError::SampleRateTooLow);
        }
        let hop = round_samples(config.hop, layout.sample_rate);
        if hop == 0 {
            return Err(ConfigError::HopTooShort);
        }
        let hop = u64::from(u32::try_from(hop).map_err(|_| ConfigError::InvalidFrames)?);
        // The window is at least the hop, so its rounding is at least the hop's.
        let window = u64::try_from(round_samples(config.window, layout.sample_rate))
            .map_err(|_| ConfigError::InvalidFrames)?;
        Ok(Self {
            layout,
            hop,
            window,
            filters: (0..layout.channels)
                .map(|_| HighPass::new(layout.sample_rate))
                .collect(),
            pending: Vec::new(),
            buffer: VecDeque::new(),
            buffer_start: 0,
            received: 0,
            values: Vec::new(),
            no_signal: Vec::new(),
        })
    }

    /// Pushes interleaved samples. A trailing partial frame is kept until the next push.
    pub fn push_interleaved<S: Sample>(&mut self, pcm: &[S]) {
        let channels = usize::from(self.layout.channels);
        let mut rest = pcm;
        if !self.pending.is_empty() {
            let take = (channels - self.pending.len()).min(rest.len());
            self.pending.extend(rest[..take].iter().map(|s| s.to_f32()));
            rest = &rest[take..];
            if self.pending.len() < channels {
                return;
            }
            let frame = core::mem::take(&mut self.pending);
            self.process(frame.iter().copied());
        }
        let mut chunks = rest.chunks_exact(channels);
        for frame in &mut chunks {
            self.process(frame.iter().map(|s| s.to_f32()));
        }
        self.pending
            .extend(chunks.remainder().iter().map(|s| s.to_f32()));
    }

    /// Pushes one slice per channel.
    ///
    /// # Panics
    ///
    /// If the number of slices differs from the channel count, the slices differ in length, or a
    /// partial interleaved frame from [`push_interleaved`](Self::push_interleaved) is pending.
    pub fn push_planar<S: Sample>(&mut self, pcm: &[&[S]]) {
        assert!(self.pending.is_empty(), "{PLANAR_PENDING}");
        assert_eq!(
            pcm.len(),
            usize::from(self.layout.channels),
            "{PLANAR_CHANNELS}"
        );
        let len = pcm.first().map_or(0, |c| c.len());
        assert!(pcm.iter().all(|c| c.len() == len), "{PLANAR_LENGTHS}");
        for i in 0..len {
            self.process(pcm.iter().map(|c| c[i].to_f32()));
        }
    }

    /// Finishes the stream and returns the frame levels. A partial interleaved frame is dropped.
    #[must_use]
    pub fn finish(mut self) -> Analysis {
        let len = self.received;
        let frames = len.div_ceil(self.hop);
        while (self.values.len() as u64) < frames {
            self.emit(len);
        }
        Analysis {
            layout: self.layout,
            len,
            frame_rate: self.layout.sample_rate,
            hop: self.hop as u32,
            domain: Domain::Decibels,
            values: self.values,
            no_signal: self.no_signal,
        }
    }

    fn process(&mut self, frame: impl Iterator<Item = f32>) {
        let mut power = 0.0;
        let mut nonzero = false;
        for (filter, x) in self.filters.iter_mut().zip(frame) {
            // One NaN or infinity would stay in the filter state and silence the rest of the file.
            let x = if x.is_finite() { x } else { 0.0 };
            nonzero |= x != 0.0;
            let y = filter.process(f64::from(x));
            power += y * y;
        }
        self.buffer
            .push_back((power / f64::from(self.layout.channels), nonzero));
        self.received += 1;
        while self.window_end(self.values.len() as u64) <= self.received {
            self.emit(self.received);
        }
    }

    /// Sample range `[start, end)` of frame `i`'s window, before clipping to the stream.
    fn window_start(&self, i: u64) -> u64 {
        (i * self.hop).saturating_sub((self.window - self.hop) / 2)
    }

    fn window_end(&self, i: u64) -> u64 {
        let ahead = self.window - (self.window - self.hop) / 2;
        (i * self.hop).saturating_add(ahead)
    }

    /// Emits the next frame with its window clipped to `[0, available)`.
    fn emit(&mut self, available: u64) {
        let i = self.values.len() as u64;
        let start = self.window_start(i);
        let end = self.window_end(i).min(available);
        let from = (start - self.buffer_start) as usize;
        let to = (end - self.buffer_start) as usize;
        let mut power = 0.0;
        let mut nonzero = false;
        for &(p, nz) in self.buffer.range(from..to) {
            power += p;
            nonzero |= nz;
        }
        // The filter rings on after the input stops, so a signal-free window gets the floor level.
        let power = if nonzero {
            power / (to - from) as f64
        } else {
            0.0
        };
        self.values.push(math::power_to_db(power) as f32);
        self.no_signal.push(!nonzero || power <= math::MIN_POWER);
        let next = self.window_start(i + 1).min(self.received);
        while self.buffer_start < next {
            self.buffer.pop_front();
            self.buffer_start += 1;
        }
    }
}

fn validate_layout(layout: Layout) -> Result<(), ConfigError> {
    if layout.sample_rate == 0 {
        return Err(ConfigError::ZeroSampleRate);
    }
    if layout.channels == 0 {
        return Err(ConfigError::ZeroChannels);
    }
    Ok(())
}

/// Frame values on the original sample timeline, from an [`Analyzer`] or external scores.
///
/// Frame `i` stands for original samples `[start(i), start(i + 1))`, where
/// `start(i) = floor(i · hop · sample_rate / frame_rate)` in exact integer arithmetic and the last
/// frame ends at the audio length. Thresholds are resolved when segmenting or planning, not here,
/// so one analysis serves any threshold.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(try_from = "AnalysisData")
)]
pub struct Analysis {
    layout: Layout,
    len: u64,
    frame_rate: u32,
    hop: u32,
    domain: Domain,
    values: Vec<f32>,
    no_signal: Vec<bool>,
}

impl Analysis {
    /// Wraps per-frame speech probabilities from an external voice activity detector.
    ///
    /// `len` is the length of the original audio in samples per channel. `frames.hop` is the
    /// detector's hop in samples at `frames.sample_rate`, which may differ from the original
    /// sample rate: a 16 kHz detector with a 512-sample hop maps frame `i` to original sample
    /// `floor(i · 512 · sample_rate / 16000)`. Each frame must span at least one original sample.
    /// The scores must cover the audio: the last score starts before `len`, and less than one hop
    /// may be missing at the end.
    ///
    /// # Errors
    ///
    /// [`ConfigError::ZeroSampleRate`], [`ConfigError::ZeroChannels`],
    /// [`ConfigError::InvalidScoreFrames`] for a zero rate or hop or a frame shorter than one
    /// original sample, [`ConfigError::InvalidValue`]
    /// for a score outside `0.0..=1.0`, or [`ConfigError::FrameCountMismatch`].
    pub fn from_scores(
        layout: Layout,
        len: u64,
        frames: ScoreFrames,
        scores: Vec<f32>,
    ) -> Result<Self, ConfigError> {
        let no_signal = alloc::vec![false; scores.len()];
        let analysis = Self {
            layout,
            len,
            frame_rate: frames.sample_rate,
            hop: frames.hop,
            domain: Domain::Scores,
            values: scores,
            no_signal,
        };
        analysis.validate()?;
        Ok(analysis)
    }

    /// Layout of the original audio.
    #[must_use]
    pub fn layout(&self) -> Layout {
        self.layout
    }

    /// Length of the original audio in samples per channel.
    #[must_use]
    pub fn sample_count(&self) -> u64 {
        self.len
    }

    fn validate(&self) -> Result<(), ConfigError> {
        validate_layout(self.layout)?;
        let frame_samples = u64::from(self.hop) * u64::from(self.layout.sample_rate);
        if self.frame_rate == 0 || self.hop == 0 || frame_samples < u64::from(self.frame_rate) {
            return Err(ConfigError::InvalidScoreFrames);
        }
        let valid = |v: &f32| match self.domain {
            Domain::Decibels => v.is_finite(),
            Domain::Scores => (0.0..=1.0).contains(v),
        };
        let marked_scores =
            self.domain == Domain::Scores && self.no_signal.iter().any(|no_signal| *no_signal);
        if !self.values.iter().all(valid) || marked_scores {
            return Err(ConfigError::InvalidValue);
        }
        let n = self.values.len();
        let len = u128::from(self.len);
        let covered = if self.len == 0 {
            n == 0
        } else {
            n > 0 && self.start(n - 1) < len && len < self.start(n + 1)
        };
        if !covered || self.no_signal.len() != n {
            return Err(ConfigError::FrameCountMismatch);
        }
        Ok(())
    }

    /// Number of frames.
    pub(crate) fn frames(&self) -> usize {
        self.values.len()
    }

    pub(crate) fn domain(&self) -> Domain {
        self.domain
    }

    pub(crate) fn values(&self) -> &[f32] {
        &self.values
    }

    /// Whether the raw window is all zero or its filtered mean power is at the floor.
    pub(crate) fn no_signal(&self) -> &[bool] {
        &self.no_signal
    }

    /// Unclipped original start sample of frame `i`.
    fn start(&self, i: usize) -> u128 {
        let num = i as u128 * u128::from(self.hop) * u128::from(self.layout.sample_rate);
        num / u128::from(self.frame_rate)
    }

    /// Original sample of frame boundary `k` in `0..=frames`; the last boundary is the length.
    pub(crate) fn position(&self, k: usize) -> u64 {
        if k >= self.frames() {
            self.len
        } else {
            // Validation keeps every frame start below the length.
            self.start(k) as u64
        }
    }

    /// Duration of one frame in seconds.
    pub(crate) fn frame_secs(&self) -> f64 {
        f64::from(self.hop) / f64::from(self.frame_rate)
    }

    /// `d` in frames as the exact fraction `(numerator, denominator)`.
    fn frame_fraction(&self, d: Duration) -> (u128, u128) {
        (
            d.as_nanos() * u128::from(self.frame_rate),
            u128::from(self.hop) * NANOS_PER_SEC,
        )
    }

    /// `d` in whole frames, rounded to nearest.
    pub(crate) fn frames_in(&self, d: Duration) -> usize {
        let (num, den) = self.frame_fraction(d);
        saturating_usize((num + den / 2) / den)
    }

    /// `d` in whole frames, rounded up.
    pub(crate) fn frames_ceil(&self, d: Duration) -> usize {
        let (num, den) = self.frame_fraction(d);
        saturating_usize(num.div_ceil(den))
    }

    /// `d` in whole frames, rounded down.
    pub(crate) fn frames_floor(&self, d: Duration) -> usize {
        let (num, den) = self.frame_fraction(d);
        saturating_usize(num / den)
    }

    /// `d` in whole original samples, rounded down.
    pub(crate) fn samples_floor(&self, d: Duration) -> u64 {
        let samples = d.as_nanos() * u128::from(self.layout.sample_rate) / NANOS_PER_SEC;
        u64::try_from(samples).unwrap_or(u64::MAX)
    }

    /// `d` in original samples as a real number.
    pub(crate) fn samples_in(&self, d: Duration) -> f64 {
        (d.as_nanos() * u128::from(self.layout.sample_rate)) as f64 / NANOS_PER_SEC as f64
    }
}

/// Unvalidated serialized form of [`Analysis`].
#[cfg(feature = "serde")]
#[derive(serde::Deserialize)]
struct AnalysisData {
    layout: Layout,
    len: u64,
    frame_rate: u32,
    hop: u32,
    domain: Domain,
    values: Vec<f32>,
    no_signal: Vec<bool>,
}

#[cfg(feature = "serde")]
impl TryFrom<AnalysisData> for Analysis {
    type Error = ConfigError;

    fn try_from(d: AnalysisData) -> Result<Self, ConfigError> {
        let analysis = Self {
            layout: d.layout,
            len: d.len,
            frame_rate: d.frame_rate,
            hop: d.hop,
            domain: d.domain,
            values: d.values,
            no_signal: d.no_signal,
        };
        analysis.validate()?;
        Ok(analysis)
    }
}
