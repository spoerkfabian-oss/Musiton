//! Utility: gain, balance, stereo width, phase invert (all smoothed).

use musiton_core::ids::ParamId;
use musiton_core::params::ParamInfo;
use musiton_dsp::smooth::OnePole;
use musiton_engine::node::{AudioIo, AudioNode, Event, ProcessContext};

use super::{db, store};

/// Parameter ids.
#[allow(missing_docs)]
pub mod id {
    pub const GAIN: u64 = 0;
    pub const PAN: u64 = 1;
    pub const WIDTH: u64 = 2;
    pub const INVERT_L: u64 = 3;
    pub const INVERT_R: u64 = 4;
}

/// Parameters.
pub const PARAMS: [ParamInfo; 5] = [
    ParamInfo::new(id::GAIN, "Gain", -48.0, 24.0, 0.0).unit("dB"),
    ParamInfo::new(id::PAN, "Balance", -1.0, 1.0, 0.0),
    ParamInfo::new(id::WIDTH, "Width", 0.0, 2.0, 1.0),
    ParamInfo::toggle(id::INVERT_L, "Invert L", false),
    ParamInfo::toggle(id::INVERT_R, "Invert R", false),
];

/// The utility effect.
#[derive(Debug)]
pub struct Utility {
    values: [f32; 5],
    /// Smoothed: left gain, right gain, width.
    s: [OnePole; 3],
}

impl Default for Utility {
    fn default() -> Self {
        Self {
            values: PARAMS.map(|p| p.default),
            s: [OnePole::default(); 3],
        }
    }
}

impl Utility {
    /// Left gain (incl. balance and phase), right gain, width.
    fn targets(&self) -> [f32; 3] {
        let v = |id: u64| self.values.get(id as usize).copied().unwrap_or(0.0);
        let (gain, pan) = (db(v(id::GAIN)), v(id::PAN));
        let sign = |inv: f32| if inv > 0.5 { -1.0 } else { 1.0 };
        [
            gain * (1.0 - pan).min(1.0) * sign(v(id::INVERT_L)),
            gain * (1.0 + pan).min(1.0) * sign(v(id::INVERT_R)),
            v(id::WIDTH),
        ]
    }
}

impl AudioNode for Utility {
    fn num_inputs(&self) -> usize {
        2
    }
    fn num_outputs(&self) -> usize {
        2
    }
    fn prepare(&mut self, sr: f32, _max_block: usize) {
        self.s = self.targets().map(|v| OnePole::new(v, 20.0, sr));
    }
    fn process(&mut self, _ctx: &ProcessContext, io: &mut AudioIo, _events: &[Event]) {
        let targets = self.targets();
        for (s, v) in self.s.iter_mut().zip(targets) {
            s.set(v);
        }
        for i in 0..io.frames() {
            let [gl, gr, width] = [0, 1, 2].map(|k| self.s[k].next_value());
            let l = io.input(0).get(i).copied().unwrap_or(0.0);
            let r = io.input(1).get(i).copied().unwrap_or(0.0);
            let (mid, side) = ((l + r) * 0.5, (l - r) * 0.5 * width);
            if let Some(o) = io.output(0).get_mut(i) {
                *o = (mid + side) * gl;
            }
            if let Some(o) = io.output(1).get_mut(i) {
                *o = (mid - side) * gr;
            }
        }
    }
    fn reset(&mut self) {}
    fn params(&self) -> &[ParamInfo] {
        &PARAMS
    }
    fn set_param(&mut self, id: ParamId, value: f32) {
        store(&mut self.values, &PARAMS, id.0, value);
    }
}
