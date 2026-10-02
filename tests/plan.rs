mod common;

use std::num::NonZeroUsize;
use std::time::Duration;

use common::{Greedy, Signal, greedy_cuts, random_talk, raw_silences};
use silence_split::{
    AdjustConfig, Analysis, ConfigError, CutReason, Gap, Kind, Layout, Placement, Plan, PlanConfig,
    ScoreFrames, SegmentConfig, Threshold, Weights, adjust, plan, write_audacity_labels,
};

const RATE: u32 = 16_000;
const HOP: u64 = 160;

fn secs(s: u64) -> Duration {
    Duration::from_secs(s)
}

fn lengths(min: u64, target: u64, max: u64) -> PlanConfig {
    PlanConfig::new(secs(min), secs(target), secs(max)).unwrap()
}

/// Default weights with one change. `Weights` is non-exhaustive, so it is built by mutation.
fn weights(change: impl FnOnce(&mut Weights)) -> Weights {
    let mut w = Weights::default();
    change(&mut w);
    w
}

fn len_secs(start: u64, end: u64) -> f64 {
    (end - start) as f64 / f64::from(RATE)
}

/// The invariants every plan keeps.
fn check_plan(plan: &Plan, analysis: &Analysis, config_max: u64, gap: Gap) {
    let len = analysis.sample_count();
    assert_eq!(plan.cuts.len() + 1, plan.pieces.len());
    for piece in &plan.pieces {
        assert!(piece.start < piece.end && piece.end <= len);
        assert!(
            piece.end - piece.start <= config_max * u64::from(RATE),
            "piece within max: {piece:?}"
        );
        assert!(!piece.too_long);
    }
    for (i, cut) in plan.cuts.iter().enumerate() {
        assert_eq!(plan.pieces[i].end, cut.previous_end);
        assert_eq!(plan.pieces[i + 1].start, cut.next_start);
        assert!(cut.previous_end <= cut.next_start);
        assert_eq!(cut.previous_end % HOP, 0, "cut on a hop boundary");
        assert_eq!(cut.next_start % HOP, 0, "cut on a hop boundary");
    }
    match gap {
        // Joined in order, the pieces are the original audio.
        Gap::Keep => {
            assert_eq!(plan.pieces.first().map(|p| p.start), Some(0));
            assert_eq!(plan.pieces.last().map(|p| p.end), Some(len));
            assert!(plan.cuts.iter().all(|c| c.previous_end == c.next_start));
        }
        // Pieces and dropped ranges joined in order are the original audio, and only silence
        // outside the padding is dropped.
        _ => {
            let mut at = 0;
            for piece in &plan.pieces {
                assert!(piece.start >= at);
                at = piece.end;
            }
            let sound: Vec<_> = analysis
                .segments(&SegmentConfig::default())
                .into_iter()
                .filter(|s| s.kind == Kind::Sound)
                .collect();
            let (first, last) = (plan.pieces.first(), plan.pieces.last());
            let edges = [
                (0, first.map_or(len, |p| p.start)),
                (last.map_or(len, |p| p.end), len),
            ];
            let dropped = edges
                .into_iter()
                .chain(plan.cuts.iter().map(|c| (c.previous_end, c.next_start)));
            for (a, b) in dropped.filter(|(a, b)| a < b) {
                assert!(
                    sound.iter().all(|s| s.end <= a || s.start >= b),
                    "dropped {a}..{b} holds sound"
                );
            }
        }
    }
}

#[test]
fn plans_keep_their_invariants_on_random_talk() {
    for seed in 1..=8 {
        let signal = random_talk(RATE, seed, 150.0);
        let analysis = signal.analyze();
        for gap in [Gap::Keep, Gap::Drop] {
            for (min, target, max) in [(3, 10, 12), (5, 30, 30), (0, 20, 45)] {
                let config = lengths(min, target, max).gap(gap);
                let a = plan(&analysis, &SegmentConfig::default(), &config);
                check_plan(&a, &analysis, max, gap);
                assert_eq!(
                    a,
                    plan(&analysis, &SegmentConfig::default(), &config),
                    "deterministic"
                );
            }
        }
    }
}

#[test]
fn plans_beat_greedy_splitting_on_short_pieces() {
    let (min, max) = (5 * u64::from(RATE), 30 * u64::from(RATE));
    let config = lengths(5, 30, 30);
    let (mut short, mut greedy_short, mut cuts, mut outside) = (0, 0, 0, 0);
    for seed in [21, 22, 23] {
        let analysis = random_talk(RATE, seed, 150.0).analyze();
        let silences = raw_silences(&analysis);
        let in_silence = |c: u64| silences.iter().any(|s| s.start <= c && c <= s.end);
        let len = analysis.sample_count();
        let plan = plan(&analysis, &SegmentConfig::default(), &config);
        check_plan(&plan, &analysis, 30, Gap::Keep);
        short += plan.pieces.iter().filter(|p| p.end - p.start < min).count();
        cuts += plan.cuts.len();
        outside += plan
            .cuts
            .iter()
            .filter(|c| !in_silence(c.previous_end))
            .count();
        let greedy = greedy_cuts(len, &silences, max, Greedy::Longest);
        let mut at = 0;
        for end in greedy.into_iter().chain([len]) {
            greedy_short += usize::from(end - at < min);
            at = end;
        }
    }
    assert!(short < greedy_short, "{short} vs greedy {greedy_short}");
    assert!(
        outside * 10 <= cuts,
        "{outside} of {cuts} cuts outside silence"
    );
}

#[test]
fn flat_audio_is_split_equally() {
    let mut signal = Signal::new(RATE, 31);
    signal.noise(100.0, -20.0);
    let analysis = signal.analyze();
    let plan = plan(&analysis, &SegmentConfig::default(), &lengths(5, 30, 30));
    let lens: Vec<f64> = plan
        .pieces
        .iter()
        .map(|p| len_secs(p.start, p.end))
        .collect();
    assert_eq!(lens.len(), 4, "{lens:?}");
    // One grid candidate per 1 s block moves each end by up to a block.
    assert!(lens.iter().all(|l| (l - 25.0).abs() <= 2.0), "{lens:?}");
}

#[test]
fn free_silence_cuts_are_only_made_when_needed() {
    // Ten 5 s sounds between 2 s of digital zero, 68 s in all; only the maximum asks for cuts.
    let mut signal = Signal::new(RATE, 32);
    for i in 0..10 {
        if i > 0 {
            signal.zeros(2.0);
        }
        signal.speech(5.0, -18.0);
    }
    let analysis = signal.analyze();
    let config = lengths(0, 30, 30)
        .weights(weights(|w| w.target = 0.0))
        .unwrap();
    let plan = plan(&analysis, &SegmentConfig::default(), &config);
    assert_eq!(plan.cuts.len(), 2, "{:?}", plan.cuts);
    assert!(plan.cuts.iter().all(|c| c.reason == CutReason::Silence));
}

#[test]
fn audio_without_silence_leaves_no_short_tail() {
    let mut signal = Signal::new(RATE, 33);
    signal.speech(65.0, -18.0);
    let analysis = signal.analyze();
    let plan = plan(&analysis, &SegmentConfig::default(), &lengths(10, 30, 30));
    assert_eq!(plan.pieces.len(), 3);
    assert!(
        plan.pieces.iter().all(|p| !p.too_short),
        "{:?}",
        plan.pieces
    );
    assert!(plan.cuts.iter().all(|c| c.reason != CutReason::Silence));
}

#[test]
fn audio_shorter_than_min_is_one_flagged_piece() {
    let mut signal = Signal::new(RATE, 34);
    signal.speech(3.0, -18.0);
    let analysis = signal.analyze();
    let plan = plan(&analysis, &SegmentConfig::default(), &lengths(5, 10, 20));
    assert_eq!(plan.pieces.len(), 1);
    assert!(plan.pieces[0].too_short);
    assert_eq!((plan.pieces[0].start, plan.pieces[0].end), (0, signal.at()));
}

#[test]
fn a_pause_beats_a_stop_closure_in_the_same_block() {
    // 2.1 s of speech, an 80 ms gap, 0.4 s of speech, a 200 ms gap, then speech. Both gaps are
    // shorter than the minimum silence and fall in the grid block from 2.01 s to 3.01 s.
    let mut signal = Signal::new(RATE, 35);
    signal
        .speech(2.1, -18.0)
        .noise(0.08, -70.0)
        .speech(0.4, -18.0);
    let pause = signal.at();
    signal.noise(0.2, -70.0).speech(3.0, -18.0);
    let analysis = signal.analyze();
    let plan = plan(&analysis, &SegmentConfig::default(), &lengths(1, 5, 5));
    assert_eq!(plan.cuts.len(), 1);
    let cut = plan.cuts[0];
    assert_eq!(cut.reason, CutReason::Quiet);
    assert!(
        cut.previous_end >= pause && cut.previous_end <= pause + 3200,
        "cut at {} in the 200 ms pause at {pause}",
        cut.previous_end
    );
}

#[test]
fn silence_longer_than_max_is_cut_or_dropped() {
    let mut signal = Signal::new(RATE, 36);
    signal
        .speech(5.0, -18.0)
        .noise(40.0, -70.0)
        .speech(5.0, -18.0);
    let analysis = signal.analyze();

    let keep = plan(&analysis, &SegmentConfig::default(), &lengths(0, 20, 20));
    check_plan(&keep, &analysis, 20, Gap::Keep);
    assert!(keep.pieces.len() >= 3);

    let drop = plan(
        &analysis,
        &SegmentConfig::default(),
        &lengths(0, 20, 20).gap(Gap::Drop),
    );
    check_plan(&drop, &analysis, 20, Gap::Drop);
    assert_eq!(drop.pieces.len(), 2, "{drop:?}");
    let dropped = len_secs(drop.cuts[0].previous_end, drop.cuts[0].next_start);
    assert!(
        (dropped - 39.7).abs() < 0.1,
        "keeps 200 ms after and 100 ms before sound: {dropped}"
    );
}

#[test]
fn leading_and_trailing_silence() {
    let mut signal = Signal::new(RATE, 37);
    signal
        .noise(3.0, -70.0)
        .speech(8.0, -18.0)
        .noise(0.5, -70.0)
        .speech(8.0, -18.0)
        .zeros(4.0);
    let analysis = signal.analyze();
    let segments = SegmentConfig::default();
    let spans = analysis.segments(&segments);
    assert_eq!(spans.first().map(|s| s.kind), Some(Kind::Silence));
    assert_eq!(spans.last().map(|s| s.kind), Some(Kind::Silence));

    // Dropped edge silence ends exactly where the padded sound begins and ends.
    let drop = plan(&analysis, &segments, &lengths(2, 10, 12).gap(Gap::Drop));
    assert_eq!(drop.pieces.len(), 2);
    assert_eq!(drop.pieces[0].start, spans[1].start);
    assert_eq!(drop.pieces[1].end, spans[spans.len() - 2].end);
    assert_eq!(
        Some(drop.pieces[0].start..drop.pieces[1].end),
        analysis.trim(&segments)
    );

    // Kept edge silence stays with the first and last pieces instead of becoming pieces of its own.
    let keep = plan(&analysis, &segments, &lengths(2, 12, 15));
    assert_eq!(keep.pieces.len(), 2, "{keep:?}");
}

#[test]
fn count_asks_for_an_exact_number_of_pieces() {
    let signal = random_talk(RATE, 38, 240.0);
    let analysis = signal.analyze();
    for n in [1, 3, 7] {
        let config = lengths(0, 60, 400).count(NonZeroUsize::new(n).unwrap());
        let plan = plan(&analysis, &SegmentConfig::default(), &config);
        assert_eq!(plan.pieces.len(), n);
        check_plan(&plan, &analysis, 400, Gap::Keep);
        let share = signal.secs() as f64 / n as f64;
        for p in &plan.pieces {
            assert!(
                (len_secs(p.start, p.end) - share).abs() < share * 0.2,
                "{:?}",
                plan.pieces
            );
        }
    }
}

#[test]
fn a_count_too_small_for_max_spreads_the_excess() {
    let analysis = random_talk(RATE, 46, 300.0).analyze();
    let config = lengths(5, 20, 30).count(NonZeroUsize::new(4).unwrap());
    let plan = plan(&analysis, &SegmentConfig::default(), &config);
    assert_eq!(plan.pieces.len(), 4);
    let share = len_secs(0, analysis.sample_count()) / 4.0;
    for p in &plan.pieces {
        assert!(p.too_long);
        assert!(
            (len_secs(p.start, p.end) - share).abs() < share * 0.2,
            "{:?}",
            plan.pieces
        );
    }
}

#[test]
fn placement_inside_a_long_silence() {
    let mut signal = Signal::new(RATE, 39);
    signal
        .speech(8.0, -18.0)
        .noise(2.0, -70.0)
        .speech(8.0, -18.0);
    let analysis = signal.analyze();
    let at = |placement| {
        let config = lengths(2, 10, 12).placement(placement).unwrap();
        let plan = plan(&analysis, &SegmentConfig::default(), &config);
        assert_eq!(plan.cuts.len(), 1);
        len_secs(0, plan.cuts[0].previous_end)
    };
    // The cut range inside the 8–10 s pause excludes 200 ms after and 100 ms before sound.
    assert!((at(Placement::Center) - 9.05).abs() < 0.1);
    assert!((at(Placement::Ratio(0.0)) - 8.2).abs() < 0.1);
    assert!((at(Placement::Ratio(1.0)) - 9.9).abs() < 0.1);
    assert_eq!(
        lengths(2, 10, 12).placement(Placement::Ratio(1.5)).err(),
        Some(ConfigError::InvalidRatio)
    );
}

/// Scores at 16 kHz with a 160-sample hop: frame `i` starts at sample `160 · i`.
fn scored(runs: &[(f32, usize)]) -> Analysis {
    let scores: Vec<f32> = runs
        .iter()
        .flat_map(|(v, n)| std::iter::repeat_n(*v, *n))
        .collect();
    let len = scores.len() as u64 * HOP;
    let layout = Layout {
        sample_rate: RATE,
        channels: 1,
    };
    Analysis::from_scores(
        layout,
        len,
        ScoreFrames {
            sample_rate: RATE,
            hop: HOP as u32,
        },
        scores,
    )
    .unwrap()
}

#[test]
fn quietest_placement_finds_the_quietest_stretch() {
    // Silence in frames 800..1000 at score 0.2, quieter at 0.05 in 900..930.
    let analysis = scored(&[(0.9, 800), (0.2, 100), (0.05, 30), (0.2, 70), (0.9, 800)]);
    let plan = plan(&analysis, &SegmentConfig::default(), &lengths(2, 10, 12));
    assert_eq!(plan.cuts.len(), 1);
    let cut = plan.cuts[0].previous_end / HOP;
    assert!((900..=930).contains(&cut), "cut at frame {cut}");
}

#[test]
fn padding_wider_than_the_maximum_allows_cuts_within_it() {
    // Two-second sounds between one-second pauses, with 1.2 s of padding before sound and a 1 s
    // maximum: the padding has to give way.
    let runs: Vec<(f32, usize)> = (0..20).flat_map(|_| [(0.9, 200), (0.0, 100)]).collect();
    let analysis = scored(&runs);
    let segments =
        SegmentConfig::default().padding(Duration::from_millis(1200), Duration::from_millis(100));
    let config = PlanConfig::new(Duration::ZERO, secs(1), secs(1)).unwrap();
    let plan = plan(&analysis, &segments, &config);
    assert!(plan.pieces.iter().all(|p| !p.too_long), "{:?}", plan.pieces);
}

#[test]
fn padding_longer_than_a_block_does_not_force_long_pieces() {
    // 1 kHz, 10 ms frames: 300 ms of silence, then 3 s of sound, with 180 ms of pre padding and a
    // 285 ms maximum.
    let layout = Layout {
        sample_rate: 1000,
        channels: 1,
    };
    let scores: Vec<f32> = [vec![0.0; 30], vec![1.0; 300]].concat();
    let analysis = Analysis::from_scores(
        layout,
        3300,
        ScoreFrames {
            sample_rate: 1000,
            hop: 10,
        },
        scores,
    )
    .unwrap();
    let segments = SegmentConfig::default()
        .threshold(Threshold::Abs(0.5))
        .unwrap()
        .min_silence(Duration::ZERO)
        .min_sound(Duration::ZERO)
        .padding(Duration::from_millis(180), Duration::ZERO);
    let ms = Duration::from_millis;
    let config = PlanConfig::new(ms(40), ms(120), ms(285)).unwrap();
    let plan = plan(&analysis, &segments, &config);
    assert!(plan.pieces.iter().all(|p| !p.too_long), "{:?}", plan.pieces);
}

#[test]
fn invalid_plan_configs_are_rejected() {
    let d = Duration::from_secs;
    assert_eq!(
        PlanConfig::new(d(5), d(3), d(10)).err(),
        Some(ConfigError::InvalidLengths)
    );
    assert_eq!(
        PlanConfig::new(d(0), d(12), d(10)).err(),
        Some(ConfigError::InvalidLengths)
    );
    assert_eq!(
        PlanConfig::new(d(0), d(0), d(10)).err(),
        Some(ConfigError::InvalidLengths)
    );
    let bad = |w: Weights| lengths(1, 2, 3).weights(w).err();
    assert_eq!(
        bad(weights(|w| w.cut = 0.0)),
        Some(ConfigError::InvalidWeights)
    );
    assert_eq!(
        bad(weights(|w| w.level = -1.0)),
        Some(ConfigError::InvalidWeights)
    );
    assert_eq!(
        bad(weights(|w| w.short = f64::NAN)),
        Some(ConfigError::InvalidWeights)
    );
}

#[test]
fn a_maximum_below_one_hop_is_flagged() {
    let mut signal = Signal::new(RATE, 40);
    signal.speech(0.1, -18.0);
    let analysis = signal.analyze();
    let config = PlanConfig::new(
        Duration::ZERO,
        Duration::from_millis(5),
        Duration::from_millis(5),
    )
    .unwrap();
    let plan = plan(&analysis, &SegmentConfig::default(), &config);
    assert!(plan.pieces.iter().all(|p| p.too_long));
    assert_eq!(plan.pieces.last().map(|p| p.end), Some(signal.at()));
}

#[test]
fn empty_audio_has_no_pieces() {
    let analysis = Signal::new(RATE, 41).analyze();
    assert_eq!(
        plan(&analysis, &SegmentConfig::default(), &lengths(1, 2, 3)),
        Plan::default()
    );
    assert_eq!(
        adjust(
            &analysis,
            &SegmentConfig::default(),
            &[5],
            &AdjustConfig::new(secs(1))
        )
        .plan,
        Plan::default()
    );
    let mut silent = Signal::new(RATE, 42);
    silent.zeros(5.0);
    let analysis = silent.analyze();
    let drop = plan(
        &analysis,
        &SegmentConfig::default(),
        &lengths(1, 2, 3).gap(Gap::Drop),
    );
    assert!(drop.pieces.is_empty(), "nothing to keep");
}

#[test]
fn adjust_moves_points_into_nearby_silence() {
    let mut signal = Signal::new(RATE, 43);
    signal.speech(10.0, -18.0);
    let pause = signal.at();
    signal
        .noise(1.0, -70.0)
        .speech(10.0, -18.0)
        .speech(10.0, -18.0);
    let analysis = signal.analyze();
    let second = u64::from(RATE);
    let points = [pause + 2 * second, 25 * second, 0, signal.at()];
    let plan = adjust(
        &analysis,
        &SegmentConfig::default(),
        &points,
        &AdjustConfig::new(secs(3)),
    )
    .plan;
    check_plan(&plan, &analysis, 60, Gap::Keep);
    assert_eq!(
        plan.cuts.len(),
        2,
        "points at the ends are ignored: {plan:?}"
    );
    let first = plan.cuts[0];
    assert_eq!(first.reason, CutReason::Silence);
    assert!(
        first.previous_end >= pause && first.previous_end <= pause + second,
        "{first:?} into the pause at {pause}"
    );
    assert_ne!(plan.cuts[1].reason, CutReason::Silence);
    assert!(plan.cuts[1].previous_end.abs_diff(25 * second) <= 3 * second);

    // Close points keep their order: each stays on its side of the midpoint between them.
    let close = [pause - second, pause + 2 * second];
    let plan = adjust(
        &analysis,
        &SegmentConfig::default(),
        &close,
        &AdjustConfig::new(secs(3)),
    )
    .plan;
    assert_eq!(plan.cuts.len(), 2, "{plan:?}");
    let mid = close[0].midpoint(close[1]);
    assert!(
        plan.cuts[0].previous_end < mid && mid <= plan.cuts[1].previous_end,
        "{plan:?}"
    );

    // With no window, a point stays at the nearest frame boundary.
    let exact = adjust(
        &analysis,
        &SegmentConfig::default(),
        &[12_345],
        &AdjustConfig::new(Duration::ZERO),
    )
    .plan;
    assert_eq!(exact.cuts[0].previous_end, 12_320);
}

#[test]
fn adjust_cuts_a_long_silence_near_the_point() {
    // One second of sound, ten of silence, one of sound; the point is in the middle of the
    // silence, far from its padding.
    let analysis = scored(&[(0.9, 100), (0.0, 1000), (0.9, 100)]);
    let point = 600 * HOP;
    let plan = adjust(
        &analysis,
        &SegmentConfig::default(),
        &[point],
        &AdjustConfig::new(secs(1)),
    )
    .plan;
    assert_eq!(plan.cuts.len(), 1);
    assert_eq!(plan.cuts[0].reason, CutReason::Silence);
    assert!(plan.cuts[0].previous_end.abs_diff(point) <= u64::from(RATE));
}

#[test]
fn audacity_labels_list_pieces_in_seconds() {
    let mut signal = Signal::new(RATE, 44);
    signal
        .speech(8.0, -18.0)
        .noise(1.0, -70.0)
        .speech(8.0, -18.0);
    let analysis = signal.analyze();
    let plan = plan(&analysis, &SegmentConfig::default(), &lengths(2, 10, 12));
    let mut out = String::new();
    write_audacity_labels(&plan, std::num::NonZeroU32::new(RATE).unwrap(), &mut out).unwrap();
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 2);
    let fields: Vec<&str> = lines[0].split('\t').collect();
    assert_eq!(fields[0], "0.000000");
    assert_eq!(fields[2], "1");
    let end: f64 = fields[1].parse().unwrap();
    assert!((end - plan.pieces[0].end as f64 / f64::from(RATE)).abs() < 1e-6);
    assert!(lines[1].ends_with("\t2"));
}

#[cfg(feature = "serde")]
#[test]
fn analysis_and_plan_round_trip_through_serde() {
    let signal = random_talk(RATE, 45, 40.0);
    let analysis = signal.analyze();
    let json = serde_json::to_string(&analysis).unwrap();
    let back: Analysis = serde_json::from_str(&json).unwrap();
    assert_eq!(back, analysis);

    let plan = plan(&analysis, &SegmentConfig::default(), &lengths(2, 10, 12));
    let back: Plan = serde_json::from_str(&serde_json::to_string(&plan).unwrap()).unwrap();
    assert_eq!(back, plan);

    let mut value: serde_json::Value = serde_json::from_str(&json).unwrap();
    value["len"] = serde_json::json!(analysis.sample_count() * 2);
    let error = serde_json::from_value::<Analysis>(value).unwrap_err();
    assert!(error.to_string().contains("frame count"), "{error}");

    let layout = silence_split::Layout {
        sample_rate: RATE,
        channels: 1,
    };
    let scores = Analysis::from_scores(
        layout,
        1600,
        ScoreFrames {
            sample_rate: RATE,
            hop: 160,
        },
        vec![0.5; 10],
    )
    .unwrap();
    let back: Analysis = serde_json::from_str(&serde_json::to_string(&scores).unwrap()).unwrap();
    assert_eq!(back, scores);
}
