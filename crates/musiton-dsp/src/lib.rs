//! Pure DSP building blocks for Musiton (no threads, no I/O, no allocation in hot paths).

pub mod biquad;
pub mod delay;
pub mod env;
pub mod fft;
pub mod filter;
pub mod osc;
pub mod resample;
pub mod smooth;

#[cfg(test)]
pub(crate) mod testutil {
    /// Magnitude (linear) of frequency `hz` in `x` via a single DFT bin.
    pub fn magnitude(x: &[f32], hz: f64, sr: f64) -> f64 {
        let (mut re, mut im) = (0.0, 0.0);
        for (n, &s) in x.iter().enumerate() {
            let w = std::f64::consts::TAU * hz * n as f64 / sr;
            // Hann window to keep leakage low.
            let win = 0.5 - 0.5 * (std::f64::consts::TAU * n as f64 / x.len() as f64).cos();
            re += f64::from(s) * w.cos() * win;
            im -= f64::from(s) * w.sin() * win;
        }
        (re * re + im * im).sqrt() * 4.0 / x.len() as f64
    }

    /// Ratio in dB.
    pub fn db(a: f64, reference: f64) -> f64 {
        20.0 * (a / reference).log10()
    }
}
