# silence-split

Split long PCM audio into pieces that never exceed your maximum length, cutting in silence, or at
the quietest spot when there is none. No model, no FFmpeg, and every position is a sample index in
the original timeline.

- **Global cut plan.** Cuts are chosen over the whole file at once under a hard maximum, a soft
  minimum and a target length, weighing where each cut falls against the piece lengths. A piece
  below the minimum, such as a short leftover at the end, is penalized rather than forbidden.
- **Any sample rate.** 44.1 kHz or 48 kHz audio is analyzed as is. Frame positions are exact
  integers, so they never drift at rates where 10 ms is not a whole number of samples.
- **Automatic threshold.** The silence threshold is derived from each file's own level
  distribution, with hysteresis, and ignores signal-free frames, including digital silence and
  filtered DC padding.
- **External voice activity scores.** Probabilities from a detector running at another rate, such
  as Silero VAD at 16 kHz, can replace the level analysis and are mapped to the original rate.
- **No default dependencies**, and `no_std` with `alloc` and `libm`.

## Install

```toml
[dependencies]
silence-split = "0.2.0"
```

The minimum supported Rust version is 1.85.

## Usage

```rust
use std::time::Duration;

use silence_split::{AnalyzeConfig, Analyzer, ConfigError, Gap, Layout, PlanConfig, SegmentConfig, plan};

/// Splits mono audio for speech recognition: pieces of at most 30 s, 30 s when possible and
/// preferably not under 5 s, with the silence at each cut dropped.
fn split(pcm: &[f32], sample_rate: u32) -> Result<Vec<&[f32]>, ConfigError> {
    let mut analyzer = Analyzer::new(Layout { sample_rate, channels: 1 }, &AnalyzeConfig::default())?;
    analyzer.push_interleaved(pcm); // or push decoded packets one at a time
    let analysis = analyzer.finish();

    let max = Duration::from_secs(30);
    let config = PlanConfig::new(Duration::from_secs(5), max, max)?.gap(Gap::Drop);
    let plan = plan(&analysis, &SegmentConfig::default(), &config);
    Ok(plan.pieces.iter().map(|p| &pcm[p.start as usize..p.end as usize]).collect())
}
```

Automatic detection uses the recording's relative energy distribution. It preserves steady sound
when that distribution cannot support a separating threshold; steady background noise can be kept
too. To remove noise at a known level, choose an absolute threshold with
`SegmentConfig::default().threshold(Threshold::Abs(-40.0))?`, using a dBFS value appropriate for
your recording after the detection high-pass filter. Use `Gap::Keep` to preserve all samples.

Each `Piece` is a half-open range of samples per channel, flagged when it is shorter than the
minimum or longer than the maximum, and each `Cut` says whether it fell in silence, in quiet sound,
or was forced.

- `Gap::Keep` (default) keeps all audio: the pieces joined in order are the original. Use it for
  chapters or when the pieces are put back together.
- `Gap::Drop` drops the silence at each cut and before the first and after the last sound, keeping
  100 ms before sound and 200 ms after it by default.
- `PlanConfig::count` asks for an exact number of pieces instead of a target length. If the count
  and maximum cannot both be met on the frame grid, the planner minimizes the longest piece and
  flags pieces that exceed the maximum. With `Gap::Drop`, this check accounts for removable silence.
- `adjust` moves cut points you already have, such as chapter marks, into nearby silence. An
  `AdjustConfig` controls the window, weights and placement; the result maps every original input
  point to its cut or `None` if skipped.
- `Analysis::segments` returns the silence and sound spans, and `Analysis::trim` the range between
  the first and last sound.
- `Analysis::from_scores` takes per-frame speech probabilities and a named
  `ScoreFrames { sample_rate, hop }` timebase from an external detector.
- `Analysis::levels` reports the floor, reference and resolved thresholds for a `SegmentConfig`,
  or `None` for empty or entirely signal-free PCM.
- `write_audacity_labels` writes the pieces as an Audacity label track for review, using a
  `NonZeroU32` sample rate.

```rust
use std::{num::NonZeroU32, time::Duration};

use silence_split::{
    AdjustConfig, Analysis, Layout, ScoreFrames, SegmentConfig, adjust, write_audacity_labels,
};

let layout = Layout { sample_rate: 44_100, channels: 1 };
let frames = ScoreFrames { sample_rate: 16_000, hop: 160 };
let analysis = Analysis::from_scores(layout, 44_100, frames, vec![0.9; 100]).unwrap();
let segments = SegmentConfig::default();
let levels = analysis.levels(&segments).unwrap();
assert_eq!(levels.on, 0.5);
let config = AdjustConfig::new(Duration::from_millis(100));
let result = adjust(&analysis, &segments, &[22_050, 22_050], &config);
assert!(result.point_cuts[0].is_some());
assert!(result.point_cuts[1].is_none()); // the duplicate was skipped
let cut = result.point_cuts[0].unwrap();
assert_eq!(cut.previous_end, cut.next_start); // adjust keeps all audio
let mut labels = String::new();
write_audacity_labels(&result.plan, NonZeroU32::new(44_100).unwrap(), &mut labels).unwrap();
```

## How it works

1. **Analysis.** Each channel is high-passed at 80 Hz for detection only, so hum and rumble do not
   read as sound. Every 10 ms hop gets one level in dBFS over a 30 ms window. Frames whose window
   is all digital zero, or whose filtered mean power is at most `1e-20` (-200 dBFS), are left out
   of the level statistics and start as silence. Samples that are not finite are read as zero.
2. **Segmentation.** With the automatic threshold, sound starts at
   `max(floor + 4, min(floor + 6, ref − 20))` dBFS, where `floor` and `ref` are the 10th and 99th
   percentile frame levels. If that threshold exceeds `ref`, it is lowered to `floor` to retain
   steady sound. Sound ends 3 dB below the resolved threshold. Silence shorter than 250 ms and sound
   shorter than 100 ms are ignored, except that silence covering the entire input stays silent.
3. **Planning.** Candidate cuts are every silence and the quietest point of each block of
   `min(1 s, max((max − min) / 2, max / 8))` outside them. A dynamic program over the candidates
   minimizes the cost of the cuts (a fixed cost per cut, the level at the cut and how short the
   pause is) plus the cost of the piece lengths (squared distance from the target and a penalty
   below the minimum), with no piece over the maximum. Silence beats quiet sound, which beats an
   equal split of flat audio, through the cost alone.

Cuts fall on analysis frame boundaries, 10 ms apart by default. The planner adds frame boundaries
when needed to meet the maximum or piece count; padding can yield to these limits. Cut reasons use
that effective padding, so a `Silence` cut can lie inside a sound span returned by `segments`,
which uses the original padding. `Cut::previous_end` ends the earlier piece and `Cut::next_start`
starts the later one; with `Drop`, the range between them is discarded.

Count mode prunes predecessor ranges using cost lower bounds while preserving the same cut path
and tie rules. Its parent table still needs about `4 × count × candidates` bytes, plus linear
working memory; large counts can exhaust memory. With `Drop`, the target is the original span
between the trimmed ends divided by the count, including internal silence that may be removed.

The default thresholds and cost weights are starting values that have not been tuned on a recorded
corpus, so treat them as experimental and check a plan against the audio before relying on it.

## Compared with greedy splitters

Greedy splitters in the style of Silero VAD at its maximum length cut once a piece would exceed
the maximum: in the middle of the longest silence within reach (its current default) or of the
last one (its legacy mode). All splitters below keep all audio. On 20 synthetic ten-minute talks
at 16 kHz (speech-like phrases of 0.5–12 s between pauses of 0.1–4 s):

| Limits | Splitter | Pieces | Shorter than min | Cuts outside silence | Mean length | Std. dev. |
|---|---|---:|---:|---:|---:|---:|
| min 5 s, target 30 s, max 30 s | silence-split `plan` | 502 | 0 | 2.7 % | 24.1 s | 3.7 s |
| | Greedy, longest silence | 672 | 35 | 0.6 % | 18.0 s | 7.6 s |
| | Greedy, last silence | 501 | 0 | 0.8 % | 24.1 s | 4.8 s |
| min 10 s, target 20 s, max 30 s | silence-split `plan` | 568 | 1 | 1.1 % | 21.3 s | 3.8 s |
| | Greedy, longest silence | 672 | 119 | 0.6 % | 18.0 s | 7.6 s |
| | Greedy, last silence | 501 | 9 | 0.8 % | 24.1 s | 4.8 s |

The plan follows the target and keeps pieces above the minimum and close in length, at the cost
of a few more cuts in quiet sound rather than silence. The greedy splitters ignore the minimum and
the target. Reproduce with `cargo run --release --example greedy_comparison`.

## Not included

Voice activity models, decoding and encoding, resampling, loudness measurement, clipping and other
quality checks, breath detection, editing-program formats (EDL, FCPXML, CUE) and CSV, music
editing such as loop points or take alignment, and speech recognition are outside this crate.

## Features

- `std` (default): the standard library's floating-point functions.
- `libm`: floating-point functions for `no_std`. Without `std`, this feature is required.
- `serde`: `Serialize` and `Deserialize` for `Analysis`, `Plan` and the result types.
  Deserializing an `Analysis` validates it.

## Development

Use the toolchain in `rust-toolchain.toml` with rustfmt and Clippy. Dependency checks use
`cargo-deny`.

```sh
cargo test --all-features --locked
cargo test --no-default-features --features libm,serde --locked
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --locked -- -D warnings
RUSTDOCFLAGS='-D warnings' cargo doc --no-deps --all-features --locked
cargo deny --locked --all-features check licenses bans sources
```

The fixed planning workloads in `cargo run --release --locked --example count_performance` print
elapsed time and cut-path checksums. On the same local host, the one-hour/5,000-piece workload took
130.09 s with 0.1.0 and 8.79 s with 0.2.0, with the same cut path. These are individual
measurements, not latency guarantees; the worst-case search is still quadratic in the candidate
count per row.

## License

Licensed under either the [Apache License, Version 2.0](https://github.com/emulette/silence-split/blob/main/LICENSE-APACHE)
or the [MIT license](https://github.com/emulette/silence-split/blob/main/LICENSE-MIT), at your option.
Both license texts are included in the published crate.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in
this project by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without
any additional terms or conditions.
