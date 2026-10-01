//! Synthetic audio fixtures shared by the integration tests.

#![allow(
    dead_code,
    reason = "each test binary uses a different subset of the fixtures"
)]

use core::f32::consts::PI;

use std::time::Duration;

use silence_split::{Analysis, AnalyzeConfig, Analyzer, Kind, Layout, SegmentConfig, Span};

/// Deterministic xorshift generator.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    /// Uniform in `[0, 1)`.
    pub fn unit(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }

    /// Uniform in `[lo, hi)`.
    pub fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.unit()
    }

    /// Uniform in `[-1, 1)`.
    pub fn white(&mut self) -> f32 {
        2.0 * self.unit() - 1.0
    }
}

/// Mono audio built piece by piece.
pub struct Signal {
    pub rate: u32,
    pub samples: Vec<f32>,
    rng: Rng,
}

pub fn db(level: f32) -> f32 {
    10f32.powf(level / 20.0)
}

impl Signal {
    pub fn new(rate: u32, seed: u64) -> Self {
        Self {
            rate,
            samples: Vec::new(),
            rng: Rng::new(seed),
        }
    }

    fn count(&self, secs: f32) -> usize {
        (secs * self.rate as f32).round() as usize
    }

    /// Current length in seconds.
    pub fn secs(&self) -> f32 {
        self.samples.len() as f32 / self.rate as f32
    }

    /// Current length in samples.
    pub fn at(&self) -> u64 {
        self.samples.len() as u64
    }

    /// Digital silence.
    pub fn zeros(&mut self, secs: f32) -> &mut Self {
        let n = self.count(secs);
        self.samples.extend(std::iter::repeat_n(0.0, n));
        self
    }

    /// White noise with peak amplitude `level` dBFS.
    pub fn noise(&mut self, secs: f32, level: f32) -> &mut Self {
        let amp = db(level);
        for _ in 0..self.count(secs) {
            let x = amp * self.rng.white();
            self.samples.push(x);
        }
        self
    }

    /// A sine tone.
    pub fn tone(&mut self, secs: f32, freq: f32, level: f32) -> &mut Self {
        let amp = db(level);
        let start = self.samples.len();
        for i in 0..self.count(secs) {
            let t = (start + i) as f32 / self.rate as f32;
            self.samples.push(amp * (2.0 * PI * freq * t).sin());
        }
        self
    }

    /// Speech-like sound: syllables of voiced tone and noise at about `level` dBFS, separated by
    /// short dips that are too short to count as silence.
    pub fn speech(&mut self, secs: f32, level: f32) -> &mut Self {
        let end = self.samples.len() + self.count(secs);
        while self.samples.len() < end {
            let syllable = self.rng.range(0.12, 0.3);
            let pitch = self.rng.range(120.0, 300.0);
            let loud = level + self.rng.range(-6.0, 3.0);
            let n = self.count(syllable).min(end - self.samples.len());
            let start = self.samples.len();
            for i in 0..n {
                let t = (start + i) as f32 / self.rate as f32;
                let envelope = (PI * i as f32 / n as f32).sin();
                let voiced = (2.0 * PI * pitch * t).sin() + 0.5 * (4.0 * PI * pitch * t).sin();
                let x = db(loud) * envelope * (0.6 * voiced + 0.4 * self.rng.white());
                self.samples.push(x);
            }
            let dip = self.rng.range(0.02, 0.08);
            let dip = self.count(dip).min(end - self.samples.len());
            for _ in 0..dip {
                let x = db(level - 30.0) * self.rng.white();
                self.samples.push(x);
            }
        }
        self
    }

    /// Adds noise at `level` dBFS peak over everything so far.
    pub fn with_noise_floor(&mut self, level: f32) -> &mut Self {
        let amp = db(level);
        for s in &mut self.samples {
            *s += amp * self.rng.white();
        }
        self
    }

    pub fn analyze(&self) -> Analysis {
        let layout = Layout {
            sample_rate: self.rate,
            channels: 1,
        };
        let mut analyzer = Analyzer::new(layout, &AnalyzeConfig::default()).expect("valid layout");
        analyzer.push_interleaved(&self.samples);
        analyzer.finish()
    }
}

/// Speech of random phrase lengths separated by pauses of random length, over a noise floor.
pub fn random_talk(rate: u32, seed: u64, secs: f32) -> Signal {
    let mut signal = Signal::new(rate, seed);
    let mut rng = Rng::new(seed ^ 0x5555);
    while signal.secs() < secs {
        let phrase = rng.range(0.5, 12.0);
        signal.speech(phrase, rng.range(-26.0, -12.0));
        let pause = if rng.unit() < 0.1 {
            rng.range(1.0, 4.0)
        } else {
            rng.range(0.1, 0.9)
        };
        signal.noise(pause, -70.0);
    }
    signal.with_noise_floor(-66.0);
    signal
}

/// Silence spans without padding.
pub fn raw_silences(analysis: &Analysis) -> Vec<Span> {
    let config = SegmentConfig::default().padding(Duration::ZERO, Duration::ZERO);
    analysis
        .segments(&config)
        .into_iter()
        .filter(|s| s.kind == Kind::Silence)
        .collect()
}

/// Which silence a greedy splitter cuts in once a piece would exceed the maximum.
#[derive(Clone, Copy, Debug)]
pub enum Greedy {
    /// The longest silence within reach, like Silero VAD's default at its maximum length.
    Longest,
    /// The last silence within reach, like Silero VAD's legacy mode and zuoer.
    Last,
}

/// Cut positions of a greedy splitter that keeps all audio: once a piece would exceed `max`, it
/// cuts in the middle of a silence within reach, or at `max` when there is none.
pub fn greedy_cuts(len: u64, silences: &[Span], max: u64, rule: Greedy) -> Vec<u64> {
    let mut cuts = Vec::new();
    let mut at = 0;
    while len - at > max {
        let mut reach = silences.iter().filter(|s| {
            let mid = s.start.midpoint(s.end);
            mid > at && mid <= at + max
        });
        let pick = match rule {
            Greedy::Longest => reach.max_by_key(|s| s.end - s.start),
            Greedy::Last => reach.next_back(),
        };
        at = pick.map_or(at + max, |s| s.start.midpoint(s.end));
        cuts.push(at);
    }
    cuts
}
