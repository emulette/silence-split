use silence_split::{Analysis, Gap, Layout, PlanConfig, SegmentConfig, plan};
use std::{num::NonZeroUsize, time::Duration};

fn ms(value: u64) -> Duration {
    Duration::from_millis(value)
}
fn scored(hop: u32, runs: &[(f32, usize)]) -> Analysis {
    let scores: Vec<f32> = runs
        .iter()
        .flat_map(|(v, n)| std::iter::repeat_n(*v, *n))
        .collect();
    Analysis::from_scores(
        Layout {
            sample_rate: 1000,
            channels: 1,
        },
        scores.len() as u64 * u64::from(hop),
        1000,
        hop,
        scores,
    )
    .unwrap()
}

#[test]
fn count_drop_keeps_max_when_a_feasible_plan_exists() {
    let input = scored(10, &[(0.9, 100), (0.0, 1000), (0.9, 200)]);
    let cfg = PlanConfig::new(ms(0), ms(1000), ms(1000))
        .unwrap()
        .gap(Gap::Drop);
    let segments = SegmentConfig::default();
    let feasible = plan(&input, &segments, &cfg);
    assert_eq!(feasible.pieces.len(), 4);
    assert!(feasible.pieces.iter().all(|p| !p.too_long));
    let counted = plan(&input, &segments, &cfg.count(NonZeroUsize::new(4).unwrap()));
    assert_eq!(counted.pieces.len(), 4);
    assert!(counted.pieces.iter().all(|p| !p.too_long), "{counted:?}");
}

#[test]
fn exact_two_thirty_second_pieces_are_feasible() {
    let input = scored(10, &[(0.9, 6000)]);
    let cfg = PlanConfig::new(ms(5000), ms(30000), ms(30000))
        .unwrap()
        .count(NonZeroUsize::new(2).unwrap());
    let result = plan(&input, &SegmentConfig::default(), &cfg);
    assert_eq!(result.pieces.len(), 2);
    assert!(result.pieces.iter().all(|p| !p.too_long), "{result:?}");
    assert_eq!(result.cuts[0].end, 30_000);
}

#[test]
fn padding_must_not_exclude_every_cut_around_a_short_sound() {
    let input = scored(100, &[(0.0, 50), (0.9, 1), (0.0, 50)]);
    let segments = SegmentConfig::default().padding(ms(880), ms(1130));
    let cfg = PlanConfig::new(ms(760), ms(1120), ms(1480)).unwrap();
    let result = plan(&input, &segments, &cfg);
    assert!(result.pieces.iter().all(|p| !p.too_long), "{result:?}");
}

/// Enumerate every possible cut subset in a small timeline. Each boundary is either a point or
/// a removable silence. This oracle checks feasibility independently of the planner's grid.
fn smallest_maximum(boundaries: &[(u64, u64)], count: usize) -> u64 {
    let inner = boundaries.len() - 2;
    (0u32..1 << inner)
        .filter(|mask| mask.count_ones() as usize + 1 == count)
        .map(|mask| {
            let mut at = boundaries[0].1;
            let mut longest = 0;
            for (i, &(left, right)) in boundaries[1..=inner].iter().enumerate() {
                if mask & (1 << i) != 0 {
                    longest = longest.max(left - at);
                    at = right;
                }
            }
            longest.max(boundaries[inner + 1].0 - at)
        })
        .min()
        .unwrap()
}

#[test]
fn count_feasibility_matches_all_frame_partitions_at_a_fractional_rate() {
    let rate = 44_100;
    let at = |i: u64| i * 512 * u64::from(rate) / 16_000;
    // Two interior silences; the final frame is partial.
    let len = at(9) - 500;
    let input = Analysis::from_scores(
        Layout {
            sample_rate: rate,
            channels: 1,
        },
        len,
        16_000,
        512,
        vec![0.9, 0.9, 0.0, 0.0, 0.9, 0.9, 0.0, 0.9, 0.9],
    )
    .unwrap();
    let segments = SegmentConfig::default()
        .min_sound(Duration::ZERO)
        .min_silence(Duration::ZERO)
        .padding(Duration::ZERO, Duration::ZERO);
    for gap in [Gap::Keep, Gap::Drop] {
        let frames: Vec<(u64, u64)> = if gap == Gap::Keep {
            (0..=9).map(|i| (i, i)).collect()
        } else {
            vec![(0, 0), (1, 1), (2, 4), (5, 5), (6, 7), (8, 8), (9, 9)]
        };
        let positions: Vec<(u64, u64)> = frames
            .iter()
            .map(|(l, r)| (at(*l).min(len), at(*r).min(len)))
            .collect();
        for requested in 1..=10 {
            let count = requested.min(positions.len() - 1);
            let attainable = smallest_maximum(&positions, count);
            for maximum in [20, 50, 70, 100, 200] {
                let config = PlanConfig::new(ms(0), ms(maximum), ms(maximum))
                    .unwrap()
                    .gap(gap)
                    .count(NonZeroUsize::new(requested).unwrap());
                let result = plan(&input, &segments, &config);
                let limit = maximum * u64::from(rate) / 1000;
                assert_eq!(result.pieces.len(), count, "{gap:?}: {result:?}");
                let longest = result.pieces.iter().map(|p| p.end - p.start).max().unwrap();
                assert!(
                    longest <= limit.max(attainable),
                    "{gap:?}, count={count}, max={maximum}, attainable={attainable}: {result:?}"
                );
                assert_eq!(result.pieces[0].start, 0);
                assert_eq!(result.pieces[count - 1].end, len);
                for piece in &result.pieces {
                    assert!(piece.start < piece.end);
                    assert_eq!(piece.too_long, piece.end - piece.start > limit);
                }
                for cut in &result.cuts {
                    assert!(positions.contains(&(cut.end, cut.start)), "{cut:?}");
                }
            }
        }
    }
}
