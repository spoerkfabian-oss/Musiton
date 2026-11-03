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
