//! Split long PCM audio into pieces that never exceed a maximum length, cutting in silence, or at
//! the quietest spot when there is none. No model, no FFmpeg, and every position is a sample index
//! in the original timeline.
//!
//! 1. [`Analyzer`] turns PCM of any sample rate into frame levels ([`Analysis`]). Scores from an
//!    external voice activity detector can be used instead with [`Analysis::from_scores`].
//! 2. [`Analysis::segments`] finds silence and sound, and [`Analysis::trim`] the audio between the
//!    first and last sound.
//! 3. [`plan`] chooses cuts over the whole file under minimum, target and maximum piece lengths.
//!    [`adjust`] instead moves cut points you already have into nearby silence.
//!
//! ```
//! use core::time::Duration;
//! use silence_split::{AnalyzeConfig, Analyzer, Gap, Layout, PlanConfig, SegmentConfig, plan};
//!
//! # fn main() -> Result<(), silence_split::ConfigError> {
//! let layout = Layout { sample_rate: 16_000, channels: 1 };
//! let mut analyzer = Analyzer::new(layout, &AnalyzeConfig::default())?;
//! # let pcm: Vec<f32> = (0..16_000 * 20)
//! #     .map(|i| if (i / 16_000) % 5 == 4 { 0.0 } else { 0.3 * (i as f32 * 0.07).sin() })
//! #     .collect();
//! analyzer.push_interleaved(&pcm);
//! let analysis = analyzer.finish();
//!
//! let config = PlanConfig::new(Duration::from_secs(2), Duration::from_secs(8), Duration::from_secs(10))?
//!     .gap(Gap::Drop);
//! let plan = plan(&analysis, &SegmentConfig::default(), &config);
//! for piece in &plan.pieces {
//!     let _samples = &pcm[piece.start as usize..piece.end as usize];
//!     assert!(piece.end - piece.start <= 160_000);
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # Features
//!
//! - `std` (default): uses the standard library's floating-point functions. Without it the crate
//!   is `no_std` with `alloc` and needs the `libm` feature.
//! - `libm`: floating-point functions for `no_std`.
//! - `serde`: `Serialize` and `Deserialize` for [`Analysis`], [`Plan`] and the result types.
//!   Deserializing an [`Analysis`] validates it.

#![no_std]

extern crate alloc;
#[cfg(feature = "std")]
extern crate std;

#[cfg(not(any(feature = "std", feature = "libm")))]
compile_error!("silence-split needs the `std` or the `libm` feature");

mod analyze;
mod candidates;
mod dp;
mod errors;
mod filter;
mod labels;
mod math;
mod plan;
mod sample;
mod segment;

pub use analyze::{Analysis, AnalyzeConfig, Analyzer};
pub use errors::ConfigError;
pub use labels::write_audacity_labels;
pub use plan::{Cut, CutReason, Gap, Piece, Placement, Plan, PlanConfig, Weights, adjust, plan};
pub use sample::{Layout, Sample};
pub use segment::{Kind, SegmentConfig, Span, Threshold};

#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
