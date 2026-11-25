//! Phaser: a chain of first-order all-pass filters whose corner frequency an LFO sweeps;
//! mixed with the dry signal they cut moving notches into the spectrum.

use musiton_core::ids::ParamId;
use musiton_core::params::ParamInfo;
use musiton_dsp::osc::sin_turns;
use musiton_engine::node::{AudioIo, AudioNode, Event, ProcessContext};

use crate::voices::Params;

/// Most all-pass stages.
const MAX_STAGES: usize = 12;
/// The all-pass coefficient is recomputed every this many samples.
const UPDATE: usize = 4;

const STAGES: &[&str] = &["4", "6", "8", "12"];

/// Parameter ids.
#[allow(missing_docs)]
pub mod id {
    pub const STAGES: u64 = 0;
    pub const RATE: u64 = 1;
    pub const DEPTH: u64 = 2;
    pub const CENTER: u64 = 3;
    pub const FEEDBACK: u64 = 4;
    pub const SPREAD: u64 = 5;
    pub const MIX: u64 = 6;
}

/// Parameters.
pub const PARAMS: [ParamInfo; 7] = [
    ParamInfo::choice(id::STAGES, "Stages", STAGES, 1),
    ParamInfo::new(id::RATE, "Rate", 0.02, 8.0, 0.4)
        .log()
        .unit("Hz"),
    ParamInfo::new(id::DEPTH, "Depth", 0.0, 1.0, 0.7),
    ParamInfo::new(id::CENTER, "Center", 100.0, 5_000.0, 800.0)
        .log()
        .unit("Hz"),
    ParamInfo::new(id::FEEDBACK, "Feedback", 0.0, 0.9, 0.4),
    ParamInfo::new(id::SPREAD, "Spread", 0.0, 1.0, 0.25),
    ParamInfo::new(id::MIX, "Mix", 0.0, 1.0, 0.5),
];

/// The phaser effect.
#[derive(Debug)]
pub struct Phaser {
    p: Params<7>,
    /// All-pass states per side and stage.
    state: [[f32; MAX_STAGES]; 2],
    coef: [f32; 2],
    last: [f32; 2],
    lfo: f32,
    sample_rate: f32,
    counter: usize,
}

impl Default for Phaser {
    fn default() -> Self {
        Self {
            p: Params::new(&PARAMS),
            state: [[0.0; MAX_STAGES]; 2],
            coef: [0.0; 2],
            last: [0.0; 2],
            lfo: 0.0,
            sample_rate: 48_000.0,
            counter: 0,
        }
    }
}

impl AudioNode for Phaser {
    fn num_inputs(&self) -> usize {
        2
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
        let stages = [4, 6, 8, 12]
            .get(self.p.index(id::STAGES))
            .copied()
            .unwrap_or(6);
        for i in 0..io.frames() {
            self.p.tick();
            let p = &self.p;
            self.lfo = (self.lfo + p.get(id::RATE) / sr).fract();
            let update = self.counter.is_multiple_of(UPDATE);
            self.counter = self.counter.wrapping_add(1);
            let (depth, center, fb, mix) = (
                p.get(id::DEPTH),
                p.get(id::CENTER),
                p.get(id::FEEDBACK),
                p.get(id::MIX),
            );
            for ch in 0..2 {
                if update {
                    // Sweep ±2 octaves around the center at full depth.
                    let m = sin_turns(self.lfo + p.get(id::SPREAD) * 0.5 * ch as f32);
                    let f = (center * (2.0 * depth * m).exp2()).clamp(20.0, sr * 0.45);
                    let t = (std::f32::consts::PI * f / sr).tan();
                    self.coef[ch] = (t - 1.0) / (t + 1.0);
                }
                let x = io.input(ch).get(i).copied().unwrap_or(0.0);
                let a = self.coef[ch];
                let mut v = x + self.last[ch] * fb;
                for s in self.state[ch].iter_mut().take(stages) {
                    // First-order all-pass: y = a·v + s; s' = v − a·y.
                    let y = a * v + *s;
                    *s = v - a * y;
                    v = y;
                }
                self.last[ch] = v.clamp(-2.0, 2.0);
                if let Some(o) = io.output(ch).get_mut(i) {
                    *o = x * (1.0 - 0.5 * mix) + v * 0.5 * mix;
                }
            }
        }
    }

    fn reset(&mut self) {
        self.state = [[0.0; MAX_STAGES]; 2];
        self.last = [0.0; 2];
    }

    fn params(&self) -> &[ParamInfo] {
        &PARAMS
    }

    fn set_param(&mut self, id: ParamId, value: f32) {
        self.p.set(id, value);
    }
}
