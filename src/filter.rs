//! The detection-only high-pass filter.

use core::f64::consts::{FRAC_1_SQRT_2, PI};

use crate::math;

/// Cutoff of the detection high-pass filter in hertz.
pub(crate) const CUTOFF_HZ: f64 = 80.0;

/// Filter state magnitudes below this are flushed to zero so long digital silence does not decay
/// into subnormal numbers, which are very slow on some processors.
const FLUSH_BELOW: f64 = 1e-30;

/// Second-order Butterworth high-pass (RBJ cookbook), transposed direct form II.
#[derive(Clone, Debug)]
pub(crate) struct HighPass {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    z1: f64,
    z2: f64,
}

impl HighPass {
    /// A filter for `sample_rate`, which must exceed twice the cutoff.
    pub(crate) fn new(sample_rate: u32) -> Self {
        let w0 = 2.0 * PI * CUTOFF_HZ / f64::from(sample_rate);
        let (sin, cos) = math::sin_cos(w0);
        let alpha = sin / (2.0 * FRAC_1_SQRT_2);
        let a0 = 1.0 + alpha;
        Self {
            b0: (1.0 + cos) / 2.0 / a0,
            b1: -(1.0 + cos) / a0,
            b2: (1.0 + cos) / 2.0 / a0,
            a1: -2.0 * cos / a0,
            a2: (1.0 - alpha) / a0,
            z1: 0.0,
            z2: 0.0,
        }
    }

    pub(crate) fn process(&mut self, x: f64) -> f64 {
        let y = self.b0 * x + self.z1;
        self.z1 = flush(self.b1 * x - self.a1 * y + self.z2);
        self.z2 = flush(self.b2 * x - self.a2 * y);
        y
    }
}

fn flush(v: f64) -> f64 {
    if v.abs() < FLUSH_BELOW { 0.0 } else { v }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn power_after_settling(filter: &mut HighPass, rate: u32, freq: f64) -> f64 {
        let n = rate as usize;
        let mut sum = 0.0;
        for i in 0..n {
            let x = math::sin_cos(2.0 * PI * freq * i as f64 / f64::from(rate)).0;
            let y = filter.process(x);
            if i >= n / 2 {
                sum += y * y;
            }
        }
        sum / (n - n / 2) as f64
    }

    #[test]
    fn passes_speech_band_and_removes_rumble_and_dc() {
        let rate = 44_100;
        let pass = power_after_settling(&mut HighPass::new(rate), rate, 1000.0);
        let hum = power_after_settling(&mut HighPass::new(rate), rate, 20.0);
        assert!((pass - 0.5).abs() < 0.01, "1 kHz passes at unity: {pass}");
        assert!(
            hum < 0.5 * 0.005,
            "20 Hz is attenuated by more than 23 dB: {hum}"
        );

        let mut dc = HighPass::new(rate);
        let last = (0..rate).map(|_| dc.process(0.5)).last().unwrap_or(1.0);
        assert!(last.abs() < 1e-6, "DC decays to zero: {last}");
    }

    #[test]
    fn state_reaches_exact_zero_after_signal_ends() {
        let mut filter = HighPass::new(48_000);
        for i in 0..4800 {
            filter.process(if i % 2 == 0 { 0.9 } else { -0.9 });
        }
        for _ in 0..48_000 {
            filter.process(0.0);
        }
        assert_eq!((filter.z1, filter.z2), (0.0, 0.0));
    }
}
