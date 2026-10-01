//! Input sample types and the stream layout.

mod sealed {
    pub trait Sealed {}
    impl Sealed for i16 {}
    impl Sealed for f32 {}
}

/// A PCM sample type the analyzer accepts: `i16` or `f32`.
///
/// `f32` samples are full scale at ±1.0 and `i16` samples at ±32768. The trait is sealed so other
/// integer widths can be added later with their own scale.
pub trait Sample: sealed::Sealed + Copy {
    /// The sample as a full-scale `f32` value.
    fn to_f32(self) -> f32;
}

impl Sample for i16 {
    fn to_f32(self) -> f32 {
        f32::from(self) / 32768.0
    }
}

impl Sample for f32 {
    fn to_f32(self) -> f32 {
        self
    }
}

/// Sample rate and channel count of the original audio.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Layout {
    /// Samples per second per channel.
    pub sample_rate: u32,
    /// Number of interleaved or planar channels.
    pub channels: u16,
}
