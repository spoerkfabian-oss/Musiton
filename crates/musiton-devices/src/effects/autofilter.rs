//! Auto filter: a resonant filter whose cutoff an LFO and the input's loudness move.
//! With the envelope amount up and the LFO down it is an auto-wah.

use musiton_core::ids::ParamId;
use musiton_core::params::ParamInfo;
use musiton_dsp::filter::{FilterMode, Svf};
use musiton_dsp::osc::sin_turns;
use musiton_engine::node::{AudioIo, AudioNode, Event, ProcessContext};

use crate::voices::Params;

/// Filter coefficients are recomputed every this many samples.
const UPDATE: usize = 4;

const MODES: &[&str] = &["Low-pass", "High-pass", "Band-pass"];
const SHAPES: &[&str] = &["Sine", "Triangle", "Square", "Random"];

/// Parameter ids.
#[allow(missing_docs)]
pub mod id {
    pub const MODE: u64 = 0;
    pub const CUTOFF: u64 = 1;
    pub const RESONANCE: u64 = 2;
    pub const RATE: u64 = 3;
    pub const SHAPE: u64 = 4;
    pub const LFO_AMOUNT: u64 = 5;
    pub const ENV_AMOUNT: u64 = 6;
    pub const SPREAD: u64 = 7;
    pub const MIX: u64 = 8;
}

/// Parameters.
pub const PARAMS: [ParamInfo; 9] = [
    ParamInfo::choice(id::MODE, "Mode", MODES, 0),
    ParamInfo::new(id::CUTOFF, "Cutoff", 40.0, 16_000.0, 800.0)
        .log()
        .unit("Hz"),
    ParamInfo::new(id::RESONANCE, "Resonance", 0.0, 0.95, 0.5),
    ParamInfo::new(id::RATE, "LFO rate", 0.02, 16.0, 1.0)
        .log()
        .unit("Hz"),
    ParamInfo::choice(id::SHAPE, "LFO shape", SHAPES, 0),
    ParamInfo::new(id::LFO_AMOUNT, "LFO amount", 0.0, 4.0, 2.0).unit("oct"),
    ParamInfo::new(id::ENV_AMOUNT, "Envelope", -4.0, 4.0, 0.0).unit("oct"),
    ParamInfo::new(id::SPREAD, "Spread", 0.0, 1.0, 0.0),
    ParamInfo::new(id::MIX, "Mix", 0.0, 1.0, 1.0),
];

/// The auto filter effect.
#[derive(Debug)]
pub struct AutoFilter {
    p: Params<9>,
    filters: [Svf; 2],
    lfo: f32,
    /// Random shape: current and next value, smoothed between them.
    random: [f32; 2],
    seed: u32,
    smooth: [f32; 2],
    env: f32,
    /// Cutoff in octaves (log2 Hz), gliding over ~30 ms so jumps sweep instead of click.
    cutoff: f32,
    sample_rate: f32,
    counter: usize,
}

impl Default for AutoFilter {
    fn default() -> Self {
        Self {
            p: Params::new(&PARAMS),
            filters: Default::default(),
            lfo: 0.0,
            random: [0.0; 2],
            seed: 0x1234_5678,
            smooth: [0.0; 2],
            env: 0.0,
            cutoff: PARAMS[id::CUTOFF as usize].default.log2(),
            sample_rate: 48_000.0,
            counter: 0,
        }
    }
}

impl AutoFilter {
    fn next_random(&mut self) -> f32 {
        // xorshift: deterministic, allocation-free.
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 17;
        self.seed ^= self.seed << 5;
        (self.seed as f32 / u32::MAX as f32) * 2.0 - 1.0
    }

    fn shape(&self, phase: f32, ch: usize) -> f32 {
        let t = phase.rem_euclid(1.0);
        match self.p.index(id::SHAPE) {
            1 => 1.0 - 4.0 * (t - 0.5).abs(),
            2 => {
                if t < 0.5 {
                    1.0
                } else {
                    -1.0
                }
            }
            3 => self.random.get(ch).copied().unwrap_or(0.0),
            _ => sin_turns(t),
        }
    }
}

impl AudioNode for AutoFilter {
    fn num_inputs(&self) -> usize {
        2
    }

    fn num_outputs(&self) -> usize {
        2
    }

    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        self.sample_rate = sample_rate;
        self.p.prepare(sample_rate);
        self.cutoff = self.p.target(id::CUTOFF).log2();
    }

    fn process(&mut self, _ctx: &ProcessContext, io: &mut AudioIo, _events: &[Event]) {
        let sr = self.sample_rate;
        let mode = FilterMode::from_index(self.p.target(id::MODE));
        let env_attack = 1.0 - (-1.0 / (0.005 * sr)).exp();
        let env_release = 1.0 - (-1.0 / (0.12 * sr)).exp();
        // Square and random shapes are smoothed (~3 ms) so they do not click.
        let glide = 1.0 - (-1.0 / (0.003 * sr)).exp();
        let cutoff_glide = 1.0 - (-1.0 / (0.03 * sr)).exp();
        for i in 0..io.frames() {
            self.p.tick();
            self.cutoff += (self.p.target(id::CUTOFF).log2() - self.cutoff) * cutoff_glide;
            let before = self.lfo;
            self.lfo = (self.lfo + self.p.get(id::RATE) / sr).fract();
            if self.lfo < before {
                let (a, b) = (self.next_random(), self.next_random());
                self.random = [a, b];
            }
            let (l, r) = (
                io.input(0).get(i).copied().unwrap_or(0.0),
                io.input(1).get(i).copied().unwrap_or(0.0),
            );
            let level = 0.5 * (l.abs() + r.abs());
            let k = if level > self.env {
                env_attack
            } else {
                env_release
            };
            self.env += (level - self.env) * k;
            let update = self.counter.is_multiple_of(UPDATE);
            self.counter = self.counter.wrapping_add(1);
            let spread = self.p.get(id::SPREAD) * 0.5;
            for ch in 0..2 {
                let target = self.shape(self.lfo + spread * ch as f32, ch);
                self.smooth[ch] += (target - self.smooth[ch]) * glide;
                if update {
                    let p = &self.p;
                    let octaves = self.smooth[ch] * p.get(id::LFO_AMOUNT)
                        + (self.env * 4.0).min(1.0) * p.get(id::ENV_AMOUNT);
                    let cutoff = (self.cutoff + octaves).exp2();
                    self.filters[ch].set(cutoff, p.get(id::RESONANCE), sr);
                }
                let x = if ch == 0 { l } else { r };
                let y = self.filters[ch].process(x, mode);
                let mix = self.p.get(id::MIX);
                if let Some(o) = io.output(ch).get_mut(i) {
                    *o = x + (y - x) * mix;
                }
            }
        }
    }

    fn reset(&mut self) {
        for f in &mut self.filters {
            f.reset();
        }
        self.env = 0.0;
    }

    fn params(&self) -> &[ParamInfo] {
        &PARAMS
    }

    fn set_param(&mut self, id: ParamId, value: f32) {
        self.p.set(id, value);
    }
}
