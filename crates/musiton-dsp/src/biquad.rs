//! Biquad filters (RBJ cookbook) for EQ bands.

/// Band types of a parametric EQ.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BandType {
    /// Bell (peaking) filter.
    #[default]
    Bell,
    /// Low shelf.
    LowShelf,
    /// High shelf.
    HighShelf,
    /// Low cut (high-pass, 12 dB/oct).
    LowCut,
    /// High cut (low-pass, 12 dB/oct).
    HighCut,
    /// Notch.
    Notch,
}

impl BandType {
    /// Type for a choice index.
    pub fn from_index(i: f32) -> Self {
        match i.round() as i32 {
            1 => BandType::LowShelf,
            2 => BandType::HighShelf,
            3 => BandType::LowCut,
            4 => BandType::HighCut,
            5 => BandType::Notch,
            _ => BandType::Bell,
        }
    }
}

/// Normalized biquad coefficients (a0 = 1).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Coefs {
    /// b0.
    pub b0: f32,
    /// b1.
    pub b1: f32,
    /// b2.
    pub b2: f32,
    /// a1.
    pub a1: f32,
    /// a2.
    pub a2: f32,
}

impl Default for Coefs {
    fn default() -> Self {
        Self {
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
        }
    }
}

impl Coefs {
    /// Designs a band: frequency (Hz), gain (dB, bells/shelves), Q.
    pub fn design(kind: BandType, freq: f32, gain_db: f32, q: f32, sample_rate: f32) -> Self {
        let f = freq.clamp(10.0, sample_rate * 0.49);
        let w = std::f32::consts::TAU * f / sample_rate;
        let (sin, cos) = w.sin_cos();
        let q = q.max(0.05);
        let alpha = sin / (2.0 * q);
        let a = 10f32.powf(gain_db / 40.0);
        let (b0, b1, b2, a0, a1, a2) = match kind {
            BandType::Bell => (
                1.0 + alpha * a,
                -2.0 * cos,
                1.0 - alpha * a,
                1.0 + alpha / a,
                -2.0 * cos,
                1.0 - alpha / a,
            ),
            BandType::LowShelf => {
                let s = 2.0 * a.sqrt() * alpha;
                (
                    a * ((a + 1.0) - (a - 1.0) * cos + s),
                    2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
                    a * ((a + 1.0) - (a - 1.0) * cos - s),
                    (a + 1.0) + (a - 1.0) * cos + s,
                    -2.0 * ((a - 1.0) + (a + 1.0) * cos),
                    (a + 1.0) + (a - 1.0) * cos - s,
                )
            }
            BandType::HighShelf => {
                let s = 2.0 * a.sqrt() * alpha;
                (
                    a * ((a + 1.0) + (a - 1.0) * cos + s),
                    -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
                    a * ((a + 1.0) + (a - 1.0) * cos - s),
                    (a + 1.0) - (a - 1.0) * cos + s,
                    2.0 * ((a - 1.0) - (a + 1.0) * cos),
                    (a + 1.0) - (a - 1.0) * cos - s,
                )
            }
            BandType::LowCut => (
                (1.0 + cos) / 2.0,
                -(1.0 + cos),
                (1.0 + cos) / 2.0,
                1.0 + alpha,
                -2.0 * cos,
                1.0 - alpha,
            ),
            BandType::HighCut => (
                (1.0 - cos) / 2.0,
                1.0 - cos,
                (1.0 - cos) / 2.0,
                1.0 + alpha,
                -2.0 * cos,
                1.0 - alpha,
            ),
            BandType::Notch => (1.0, -2.0 * cos, 1.0, 1.0 + alpha, -2.0 * cos, 1.0 - alpha),
        };
        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
        }
    }

    /// Magnitude response in dB at `freq`.
    pub fn response_db(&self, freq: f32, sample_rate: f32) -> f32 {
        let w = std::f64::consts::TAU * f64::from(freq) / f64::from(sample_rate);
        let (c1, s1) = (w.cos(), -w.sin());
        let (c2, s2) = ((2.0 * w).cos(), -(2.0 * w).sin());
        let num = (
            f64::from(self.b0) + f64::from(self.b1) * c1 + f64::from(self.b2) * c2,
            f64::from(self.b1) * s1 + f64::from(self.b2) * s2,
        );
        let den = (
            1.0 + f64::from(self.a1) * c1 + f64::from(self.a2) * c2,
            f64::from(self.a1) * s1 + f64::from(self.a2) * s2,
        );
        let m = (num.0 * num.0 + num.1 * num.1).sqrt()
            / (den.0 * den.0 + den.1 * den.1).sqrt().max(1e-12);
        (20.0 * m.max(1e-12).log10()) as f32
    }
}

/// A biquad (transposed direct form II) whose coefficients glide towards new targets,
/// so parameter changes don't click.
#[derive(Debug, Clone, Default)]
pub struct Biquad {
    c: Coefs,
    target: Coefs,
    z1: f32,
    z2: f32,
}

/// Fraction of the remaining distance covered per sample when coefficients change.
const GLIDE: f32 = 0.002;

impl Biquad {
    /// Sets new target coefficients (`immediate` jumps, e.g. on start).
    pub fn set(&mut self, c: Coefs, immediate: bool) {
        self.target = c;
        if immediate {
            self.c = c;
        }
    }

    /// Filters one sample.
    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let t = &self.target;
        let c = &mut self.c;
        c.b0 += (t.b0 - c.b0) * GLIDE;
        c.b1 += (t.b1 - c.b1) * GLIDE;
        c.b2 += (t.b2 - c.b2) * GLIDE;
        c.a1 += (t.a1 - c.a1) * GLIDE;
        c.a2 += (t.a2 - c.a2) * GLIDE;
        let y = c.b0 * x + self.z1;
        self.z1 = c.b1 * x - c.a1 * y + self.z2;
        self.z2 = c.b2 * x - c.a2 * y;
        y
    }

    /// Clears the state.
    pub fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{db, magnitude};

    fn measure(c: Coefs, hz: f32) -> f64 {
        let mut f = Biquad::default();
        f.set(c, true);
        let x: Vec<f32> = (0..9_600)
            .map(|n| (std::f32::consts::TAU * hz * n as f32 / 48_000.0).sin())
            .collect();
        let y: Vec<f32> = x.iter().map(|&s| f.process(s)).collect();
        db(
            magnitude(&y[4_800..], f64::from(hz), 48_000.0),
            magnitude(&x[4_800..], f64::from(hz), 48_000.0),
        )
    }

    #[test]
    fn bands_have_their_gain() {
        let sr = 48_000.0;
        let bell = Coefs::design(BandType::Bell, 1000.0, 6.0, 1.0, sr);
        assert!((measure(bell, 1000.0) - 6.0).abs() < 0.2);
        assert!(measure(bell, 100.0).abs() < 0.5);
        assert!((bell.response_db(1000.0, sr) - 6.0).abs() < 0.05);
        let shelf = Coefs::design(BandType::HighShelf, 2000.0, -9.0, 0.707, sr);
        assert!((measure(shelf, 15_000.0) + 9.0).abs() < 0.5);
        let cut = Coefs::design(BandType::LowCut, 200.0, 0.0, 0.707, sr);
        assert!(measure(cut, 25.0) < -30.0);
        let notch = Coefs::design(BandType::Notch, 3000.0, 0.0, 4.0, sr);
        assert!(notch.response_db(3000.0, sr) < -40.0);
    }
}
