//! Waveshaping distortion with first-order antiderivative anti-aliasing (ADAA): instead of
//! `f(x[n])` it outputs the mean of `f` between the last two inputs,
//! `(F(x[n]) − F(x[n−1])) / (x[n] − x[n−1])`, which removes most of the aliasing heavy
//! drive would otherwise fold back into the audible range, without oversampling.

/// Curve of a [`Shaper`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Curve {
    /// `tanh`: smooth, odd harmonics.
    #[default]
    Soft,
    /// Hard clip at ±1: buzzy, odd harmonics.
    Hard,
    /// Offset `tanh`: even and odd harmonics (a warm, "raw" growl).
    Asym,
    /// Sine fold: the louder the input, the more it folds back (metallic).
    Fold,
}

impl Curve {
    /// Curve for a choice parameter value (0 = Soft, 1 = Hard, 2 = Asym, 3 = Fold).
    pub fn from_index(i: f32) -> Self {
        match i.round() as i32 {
            1 => Self::Hard,
            2 => Self::Asym,
            3 => Self::Fold,
            _ => Self::Soft,
        }
    }
}

/// Bias of the asymmetric curve.
const BIAS: f64 = 0.4;

/// `ln(cosh(x))`, stable for large `|x|`.
fn ln_cosh(x: f64) -> f64 {
    let a = x.abs();
    a + (-2.0 * a).exp().ln_1p() - std::f64::consts::LN_2
}

fn f(c: Curve, x: f64) -> f64 {
    match c {
        Curve::Soft => x.tanh(),
        Curve::Hard => x.clamp(-1.0, 1.0),
        Curve::Asym => (x + BIAS).tanh() - BIAS.tanh(),
        Curve::Fold => x.sin(),
    }
}

/// Antiderivative of [`f`].
fn big_f(c: Curve, x: f64) -> f64 {
    match c {
        Curve::Soft => ln_cosh(x),
        Curve::Hard => {
            if x.abs() <= 1.0 {
                0.5 * x * x
            } else {
                x.abs() - 0.5
            }
        }
        Curve::Asym => ln_cosh(x + BIAS) - x * BIAS.tanh(),
        Curve::Fold => -x.cos(),
    }
}

/// One channel of anti-aliased waveshaping followed by a DC blocker (the asymmetric curve
/// adds a DC offset).
#[derive(Debug, Clone)]
pub struct Shaper {
    /// Curve `f1` was computed with.
    curve: Curve,
    x1: f64,
    f1: f64,
    dc_x: f32,
    dc_y: f32,
}

impl Shaper {
    /// Shapes one sample with `curve`.
    #[inline]
    pub fn process(&mut self, x: f32, curve: Curve) -> f32 {
        if curve != self.curve {
            self.curve = curve;
            self.f1 = big_f(curve, self.x1);
        }
        let x = f64::from(x);
        let fx = big_f(curve, x);
        let d = x - self.x1;
        let y = if d.abs() > 1e-5 {
            (fx - self.f1) / d
        } else {
            f(curve, 0.5 * (x + self.x1))
        };
        self.x1 = x;
        self.f1 = fx;
        // DC blocker (≈ 7 Hz at 48 kHz).
        let y = y as f32;
        let out = y - self.dc_x + 0.999 * self.dc_y;
        self.dc_x = y;
        self.dc_y = out;
        out
    }

    /// Forgets the past input.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

impl Default for Shaper {
    fn default() -> Self {
        Self {
            curve: Curve::Soft,
            x1: 0.0,
            f1: big_f(Curve::Soft, 0.0),
            dc_x: 0.0,
            dc_y: 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine_through(curve: Curve, drive: f32, hz: f32) -> Vec<f32> {
        let mut s = Shaper::default();
        (0..48_000)
            .map(|n| {
                let x = (std::f32::consts::TAU * hz * n as f32 / 48_000.0).sin();
                s.process(x * drive, curve)
            })
            .collect()
    }

    #[test]
    fn first_sample_does_not_jump() {
        for c in [Curve::Soft, Curve::Hard, Curve::Asym, Curve::Fold] {
            let mut s = Shaper::default();
            let y = s.process(0.01, c);
            assert!(y.abs() < 0.02, "{c:?}: {y}");
        }
    }

    #[test]
    fn quiet_input_passes_almost_unchanged() {
        for c in [Curve::Soft, Curve::Hard, Curve::Asym, Curve::Fold] {
            let y = sine_through(c, 0.01, 100.0);
            let peak = y[24_000..].iter().fold(0.0_f32, |m, v| m.max(v.abs()));
            let expect = if c == Curve::Asym {
                0.01 * (1.0 - 0.38_f32 * 0.38)
            } else {
                0.01
            };
            assert!((peak - expect).abs() < 0.001, "{c:?}: {peak}");
        }
    }

    #[test]
    fn output_stays_bounded_and_centred() {
        for c in [Curve::Soft, Curve::Hard, Curve::Asym, Curve::Fold] {
            let y = sine_through(c, 30.0, 110.0);
            let tail = &y[24_000..];
            let peak = tail.iter().fold(0.0_f32, |m, v| m.max(v.abs()));
            let mean = tail.iter().sum::<f32>() / tail.len() as f32;
            assert!(peak < 2.1, "{c:?}: peak {peak}");
            assert!(mean.abs() < 0.01, "{c:?}: DC {mean}");
        }
    }

    #[test]
    fn heavy_drive_aliases_less_than_naive_clipping() {
        // A 5 kHz sine clipped hard: harmonics above Nyquist fold back, the 9th
        // (45 kHz) to 3 kHz, where it is most audible. Compare with and without ADAA.
        let hz = 5_000.0;
        let adaa = sine_through(Curve::Hard, 20.0, hz);
        let naive: Vec<f32> = (0..48_000)
            .map(|n| {
                (20.0 * (std::f32::consts::TAU * hz * n as f32 / 48_000.0).sin()).clamp(-1.0, 1.0)
            })
            .collect();
        let m = |x: &[f32]| crate::testutil::magnitude(&x[8_000..40_000], 3_000.0, 48_000.0);
        assert!(m(&adaa) < 0.5 * m(&naive), "{} vs {}", m(&adaa), m(&naive));
    }
}
