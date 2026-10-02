mod common;

use silence_split::{
    AdjustConfig, Analysis, AnalyzeConfig, Analyzer, ConfigError, Layout, Placement, PlanConfig,
    ScoreFrames, SegmentConfig, Threshold, Weights, adjust, plan, write_audacity_labels,
};
use std::{num::NonZeroU32, time::Duration};

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}
fn scores(hop: u32, values: Vec<f32>) -> Analysis {
    Analysis::from_scores(
        Layout {
            sample_rate: 1000,
            channels: 1,
        },
        values.len() as u64 * u64::from(hop),
        ScoreFrames {
            sample_rate: 1000,
            hop,
        },
        values,
    )
    .unwrap()
}

#[test]
fn adjustment_maps_unsorted_duplicates_and_collisions_back_to_inputs() {
    let analysis = scores(100, vec![0.9; 10]);
    let points = [800, 250, 250, 251, 0, 1000, u64::MAX, 1, 999, 200];
    let result = adjust(
        &analysis,
        &SegmentConfig::default(),
        &points,
        &AdjustConfig::new(Duration::ZERO),
    );
    let mapped: Vec<_> = result
        .point_cuts
        .iter()
        .map(|c| c.map(|c| c.previous_end))
        .collect();
    assert_eq!(
        mapped,
        [
            Some(800),
            None,
            None,
            Some(300),
            None,
            None,
            None,
            None,
            None,
            Some(200)
        ]
    );
    assert_eq!(
        result
            .plan
            .cuts
            .iter()
            .map(|c| c.previous_end)
            .collect::<Vec<_>>(),
        [200, 300, 800]
    );
    for cut in result.point_cuts.iter().flatten() {
        assert!(result.plan.cuts.contains(cut));
        assert_eq!(cut.previous_end, cut.next_start);
    }
    assert_eq!(result.plan.pieces.first().unwrap().start, 0);
    assert_eq!(result.plan.pieces.last().unwrap().end, 1000);
    assert!(
        result
            .plan
            .pieces
            .iter()
            .all(|p| !p.too_long && !p.too_short)
    );
    let duplicate = adjust(
        &analysis,
        &SegmentConfig::default(),
        &[450, 450],
        &AdjustConfig::new(Duration::ZERO),
    );
    assert!(duplicate.point_cuts[0].is_some());
    assert!(duplicate.point_cuts[1].is_none());
}

#[test]
fn adjustment_empty_audio_preserves_one_result_per_input() {
    let analysis = scores(10, vec![]);
    let result = adjust(
        &analysis,
        &SegmentConfig::default(),
        &[0, 1, 50],
        &AdjustConfig::new(ms(100)),
    );
    assert!(result.plan.pieces.is_empty());
    assert_eq!(result.point_cuts, [None, None, None]);
}

#[test]
fn adjustment_uses_configured_placement_and_level_weight() {
    let analysis = scores(10, [vec![0.9; 100], vec![0.0; 60], vec![0.9; 100]].concat());
    let segment = SegmentConfig::default().padding(Duration::ZERO, Duration::ZERO);
    let at = |placement| {
        adjust(
            &analysis,
            &segment,
            &[1350],
            &AdjustConfig::new(ms(600)).placement(placement).unwrap(),
        )
        .point_cuts[0]
            .unwrap()
            .previous_end
    };
    assert_eq!(at(Placement::Center), 1300);
    assert_ne!(at(Placement::Ratio(0.0)), 1300);

    let mut values = vec![0.9; 500];
    values[200..220].fill(0.5);
    let analysis = scores(10, values);
    let at = |level| {
        let mut w = Weights::default();
        w.level = level;
        adjust(
            &analysis,
            &segment,
            &[3000],
            &AdjustConfig::new(ms(2000)).weights(w).unwrap(),
        )
        .point_cuts[0]
            .unwrap()
            .previous_end
    };
    assert!(at(0.0).abs_diff(3000) < 100);
    assert!((2000..=2200).contains(&at(100.0)));
    assert_eq!(
        AdjustConfig::new(ms(1))
            .placement(Placement::Ratio(f32::NAN))
            .err(),
        Some(ConfigError::InvalidRatio)
    );
    let mut w = Weights::default();
    w.level = -1.0;
    assert_eq!(
        AdjustConfig::new(ms(1)).weights(w).err(),
        Some(ConfigError::InvalidWeights)
    );
}

#[test]
fn reported_levels_match_thresholds_for_pcm_and_scores() {
    let analysis = scores(10, [vec![0.1; 50], vec![0.9; 50]].concat());
    let config = SegmentConfig::default();
    let auto = analysis.levels(&config).unwrap();
    assert_eq!(
        (auto.floor, auto.reference, auto.on, auto.off),
        (0.1, 0.9, 0.5, 0.35)
    );
    let absolute = analysis
        .levels(&config.clone().threshold(Threshold::Abs(0.25)).unwrap())
        .unwrap();
    assert_eq!((absolute.on, absolute.off), (0.25, 0.25));
    let relative = analysis
        .levels(&config.threshold(Threshold::BelowRef(0.2)).unwrap())
        .unwrap();
    assert_eq!(relative.on, relative.reference - 0.2);
    assert_eq!(relative.off, relative.on);

    let mut signal = common::Signal::new(16000, 7);
    signal.noise(1.0, -65.0).speech(2.0, -15.0);
    let a = signal.analyze();
    let levels = a.levels(&SegmentConfig::default()).unwrap();
    assert!(levels.floor < levels.reference);
    assert_eq!(
        levels.on,
        (levels.floor + 4.0).max((levels.floor + 6.0).min(levels.reference - 20.0))
    );
    assert_eq!(levels.off, levels.on - 3.0);
}

#[test]
fn empty_or_signal_free_pcm_has_no_levels_but_zero_scores_do() {
    for value in [0.0, f32::from_bits(1)] {
        for len in [0, 160, 16000] {
            let mut analyzer = Analyzer::new(
                Layout {
                    sample_rate: 16000,
                    channels: 1,
                },
                &AnalyzeConfig::default(),
            )
            .unwrap();
            analyzer.push_interleaved(&vec![value; len]);
            let a = analyzer.finish();
            assert!(a.levels(&SegmentConfig::default()).is_none());
            assert!(a.trim(&SegmentConfig::default()).is_none());
        }
    }
    assert!(
        scores(10, vec![])
            .levels(&SegmentConfig::default())
            .is_none()
    );
    assert!(
        scores(10, vec![0.0; 10])
            .levels(&SegmentConfig::default())
            .is_some()
    );
}

#[test]
fn labels_require_a_nonzero_rate_and_emit_finite_times() {
    let a = scores(10, vec![0.9; 100]);
    let result = plan(
        &a,
        &SegmentConfig::default(),
        &PlanConfig::new(Duration::ZERO, ms(1000), ms(1000)).unwrap(),
    );
    let mut out = String::new();
    write_audacity_labels(&result, NonZeroU32::new(1000).unwrap(), &mut out).unwrap();
    assert_eq!(out, "0.000000\t1.000000\t1\n");
}

#[cfg(feature = "serde")]
#[test]
fn new_serialized_analysis_rejects_old_signal_flags_and_invalid_scores() {
    let a = scores(10, vec![0.9; 10]);
    let mut old = serde_json::to_value(&a).unwrap();
    let flags = old.as_object_mut().unwrap().remove("no_signal").unwrap();
    old["zero"] = flags;
    assert!(serde_json::from_value::<Analysis>(old).is_err());
    let mut invalid = serde_json::to_value(&a).unwrap();
    invalid["no_signal"][0] = serde_json::json!(true);
    assert!(serde_json::from_value::<Analysis>(invalid).is_err());
    let back: Analysis = serde_json::from_value(serde_json::to_value(&a).unwrap()).unwrap();
    assert_eq!(a, back);
}
