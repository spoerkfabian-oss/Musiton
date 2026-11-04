//! 8-band parametric EQ with an analyzer tap for the spectrum display.

use std::sync::Arc;

use musiton_core::ids::ParamId;
use musiton_core::params::ParamInfo;
use musiton_dsp::biquad::{BandType, Biquad, Coefs};
use musiton_dsp::smooth::OnePole;
use musiton_engine::node::{AudioIo, AudioNode, Event, ProcessContext};
use musiton_engine::nodes::Analyzer;

use super::{db, store};

/// Number of bands.
pub const BANDS: usize = 8;
/// Parameters per band: type, frequency, gain, Q, on.
pub const PER_BAND: u64 = 5;
/// Id of the output gain parameter.
pub const OUTPUT: u64 = BANDS as u64 * PER_BAND;

const TYPES: &[&str] = &[
    "Bell",
    "Low shelf",
    "High shelf",
    "Low cut",
    "High cut",
    "Notch",
];
const NAMES: [[&str; 5]; BANDS] = [
    ["1 type", "1 freq", "1 gain", "1 Q", "1 on"],
    ["2 type", "2 freq", "2 gain", "2 Q", "2 on"],
    ["3 type", "3 freq", "3 gain", "3 Q", "3 on"],
    ["4 type", "4 freq", "4 gain", "4 Q", "4 on"],
    ["5 type", "5 freq", "5 gain", "5 Q", "5 on"],
    ["6 type", "6 freq", "6 gain", "6 Q", "6 on"],
    ["7 type", "7 freq", "7 gain", "7 Q", "7 on"],
    ["8 type", "8 freq", "8 gain", "8 Q", "8 on"],
];
/// Default (type, frequency, on) per band.
const DEFAULTS: [(usize, f32, bool); BANDS] = [
    (3, 30.0, false),
    (1, 100.0, true),
    (0, 250.0, true),
    (0, 800.0, true),
    (0, 2_500.0, true),
    (0, 6_000.0, true),
    (2, 10_000.0, true),
    (4, 18_000.0, false),
];

const fn band(i: usize) -> [ParamInfo; 5] {
    let base = i as u64 * PER_BAND;
    let (ty, freq, on) = DEFAULTS[i];
    let n = NAMES[i];
    [
        ParamInfo::choice(base, n[0], TYPES, ty),
        ParamInfo::new(base + 1, n[1], 20.0, 20_000.0, freq)
            .log()
            .unit("Hz"),
        ParamInfo::new(base + 2, n[2], -24.0, 24.0, 0.0).unit("dB"),
        ParamInfo::new(base + 3, n[3], 0.1, 18.0, 0.707).log(),
        ParamInfo::toggle(base + 4, n[4], on),
    ]
}

const fn all_params() -> [ParamInfo; BANDS * 5 + 1] {
    let mut out = [ParamInfo::new(OUTPUT, "Output", -24.0, 24.0, 0.0); BANDS * 5 + 1];
    let mut i = 0;
    while i < BANDS {
        let b = band(i);
        let mut k = 0;
        while k < 5 {
            out[i * 5 + k] = b[k];
            k += 1;
        }
        i += 1;
    }
    out[BANDS * 5] = ParamInfo::new(OUTPUT, "Output", -24.0, 24.0, 0.0).unit("dB");
    out
}

/// Parameters (index = id).
pub const PARAMS: [ParamInfo; BANDS * 5 + 1] = all_params();

/// Coefficients of band `b` for parameter values `v` (identity when off).
pub fn band_coefs(v: &[f32], b: usize, sample_rate: f32) -> Coefs {
    let at = |k: u64| {
        v.get((b as u64 * PER_BAND + k) as usize)
            .copied()
            .unwrap_or(0.0)
    };
    if at(4) < 0.5 {
        return Coefs::default();
    }
    Coefs::design(
        BandType::from_index(at(0)),
        at(1),
        at(2),
        at(3),
        sample_rate,
    )
}

/// The EQ effect.
pub struct Eq {
    values: [f32; PARAMS.len()],
    filters: [[Biquad; BANDS]; 2],
    sample_rate: f32,
    dirty: bool,
    out_gain: OnePole,
    analyzer: Arc<Analyzer>,
}

impl Eq {
    /// EQ feeding `analyzer` with its (mono) output.
    pub fn new(analyzer: Arc<Analyzer>) -> Self {
        Self {
            values: PARAMS.map(|p| p.default),
            filters: Default::default(),
            sample_rate: 48_000.0,
            dirty: true,
            out_gain: OnePole::default(),
            analyzer,
        }
    }

    fn update(&mut self, immediate: bool) {
        for b in 0..BANDS {
            let c = band_coefs(&self.values, b, self.sample_rate);
            for ch in &mut self.filters {
                if let Some(f) = ch.get_mut(b) {
                    f.set(c, immediate);
                }
            }
        }
        self.out_gain.set(db(self.values[OUTPUT as usize]));
        self.dirty = false;
    }
}

impl AudioNode for Eq {
    fn num_inputs(&self) -> usize {
        2
    }
    fn num_outputs(&self) -> usize {
        2
    }
    fn prepare(&mut self, sr: f32, _max_block: usize) {
        self.sample_rate = sr;
        self.out_gain = OnePole::new(db(self.values[OUTPUT as usize]), 20.0, sr);
        self.update(true);
    }
    fn process(&mut self, _ctx: &ProcessContext, io: &mut AudioIo, _events: &[Event]) {
        if self.dirty {
            self.update(false);
        }
        for i in 0..io.frames() {
            let g = self.out_gain.next_value();
            let mut mono = 0.0;
            for (ch, chain) in self.filters.iter_mut().enumerate() {
                let mut x = io.input(ch).get(i).copied().unwrap_or(0.0);
                for f in chain.iter_mut() {
                    x = f.process(x);
                }
                let y = x * g;
                mono += y * 0.5;
                if let Some(o) = io.output(ch).get_mut(i) {
                    *o = y;
                }
            }
            self.analyzer.push(mono);
        }
    }
    fn reset(&mut self) {
        for f in self.filters.iter_mut().flatten() {
            f.reset();
        }
    }
    fn params(&self) -> &[ParamInfo] {
        &PARAMS
    }
    fn set_param(&mut self, id: ParamId, value: f32) {
        if store(&mut self.values, &PARAMS, id.0, value) {
            self.dirty = true;
        }
    }
}

/// `n` log-spaced frequencies from 20 Hz to 20 kHz (for curves and spectra).
pub fn display_freqs(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| 20.0 * 1000f32.powf(i as f32 / (n.max(2) - 1) as f32))
        .collect()
}

/// EQ response in dB at `freqs` for the given parameter values (settings over defaults).
pub fn curve(settings: &[(u64, f32)], freqs: &[f32], sample_rate: f32) -> Vec<f32> {
    let mut v = PARAMS.map(|p| p.default);
    for &(id, x) in settings {
        if let Some(slot) = v.get_mut(id as usize) {
            *slot = x;
        }
    }
    let bands: Vec<Coefs> = (0..BANDS).map(|b| band_coefs(&v, b, sample_rate)).collect();
    let out = v[OUTPUT as usize];
    freqs
        .iter()
        .map(|&f| {
            bands
                .iter()
                .map(|c| c.response_db(f, sample_rate))
                .sum::<f32>()
                + out
        })
        .collect()
}

/// Spectrum (dB) of the analyzer's latest 4096 samples, sampled at `freqs`.
pub fn spectrum(analyzer: &Analyzer, freqs: &[f32], sample_rate: f32) -> Vec<f32> {
    let n = musiton_engine::nodes::ANALYZER_SIZE;
    let mags = musiton_dsp::fft::magnitude_spectrum(&analyzer.latest(n));
    let bin_hz = sample_rate / n as f32;
    freqs
        .iter()
        .map(|&f| {
            let i = (f / bin_hz).round() as usize;
            let m = mags
                .get(i.saturating_sub(1)..(i + 2).min(mags.len()))
                .map_or(0.0, |s| s.iter().fold(0.0_f32, |a, b| a.max(*b)));
            20.0 * m.max(1e-6).log10()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spectrum_shows_a_sine() {
        let a = Analyzer::new();
        for i in 0..8192 {
            a.push((std::f32::consts::TAU * 1000.0 * i as f32 / 48_000.0).sin() * 0.5);
        }
        let s = spectrum(&a, &[1000.0, 5000.0], 48_000.0);
        assert!((s[0] + 6.0).abs() < 1.5, "{s:?}");
        assert!(s[1] < -40.0, "{s:?}");
    }

    #[test]
    fn flat_by_default_and_curve_shows_a_boost() {
        let f = display_freqs(100);
        assert!(curve(&[], &f, 48_000.0).iter().all(|d| d.abs() < 0.01));
        let c = curve(&[(PER_BAND * 3 + 2, 6.0)], &[800.0], 48_000.0);
        assert!((c[0] - 6.0).abs() < 0.1);
    }
}
