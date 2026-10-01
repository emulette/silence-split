mod common;

use std::time::Duration;

use common::Signal;
use silence_split::{
    Analysis, AnalyzeConfig, Analyzer, ConfigError, Kind, Layout, PlanConfig, SegmentConfig, plan,
};

fn layout(sample_rate: u32, channels: u16) -> Layout {
    Layout {
        sample_rate,
        channels,
    }
}

#[test]
fn push_size_and_layout_do_not_change_the_analysis() {
    let mut signal = Signal::new(22_050, 1);
    signal.speech(2.0, -20.0).zeros(1.0).tone(1.5, 440.0, -12.0);
    let whole = signal.analyze();

    let mut analyzer = Analyzer::new(layout(22_050, 1), &AnalyzeConfig::default()).unwrap();
    for chunk in signal.samples.chunks(97) {
        analyzer.push_interleaved(chunk);
    }
    assert_eq!(analyzer.finish(), whole);

    // The same signal in two identical channels, interleaved in odd-sized blocks and planar.
    let stereo: Vec<f32> = signal.samples.iter().flat_map(|s| [*s, *s]).collect();
    let mut interleaved = Analyzer::new(layout(22_050, 2), &AnalyzeConfig::default()).unwrap();
    for chunk in stereo.chunks(333) {
        interleaved.push_interleaved(chunk);
    }
    let mut planar = Analyzer::new(layout(22_050, 2), &AnalyzeConfig::default()).unwrap();
    planar.push_planar(&[&signal.samples[..], &signal.samples[..]]);
    let (interleaved, planar) = (interleaved.finish(), planar.finish());
    assert_eq!(interleaved, planar);
    assert_eq!(
        interleaved.segments(&SegmentConfig::default()),
        whole.segments(&SegmentConfig::default())
    );
}

#[test]
fn i16_and_f32_input_agree() {
    let mut signal = Signal::new(16_000, 2);
    signal
        .speech(1.0, -18.0)
        .noise(0.5, -60.0)
        .speech(1.0, -18.0);
    let ints: Vec<i16> = signal
        .samples
        .iter()
        .map(|s| (s * 32768.0).round() as i16)
        .collect();
    let quantized: Vec<f32> = ints.iter().map(|s| f32::from(*s) / 32768.0).collect();

    let mut a = Analyzer::new(layout(16_000, 1), &AnalyzeConfig::default()).unwrap();
    a.push_interleaved(&ints);
    let mut b = Analyzer::new(layout(16_000, 1), &AnalyzeConfig::default()).unwrap();
    b.push_interleaved(&quantized);
    assert_eq!(a.finish(), b.finish());
}

#[test]
fn frames_cover_the_audio_at_rates_with_fractional_hops() {
    for rate in [8_000, 11_025, 22_050, 44_100, 48_000] {
        let mut signal = Signal::new(rate, 3);
        signal
            .speech(3.3, -20.0)
            .noise(1.0, -70.0)
            .speech(2.1, -20.0);
        let analysis = signal.analyze();
        assert_eq!(analysis.sample_count(), signal.at());
        let spans = analysis.segments(&SegmentConfig::default());
        assert_eq!(spans.first().map(|s| s.start), Some(0));
        assert_eq!(spans.last().map(|s| s.end), Some(signal.at()));
        assert!(spans.windows(2).all(|w| w[0].end == w[1].start));
        let hop = (u64::from(rate) + 50) / 100;
        assert!(
            spans[1..].iter().all(|s| s.start % hop == 0),
            "frame boundaries at {rate} Hz"
        );
        assert!(
            spans.iter().any(|s| s.kind == Kind::Silence),
            "the pause at {rate} Hz"
        );
    }
}

#[test]
fn late_positions_do_not_drift() {
    // 10 ms is 220.5 samples at 22.05 kHz; the hop is rounded once to whole samples, so a pause
    // 20 minutes in is found where it is.
    let rate = 22_050;
    let hop = 221;
    let mut signal = Signal::new(rate, 4);
    signal.tone(1200.0, 440.0, -12.0);
    let pause = signal.at();
    signal.zeros(1.0).tone(5.0, 440.0, -12.0);
    let config = SegmentConfig::default()
        .threshold(silence_split::Threshold::Abs(-40.0))
        .unwrap()
        .padding(Duration::ZERO, Duration::ZERO);
    let spans = signal.analyze().segments(&config);
    assert_eq!(spans.len(), 3, "{spans:?}");
    assert_eq!(spans[1].start % hop, 0);
    assert!(
        spans[1].start.abs_diff(pause) <= 2 * hop,
        "{} vs {pause}",
        spans[1].start
    );
    assert!(spans[1].end.abs_diff(pause + u64::from(rate)) <= 2 * hop);
}

#[test]
fn three_hours_of_scores_map_exactly_to_the_original_rate() {
    // 16 kHz scores with a 512-sample hop over 3 hours of 44.1 kHz audio.
    let frames: usize = 3 * 3600 * 16_000 / 512;
    let len = (frames as u64 * 512 * 44_100).div_ceil(16_000);
    let mut scores = vec![0.9; frames];
    let pause = frames - 1000;
    scores[pause..pause + 100].fill(0.0);
    let analysis = Analysis::from_scores(layout(44_100, 1), len, 16_000, 512, scores).unwrap();
    let spans =
        analysis.segments(&SegmentConfig::default().padding(Duration::ZERO, Duration::ZERO));
    let at = |i: u64| i * 512 * 44_100 / 16_000;
    let silence: Vec<(u64, u64)> = spans
        .iter()
        .filter(|s| s.kind == Kind::Silence)
        .map(|s| (s.start, s.end))
        .collect();
    assert_eq!(silence, [(at(pause as u64), at(pause as u64 + 100))]);
    assert_eq!(spans.last().map(|s| s.end), Some(len));
}

#[test]
fn non_finite_samples_read_as_zero() {
    let mut signal = Signal::new(16_000, 6);
    signal.tone(5.0, 440.0, -12.0);
    signal.samples[32_000] = f32::NAN;
    signal.samples[48_000] = f32::INFINITY;
    // A steady tone is all at the noise floor for the automatic threshold, so use a fixed one.
    let config = SegmentConfig::default()
        .threshold(silence_split::Threshold::Abs(-40.0))
        .unwrap();
    let spans = signal.analyze().segments(&config);
    assert_eq!(spans.len(), 1, "{spans:?}");
    assert_eq!(spans[0].kind, Kind::Sound);
}

#[test]
#[should_panic(expected = "partial interleaved frame")]
fn planar_input_after_a_partial_interleaved_frame_panics() {
    let mut analyzer = Analyzer::new(layout(16_000, 2), &AnalyzeConfig::default()).unwrap();
    analyzer.push_interleaved(&[0.1f32]);
    analyzer.push_planar(&[&[0.2f32][..], &[0.3f32][..]]);
}

#[test]
fn level_is_dbfs_of_the_high_passed_signal() {
    let mut signal = Signal::new(48_000, 5);
    signal
        .tone(1.0, 1000.0, 0.0)
        .tone(1.0, 20.0, 0.0)
        .zeros(1.0);
    let analysis = signal.analyze();
    let at = |level| {
        let config = SegmentConfig::default()
            .threshold(silence_split::Threshold::Abs(level))
            .unwrap()
            .padding(Duration::ZERO, Duration::ZERO);
        analysis.segments(&config)
    };
    // A full-scale 1 kHz sine is -3.01 dBFS; the 20 Hz rumble is filtered for detection.
    let spans = at(-3.3);
    assert_eq!(spans[0].kind, Kind::Sound);
    assert!(
        spans[0].end.abs_diff(48_000) <= 480,
        "sound ends with the 1 kHz tone: {spans:?}"
    );
    assert_eq!(spans[1].kind, Kind::Silence);
    assert_eq!(spans.len(), 2);
    assert!(at(-2.8).iter().all(|s| s.kind == Kind::Silence));
}

#[test]
fn invalid_layouts_and_frames_are_rejected() {
    let config = AnalyzeConfig::default();
    assert_eq!(
        Analyzer::new(layout(0, 1), &config).err(),
        Some(ConfigError::ZeroSampleRate)
    );
    assert_eq!(
        Analyzer::new(layout(8_000, 0), &config).err(),
        Some(ConfigError::ZeroChannels)
    );
    assert_eq!(
        Analyzer::new(layout(160, 1), &config).err(),
        Some(ConfigError::SampleRateTooLow)
    );
    let frames =
        |w, h| AnalyzeConfig::default().frames(Duration::from_millis(w), Duration::from_millis(h));
    assert_eq!(frames(10, 20).err(), Some(ConfigError::InvalidFrames));
    assert_eq!(frames(10, 0).err(), Some(ConfigError::InvalidFrames));
    let tiny = AnalyzeConfig::default()
        .frames(Duration::from_micros(3), Duration::from_micros(1))
        .unwrap();
    assert_eq!(
        Analyzer::new(layout(8_000, 1), &tiny).err(),
        Some(ConfigError::HopTooShort)
    );
}

#[test]
fn scores_map_to_the_original_rate_exactly() {
    // A 16 kHz detector with a 512-sample hop over 44.1 kHz audio: 1411.2 samples per frame.
    let frames = 400;
    let len = (frames as u64 * 512 * 44_100).div_ceil(16_000);
    let scores: Vec<f32> = (0..frames)
        .map(|i| if (i / 50) % 2 == 0 { 0.9 } else { 0.05 })
        .collect();
    let analysis = Analysis::from_scores(layout(44_100, 1), len, 16_000, 512, scores).unwrap();
    let spans =
        analysis.segments(&SegmentConfig::default().padding(Duration::ZERO, Duration::ZERO));
    let expected = |i: u64| i * 512 * 44_100 / 16_000;
    let starts: Vec<u64> = spans.iter().map(|s| s.start).collect();
    assert_eq!(starts, (0..8).map(|k| expected(k * 50)).collect::<Vec<_>>());
    assert_eq!(spans.last().map(|s| s.end), Some(len));

    let config = PlanConfig::new(
        Duration::from_secs(1),
        Duration::from_secs(3),
        Duration::from_secs(4),
    )
    .unwrap();
    let plan = plan(&analysis, &SegmentConfig::default(), &config);
    for cut in &plan.cuts {
        let frame = (cut.end * 16_000).div_ceil(512 * 44_100);
        assert_eq!(cut.end, expected(frame), "cut on a detector frame boundary");
    }
}

#[test]
fn scores_are_validated() {
    let l = layout(44_100, 1);
    assert_eq!(
        Analysis::from_scores(l, 44_100, 0, 512, vec![0.5]).err(),
        Some(ConfigError::InvalidScoreFrames)
    );
    assert_eq!(
        Analysis::from_scores(l, 1411, 16_000, 512, vec![1.5]).err(),
        Some(ConfigError::InvalidValue)
    );
    assert_eq!(
        Analysis::from_scores(l, 1411, 16_000, 512, vec![f32::NAN]).err(),
        Some(ConfigError::InvalidValue)
    );
    // Frame 31 starts at sample 43 747 and frame 32 at 44 697: less than a hop may be missing.
    let scores = |n| vec![0.5; n];
    let with = |len, n| Analysis::from_scores(l, len, 16_000, 512, scores(n)).err();
    assert_eq!(with(44_100, 32), None);
    assert_eq!(with(44_100, 31), None);
    assert_eq!(with(44_100, 30), Some(ConfigError::FrameCountMismatch));
    assert_eq!(with(44_100, 33), Some(ConfigError::FrameCountMismatch));
    // A whole missing hop is not allowed.
    let same_rate = layout(16_000, 1);
    let exact = |n| Analysis::from_scores(same_rate, 1024, 16_000, 512, scores(n)).err();
    assert_eq!(exact(2), None);
    assert_eq!(exact(1), Some(ConfigError::FrameCountMismatch));
    // Frames shorter than one original sample.
    let short = Analysis::from_scores(layout(8_000, 1), 4, 16_000, 1, scores(8)).err();
    assert_eq!(short, Some(ConfigError::InvalidScoreFrames));
    assert!(Analysis::from_scores(l, 0, 16_000, 512, Vec::new()).is_ok());
}
