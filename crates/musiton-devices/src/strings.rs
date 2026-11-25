//! String ensemble in the style of the classic string machines: two detuned saws per
//! voice in two registers (8' and 4'), slow envelopes, a tone filter and a three-line
//! ensemble chorus that makes the sound wide and lush.

use musiton_core::ids::ParamId;
use musiton_core::params::ParamInfo;
use musiton_dsp::delay::DelayLine;
use musiton_dsp::env::Adsr;
use musiton_dsp::filter::{FilterMode, Svf};
use musiton_dsp::osc::{sin_turns, BlepOsc, Wave};
use musiton_engine::node::{AudioIo, AudioNode, Event, ProcessContext};

use crate::voices::{handle_event, note_hz, split_block, Params, Voice, Voices};

/// Number of voices.
const VOICES: usize = 16;
/// Output headroom.
const HEADROOM: f32 = 0.12;
/// Base delay of the ensemble lines (seconds) and the most they move.
const CHORUS_BASE: f32 = 0.007;
const CHORUS_DEPTH: f32 = 0.004;

/// Parameter ids.
#[allow(missing_docs)]
pub mod id {
    pub const LOW: u64 = 0;
    pub const HIGH: u64 = 1;
    pub const DETUNE: u64 = 2;
    pub const ATTACK: u64 = 3;
    pub const RELEASE: u64 = 4;
    pub const TONE: u64 = 5;
    pub const ENSEMBLE: u64 = 6;
    pub const VELOCITY: u64 = 7;
    pub const VOLUME: u64 = 8;
}

/// All parameters (index = id).
pub const PARAMS: [ParamInfo; 9] = [
    ParamInfo::new(id::LOW, "8' (cello)", 0.0, 1.0, 1.0),
    ParamInfo::new(id::HIGH, "4' (violin)", 0.0, 1.0, 0.5),
    ParamInfo::new(id::DETUNE, "Detune", 0.0, 30.0, 8.0).unit("ct"),
    ParamInfo::new(id::ATTACK, "Attack", 0.005, 4.0, 0.4)
        .log()
        .unit("s"),
    ParamInfo::new(id::RELEASE, "Release", 0.02, 6.0, 1.2)
        .log()
        .unit("s"),
    ParamInfo::new(id::TONE, "Tone", 300.0, 16_000.0, 4_000.0)
        .log()
        .unit("Hz"),
    ParamInfo::new(id::ENSEMBLE, "Ensemble", 0.0, 1.0, 0.7),
    ParamInfo::new(id::VELOCITY, "Velocity", 0.0, 1.0, 0.3),
    ParamInfo::new(id::VOLUME, "Volume", 0.0, 1.0, 0.7),
];

#[derive(Debug, Clone)]
struct StringVoice {
    pitch: f32,
    vel: f32,
    /// Two saws at 8' and two at 4' (detuned pairs).
    osc: [BlepOsc; 4],
    env: Adsr,
}

impl Default for StringVoice {
    fn default() -> Self {
        Self {
            pitch: 0.0,
            vel: 0.0,
            osc: std::array::from_fn(|i| BlepOsc::with_phase(i as f32 * 0.31)),
            env: Adsr::default(),
        }
    }
}

impl Voice for StringVoice {
    fn release(&mut self) {
        self.env.gate_off();
    }

    fn is_active(&self) -> bool {
        self.env.is_active()
    }
}

/// The string ensemble node (no inputs, stereo out).
#[derive(Debug)]
pub struct Strings {
    p: Params<9>,
    voices: Voices<StringVoice>,
    sample_rate: f32,
    tone: Svf,
    lines: [DelayLine; 3],
    /// Slow and fast chorus LFO phases (turns).
    slow: f32,
    fast: f32,
}

impl Default for Strings {
    fn default() -> Self {
        Self {
            p: Params::new(&PARAMS),
            voices: Voices::default(),
            sample_rate: 48_000.0,
            tone: Svf::default(),
            lines: Default::default(),
            slow: 0.0,
            fast: 0.0,
        }
    }
}

impl Strings {
    fn note_on(&mut self, key: u8, vel: f32) {
        let sens = self.p.target(id::VELOCITY);
        if let Some(v) = self.voices.note_on(key) {
            v.pitch = f32::from(key);
            v.vel = 1.0 - sens + sens * vel;
            v.env.gate_on();
        }
    }

    fn render(&mut self, io: &mut AudioIo, from: usize, to: usize) {
        let sr = self.sample_rate;
        let gain = self.voices.controllers.gain();
        let (a, r) = (self.p.target(id::ATTACK), self.p.target(id::RELEASE));
        for v in self.voices.all() {
            v.env.set(a, 0.1, 1.0, r, sr);
        }
        for i in from..to {
            self.p.tick();
            let p = &self.p;
            let (low, high) = (p.get(id::LOW), p.get(id::HIGH));
            let d = p.get(id::DETUNE) / 200.0;
            let mut sum = 0.0;
            for v in self.voices.active() {
                let e = v.env.next_value() * v.vel;
                let mut s = 0.0;
                for (k, o) in v.osc.iter_mut().enumerate() {
                    let octave = if k < 2 { 0.0 } else { 12.0 };
                    let detune = if k % 2 == 0 { -d } else { d };
                    let level = if k < 2 { low } else { high };
                    let x = o.next_sample(Wave::Saw, note_hz(v.pitch + octave + detune) / sr);
                    s += x * level;
                }
                sum += s * e;
            }
            if i % 8 == 0 {
                self.tone.set(p.get(id::TONE), 0.1, sr);
            }
            let x = self.tone.process(sum, FilterMode::LowPass) * HEADROOM;
            // Ensemble: three delay lines swept by a slow and a fast LFO, 120° apart.
            self.slow = (self.slow + 0.6 / sr).fract();
            self.fast = (self.fast + 5.8 / sr).fract();
            let depth = p.get(id::ENSEMBLE);
            let mut taps = [0.0_f32; 3];
            for (k, (line, tap)) in self.lines.iter_mut().zip(taps.iter_mut()).enumerate() {
                line.push(x);
                let off = k as f32 / 3.0;
                let m = 0.8 * sin_turns(self.slow + off) + 0.2 * sin_turns(self.fast + off);
                *tap = line.read((CHORUS_BASE + CHORUS_DEPTH * depth * m) * sr);
            }
            let wet_l = (taps[0] + 0.5 * taps[1]) / 1.5;
            let wet_r = (taps[2] + 0.5 * taps[1]) / 1.5;
            let k = p.get(id::VOLUME) * gain;
            let out = [(x + (wet_l - x) * depth) * k, (x + (wet_r - x) * depth) * k];
            for (ch, y) in out.into_iter().enumerate() {
                if let Some(o) = io.output(ch).get_mut(i) {
                    *o = y;
                }
            }
        }
    }
}

impl AudioNode for Strings {
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
        let len = ((CHORUS_BASE + CHORUS_DEPTH) * sample_rate) as usize + 4;
        self.lines = std::array::from_fn(|_| DelayLine::new(len));
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
            *v = StringVoice::default();
        }
        for l in &mut self.lines {
            l.clear();
        }
        self.tone.reset();
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
        "Lush strings",
        &[(id::ENSEMBLE, 1.0), (id::DETUNE, 12.0), (id::ATTACK, 0.8)],
    ),
    (
        "Dark pad",
        &[
            (id::HIGH, 0.0),
            (id::TONE, 1_200.0),
            (id::ATTACK, 1.5),
            (id::RELEASE, 2.5),
        ],
    ),
    (
        "Bright violins",
        &[
            (id::LOW, 0.3),
            (id::HIGH, 1.0),
            (id::TONE, 9_000.0),
            (id::ATTACK, 0.15),
        ],
    ),
];
