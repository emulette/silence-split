# Changelog

All notable changes to silence-split are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). Releases follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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

[0.1.0]: https://github.com/emulette/silence-split/releases/tag/v0.1.0
