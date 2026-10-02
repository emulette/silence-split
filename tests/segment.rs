mod common;

use std::time::Duration;

use common::{Rng, Signal, db};
use silence_split::{
    Analysis, ConfigError, Kind, Layout, ScoreFrames, SegmentConfig, Span, Threshold,
};

const NO_PADDING: Duration = Duration::ZERO;

fn raw() -> SegmentConfig {
    SegmentConfig::default().padding(NO_PADDING, NO_PADDING)
}

fn silences(spans: &[Span]) -> Vec<(f64, f64)> {
    spans
        .iter()
        .filter(|s| s.kind == Kind::Silence)
        .map(|s| (s.start as f64 / 16_000.0, s.end as f64 / 16_000.0))
        .collect()
}

/// Within 100 ms: speech fixtures end with a dip of up to 80 ms that loud hum makes silent.
fn assert_near(actual: f64, expected: f64, what: &str) {
    assert!(
        (actual - expected).abs() <= 0.1,
        "{what}: {actual} vs {expected}"
    );
}

/// Sound 0–2 s, pause 2–3 s, sound 3–5 s, built by `pause`.
fn pause_between_speech(pause: impl FnOnce(&mut Signal)) -> Signal {
    let mut signal = Signal::new(16_000, 11);
    signal.speech(2.0, -18.0);
    pause(&mut signal);
    signal.speech(2.0, -18.0);
    signal
}

#[test]
fn finds_a_pause_in_quiet_room_noise() {
    let signal = pause_between_speech(|s| {
        s.noise(1.0, -65.0);
    });
    let found = silences(&signal.analyze().segments(&raw()));
    assert_eq!(found.len(), 1, "{found:?}");
    assert_near(found[0].0, 2.0, "pause start");
    assert_near(found[0].1, 3.0, "pause end");
}

#[test]
fn finds_a_pause_under_mains_hum() {
    let mut signal = pause_between_speech(|s| {
        s.noise(1.0, -70.0);
    });
    let rate = signal.rate as f32;
    for (i, x) in signal.samples.iter_mut().enumerate() {
        let t = i as f32 / rate;
        let hum: f32 = [(60.0, -30.0), (120.0, -40.0), (180.0, -46.0)]
            .iter()
            .map(|(f, level)| db(*level) * (2.0 * std::f32::consts::PI * f * t).sin())
            .sum();
        *x += hum;
    }
    let found = silences(&signal.analyze().segments(&raw()));
    assert_eq!(found.len(), 1, "{found:?}");
    assert_near(found[0].0, 2.0, "pause start");
    assert_near(found[0].1, 3.0, "pause end");
}

#[test]
fn finds_a_pause_in_pink_noise_at_10_db_snr() {
    let mut signal = pause_between_speech(|s| {
        s.zeros(1.0);
    });
    // Paul Kellet's pink filter over white noise, scaled to 10 dB below the speech level.
    let mut rng = Rng::new(12);
    let mut b = [0f32; 3];
    let pink: Vec<f32> = (0..signal.samples.len())
        .map(|_| {
            let w = rng.white();
            b[0] = 0.99765 * b[0] + w * 0.0990460;
            b[1] = 0.96300 * b[1] + w * 0.2965164;
            b[2] = 0.57000 * b[2] + w * 1.0526913;
            b[0] + b[1] + b[2] + w * 0.1848
        })
        .collect();
    let power = |x: &[f32]| x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32;
    let speech_power = power(&signal.samples[..32_000]);
    let scale = (speech_power / power(&pink) / 10.0).sqrt();
    for (x, p) in signal.samples.iter_mut().zip(&pink) {
        *x += scale * p;
    }
    let found = silences(&signal.analyze().segments(&raw()));
    assert_eq!(found.len(), 1, "{found:?}");
    assert_near(found[0].0, 2.0, "pause start");
    assert_near(found[0].1, 3.0, "pause end");
}

#[test]
fn clicks_in_silence_and_short_dips_in_speech_do_not_split() {
    let signal = pause_between_speech(|s| {
        s.noise(0.45, -70.0).noise(0.01, -6.0).noise(0.54, -70.0);
    });
    let found = silences(&signal.analyze().segments(&raw()));
    assert_eq!(found.len(), 1, "a 10 ms click is not sound: {found:?}");

    let mut signal = Signal::new(16_000, 13);
    signal
        .speech(1.0, -18.0)
        .noise(0.15, -70.0)
        .speech(1.0, -18.0);
    let found = silences(&signal.analyze().segments(&raw()));
    assert!(
        found.is_empty(),
        "150 ms is shorter than the minimum silence: {found:?}"
    );
}

#[test]
fn digital_zero_padding_does_not_skew_the_automatic_threshold() {
    // 90 % of the file is digital zero; the room noise pause must still be found.
    let mut signal = Signal::new(16_000, 14);
    signal
        .zeros(30.0)
        .speech(2.0, -18.0)
        .noise(1.0, -60.0)
        .speech(2.0, -18.0)
        .zeros(30.0);
    let found = silences(&signal.analyze().segments(&raw()));
    assert_eq!(found.len(), 3, "{found:?}");
    assert_near(found[1].0, 32.0, "pause start");
    assert_near(found[1].1, 33.0, "pause end");
}

#[test]
fn padding_keeps_the_reverb_tail() {
    let mut signal = Signal::new(16_000, 15);
    signal.speech(1.0, -18.0);
    // A 150 ms decaying tail, then quiet room noise.
    for i in 0..2400 {
        let x = db(-24.0 - 0.02 * i as f32) * (i as f32 * 0.3).sin();
        signal.samples.push(x);
    }
    signal
        .noise(0.85, -70.0)
        .speech(1.0, -18.0)
        .noise(0.3, -70.0)
        .speech(1.0, -18.0);
    let analysis = signal.analyze();
    let spans = analysis.segments(&SegmentConfig::default());
    let kinds: Vec<Kind> = spans.iter().map(|s| s.kind).collect();
    // The 300 ms pause is shorter than 100 ms + 200 ms of padding: two adjacent sound spans.
    assert_eq!(
        kinds,
        [Kind::Sound, Kind::Silence, Kind::Sound, Kind::Sound]
    );
    let secs = |s: u64| s as f64 / 16_000.0;
    assert!(secs(spans[0].end) >= 1.15, "the tail is kept: {spans:?}");
    assert_near(secs(spans[1].end), 2.0 - 0.1, "pre padding");

    let trimmed = analysis.trim(&SegmentConfig::default()).unwrap();
    assert_eq!(trimmed, 0..signal.at());
}

#[test]
fn trim_drops_leading_and_trailing_silence() {
    let mut signal = Signal::new(16_000, 16);
    signal.noise(2.0, -70.0).speech(3.0, -18.0).zeros(2.0);
    let analysis = signal.analyze();
    let trimmed = analysis.trim(&SegmentConfig::default()).unwrap();
    assert_near(
        trimmed.start as f64 / 16_000.0,
        1.9,
        "start keeps 100 ms before sound",
    );
    assert_near(
        trimmed.end as f64 / 16_000.0,
        5.2,
        "end keeps 200 ms after sound",
    );

    let mut quiet = Signal::new(16_000, 17);
    quiet.zeros(1.0);
    let analysis = quiet.analyze();
    assert_eq!(analysis.trim(&SegmentConfig::default()), None);
    let spans = analysis.segments(&SegmentConfig::default());
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].kind, Kind::Silence);
}

#[test]
fn fixed_thresholds() {
    let signal = pause_between_speech(|s| {
        s.noise(1.0, -45.0);
    });
    let analysis = signal.analyze();
    let abs = |level| raw().threshold(Threshold::Abs(level)).unwrap();
    assert_eq!(silences(&analysis.segments(&abs(-40.0))).len(), 1);
    assert_eq!(
        silences(&analysis.segments(&abs(-60.0))).len(),
        0,
        "-45 dBFS noise is sound"
    );
    let below = |db| raw().threshold(Threshold::BelowRef(db)).unwrap();
    assert_eq!(silences(&analysis.segments(&below(15.0))).len(), 1);

    assert_eq!(
        raw().threshold(Threshold::Abs(f32::NAN)).err(),
        Some(ConfigError::InvalidThreshold)
    );
    assert_eq!(
        raw().threshold(Threshold::BelowRef(-1.0)).err(),
        Some(ConfigError::InvalidThreshold)
    );
}

/// Scores at 16 kHz with a 160-sample hop: frame `i` starts at sample `160 · i`.
fn scored(runs: &[(f32, usize)]) -> Analysis {
    let scores: Vec<f32> = runs
        .iter()
        .flat_map(|(v, n)| std::iter::repeat_n(*v, *n))
        .collect();
    let len = scores.len() as u64 * 160;
    let layout = Layout {
        sample_rate: 16_000,
        channels: 1,
    };
    Analysis::from_scores(
        layout,
        len,
        ScoreFrames {
            sample_rate: 16_000,
            hop: 160,
        },
        scores,
    )
    .unwrap()
}

fn frame(i: u64) -> u64 {
    i * 160
}

#[test]
fn overlapping_padding_is_split_in_the_middle_of_the_overlap() {
    // Silence in frames 100..120. Sound before keeps 20 frames after it, sound after keeps 10
    // before it: the paddings overlap in 110..120 and meet at 115.
    let analysis = scored(&[(0.9, 100), (0.0, 20), (0.9, 100)]);
    let config = SegmentConfig::default().min_silence(Duration::from_millis(100));
    let spans = analysis.segments(&config);
    let got: Vec<(u64, u64, Kind)> = spans.iter().map(|s| (s.start, s.end, s.kind)).collect();
    assert_eq!(
        got,
        [
            (0, frame(115), Kind::Sound),
            (frame(115), frame(220), Kind::Sound)
        ]
    );
}

#[test]
fn hysteresis_keeps_sound_until_it_falls_below_off() {
    // Automatic score thresholds: sound starts at 0.5 and ends below 0.35. A value of 0.4 does not
    // start sound but keeps it going.
    let analysis = scored(&[(0.1, 50), (0.4, 30), (0.9, 50), (0.4, 30), (0.1, 50)]);
    let spans = analysis.segments(&raw());
    let silence: Vec<(u64, u64)> = spans
        .iter()
        .filter(|s| s.kind == Kind::Silence)
        .map(|s| (s.start, s.end))
        .collect();
    assert_eq!(silence, [(0, frame(80)), (frame(160), frame(210))]);
}

#[test]
fn trim_matches_the_padded_sound() {
    let analysis = scored(&[(0.0, 50), (0.9, 100), (0.0, 50)]);
    let trimmed = analysis.trim(&SegmentConfig::default()).unwrap();
    assert_eq!(trimmed, frame(40)..frame(170));
}
