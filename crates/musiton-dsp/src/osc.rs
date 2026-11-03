//! Oscillators.

use std::f64::consts::TAU;

/// Sine oscillator with a double-precision phase accumulator (no drift over long runs).
#[derive(Debug, Clone, Default)]
pub struct SineOsc {
    phase: f64,
    inc: f64,
}

impl SineOsc {
    /// Creates an oscillator at `freq` Hz for the given sample rate.
    pub fn new(freq: f64, sample_rate: f64) -> Self {
        let mut osc = Self::default();
        osc.set_freq(freq, sample_rate);
        osc
    }

    /// Changes the frequency without resetting the phase (click-free).
    pub fn set_freq(&mut self, freq: f64, sample_rate: f64) {
        self.inc = if sample_rate > 0.0 {
            freq / sample_rate
        } else {
            0.0
        };
    }

    /// Restarts the waveform at phase 0.
    pub fn reset(&mut self) {
        self.phase = 0.0;
    }

    /// Returns the next sample in `-1.0..=1.0`.
    #[inline]
    pub fn next_sample(&mut self) -> f32 {
        let out = (self.phase * TAU).sin() as f32;
        self.phase += self.inc;
        self.phase -= self.phase.floor();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_abs_diff_eq;

    #[test]
    fn matches_reference_sine() {
        let sr = 48_000.0;
        let mut osc = SineOsc::new(440.0, sr);
        for n in 0..10_000 {
            let expected = (TAU * 440.0 * n as f64 / sr).sin() as f32;
            assert_abs_diff_eq!(osc.next_sample(), expected, epsilon = 1e-4);
        }
    }

    #[test]
    fn one_second_has_expected_zero_crossings() {
        let sr = 48_000.0;
        let mut osc = SineOsc::new(440.0, sr);
        let samples: Vec<f32> = (0..48_000).map(|_| osc.next_sample()).collect();
        let rising = samples
            .windows(2)
            .filter(|w| w[0] < 0.0 && w[1] >= 0.0)
            .count();
        assert!((439..=441).contains(&rising), "rising crossings: {rising}");
    }

    #[test]
    fn zero_sample_rate_outputs_silence() {
        let mut osc = SineOsc::new(440.0, 0.0);
        assert!((0..100).all(|_| osc.next_sample() == 0.0));
    }
}
