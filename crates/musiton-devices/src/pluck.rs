//! Plucked strings (extended Karplus-Strong): a burst of filtered noise excites a delay
//! line tuned to the note; a damping filter in the loop makes high partials die first.
//! Two strings per voice, slightly detuned and panned, and a resonant body.

use musiton_core::ids::ParamId;
use musiton_core::params::ParamInfo;
use musiton_dsp::biquad::{BandType, Biquad, Coefs};
use musiton_dsp::delay::DelayLine;
use musiton_dsp::osc::{sin_turns, Noise};
use musiton_engine::node::{AudioIo, AudioNode, Event, ProcessContext};

use crate::voices::{handle_event, note_hz, split_block, Params, Voice, Voices};

/// Number of voices.
const VOICES: usize = 16;
/// Output headroom.
const HEADROOM: f32 = 0.35;
/// Lowest note the strings are long enough for (Hz).
const LOWEST_HZ: f32 = 25.0;
/// Length of the pluck (noise burst) in periods of the note.
const BURST_PERIODS: f32 = 1.0;

const BODIES: &[&str] = &["Guitar", "Harp", "Bright box"];

/// Body resonances (Hz, gain dB, Q) per body type.
const BODY: [[(f32, f32, f32); 3]; 3] = [
    [(110.0, 9.0, 4.0), (220.0, 6.0, 5.0), (3_000.0, -4.0, 0.7)],
    [(180.0, 4.0, 2.0), (700.0, 3.0, 2.0), (4_000.0, 2.0, 1.0)],
    [(400.0, 5.0, 3.0), (1_500.0, 6.0, 3.0), (5_000.0, 4.0, 1.0)],
];

/// Parameter ids.
#[allow(missing_docs)]
pub mod id {
    pub const BRIGHTNESS: u64 = 0;
    pub const DECAY: u64 = 1;
    pub const DAMPING: u64 = 2;
    pub const PICK: u64 = 3;
    pub const BODY_TYPE: u64 = 4;
    pub const BODY: u64 = 5;
    pub const DETUNE: u64 = 6;
    pub const RELEASE: u64 = 7;
    pub const VELOCITY: u64 = 8;
    pub const VOLUME: u64 = 9;
}

/// All parameters (index = id).
pub const PARAMS: [ParamInfo; 10] = [
    ParamInfo::new(id::BRIGHTNESS, "Brightness", 0.0, 1.0, 0.6),
    ParamInfo::new(id::DECAY, "Decay", 0.3, 15.0, 4.0)
        .log()
        .unit("s"),
    ParamInfo::new(id::DAMPING, "Damping", 0.0, 1.0, 0.4),
    ParamInfo::new(id::PICK, "Pick position", 0.05, 0.5, 0.15),
    ParamInfo::choice(id::BODY_TYPE, "Body", BODIES, 0),
    ParamInfo::new(id::BODY, "Body amount", 0.0, 1.0, 0.5),
    ParamInfo::new(id::DETUNE, "Detune", 0.0, 20.0, 4.0).unit("ct"),
    ParamInfo::new(id::RELEASE, "Release", 0.02, 2.0, 0.15)
        .log()
        .unit("s"),
    ParamInfo::new(id::VELOCITY, "Velocity", 0.0, 1.0, 0.7),
    ParamInfo::new(id::VOLUME, "Volume", 0.0, 1.0, 0.7),
];

#[derive(Debug, Clone, Default)]
struct Str {
    line: DelayLine,
    /// Previous loop sample (for the two-point damping average).
    last: f32,
    /// Loop period in samples.
    period: f32,
}

impl Str {
    /// One sample of the string loop; `s` (0.1 … 0.5) is how much the damping filter
    /// averages with the previous sample (it delays by `s` samples).
    fn step(&mut self, input: f32, gain: f32, s: f32) -> f32 {
        let y = self.line.read(self.period - s);
        let lp = y * (1.0 - s) + self.last * s;
        self.last = y;
        self.line.push(lp * gain + input);
        y
    }
}

#[derive(Debug, Clone, Default)]
struct PluckVoice {
    pitch: f32,
    vel: f32,
    strings: [Str; 2],
    /// Samples of excitation left, and its length.
    burst: u32,
    burst_len: u32,
    /// Excitation low-pass state and the pick-position comb.
    burst_lp: f32,
    pick: DelayLine,
    /// Pick-position comb delay (samples).
    pick_samples: f32,
    released: bool,
    /// Extra damping after release (1 = none).
    mute: f32,
    /// Output level (for when the voice may stop).
    level: f32,
}

impl Voice for PluckVoice {
    fn release(&mut self) {
        self.released = true;
    }

    fn is_active(&self) -> bool {
        self.burst > 0 || self.level > 1e-4
    }
}

/// The plucked-strings node (no inputs, stereo out).
#[derive(Debug)]
pub struct Pluck {
    p: Params<10>,
    voices: Voices<PluckVoice>,
    sample_rate: f32,
    noise: Noise,
    body: [[Biquad; 3]; 2],
    body_type: usize,
}

impl Default for Pluck {
    fn default() -> Self {
        Self {
            p: Params::new(&PARAMS),
            voices: Voices::default(),
            sample_rate: 48_000.0,
            noise: Noise::default(),
            body: Default::default(),
            body_type: usize::MAX,
        }
    }
}

impl Pluck {
    fn note_on(&mut self, key: u8, vel: f32) {
        let sr = self.sample_rate;
        let sens = self.p.target(id::VELOCITY);
        let detune = self.p.target(id::DETUNE) / 200.0;
        let pick = self.p.target(id::PICK);
        if let Some(v) = self.voices.note_on(key) {
            let pitch = f32::from(key);
            v.pitch = pitch;
            v.vel = 1.0 - sens + sens * vel;
            for (s, d) in v.strings.iter_mut().zip([-detune, detune]) {
                s.period = sr / note_hz(pitch + d);
            }
            let period = sr / note_hz(pitch);
            v.burst = (period * BURST_PERIODS) as u32;
            v.burst_len = v.burst.max(1);
            v.burst_lp = 0.0;
            v.pick.clear();
            // The pick-position comb notches the partials with a node at the pick point.
            v.pick_samples = (period * pick).max(1.0);
            v.released = false;
            v.mute = 1.0;
            v.level = 1.0;
        }
    }

    fn update_body(&mut self) {
        let t = self.p.index(id::BODY_TYPE).min(BODY.len() - 1);
        if t == self.body_type {
            return;
        }
        self.body_type = t;
        let sr = self.sample_rate;
        if let Some(bands) = BODY.get(t) {
            for side in &mut self.body {
                for (f, &(hz, db, q)) in side.iter_mut().zip(bands) {
                    f.set(Coefs::design(BandType::Bell, hz, db, q, sr), false);
                }
            }
        }
    }

    fn render(&mut self, io: &mut AudioIo, from: usize, to: usize) {
        self.update_body();
        let sr = self.sample_rate;
        let gain = self.voices.controllers.gain();
        for i in from..to {
            self.p.tick();
            let p = &self.p;
            let brightness = p.get(id::BRIGHTNESS);
            let damping = 0.1 + 0.4 * p.get(id::DAMPING);
            let decay = p.get(id::DECAY);
            let release = (-4.6 / (p.get(id::RELEASE) * sr)).exp();
            let lp_coef = 0.05 + 0.95 * brightness * brightness;
            let (mut l, mut r) = (0.0, 0.0);
            for v in self.voices.active() {
                let input = if v.burst > 0 {
                    v.burst -= 1;
                    // A smooth finger-like push plus some noise for the attack.
                    let t = 1.0 - v.burst as f32 / v.burst_len as f32;
                    let push = sin_turns(0.5 * t);
                    let n = (push + 0.5 * self.noise.next_sample()) * v.vel;
                    v.burst_lp += (n - v.burst_lp) * lp_coef;
                    v.pick.push(v.burst_lp);
                    v.burst_lp - v.pick.read(v.pick_samples)
                } else {
                    0.0
                };
                if v.released {
                    v.mute *= release;
                }
                // Loop gain for a 60 dB decay in `decay` seconds (shorter up high).
                let periods = decay * note_hz(v.pitch);
                let g = (0.001_f32.powf(1.0 / periods.max(1.0))) * v.mute;
                let [a, b] = &mut v.strings;
                let ya = a.step(input, g, damping);
                let yb = b.step(input, g, damping);
                // Peak follower: the voice stops once the string is quiet.
                v.level = ya.abs().max(v.level * 0.9995);
                if v.released && v.mute < 1e-3 {
                    v.level = 0.0;
                }
                l += ya * 0.75 + yb * 0.25;
                r += yb * 0.75 + ya * 0.25;
            }
            let body = p.get(id::BODY);
            let k = HEADROOM * p.get(id::VOLUME) * gain;
            for (ch, x) in [l, r].into_iter().enumerate() {
                let mut y = x;
                if let Some(side) = self.body.get_mut(ch) {
                    for f in side.iter_mut() {
                        y = f.process(y);
                    }
                }
                let out = (x + (y - x) * body) * k;
                if let Some(o) = io.output(ch).get_mut(i) {
                    *o = out;
                }
            }
        }
    }
}

impl AudioNode for Pluck {
    fn num_inputs(&self) -> usize {
        0
    }

    fn num_outputs(&self) -> usize {
        2
    }

    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        self.sample_rate = sample_rate;
        self.p.prepare(sample_rate);
        self.voices.prepare(VOICES);
        let len = (sample_rate / LOWEST_HZ) as usize + 4;
        for v in self.voices.all() {
            for s in &mut v.strings {
                s.line = DelayLine::new(len);
            }
            v.pick = DelayLine::new(len);
        }
        self.body_type = usize::MAX;
    }

    fn process(&mut self, _ctx: &ProcessContext, io: &mut AudioIo, events: &[Event]) {
        let n = io.frames();
        split_block(
            self,
            n,
            events,
            |s, a, b| s.render(io, a, b),
            |s, e| {
                if let Some((key, vel)) = handle_event(&mut s.voices, &mut s.p, e) {
                    s.note_on(key, vel);
                }
            },
        );
    }

    fn reset(&mut self) {
        for v in self.voices.all() {
            for s in &mut v.strings {
                s.line.clear();
                s.last = 0.0;
            }
            v.burst = 0;
            v.level = 0.0;
        }
    }

    fn params(&self) -> &[ParamInfo] {
        &PARAMS
    }

    fn set_param(&mut self, id: ParamId, value: f32) {
        self.p.set(id, value);
    }
}

/// Factory presets.
pub const PRESETS: &[(&str, &[(u64, f32)])] = &[
    ("Init", &[]),
    (
        "Nylon guitar",
        &[(id::BRIGHTNESS, 0.4), (id::DAMPING, 0.6), (id::BODY, 0.7)],
    ),
    (
        "Harp",
        &[
            (id::BODY_TYPE, 1.0),
            (id::BRIGHTNESS, 0.5),
            (id::DECAY, 6.0),
            (id::PICK, 0.3),
            (id::RELEASE, 1.0),
        ],
    ),
    (
        "Pizzicato",
        &[
            (id::DECAY, 0.6),
            (id::DAMPING, 0.8),
            (id::BRIGHTNESS, 0.35),
            (id::BODY_TYPE, 2.0),
        ],
    ),
    (
        "Steel strings",
        &[
            (id::BRIGHTNESS, 0.9),
            (id::DAMPING, 0.15),
            (id::DECAY, 8.0),
            (id::DETUNE, 7.0),
            (id::PICK, 0.08),
        ],
    ),
];
