//! Look-ahead peak limiter: the output never exceeds the ceiling.

use musiton_core::ids::ParamId;
use musiton_core::params::ParamInfo;
use musiton_dsp::delay::DelayLine;
use musiton_dsp::smooth::OnePole;
use musiton_engine::node::{AudioIo, AudioNode, Event, ProcessContext};

use super::{db, store};

/// Look-ahead time.
const LOOKAHEAD_MS: f32 = 1.5;

/// Parameter ids.
#[allow(missing_docs)]
pub mod id {
    pub const CEILING: u64 = 0;
    pub const RELEASE: u64 = 1;
    pub const INPUT: u64 = 2;
}

/// Parameters.
pub const PARAMS: [ParamInfo; 3] = [
    ParamInfo::new(id::CEILING, "Ceiling", -12.0, 0.0, -0.3).unit("dB"),
    ParamInfo::new(id::RELEASE, "Release", 0.001, 1.0, 0.05)
        .log()
        .unit("s"),
    ParamInfo::new(id::INPUT, "Input gain", 0.0, 24.0, 0.0).unit("dB"),
];

/// The limiter (latency = look-ahead).
#[derive(Debug)]
pub struct Limiter {
    values: [f32; PARAMS.len()],
    lines: [DelayLine; 2],
    /// Recent peak levels for the look-ahead window.
    peaks: Vec<f32>,
    peak_pos: usize,
    look: usize,
    gain: f32,
    sample_rate: f32,
    input: OnePole,
    ceiling: OnePole,
}

impl Default for Limiter {
    fn default() -> Self {
        Self {
            values: PARAMS.map(|p| p.default),
            lines: Default::default(),
            peaks: Vec::new(),
            peak_pos: 0,
            look: 1,
            gain: 1.0,
            sample_rate: 48_000.0,
            input: OnePole::default(),
            ceiling: OnePole::default(),
        }
    }
}

impl Limiter {
    fn v(&self, id: u64) -> f32 {
        self.values.get(id as usize).copied().unwrap_or(0.0)
    }
}

impl AudioNode for Limiter {
    fn num_inputs(&self) -> usize {
        2
    }
    fn num_outputs(&self) -> usize {
        2
    }
    fn prepare(&mut self, sr: f32, _max_block: usize) {
        self.sample_rate = sr;
        self.look = ((LOOKAHEAD_MS * 0.001 * sr) as usize).max(1);
        self.lines = [DelayLine::new(self.look + 4), DelayLine::new(self.look + 4)];
        self.peaks = vec![0.0; self.look + 1];
        self.input = OnePole::new(db(self.v(id::INPUT)), 20.0, sr);
        self.ceiling = OnePole::new(db(self.v(id::CEILING)), 20.0, sr);
    }
    fn process(&mut self, _ctx: &ProcessContext, io: &mut AudioIo, _events: &[Event]) {
        let rel = (-1.0 / (self.v(id::RELEASE) * self.sample_rate)).exp();
        // Attack must reach any gain within the look-ahead time.
        let att = (-4.6 / self.look as f32).exp();
        self.input.set(db(self.v(id::INPUT)));
        self.ceiling.set(db(self.v(id::CEILING)));
        let len = self.peaks.len().max(1);
        for i in 0..io.frames() {
            let (g_in, ceil) = (self.input.next_value(), self.ceiling.next_value());
            let x = [
                io.input(0).get(i).copied().unwrap_or(0.0) * g_in,
                io.input(1).get(i).copied().unwrap_or(0.0) * g_in,
            ];
            if let Some(p) = self.peaks.get_mut(self.peak_pos) {
                *p = x[0].abs().max(x[1].abs());
            }
            self.peak_pos = (self.peak_pos + 1) % len;
            let window_peak = self.peaks.iter().fold(0.0_f32, |m, p| m.max(*p));
            let target = if window_peak > ceil {
                ceil / window_peak
            } else {
                1.0
            };
            let coef = if target < self.gain { att } else { rel };
            self.gain = target + (self.gain - target) * coef;
            for (ch, line) in self.lines.iter_mut().enumerate() {
                line.push(x[ch]);
                let delayed = line.read(self.look as f32);
                let y = (delayed * self.gain).clamp(-ceil, ceil);
                if let Some(o) = io.output(ch).get_mut(i) {
                    *o = y;
                }
            }
        }
    }
    fn reset(&mut self) {
        for l in &mut self.lines {
            l.clear();
        }
        self.peaks.fill(0.0);
        self.gain = 1.0;
    }
    fn latency(&self) -> u32 {
        self.look as u32
    }
    fn params(&self) -> &[ParamInfo] {
        &PARAMS
    }
    fn set_param(&mut self, id: ParamId, value: f32) {
        store(&mut self.values, &PARAMS, id.0, value);
    }
}
