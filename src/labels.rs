//! Audacity label track export.

use core::fmt;

use crate::plan::Plan;

/// Writes one Audacity label per piece, numbered from 1, as `start\tend\tlabel` lines in seconds.
///
/// Import the text with *File → Import → Labels* to review a plan against the audio.
///
/// # Errors
///
/// Any error from the writer.
pub fn write_audacity_labels(
    plan: &Plan,
    sample_rate: u32,
    out: &mut impl fmt::Write,
) -> fmt::Result {
    let secs = |sample: u64| sample as f64 / f64::from(sample_rate);
    for (i, piece) in plan.pieces.iter().enumerate() {
        writeln!(
            out,
            "{:.6}\t{:.6}\t{}",
            secs(piece.start),
            secs(piece.end),
            i + 1
        )?;
    }
    Ok(())
}
