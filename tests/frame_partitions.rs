mod common;

use common::Rng;
use silence_split::{Analysis, Gap, Layout, PlanConfig, ScoreFrames, SegmentConfig, plan};
use std::{num::NonZeroUsize, time::Duration};

/// Enumerate all subsets of the retained frame boundaries, independently of acoustic candidates
/// and the planner's feasibility search. A paired boundary represents removable silence.
fn smallest_maximum(boundaries: &[(u64, u64)], count: usize) -> u64 {
    let inner = boundaries.len() - 2;
    (0usize..1 << inner)
        .filter(|mask| mask.count_ones() as usize + 1 == count)
        .map(|mask| {
            let mut start = boundaries[0].1;
            let mut longest = 0;
            for (i, &(left, right)) in boundaries[1..=inner].iter().enumerate() {
                if mask & (1 << i) != 0 {
                    longest = longest.max(left - start);
                    start = right;
                }
            }
            longest.max(boundaries.last().unwrap().0 - start)
        })
        .min()
        .unwrap()
}

#[test]
fn random_fractional_grids_padding_and_drop_match_exhaustive_feasibility() {
    let mut rng = Rng::new(0x37d1_829a);
    for trial in 0..2000 {
        let rate = [8000, 11025, 16000, 22050, 44100, 48000][(rng.next_u64() % 6) as usize];
        let score_rate = [1000, 8000, 16000, 22050][(rng.next_u64() % 4) as usize];
        let hop = 1 + (rng.next_u64() % 2000) as u32;
        if u64::from(hop) * u64::from(rate) < u64::from(score_rate) {
            continue;
        }
        let n = 3 + (rng.next_u64() % 8) as usize;
        let start = |k: usize| k as u64 * u64::from(hop) * u64::from(rate) / u64::from(score_rate);
        let len = start(n) - rng.next_u64() % (start(n) - start(n - 1));
        let position = |k: usize| if k == n { len } else { start(k) };
        let values: Vec<f32> = (0..n)
            .map(|_| if rng.next_u64() % 3 == 0 { 0.0 } else { 0.9 })
            .collect();
        let mut silences = Vec::new();
        let mut i = 0;
        while i < n {
            if values[i] != 0.0 {
                i += 1;
                continue;
            }
            let begin = i;
            while i < n && values[i] == 0.0 {
                i += 1;
            }
            silences.push(begin..i);
        }
        let analysis = Analysis::from_scores(
            Layout {
                sample_rate: rate,
                channels: 1,
            },
            len,
            ScoreFrames {
                sample_rate: score_rate,
                hop,
            },
            values,
        )
        .unwrap();
        let frame_nanos = u64::from(hop) * 1_000_000_000 / u64::from(score_rate);
        let pre = Duration::from_nanos((rng.next_u64() % 5) * frame_nanos);
        let tail = Duration::from_nanos((rng.next_u64() % 5) * frame_nanos);
        let segment = SegmentConfig::default()
            .min_silence(Duration::ZERO)
            .min_sound(Duration::ZERO)
            .padding(pre, tail);
        for gap in [Gap::Keep, Gap::Drop] {
            for fraction in [1, 3, 9] {
                let max = Duration::from_nanos((frame_nanos * fraction / 2).max(1));
                let min = max / (1 + (rng.next_u64() % 3) as u32);
                let frames_floor = |d: Duration| {
                    (d.as_nanos() * u128::from(score_rate) / (u128::from(hop) * 1_000_000_000))
                        as usize
                };
                let frames_round = |d: Duration| {
                    ((d.as_nanos() * u128::from(score_rate) + u128::from(hop) * 500_000_000)
                        / (u128::from(hop) * 1_000_000_000)) as usize
                };
                let block =
                    frames_floor(((max - min) / 2).max(max / 8).min(Duration::from_secs(1))).max(1);
                let cap = frames_floor(max).saturating_sub(2 * block);
                let (pre, tail) = (frames_round(pre).min(cap), frames_round(tail).min(cap));
                let all_silent = silences.first().is_some_and(|r| r.len() == n);
                let first = if gap == Gap::Drop {
                    silences
                        .first()
                        .filter(|r| r.start == 0)
                        .map_or(0, |r| r.end.saturating_sub(pre))
                } else {
                    0
                };
                let last = if gap == Gap::Drop {
                    silences
                        .last()
                        .filter(|r| r.end == n)
                        .map_or(n, |r| (r.start + tail).min(n))
                } else {
                    n
                };
                let mut boundaries = Vec::new();
                if !(gap == Gap::Drop && all_silent) {
                    let gaps: Vec<_> = silences
                        .iter()
                        .filter(|r| gap == Gap::Drop && r.start > 0 && r.end < n)
                        .map(|r| ((r.start + tail).min(n), r.end.saturating_sub(pre)))
                        .filter(|(l, r)| l < r)
                        .collect();
                    let mut k = first;
                    while k <= last {
                        if let Some(&(_, end)) = gaps.iter().find(|(begin, _)| *begin == k) {
                            boundaries.push((position(k), position(end)));
                            k = end + 1;
                        } else {
                            boundaries.push((position(k), position(k)));
                            k += 1;
                        }
                    }
                }
                for requested in [1, 2, n, n + 2] {
                    let config = PlanConfig::new(min, max, max)
                        .unwrap()
                        .gap(gap)
                        .count(NonZeroUsize::new(requested).unwrap());
                    let result = plan(&analysis, &segment, &config);
                    if boundaries.is_empty() {
                        assert!(result.pieces.is_empty());
                        continue;
                    }
                    let count = requested.min(boundaries.len() - 1);
                    let attainable = smallest_maximum(&boundaries, count);
                    let limit = (max.as_nanos() * u128::from(rate) / 1_000_000_000) as u64;
                    assert_eq!(
                        result.pieces.len(),
                        count,
                        "trial={trial}, {gap:?}, requested={requested}: {result:?}"
                    );
                    let longest = result.pieces.iter().map(|p| p.end - p.start).max().unwrap();
                    assert!(
                        longest <= limit.max(attainable),
                        "trial={trial}, {gap:?}, requested={requested}, max={max:?}, attainable={attainable}, boundaries={boundaries:?}: {result:?}"
                    );
                    if attainable > limit {
                        assert_eq!(longest, attainable);
                    }
                    assert_eq!(result.pieces[0].start, position(first));
                    assert_eq!(result.pieces[count - 1].end, position(last));
                    assert_eq!(result.cuts.len(), count - 1);
                    for (i, cut) in result.cuts.iter().enumerate() {
                        assert!(boundaries.contains(&(cut.previous_end, cut.next_start)));
                        assert_eq!(result.pieces[i].end, cut.previous_end);
                        assert_eq!(result.pieces[i + 1].start, cut.next_start);
                    }
                    for piece in result.pieces {
                        assert!(piece.start < piece.end);
                        assert_eq!(piece.too_long, piece.end - piece.start > limit);
                    }
                }
            }
        }
    }
}
