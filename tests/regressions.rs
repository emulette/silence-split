mod common;

use silence_split::{
    AdjustConfig, Analysis, AnalyzeConfig, Analyzer, CutReason, Gap, Kind, Layout, PlanConfig,
    ScoreFrames, SegmentConfig, Threshold, adjust, plan,
};
use std::{num::NonZeroUsize, time::Duration};

#[test]
fn dc_and_subnormal_padding_preserve_room_noise_pauses() {
    let mut signal = common::Signal::new(16_000, 42);
    signal
        .speech(2.0, -15.0)
        .noise(1.0, -65.0)
        .speech(2.0, -15.0);
    for padding in [0.0, 1.0 / 32768.0, -1.0 / 32768.0, f32::from_bits(1)] {
        let mut pcm = vec![padding; 160_000];
        pcm.extend_from_slice(&signal.samples);
        pcm.extend(std::iter::repeat_n(padding, 160_000));
        let mut analyzer = Analyzer::new(
            Layout {
                sample_rate: 16_000,
                channels: 1,
            },
            &AnalyzeConfig::default(),
        )
        .unwrap();
        analyzer.push_interleaved(&pcm);
        let analysis = analyzer.finish();
        let config = SegmentConfig::default();
        assert!(
            analysis
                .segments(&config)
                .iter()
                .any(|s| s.kind == Kind::Silence && s.start <= 200_000 && 200_000 < s.end),
            "padding {padding:e}"
        );
        let lengths = PlanConfig::new(
            Duration::ZERO,
            Duration::from_secs(13),
            Duration::from_secs(13),
        )
        .unwrap()
        .count(NonZeroUsize::new(2).unwrap());
        let result = plan(&analysis, &config, &lengths);
        assert_eq!(
            result.cuts[0].reason,
            CutReason::Silence,
            "padding {padding:e}"
        );
        assert!((192_000..208_000).contains(&result.cuts[0].previous_end));
    }
}

#[test]
fn whole_file_silence_is_not_filled_by_minimum_duration() {
    for len in [1, 160, 1600, 3999] {
        let mut analyzer = Analyzer::new(
            Layout {
                sample_rate: 16_000,
                channels: 1,
            },
            &AnalyzeConfig::default(),
        )
        .unwrap();
        analyzer.push_interleaved(&vec![0i16; len]);
        let analysis = analyzer.finish();
        let config = SegmentConfig::default();
        let spans = analysis.segments(&config);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].kind, Kind::Silence);
        assert_eq!((spans[0].start, spans[0].end), (0, len as u64));
        assert_eq!(analysis.trim(&config), None);
        let lengths = PlanConfig::new(
            Duration::ZERO,
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .unwrap();
        assert_eq!(plan(&analysis, &config, &lengths).pieces.len(), 1);
        assert!(
            plan(&analysis, &config, &lengths.gap(Gap::Drop))
                .pieces
                .is_empty()
        );
    }
}

#[test]
fn supplemental_cuts_respect_the_same_padding_as_grid_cuts() {
    let cases = [
        (
            10,
            [vec![0.9; 100], vec![0.0; 100], vec![0.9; 100]].concat(),
            3000,
            vec![1200..=1900],
        ),
        (
            10,
            [vec![0.0; 100], vec![0.9; 100], vec![0.0; 100]].concat(),
            3000,
            vec![0..=900, 2200..=3000],
        ),
        // max 400 ms makes the padding cap zero. Leading/trailing silence keeps its edge bounds.
        (
            100,
            [vec![0.0; 50], vec![0.9; 3], vec![0.0; 50]].concat(),
            400,
            vec![0..=5000, 5300..=10300],
        ),
    ];
    for (hop, scores, maximum, silent) in cases {
        let count = scores.len();
        let analysis = Analysis::from_scores(
            Layout {
                sample_rate: 1000,
                channels: 1,
            },
            count as u64 * u64::from(hop),
            ScoreFrames {
                sample_rate: 1000,
                hop,
            },
            scores,
        )
        .unwrap();
        let lengths = PlanConfig::new(
            Duration::ZERO,
            Duration::from_millis(maximum),
            Duration::from_millis(maximum),
        )
        .unwrap()
        .count(NonZeroUsize::new(count).unwrap());
        let result = plan(&analysis, &SegmentConfig::default(), &lengths);
        assert_eq!(result.pieces.len(), count);
        for cut in result.cuts {
            assert_eq!(
                cut.reason == CutReason::Silence,
                silent.iter().any(|r| r.contains(&cut.previous_end)),
                "hop={hop}, max={maximum}, {cut:?}"
            );
        }
    }
}

fn padding_input() -> Analysis {
    Analysis::from_scores(
        Layout {
            sample_rate: 1000,
            channels: 1,
        },
        4000,
        ScoreFrames {
            sample_rate: 1000,
            hop: 10,
        },
        [
            vec![0.0; 50],
            vec![0.9; 100],
            vec![0.0; 100],
            vec![0.9; 100],
            vec![0.0; 50],
        ]
        .concat(),
    )
    .unwrap()
}

#[test]
fn extreme_padding_is_clipped_to_the_audio_when_segmenting() {
    let analysis = padding_input();
    let duration = Duration::from_secs(4);
    for pre in [Duration::ZERO, Duration::MAX] {
        let bounded = SegmentConfig::default().padding(pre.min(duration), duration);
        let extreme = SegmentConfig::default().padding(pre, Duration::MAX);
        let spans = analysis.segments(&extreme);
        assert_eq!(spans, analysis.segments(&bounded));
        assert_eq!(analysis.trim(&extreme), analysis.trim(&bounded));
        assert!(spans.iter().all(|s| s.start < s.end));
        assert_eq!(spans.first().unwrap().start, 0);
        assert_eq!(spans.last().unwrap().end, 4000);
    }
}

#[test]
fn extreme_padding_is_clipped_to_the_audio_when_planning() {
    let analysis = padding_input();
    let duration = Duration::from_secs(4);
    for pre in [Duration::ZERO, Duration::MAX] {
        let bounded = SegmentConfig::default().padding(pre.min(duration), duration);
        let extreme = SegmentConfig::default().padding(pre, Duration::MAX);
        for gap in [Gap::Keep, Gap::Drop] {
            let config = PlanConfig::new(Duration::ZERO, duration, Duration::MAX)
                .unwrap()
                .gap(gap)
                .count(NonZeroUsize::new(2).unwrap());
            assert_eq!(
                plan(&analysis, &extreme, &config),
                plan(&analysis, &bounded, &config)
            );
        }
    }
}

#[test]
fn extreme_padding_is_clipped_to_the_audio_when_adjusting() {
    let analysis = padding_input();
    let duration = Duration::from_secs(4);
    for pre in [Duration::ZERO, Duration::MAX] {
        let bounded = SegmentConfig::default().padding(pre.min(duration), duration);
        let extreme = SegmentConfig::default().padding(pre, Duration::MAX);
        let config = AdjustConfig::new(Duration::from_millis(500));
        assert_eq!(
            adjust(&analysis, &extreme, &[1250, 3250], &config),
            adjust(&analysis, &bounded, &[1250, 3250], &config)
        );
    }
}

#[test]
fn extreme_analysis_window_is_clipped_to_the_available_samples() {
    let layout = Layout {
        sample_rate: 1000,
        channels: 1,
    };
    // Both windows cover the whole short input. The larger one is exactly u64::MAX samples.
    let extreme = Duration::new(u64::MAX / 1000, (u64::MAX % 1000) as u32 * 1_000_000);
    let analyze = |window: Duration| -> Analysis {
        let config = AnalyzeConfig::default()
            .frames(window, Duration::from_millis(1))
            .unwrap();
        let mut analyzer = Analyzer::new(layout, &config).unwrap();
        analyzer.push_interleaved(&[0.5f32, -0.5]);
        analyzer.push_interleaved(&[0.25f32, -0.25]);
        analyzer.finish()
    };
    let result = analyze(extreme);
    assert_eq!(result.sample_count(), 4);
    assert_eq!(result, analyze(Duration::from_secs(1)));
}

#[test]
fn automatic_threshold_preserves_constant_pcm_instead_of_dropping_it() {
    for level in [-6.0, -60.0] {
        for noise in [false, true] {
            for padded in [false, true] {
                let mut signal = common::Signal::new(16_000, 93);
                if padded {
                    signal.zeros(1.0);
                }
                let start = signal.at();
                if noise {
                    signal.noise(5.0, level);
                } else {
                    signal.tone(5.0, 440.0, level);
                }
                let end = signal.at();
                if padded {
                    signal.zeros(1.0);
                }
                let analysis = signal.analyze();
                let segment = SegmentConfig::default();
                let trim = analysis.trim(&segment).expect("constant sound is retained");
                assert!(trim.start <= start && trim.end >= end);
                if padded {
                    assert!(trim.start > 0 && trim.end < signal.at());
                } else {
                    assert_eq!(trim, 0..signal.at());
                }
                let config = PlanConfig::new(
                    Duration::ZERO,
                    Duration::from_secs(10),
                    Duration::from_secs(10),
                )
                .unwrap()
                .gap(Gap::Drop);
                let result = plan(&analysis, &segment, &config);
                assert_eq!(result.pieces.len(), 1);
                assert_eq!(result.pieces[0].start..result.pieces[0].end, trim);

                // An explicit threshold can still remove steady quiet background noise.
                if level == -60.0 {
                    let fixed = segment.threshold(Threshold::Abs(-40.0)).unwrap();
                    assert_eq!(analysis.trim(&fixed), None);
                    assert!(plan(&analysis, &fixed, &config).pieces.is_empty());
                }
            }
        }
    }
}
