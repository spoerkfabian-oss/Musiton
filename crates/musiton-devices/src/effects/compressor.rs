//! Compressor with soft knee, makeup gain, dry/wet mix and an optional sidechain input.

use musiton_core::ids::ParamId;
use musiton_core::params::ParamInfo;
use musiton_dsp::smooth::OnePole;
use musiton_engine::node::{AudioIo, AudioNode, Event, ProcessContext};

use super::{db, store};

/// Parameter ids.
#[allow(missing_docs)]
pub mod id {
    pub const THRESHOLD: u64 = 0;
    pub const RATIO: u64 = 1;
    pub const ATTACK: u64 = 2;
    pub const RELEASE: u64 = 3;
    pub const KNEE: u64 = 4;
    pub const MAKEUP: u64 = 5;
    pub const MIX: u64 = 6;
    pub const SIDECHAIN: u64 = 7;
}

/// Parameters.
pub const PARAMS: [ParamInfo; 8] = [
    ParamInfo::new(id::THRESHOLD, "Threshold", -60.0, 0.0, -18.0).unit("dB"),
    ParamInfo::new(id::RATIO, "Ratio", 1.0, 20.0, 4.0).log(),
    ParamInfo::new(id::ATTACK, "Attack", 0.0001, 0.1, 0.005)
        .log()
        .unit("s"),
    ParamInfo::new(id::RELEASE, "Release", 0.01, 1.0, 0.15)
        .log()
        .unit("s"),
    ParamInfo::new(id::KNEE, "Knee", 0.0, 12.0, 3.0).unit("dB"),
    ParamInfo::new(id::MAKEUP, "Makeup", 0.0, 24.0, 0.0).unit("dB"),
    ParamInfo::new(id::MIX, "Mix", 0.0, 1.0, 1.0),
    ParamInfo::toggle(id::SIDECHAIN, "Sidechain", false),
];

/// Static gain reduction (dB, ≤ 0) for an input level (dB).
pub fn gain_computer(level: f32, threshold: f32, ratio: f32, knee: f32) -> f32 {
    let over = level - threshold;
    let slope = 1.0 / ratio.max(1.0) - 1.0;
    if knee > 0.0 && over.abs() <= knee / 2.0 {
        slope * (over + knee / 2.0).powi(2) / (2.0 * knee)
    } else if over > 0.0 {
        slope * over
    } else {
        0.0
    }
}

/// The compressor. Inputs 0/1 = signal, 2/3 = sidechain (used when "Sidechain" is on).
#[derive(Debug)]
pub struct Compressor {
    values: [f32; PARAMS.len()],
    sample_rate: f32,
    /// Current gain reduction in dB (≤ 0).
    gr: f32,
    makeup: OnePole,
    mix: OnePole,
    thresh: OnePole,
}

impl Default for Compressor {
    fn default() -> Self {
        Self {
            values: PARAMS.map(|p| p.default),
            sample_rate: 48_000.0,
            gr: 0.0,
            makeup: OnePole::default(),
            mix: OnePole::default(),
            thresh: OnePole::default(),
        }
    }
}

impl Compressor {
    fn v(&self, id: u64) -> f32 {
        self.values.get(id as usize).copied().unwrap_or(0.0)
    }

    /// Current gain reduction in dB (for meters).
    pub fn gain_reduction(&self) -> f32 {
        self.gr
    }
}

impl AudioNode for Compressor {
    fn num_inputs(&self) -> usize {
        4
    }
    fn num_outputs(&self) -> usize {
        2
    }
    fn prepare(&mut self, sr: f32, _max_block: usize) {
        self.sample_rate = sr;
        self.makeup = OnePole::new(db(self.v(id::MAKEUP)), 20.0, sr);
        self.mix = OnePole::new(self.v(id::MIX), 20.0, sr);
        self.thresh = OnePole::new(self.v(id::THRESHOLD), 20.0, sr);
    }
    fn process(&mut self, _ctx: &ProcessContext, io: &mut AudioIo, _events: &[Event]) {
        let sr = self.sample_rate;
        let att = (-1.0 / (self.v(id::ATTACK) * sr)).exp();
        let rel = (-1.0 / (self.v(id::RELEASE) * sr)).exp();
        let (ratio, knee) = (self.v(id::RATIO), self.v(id::KNEE));
        let side = self.v(id::SIDECHAIN) > 0.5;
        self.makeup.set(db(self.v(id::MAKEUP)));
        self.mix.set(self.v(id::MIX));
        self.thresh.set(self.v(id::THRESHOLD));
        for i in 0..io.frames() {
            let l = io.input(0).get(i).copied().unwrap_or(0.0);
            let r = io.input(1).get(i).copied().unwrap_or(0.0);
            let (dl, dr) = if side {
                (
                    io.input(2).get(i).copied().unwrap_or(0.0),
                    io.input(3).get(i).copied().unwrap_or(0.0),
                )
            } else {
                (l, r)
            };
            let peak = dl.abs().max(dr.abs()).max(1e-9);
            let target = gain_computer(20.0 * peak.log10(), self.thresh.next_value(), ratio, knee);
            let coef = if target < self.gr { att } else { rel };
            self.gr = target + (self.gr - target) * coef;
            let g = db(self.gr) * self.makeup.next_value();
            let m = self.mix.next_value();
            for (ch, x) in [l, r].into_iter().enumerate() {
                if let Some(o) = io.output(ch).get_mut(i) {
                    *o = x * (g * m + (1.0 - m));
                }
            }
        }
    }
    fn reset(&mut self) {
        self.gr = 0.0;
    }
    fn params(&self) -> &[ParamInfo] {
        &PARAMS
    }
    fn set_param(&mut self, id: ParamId, value: f32) {
        store(&mut self.values, &PARAMS, id.0, value);
    }
}
