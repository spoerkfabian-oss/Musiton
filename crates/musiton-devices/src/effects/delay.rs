//! Stereo delay with tempo sync, feedback filter and ping-pong.

use musiton_core::ids::ParamId;
use musiton_core::params::ParamInfo;
use musiton_dsp::delay::DelayLine;
use musiton_dsp::smooth::OnePole;
use musiton_engine::node::{AudioIo, AudioNode, Event, ProcessContext};

use super::store;

/// Longest delay in seconds.
pub const MAX_SECONDS: f32 = 4.0;

/// Parameter ids.
#[allow(missing_docs)]
pub mod id {
    pub const TIME: u64 = 0;
    pub const SYNC: u64 = 1;
    pub const DIVISION: u64 = 2;
    pub const FEEDBACK: u64 = 3;
    pub const MIX: u64 = 4;
    pub const PING_PONG: u64 = 5;
    pub const HIGH_CUT: u64 = 6;
}

const DIVISIONS: &[&str] = &[
    "1/1",
    "1/2",
    "1/4",
    "1/8",
    "1/16",
    "1/32",
    "1/4 dotted",
    "1/8 dotted",
    "1/4 triplet",
    "1/8 triplet",
];
/// Length of each division in quarter notes.
const DIVISION_BEATS: [f32; 10] = [
    4.0,
    2.0,
    1.0,
    0.5,
    0.25,
    0.125,
    1.5,
    0.75,
    2.0 / 3.0,
    1.0 / 3.0,
];

/// Parameters.
pub const PARAMS: [ParamInfo; 7] = [
    ParamInfo::new(id::TIME, "Time", 0.001, MAX_SECONDS, 0.375)
        .log()
        .unit("s"),
    ParamInfo::toggle(id::SYNC, "Tempo sync", true),
    ParamInfo::choice(id::DIVISION, "Division", DIVISIONS, 7),
    ParamInfo::new(id::FEEDBACK, "Feedback", 0.0, 0.95, 0.35),
    ParamInfo::new(id::MIX, "Mix", 0.0, 1.0, 0.3),
    ParamInfo::toggle(id::PING_PONG, "Ping-pong", false),
    ParamInfo::new(id::HIGH_CUT, "High cut", 500.0, 20_000.0, 8_000.0)
        .log()
        .unit("Hz"),
];

/// The delay effect.
#[derive(Debug)]
pub struct Delay {
    values: [f32; PARAMS.len()],
    lines: [DelayLine; 2],
    lp: [f32; 2],
    sample_rate: f32,
    time: OnePole,
    feedback: OnePole,
    mix: OnePole,
}

impl Default for Delay {
    fn default() -> Self {
        Self {
            values: PARAMS.map(|p| p.default),
            lines: Default::default(),
            lp: [0.0; 2],
            sample_rate: 48_000.0,
            time: OnePole::default(),
            feedback: OnePole::default(),
            mix: OnePole::default(),
        }
    }
}

impl Delay {
    fn v(&self, id: u64) -> f32 {
        self.values.get(id as usize).copied().unwrap_or(0.0)
    }

    /// Delay time in samples at `bpm`.
    fn samples(&self, bpm: f32) -> f32 {
        let secs = if self.v(id::SYNC) > 0.5 {
            let beats = DIVISION_BEATS
                .get(self.v(id::DIVISION).round() as usize)
                .copied()
                .unwrap_or(1.0);
            beats * 60.0 / bpm.max(1.0)
        } else {
            self.v(id::TIME)
        };
        (secs.min(MAX_SECONDS) * self.sample_rate).max(1.0)
    }
}

impl AudioNode for Delay {
    fn num_inputs(&self) -> usize {
        2
    }
    fn num_outputs(&self) -> usize {
        2
    }
    fn prepare(&mut self, sr: f32, _max_block: usize) {
        self.sample_rate = sr;
        let max = (MAX_SECONDS * sr) as usize + 4;
        self.lines = [DelayLine::new(max), DelayLine::new(max)];
        self.time = OnePole::new(self.samples(120.0), 80.0, sr);
        self.feedback = OnePole::new(self.v(id::FEEDBACK), 20.0, sr);
        self.mix = OnePole::new(self.v(id::MIX), 20.0, sr);
    }
    fn process(&mut self, ctx: &ProcessContext, io: &mut AudioIo, _events: &[Event]) {
        let bpm = ctx.tempo.bpm_at(
            ctx.tempo
                .sample_to_tick(ctx.sample_pos as f64, f64::from(ctx.sample_rate)),
        ) as f32;
        self.time.set(self.samples(bpm));
        self.feedback.set(self.v(id::FEEDBACK));
        self.mix.set(self.v(id::MIX));
        let lp_coef =
            1.0 - (-std::f32::consts::TAU * self.v(id::HIGH_CUT) / self.sample_rate).exp();
        let ping = self.v(id::PING_PONG) > 0.5;
        for i in 0..io.frames() {
            let (t, fb, mix) = (
                self.time.next_value(),
                self.feedback.next_value(),
                self.mix.next_value(),
            );
            let x = [
                io.input(0).get(i).copied().unwrap_or(0.0),
                io.input(1).get(i).copied().unwrap_or(0.0),
            ];
            let wet = [self.lines[0].read(t), self.lines[1].read(t)];
            for ch in 0..2 {
                self.lp[ch] += (wet[ch] - self.lp[ch]) * lp_coef;
            }
            let (in_l, in_r) = if ping {
                // Input enters on the left; the echoes bounce between the sides.
                ((x[0] + x[1]) * 0.5 + self.lp[1] * fb, self.lp[0] * fb)
            } else {
                (x[0] + self.lp[0] * fb, x[1] + self.lp[1] * fb)
            };
            self.lines[0].push(in_l);
            self.lines[1].push(in_r);
            for ch in 0..2 {
                if let Some(o) = io.output(ch).get_mut(i) {
                    *o = x[ch] * (1.0 - mix) + wet[ch] * mix;
                }
            }
        }
    }
    fn reset(&mut self) {
        for l in &mut self.lines {
            l.clear();
        }
        self.lp = [0.0; 2];
    }
    fn params(&self) -> &[ParamInfo] {
        &PARAMS
    }
    fn set_param(&mut self, id: ParamId, value: f32) {
        store(&mut self.values, &PARAMS, id.0, value);
    }
}
