//! Feedback-delay-network reverb (8 lines, Hadamard mixing, damping, pre-delay).

use musiton_core::ids::ParamId;
use musiton_core::params::ParamInfo;
use musiton_dsp::delay::DelayLine;
use musiton_dsp::smooth::OnePole;
use musiton_engine::node::{AudioIo, AudioNode, Event, ProcessContext};

use super::store;

/// Number of delay lines.
const LINES: usize = 8;
/// Base line lengths in ms (mutually prime-ish), scaled by "Size".
const BASE_MS: [f32; LINES] = [29.7, 37.1, 41.1, 43.7, 53.3, 59.9, 67.7, 73.1];
/// Largest size factor.
const MAX_SIZE: f32 = 2.0;
/// Longest pre-delay.
const MAX_PREDELAY: f32 = 0.2;

/// Parameter ids.
#[allow(missing_docs)]
pub mod id {
    pub const DECAY: u64 = 0;
    pub const SIZE: u64 = 1;
    pub const DAMPING: u64 = 2;
    pub const PREDELAY: u64 = 3;
    pub const MIX: u64 = 4;
    pub const WIDTH: u64 = 5;
}

/// Parameters.
pub const PARAMS: [ParamInfo; 6] = [
    ParamInfo::new(id::DECAY, "Decay", 0.2, 20.0, 2.0)
        .log()
        .unit("s"),
    ParamInfo::new(id::SIZE, "Size", 0.3, MAX_SIZE, 1.0),
    ParamInfo::new(id::DAMPING, "Damping", 1_000.0, 20_000.0, 6_000.0)
        .log()
        .unit("Hz"),
    ParamInfo::new(id::PREDELAY, "Pre-delay", 0.0, MAX_PREDELAY, 0.02).unit("s"),
    ParamInfo::new(id::MIX, "Mix", 0.0, 1.0, 0.25),
    ParamInfo::new(id::WIDTH, "Width", 0.0, 1.0, 1.0),
];

/// The reverb effect.
#[derive(Debug)]
pub struct Reverb {
    values: [f32; PARAMS.len()],
    lines: Vec<DelayLine>,
    damp: [f32; LINES],
    pre: [DelayLine; 2],
    sample_rate: f32,
    size: OnePole,
    predelay: OnePole,
    mix: OnePole,
    width: OnePole,
    decay: OnePole,
    damping: OnePole,
    /// Samples processed so far (block-size independent update points).
    counter: u64,
    /// Feedback gains and damping coefficient from the last update.
    gains: [f32; LINES],
    damp_coef: f32,
}

impl Default for Reverb {
    fn default() -> Self {
        Self {
            values: PARAMS.map(|p| p.default),
            lines: Vec::new(),
            damp: [0.0; LINES],
            pre: Default::default(),
            sample_rate: 48_000.0,
            size: OnePole::default(),
            predelay: OnePole::default(),
            mix: OnePole::default(),
            width: OnePole::default(),
            decay: OnePole::default(),
            damping: OnePole::default(),
            counter: 0,
            gains: [0.0; LINES],
            damp_coef: 0.0,
        }
    }
}

/// In-place 8-point fast Hadamard transform, normalized (energy preserving).
#[inline]
fn hadamard(x: &mut [f32; LINES]) {
    let mut h = 1;
    while h < LINES {
        let mut i = 0;
        while i < LINES {
            for j in i..i + h {
                let (a, b) = (x[j], x[j + h]);
                x[j] = a + b;
                x[j + h] = a - b;
            }
            i += 2 * h;
        }
        h *= 2;
    }
    let norm = 1.0 / (LINES as f32).sqrt();
    for v in x.iter_mut() {
        *v *= norm;
    }
}

impl Reverb {
    fn v(&self, id: u64) -> f32 {
        self.values.get(id as usize).copied().unwrap_or(0.0)
    }
}

impl AudioNode for Reverb {
    fn num_inputs(&self) -> usize {
        2
    }
    fn num_outputs(&self) -> usize {
        2
    }
    fn prepare(&mut self, sr: f32, _max_block: usize) {
        self.sample_rate = sr;
        self.lines = BASE_MS
            .iter()
            .map(|ms| DelayLine::new((ms * 0.001 * MAX_SIZE * sr) as usize + 4))
            .collect();
        self.pre = [
            DelayLine::new((MAX_PREDELAY * sr) as usize + 4),
            DelayLine::new((MAX_PREDELAY * sr) as usize + 4),
        ];
        self.size = OnePole::new(self.v(id::SIZE), 1500.0, sr);
        self.predelay = OnePole::new(self.v(id::PREDELAY) * sr, 100.0, sr);
        self.mix = OnePole::new(self.v(id::MIX), 20.0, sr);
        self.width = OnePole::new(self.v(id::WIDTH), 20.0, sr);
        self.decay = OnePole::new(self.v(id::DECAY), 100.0, sr);
        self.damping = OnePole::new(self.v(id::DAMPING), 50.0, sr);
    }
    fn process(&mut self, _ctx: &ProcessContext, io: &mut AudioIo, _events: &[Event]) {
        let sr = self.sample_rate;
        self.decay.set(self.v(id::DECAY));
        self.damping.set(self.v(id::DAMPING));
        self.size.set(self.v(id::SIZE));
        self.predelay.set(self.v(id::PREDELAY) * sr);
        self.mix.set(self.v(id::MIX));
        self.width.set(self.v(id::WIDTH));
        let mut lens = [0.0_f32; LINES];
        let mut gains = self.gains;
        for i in 0..io.frames() {
            // Line lengths follow the (slowly) smoothed size every sample, so the read
            // positions glide; gains and damping are updated every 32 samples.
            let size = self.size.next_value();
            let decay = self.decay.next_value();
            let damping = self.damping.next_value();
            for k in 0..LINES {
                lens[k] = BASE_MS[k] * 0.001 * size * sr;
            }
            let update = self.counter.is_multiple_of(32);
            self.counter += 1;
            if update {
                self.damp_coef = 1.0 - (-std::f32::consts::TAU * damping / sr).exp();
                for k in 0..LINES {
                    gains[k] = 10f32.powf(-3.0 * lens[k] / (decay * sr));
                }
                self.gains = gains;
            }
            let damp = self.damp_coef;
            let x = [
                io.input(0).get(i).copied().unwrap_or(0.0),
                io.input(1).get(i).copied().unwrap_or(0.0),
            ];
            let pd = self.predelay.next_value().max(1.0);
            self.pre[0].push(x[0]);
            self.pre[1].push(x[1]);
            let inp = [self.pre[0].read(pd), self.pre[1].read(pd)];
            let mut s = [0.0_f32; LINES];
            for (k, line) in self.lines.iter().enumerate().take(LINES) {
                let y = line.read(lens[k]);
                self.damp[k] += (y - self.damp[k]) * damp;
                s[k] = self.damp[k] * gains[k];
            }
            let out_l = s[0] + s[2] + s[4] + s[6];
            let out_r = s[1] + s[3] + s[5] + s[7];
            hadamard(&mut s);
            for (k, line) in self.lines.iter_mut().enumerate().take(LINES) {
                let feed = if k % 2 == 0 { inp[0] } else { inp[1] };
                line.push(s[k] + feed * 0.5);
            }
            let (w, m) = (self.width.next_value(), self.mix.next_value());
            let mid = (out_l + out_r) * 0.5;
            let wl = mid + (out_l - mid) * w;
            let wr = mid + (out_r - mid) * w;
            for (ch, wet) in [wl, wr].into_iter().enumerate() {
                if let Some(o) = io.output(ch).get_mut(i) {
                    *o = x[ch] * (1.0 - m) + wet * 0.5 * m;
                }
            }
        }
    }
    fn reset(&mut self) {
        for l in &mut self.lines {
            l.clear();
        }
        self.damp = [0.0; LINES];
    }
    fn params(&self) -> &[ParamInfo] {
        &PARAMS
    }
    fn set_param(&mut self, id: ParamId, value: f32) {
        store(&mut self.values, &PARAMS, id.0, value);
    }
}
