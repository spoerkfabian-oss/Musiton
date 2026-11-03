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

/// Oscillator waveforms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Wave {
    /// Sawtooth (band-limited).
    #[default]
    Saw,
    /// Square (band-limited).
    Square,
    /// Triangle.
    Triangle,
    /// Sine.
    Sine,
}

impl Wave {
    /// Wave for a choice index 0..=3.
    pub fn from_index(i: f32) -> Self {
        match i.round() as i32 {
            1 => Wave::Square,
            2 => Wave::Triangle,
            3 => Wave::Sine,
            _ => Wave::Saw,
        }
    }
}

/// PolyBLEP correction for a discontinuity at phase 0 (t = phase, dt = increment).
#[inline]
fn poly_blep(t: f32, dt: f32) -> f32 {
    if t < dt {
        let x = t / dt;
        x + x - x * x - 1.0
    } else if t > 1.0 - dt {
        let x = (t - 1.0) / dt;
        x * x + x + x + 1.0
    } else {
        0.0
    }
}

/// Anti-aliased oscillator (PolyBLEP saw/square, naive triangle, sine). The phase is never
/// reset by frequency changes, so retriggering is click-free.
#[derive(Debug, Clone, Default)]
pub struct BlepOsc {
    phase: f32,
}

impl BlepOsc {
    /// Next sample of `wave` with phase increment `inc` (= Hz / sample rate, < 0.5).
    #[inline]
    pub fn next_sample(&mut self, wave: Wave, inc: f32) -> f32 {
        let dt = inc.clamp(1e-7, 0.49);
        let t = self.phase;
        let out = match wave {
            Wave::Saw => 2.0 * t - 1.0 - poly_blep(t, dt),
            Wave::Square => {
                let naive = if t < 0.5 { 1.0 } else { -1.0 };
                naive + poly_blep(t, dt) - poly_blep((t + 0.5).fract(), dt)
            }
            Wave::Triangle => 1.0 - 4.0 * (t - 0.5).abs(),
            Wave::Sine => (t * std::f32::consts::TAU).sin(),
        };
        self.phase = (t + dt).fract();
        out
    }
}

/// White noise (xorshift), −1..1.
#[derive(Debug, Clone)]
pub struct Noise {
    state: u32,
}

impl Default for Noise {
    fn default() -> Self {
        Self { state: 0x1234_5678 }
    }
}

impl Noise {
    /// Next sample.
    #[inline]
    pub fn next_sample(&mut self) -> f32 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.state = x;
        (x as f32 / u32::MAX as f32) * 2.0 - 1.0
    }
}

#[cfg(test)]
mod blep_tests {
    use super::*;
    use crate::testutil::{db, magnitude};

    fn render(wave: Wave, hz: f32) -> Vec<f32> {
        let mut o = BlepOsc::default();
        (0..9_600)
            .map(|_| o.next_sample(wave, hz / 48_000.0))
            .collect()
    }

    #[test]
    fn high_saw_and_square_have_little_aliasing() {
        // 5 kHz: harmonics 5,10,15,20 kHz are real; 25/30/35/40/45 kHz fold back to
        // 23/18/13/8/3 kHz. A naive saw has the 3 kHz alias only ~19 dB down.
        for wave in [Wave::Saw, Wave::Square] {
            let x = render(wave, 5_000.0);
            let fund = magnitude(&x, 5_000.0, 48_000.0);
            for alias in [3_000.0, 8_000.0, 13_000.0] {
                let a = db(magnitude(&x, alias, 48_000.0), fund);
                assert!(
                    a < -30.0,
                    "{wave:?} alias at {alias} Hz only {a:.1} dB down"
                );
            }
        }
    }

    #[test]
    fn saw_has_the_right_level_and_noise_is_bounded() {
        let x = render(Wave::Saw, 100.0);
        let peak = x.iter().fold(0.0_f32, |m, s| m.max(s.abs()));
        assert!(peak > 0.95 && peak <= 1.05);
        let mut n = Noise::default();
        let v: Vec<f32> = (0..10_000).map(|_| n.next_sample()).collect();
        assert!(v.iter().all(|s| (-1.0..=1.0).contains(s)));
        let mean = v.iter().sum::<f32>() / v.len() as f32;
        assert!(mean.abs() < 0.05);
    }
}
