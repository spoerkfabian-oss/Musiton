//! Saturation: soft/hard/tube-style waveshaping with tone filter and dry/wet mix.

use musiton_core::ids::ParamId;
use musiton_core::params::ParamInfo;
use musiton_dsp::smooth::OnePole;
use musiton_engine::node::{AudioIo, AudioNode, Event, ProcessContext};

use super::{db, store};

/// Parameter ids.
#[allow(missing_docs)]
pub mod id {
    pub const DRIVE: u64 = 0;
    pub const KIND: u64 = 1;
    pub const TONE: u64 = 2;
    pub const OUTPUT: u64 = 3;
    pub const MIX: u64 = 4;
}

const KINDS: &[&str] = &["Soft", "Hard", "Tube"];

/// Parameters.
pub const PARAMS: [ParamInfo; 5] = [
    ParamInfo::new(id::DRIVE, "Drive", 0.0, 36.0, 12.0).unit("dB"),
    ParamInfo::choice(id::KIND, "Character", KINDS, 0),
    ParamInfo::new(id::TONE, "Tone", 1_000.0, 20_000.0, 12_000.0)
        .log()
        .unit("Hz"),
    ParamInfo::new(id::OUTPUT, "Output", -24.0, 6.0, -6.0).unit("dB"),
    ParamInfo::new(id::MIX, "Mix", 0.0, 1.0, 1.0),
];

/// Shapes one sample.
#[inline]
pub fn shape(x: f32, kind: usize) -> f32 {
    match kind {
        1 => x.clamp(-1.0, 1.0),
        2 => {
            // Asymmetric: positive half softer than negative (even harmonics).
            if x >= 0.0 {
                (x * 1.2).tanh() / 1.2_f32.tanh()
            } else {
                x.tanh()
            }
        }
        _ => x.tanh(),
    }
}

/// The saturation effect.
#[derive(Debug)]
pub struct Saturation {
    values: [f32; PARAMS.len()],
    lp: [f32; 2],
    dc: [(f32, f32); 2],
    sample_rate: f32,
    drive: OnePole,
    out: OnePole,
    mix: OnePole,
    tone: OnePole,
}

impl Default for Saturation {
    fn default() -> Self {
        Self {
            values: PARAMS.map(|p| p.default),
            lp: [0.0; 2],
            dc: [(0.0, 0.0); 2],
            sample_rate: 48_000.0,
            drive: OnePole::default(),
            out: OnePole::default(),
            mix: OnePole::default(),
            tone: OnePole::default(),
        }
    }
}

impl Saturation {
    fn v(&self, id: u64) -> f32 {
        self.values.get(id as usize).copied().unwrap_or(0.0)
    }
}

impl AudioNode for Saturation {
    fn num_inputs(&self) -> usize {
        2
    }
    fn num_outputs(&self) -> usize {
        2
    }
    fn prepare(&mut self, sr: f32, _max_block: usize) {
        self.sample_rate = sr;
        self.drive = OnePole::new(db(self.v(id::DRIVE)), 20.0, sr);
        self.out = OnePole::new(db(self.v(id::OUTPUT)), 20.0, sr);
        self.mix = OnePole::new(self.v(id::MIX), 20.0, sr);
        self.tone = OnePole::new(self.v(id::TONE), 20.0, sr);
    }
    fn process(&mut self, _ctx: &ProcessContext, io: &mut AudioIo, _events: &[Event]) {
        let kind = self.v(id::KIND).round() as usize;
        self.drive.set(db(self.v(id::DRIVE)));
        self.out.set(db(self.v(id::OUTPUT)));
        self.mix.set(self.v(id::MIX));
        self.tone.set(self.v(id::TONE));
        let sr = self.sample_rate;
        // DC blocker: y = x - x1 + R*y1.
        let r = 1.0 - std::f32::consts::TAU * 10.0 / sr;
        for i in 0..io.frames() {
            let (d, o, m) = (
                self.drive.next_value(),
                self.out.next_value(),
                self.mix.next_value(),
            );
            let lp = 1.0 - (-std::f32::consts::TAU * self.tone.next_value() / sr).exp();
            for ch in 0..2 {
                let x = io.input(ch).get(i).copied().unwrap_or(0.0);
                let s = shape(x * d, kind);
                let (x1, y1) = self.dc[ch];
                let y = s - x1 + r * y1;
                self.dc[ch] = (s, y);
                self.lp[ch] += (y - self.lp[ch]) * lp;
                if let Some(out) = io.output(ch).get_mut(i) {
                    *out = x * (1.0 - m) + self.lp[ch] * o * m;
                }
            }
        }
    }
    fn reset(&mut self) {
        self.lp = [0.0; 2];
        self.dc = [(0.0, 0.0); 2];
    }
    fn params(&self) -> &[ParamInfo] {
        &PARAMS
    }
    fn set_param(&mut self, id: ParamId, value: f32) {
        store(&mut self.values, &PARAMS, id.0, value);
    }
}
