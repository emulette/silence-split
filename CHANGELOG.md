# Changelog

All notable changes to silence-split are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). Releases follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.0] - 2026-10-02

### Added

- `Analysis::levels(&SegmentConfig)` returns the floor, reference and resolved on/off thresholds,
  or `None` for empty input or entirely signal-free PCM.
- `AdjustConfig` controls the adjustment window, weights and placement. `AdjustResult` includes
  the plan and one optional cut per original input, preserving unsorted points and skipped inputs.

### Changed

- **Breaking:** `adjust` takes `&AdjustConfig` instead of a `Duration` and returns `AdjustResult`.
  Read the plan from `.plan` and input correspondence from `.point_cuts`.
- **Breaking:** `Cut::end` and `Cut::start` are now `previous_end` and `next_start`.
- **Breaking:** `Analysis::from_scores` takes `ScoreFrames { sample_rate, hop }` instead of two
  positional integers. `write_audacity_labels` requires a `NonZeroU32` sample rate.
- **Breaking serialization:** `Analysis::zero` is replaced by `no_signal`; serialized cut fields
  use their new names. Recreate stored 0.1 analyses and plans from their original inputs.
  Serialized formats are compatible only within the same minor version; no migration is provided.
- Count-mode DP prunes predecessor ranges with safe cost lower bounds, preserving the existing
  cut path, floating-point tie rules and over-maximum behavior. Parent memory still scales with
  piece count times candidate count, and the worst-case search remains quadratic per row.

### Fixed

- Automatic PCM thresholds above the 99th percentile are lowered to the 10th percentile, retaining
  steady sound that was previously classified entirely as silence. This also retains steady
  background noise; use an absolute threshold to remove noise at a known level.
- Very large padding, including `Duration::MAX`, is clipped to the audio bounds without integer
  overflow in segmentation, planning or cut adjustment.
- Large valid PCM analysis windows no longer overflow while computing their centered end.
- Frames at or below the filtered power floor are excluded from level statistics, so DC and
  subnormal padding no longer hide real pauses by depressing the automatic threshold.
- Supplemental cuts and grid cuts classify silence using the same effective padding, including
  leading/trailing silence. Planning padding may still differ from `segments` to meet the maximum.
- Entirely silent inputs shorter than the minimum silence stay silent, trim to `None` and produce
  an empty `Drop` plan.
- Percentile indexing uses 64-bit arithmetic to avoid overflow on long recordings on 32-bit targets.
- Audacity labels cannot be written with a zero sample rate.

## [0.1.0] - 2026-10-01

### Added

- `Analyzer`: incremental PCM analysis of `i16` or `f32` audio at any sample rate above 160 Hz
  and any channel count, with an 80 Hz detection-only high-pass filter and frame levels in dBFS on
  exact integer frame positions. Digital-zero frames are left out of the level statistics.
- `Analysis::from_scores`: per-frame speech probabilities from an external voice activity detector
  at its own rate and hop, mapped exactly to the original sample rate.
- `Analysis::segments` and `Analysis::trim`: silence and sound spans with an automatic threshold
  with hysteresis or an absolute or reference-relative one, minimum silence and sound lengths, and
  asymmetric padding.
- `plan`: a global cut plan under a hard maximum, soft minimum and target length, or an exact piece
  count, with silence kept or dropped at cuts and a cut reason and length flags in the result. An
  exact count relaxes the maximum only when no split at frame boundaries outside dropped silence
  can meet it.
- `adjust`: moves given cut points into nearby silence.
- `write_audacity_labels`: the pieces as an Audacity label track.
- `serde` feature for the analysis and result types, and `no_std` support with the `libm` feature.

[0.2.0]: https://github.com/emulette/silence-split/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/emulette/silence-split/releases/tag/v0.1.0
