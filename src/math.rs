//! Floating-point functions from `std`, or from `libm` without it.

#[cfg(feature = "std")]
mod imp {
    pub(crate) fn round(x: f64) -> f64 {
        x.round()
    }
    pub(crate) fn log10(x: f64) -> f64 {
        x.log10()
    }
    pub(crate) fn exp10(x: f64) -> f64 {
        10f64.powf(x)
    }
    pub(crate) fn sin_cos(x: f64) -> (f64, f64) {
        x.sin_cos()
    }
}

#[cfg(not(feature = "std"))]
mod imp {
    pub(crate) fn round(x: f64) -> f64 {
        libm::round(x)
    }
    pub(crate) fn log10(x: f64) -> f64 {
        libm::log10(x)
    }
    pub(crate) fn exp10(x: f64) -> f64 {
        libm::exp10(x)
    }
    pub(crate) fn sin_cos(x: f64) -> (f64, f64) {
        libm::sincos(x)
    }
}

pub(crate) use imp::*;

/// Power (mean square) to decibels, with a floor far below any real recording.
pub(crate) fn power_to_db(power: f64) -> f64 {
    const MIN_POWER: f64 = 1e-20;
    10.0 * log10(power.max(MIN_POWER))
}

/// Decibels to power (mean square).
pub(crate) fn db_to_power(db: f64) -> f64 {
    exp10(db / 10.0)
}
