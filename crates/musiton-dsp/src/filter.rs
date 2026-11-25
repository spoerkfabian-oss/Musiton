//! State-variable filter (topology-preserving, Cytomic/Simper style): stable under fast
//! cutoff modulation.

/// Filter response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FilterMode {
    /// Low-pass.
    #[default]
    LowPass,
    /// High-pass.
    HighPass,
    /// Band-pass.
    BandPass,
}

impl FilterMode {
    /// Mode for a choice index 0..=2.
    pub fn from_index(i: f32) -> Self {
        match i.round() as i32 {
            1 => FilterMode::HighPass,
            2 => FilterMode::BandPass,
            _ => FilterMode::LowPass,
        }
    }
}

/// 24 dB/octave low-pass ladder (four one-pole stages in a resonant loop, zero-delay
/// feedback, soft saturation in the loop): the classic acid-bass filter.
#[derive(Debug, Clone, Default)]
pub struct Ladder {
    s: [f32; 4],
    g: f32,
    k: f32,
}

impl Ladder {
    /// Sets cutoff (Hz) and resonance (0 = none … 1 = self-oscillation).
    pub fn set(&mut self, cutoff: f32, resonance: f32, sample_rate: f32) {
        let fc = cutoff.clamp(10.0, sample_rate * 0.45);
        let g = (std::f32::consts::PI * fc / sample_rate).tan();
        self.g = g / (1.0 + g);
        self.k = 4.0 * resonance.clamp(0.0, 1.0);
    }

    /// Filters one sample.
    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let g = self.g;
        // Response of the four stages to their states (the loop is solved, not delayed).
        let g2 = g * g;
        let big_g = g2 * g2;
        let sigma = (1.0 - g) * (g2 * g * self.s[0] + g2 * self.s[1] + g * self.s[2] + self.s[3]);
        // Bass loss at high resonance is made up partly by mixing the input back in.
        let u = (x * (1.0 + 0.5 * self.k) - self.k * sigma) / (1.0 + self.k * big_g);
        let mut v = soft_clip(u);
        for s in &mut self.s {
            let y = g * (v - *s) + *s;
            *s = 2.0 * y - *s;
            v = y;
        }
        v
    }

    /// Clears the state.
    pub fn reset(&mut self) {
        self.s = [0.0; 4];
    }
}

/// Smooth saturation: about linear below 0.5, reaches ±1 at ±3 and stays there.
#[inline]
fn soft_clip(x: f32) -> f32 {
    let x = x.clamp(-3.0, 3.0);
    x * (27.0 + x * x) / (27.0 + 9.0 * x * x)
}

/// 12 dB/octave state-variable filter.
#[derive(Debug, Clone, Default)]
pub struct Svf {
    ic1: f32,
    ic2: f32,
    g: f32,
    k: f32,
    a1: f32,
    a2: f32,
    a3: f32,
}

impl Svf {
    /// Sets cutoff (Hz) and resonance (0 = none … 1 = self-oscillation edge).
    pub fn set(&mut self, cutoff: f32, resonance: f32, sample_rate: f32) {
        let fc = cutoff.clamp(10.0, sample_rate * 0.49);
        self.g = (std::f32::consts::PI * fc / sample_rate).tan();
        // Q from 0.5 to ~25.
        self.k = 2.0 - 1.96 * resonance.clamp(0.0, 1.0);
        self.a1 = 1.0 / (1.0 + self.g * (self.g + self.k));
        self.a2 = self.g * self.a1;
        self.a3 = self.g * self.a2;
    }

    /// Filters one sample.
    #[inline]
    pub fn process(&mut self, x: f32, mode: FilterMode) -> f32 {
        let v3 = x - self.ic2;
        let v1 = self.a1 * self.ic1 + self.a2 * v3;
        let v2 = self.ic2 + self.a2 * self.ic1 + self.a3 * v3;
        self.ic1 = 2.0 * v1 - self.ic1;
        self.ic2 = 2.0 * v2 - self.ic2;
        match mode {
            FilterMode::LowPass => v2,
            FilterMode::BandPass => v1,
            FilterMode::HighPass => x - self.k * v1 - v2,
        }
    }

    /// Clears the state.
    pub fn reset(&mut self) {
        self.ic1 = 0.0;
        self.ic2 = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{db, magnitude};

    fn response(mode: FilterMode, cutoff: f32, hz: f32) -> f64 {
        let mut f = Svf::default();
        f.set(cutoff, 0.0, 48_000.0);
        let x: Vec<f32> = (0..9_600)
            .map(|n| (std::f32::consts::TAU * hz * n as f32 / 48_000.0).sin())
            .collect();
        let y: Vec<f32> = x.iter().map(|&s| f.process(s, mode)).collect();
        db(
            magnitude(&y[4_800..], f64::from(hz), 48_000.0),
            magnitude(&x[4_800..], f64::from(hz), 48_000.0),
        )
    }

    #[test]
    fn modes_pass_and_cut_where_expected() {
        assert!(response(FilterMode::LowPass, 1000.0, 100.0).abs() < 0.5);
        assert!(response(FilterMode::LowPass, 1000.0, 10_000.0) < -35.0);
        assert!(response(FilterMode::HighPass, 1000.0, 10_000.0).abs() < 0.5);
        assert!(response(FilterMode::HighPass, 1000.0, 100.0) < -35.0);
        assert!(response(FilterMode::BandPass, 1000.0, 1000.0) > -7.0);
        assert!(response(FilterMode::BandPass, 1000.0, 10_000.0) < -15.0);
    }

    #[test]
    fn stays_stable_under_wild_modulation() {
        let mut f = Svf::default();
        let mut n = crate::osc::Noise::default();
        for i in 0..96_000 {
            let cutoff = if i % 7 == 0 {
                20_000.0
            } else {
                30.0 + (i % 1000) as f32 * 10.0
            };
            f.set(cutoff, 0.99, 48_000.0);
            let y = f.process(n.next_sample(), FilterMode::LowPass);
            assert!(y.is_finite() && y.abs() < 100.0);
        }
    }
}
