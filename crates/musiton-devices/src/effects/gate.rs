//! Noise gate: lets the signal through while it (or the sidechain) is above the
//! threshold, holds briefly, then closes down to the range level. Opening and closing
//! are ramps, so the gate itself never clicks.

use musiton_core::ids::ParamId;
use musiton_core::params::ParamInfo;
use musiton_engine::node::{AudioIo, AudioNode, Event, ProcessContext};

use super::db;
use crate::voices::Params;

/// Parameter ids.
#[allow(missing_docs)]
pub mod id {
    pub const THRESHOLD: u64 = 0;
    pub const ATTACK: u64 = 1;
    pub const HOLD: u64 = 2;
    pub const RELEASE: u64 = 3;
    pub const RANGE: u64 = 4;
    pub const SIDECHAIN: u64 = 5;
}

/// Parameters.
pub const PARAMS: [ParamInfo; 6] = [
    ParamInfo::new(id::THRESHOLD, "Threshold", -80.0, 0.0, -40.0).unit("dB"),
    ParamInfo::new(id::ATTACK, "Attack", 0.0001, 0.05, 0.001)
        .log()
        .unit("s"),
    ParamInfo::new(id::HOLD, "Hold", 0.0, 0.5, 0.05).unit("s"),
    ParamInfo::new(id::RELEASE, "Release", 0.005, 2.0, 0.1)
        .log()
        .unit("s"),
    ParamInfo::new(id::RANGE, "Range", -80.0, 0.0, -80.0).unit("dB"),
    ParamInfo::toggle(id::SIDECHAIN, "Sidechain", false),
];

/// The gate. Inputs 0/1 = signal, 2/3 = sidechain (used when "Sidechain" is on).
#[derive(Debug)]
pub struct Gate {
    p: Params<6>,
    /// Peak detector, gain (0 … 1) and samples left to hold.
    level: f32,
    gain: f32,
    hold: u32,
    sample_rate: f32,
}

impl Default for Gate {
    fn default() -> Self {
        Self {
            p: Params::new(&PARAMS),
            level: 0.0,
            gain: 0.0,
            hold: 0,
            sample_rate: 48_000.0,
        }
    }
}

impl AudioNode for Gate {
    fn num_inputs(&self) -> usize {
        4
    }

    fn num_outputs(&self) -> usize {
        2
    }

    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        self.sample_rate = sample_rate;
        self.p.prepare(sample_rate);
    }

    fn process(&mut self, _ctx: &ProcessContext, io: &mut AudioIo, _events: &[Event]) {
        let sr = self.sample_rate;
        let side = self.p.target(id::SIDECHAIN) > 0.5;
        let detect_release = (-1.0 / (0.01 * sr)).exp();
        for i in 0..io.frames() {
            self.p.tick();
            let p = &self.p;
            let x = [0, 1].map(|ch| io.input(ch).get(i).copied().unwrap_or(0.0));
            let key = if side {
                [2, 3].map(|ch| io.input(ch).get(i).copied().unwrap_or(0.0))
            } else {
                x
            };
            let peak = key[0].abs().max(key[1].abs());
            self.level = peak.max(self.level * detect_release);
            let open = self.level > db(p.get(id::THRESHOLD));
            if open {
                self.hold = (p.get(id::HOLD) * sr) as u32;
            } else {
                self.hold = self.hold.saturating_sub(1);
            }
            let floor = db(p.get(id::RANGE));
            if open || self.hold > 0 {
                let step = 1.0 / (p.get(id::ATTACK) * sr).max(1.0);
                self.gain = (self.gain + step).min(1.0);
            } else {
                let k = (-4.6 / (p.get(id::RELEASE) * sr)).exp();
                self.gain *= k;
            }
            let g = floor + (1.0 - floor) * self.gain;
            for (ch, s) in x.into_iter().enumerate() {
                if let Some(o) = io.output(ch).get_mut(i) {
                    *o = s * g;
                }
            }
        }
    }

    fn reset(&mut self) {
        self.level = 0.0;
        self.gain = 0.0;
        self.hold = 0;
    }

    fn params(&self) -> &[ParamInfo] {
        &PARAMS
    }

    fn set_param(&mut self, id: ParamId, value: f32) {
        self.p.set(id, value);
    }
}
