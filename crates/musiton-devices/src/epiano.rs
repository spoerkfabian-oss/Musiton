//! Electric piano in the style of a tine piano: a sine "tine" with a velocity-dependent
//! bark (phase modulation that decays fast), a quiet metallic bell partial, a decay that
//! gets shorter up the keyboard, a felt damper on release and stereo tremolo.

use musiton_core::ids::ParamId;
use musiton_core::params::ParamInfo;
use musiton_dsp::osc::sin_turns;
use musiton_engine::node::{AudioIo, AudioNode, Event, ProcessContext};

use crate::voices::{handle_event, note_hz, split_block, Params, Voice, Voices};

/// Number of voices.
const VOICES: usize = 16;
/// Output headroom.
const HEADROOM: f32 = 0.25;
/// Ratio of the bell partial to the tine.
const BELL_RATIO: f32 = 14.0;

/// Parameter ids.
#[allow(missing_docs)]
pub mod id {
    pub const TONE: u64 = 0;
    pub const BELL: u64 = 1;
    pub const DECAY: u64 = 2;
    pub const RELEASE: u64 = 3;
    pub const TREMOLO_RATE: u64 = 4;
    pub const TREMOLO_DEPTH: u64 = 5;
    pub const VELOCITY: u64 = 6;
    pub const VOLUME: u64 = 7;
}

/// All parameters (index = id).
pub const PARAMS: [ParamInfo; 8] = [
    ParamInfo::new(id::TONE, "Tone", 0.0, 1.0, 0.5),
    ParamInfo::new(id::BELL, "Bell", 0.0, 1.0, 0.3),
    ParamInfo::new(id::DECAY, "Decay", 0.5, 12.0, 4.0)
        .log()
        .unit("s"),
    ParamInfo::new(id::RELEASE, "Release", 0.02, 1.0, 0.12)
        .log()
        .unit("s"),
    ParamInfo::new(id::TREMOLO_RATE, "Tremolo rate", 0.5, 10.0, 4.5)
        .log()
        .unit("Hz"),
    ParamInfo::new(id::TREMOLO_DEPTH, "Tremolo", 0.0, 1.0, 0.0),
    ParamInfo::new(id::VELOCITY, "Velocity", 0.0, 1.0, 0.8),
    ParamInfo::new(id::VOLUME, "Volume", 0.0, 1.0, 0.7),
];

#[derive(Debug, Clone, Default)]
struct EpVoice {
    pitch: f32,
    vel: f32,
    tine: f32,
    modulator: f32,
    bell: f32,
    /// Overall level (decays while held, faster after release).
    amp: f32,
    /// Bark (modulation index) and bell level, both decaying fast.
    bark: f32,
    ring: f32,
    /// What is heard: follows `amp` within ~2 ms (no click at a new or stolen note).
    level: f32,
    released: bool,
}

impl Voice for EpVoice {
    fn release(&mut self) {
        self.released = true;
    }

    fn is_active(&self) -> bool {
        self.amp > 1e-4 || self.level > 1e-4
    }
}

/// The electric piano node (no inputs, stereo out).
#[derive(Debug)]
pub struct EPiano {
    p: Params<8>,
    voices: Voices<EpVoice>,
    sample_rate: f32,
    tremolo: f32,
}

impl Default for EPiano {
    fn default() -> Self {
        Self {
            p: Params::new(&PARAMS),
            voices: Voices::default(),
            sample_rate: 48_000.0,
            tremolo: 0.0,
        }
    }
}

impl EPiano {
    fn note_on(&mut self, key: u8, vel: f32) {
        let sens = self.p.target(id::VELOCITY);
        if let Some(v) = self.voices.note_on(key) {
            let vel = 1.0 - sens + sens * vel;
            v.pitch = f32::from(key);
            v.vel = vel;
            v.amp = 1.0;
            v.bark = vel * vel;
            v.ring = vel;
            v.released = false;
        }
    }

    fn render(&mut self, io: &mut AudioIo, from: usize, to: usize) {
        let sr = self.sample_rate;
        let gain = self.voices.controllers.gain();
        let follow = 1.0 - (-1.0 / (0.0005 * sr)).exp();
        let bark_decay = (-4.6 / (0.25 * sr)).exp();
        let ring_decay = (-4.6 / (0.6 * sr)).exp();
        for i in from..to {
            self.p.tick();
            let p = &self.p;
            let decay_time = p.get(id::DECAY);
            let release = (-4.6 / (p.get(id::RELEASE) * sr)).exp();
            let tone = p.get(id::TONE);
            let bell = p.get(id::BELL);
            self.tremolo = (self.tremolo + p.get(id::TREMOLO_RATE) / sr).fract();
            let depth = p.get(id::TREMOLO_DEPTH);
            let pan = sin_turns(self.tremolo) * depth;
            let (mut l, mut r) = (0.0, 0.0);
            for v in self.voices.active() {
                // Higher notes ring shorter (about half per two octaves above middle C).
                let t = decay_time * ((60.0 - v.pitch) / 24.0).exp2().clamp(0.15, 2.0);
                let hold = (-4.6 / (t * sr)).exp();
                v.amp *= if v.released { release } else { hold };
                v.level += (v.amp - v.level) * follow;
                v.bark *= bark_decay;
                v.ring *= ring_decay;
                let f = note_hz(v.pitch) / sr;
                let index = 0.05 + tone * (0.4 * v.bark + 0.15 * v.vel);
                let m = sin_turns(v.modulator);
                let tine = sin_turns(v.tine + index * m);
                let bell_part = if f * BELL_RATIO < 0.45 {
                    sin_turns(v.bell) * v.ring * bell * 0.25
                } else {
                    0.0
                };
                v.tine = (v.tine + f).fract();
                v.modulator = (v.modulator + f).fract();
                v.bell = (v.bell + f * BELL_RATIO).fract();
                let s = (tine + bell_part) * v.vel * v.level;
                l += s;
                r += s;
            }
            let k = HEADROOM * p.get(id::VOLUME) * gain;
            let out = [l * k * (1.0 - pan.max(0.0)), r * k * (1.0 + pan.min(0.0))];
            for (ch, y) in out.into_iter().enumerate() {
                if let Some(o) = io.output(ch).get_mut(i) {
                    *o = y;
                }
            }
        }
    }
}

impl AudioNode for EPiano {
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
            *v = EpVoice::default();
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
        "Suitcase",
        &[
            (id::TREMOLO_DEPTH, 0.6),
            (id::TREMOLO_RATE, 4.0),
            (id::TONE, 0.6),
        ],
    ),
    (
        "Mellow",
        &[(id::TONE, 0.15), (id::BELL, 0.1), (id::DECAY, 6.0)],
    ),
    (
        "Bright",
        &[(id::TONE, 0.9), (id::BELL, 0.6), (id::VELOCITY, 0.6)],
    ),
];
