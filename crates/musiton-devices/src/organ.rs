//! Drawbar organ: nine sine drawbars per voice, percussion on the second or third
//! harmonic (only on notes played after all keys were up), key click, and a rotary
//! speaker (slow/fast with spin-up) for the stereo movement.

use musiton_core::ids::ParamId;
use musiton_core::params::ParamInfo;
use musiton_dsp::delay::DelayLine;
use musiton_dsp::osc::{sin_turns, Noise};
use musiton_engine::node::{AudioIo, AudioNode, Event, ProcessContext};

use crate::voices::{handle_event, note_hz, split_block, Params, Voice, Voices};

/// Number of voices.
const VOICES: usize = 16;
/// Output headroom.
const HEADROOM: f32 = 0.06;
/// Pitch of each drawbar relative to the played note (16' … 1').
const FOOTAGES: [f32; 9] = [0.5, 1.5, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 8.0];
/// Rotary speeds (Hz) for slow and fast.
const ROTOR_SLOW: f32 = 0.8;
const ROTOR_FAST: f32 = 6.5;

const ROTARY: &[&str] = &["Off", "Slow", "Fast"];
const PERC_HARMONIC: &[&str] = &["Second", "Third"];

/// Parameter ids. Drawbar `n` (0..9, 16' first) is `DRAWBAR + n`.
#[allow(missing_docs)]
pub mod id {
    pub const DRAWBAR: u64 = 0;
    pub const PERCUSSION: u64 = 9;
    pub const PERC_HARMONIC: u64 = 10;
    pub const PERC_DECAY: u64 = 11;
    pub const CLICK: u64 = 12;
    pub const ROTARY: u64 = 13;
    pub const DRIVE: u64 = 14;
    pub const VOLUME: u64 = 15;
}

const fn drawbar(n: u64, name: &'static str, default: f32) -> ParamInfo {
    ParamInfo::new(id::DRAWBAR + n, name, 0.0, 8.0, default)
}

/// All parameters (index = id).
pub const PARAMS: [ParamInfo; 16] = [
    drawbar(0, "16'", 6.0),
    drawbar(1, "5⅓'", 0.0),
    drawbar(2, "8'", 8.0),
    drawbar(3, "4'", 4.0),
    drawbar(4, "2⅔'", 0.0),
    drawbar(5, "2'", 0.0),
    drawbar(6, "1⅗'", 0.0),
    drawbar(7, "1⅓'", 0.0),
    drawbar(8, "1'", 0.0),
    ParamInfo::toggle(id::PERCUSSION, "Percussion", false),
    ParamInfo::choice(id::PERC_HARMONIC, "Perc harmonic", PERC_HARMONIC, 1),
    ParamInfo::new(id::PERC_DECAY, "Perc decay", 0.1, 1.5, 0.4)
        .log()
        .unit("s"),
    ParamInfo::new(id::CLICK, "Key click", 0.0, 1.0, 0.3),
    ParamInfo::choice(id::ROTARY, "Rotary", ROTARY, 1),
    ParamInfo::new(id::DRIVE, "Drive", 0.0, 1.0, 0.2),
    ParamInfo::new(id::VOLUME, "Volume", 0.0, 1.0, 0.7),
];

#[derive(Debug, Clone, Default)]
struct OrganVoice {
    pitch: f32,
    phase: [f32; 9],
    /// Key contact (0 … 1, a few milliseconds each way).
    level: f32,
    on: bool,
    perc: f32,
    click: f32,
}

impl Voice for OrganVoice {
    fn release(&mut self) {
        self.on = false;
    }

    fn is_active(&self) -> bool {
        self.on || self.level > 1e-4
    }
}

/// The organ node (no inputs, stereo out).
#[derive(Debug)]
pub struct Organ {
    p: Params<16>,
    voices: Voices<OrganVoice>,
    sample_rate: f32,
    noise: Noise,
    /// Rotary speaker: horn and drum rotor phases (turns) and speed (Hz).
    horn: f32,
    drum: f32,
    speed: f32,
    doppler: [DelayLine; 2],
}

impl Default for Organ {
    fn default() -> Self {
        Self {
            p: Params::new(&PARAMS),
            voices: Voices::default(),
            sample_rate: 48_000.0,
            noise: Noise::default(),
            horn: 0.0,
            drum: 0.25,
            speed: ROTOR_SLOW,
            doppler: [DelayLine::default(), DelayLine::default()],
        }
    }
}

impl Organ {
    fn note_on(&mut self, key: u8) {
        // Percussion only sounds on a note played after every key was up.
        let fresh = !self.voices.any_held();
        let click = self.p.target(id::CLICK);
        if let Some(v) = self.voices.note_on(key) {
            v.pitch = f32::from(key);
            v.on = true;
            v.perc = if fresh { 1.0 } else { 0.0 };
            v.click = click;
        }
    }

    fn render(&mut self, io: &mut AudioIo, from: usize, to: usize) {
        let sr = self.sample_rate;
        let gain = self.voices.controllers.gain();
        let contact = 1.0 - (-1.0 / (0.003 * sr)).exp();
        let click_decay = (-1.0 / (0.004 * sr)).exp();
        let rotary = self.p.index(id::ROTARY);
        let target_speed = if rotary == 2 { ROTOR_FAST } else { ROTOR_SLOW };
        // The rotors take about a second to change speed.
        let spin = 1.0 - (-1.0 / (0.7 * sr)).exp();
        let perc_on = self.p.target(id::PERCUSSION) > 0.5;
        let perc_bar = if self.p.index(id::PERC_HARMONIC) == 0 {
            3
        } else {
            4
        };
        for i in from..to {
            self.p.tick();
            let p = &self.p;
            let bars: [f32; 9] = std::array::from_fn(|b| p.get(id::DRAWBAR + b as u64) / 8.0);
            let perc_decay = (-4.6 / (p.get(id::PERC_DECAY) * sr)).exp();
            let mut out = 0.0;
            for v in self.voices.active() {
                let target = if v.on { 1.0 } else { 0.0 };
                v.level += (target - v.level) * contact;
                let base = note_hz(v.pitch) / sr;
                let mut s = 0.0;
                for (b, (ph, ratio)) in v.phase.iter_mut().zip(FOOTAGES).enumerate() {
                    let inc = base * ratio;
                    // Drawbars above the audible range stay silent (no aliasing).
                    if inc < 0.45 {
                        let w = sin_turns(*ph);
                        s += w * bars.get(b).copied().unwrap_or(0.0);
                        if perc_on && b == perc_bar {
                            s += w * v.perc * 1.5;
                        }
                    }
                    *ph = (*ph + inc).fract();
                }
                v.perc *= perc_decay;
                let click = v.click * self.noise.next_sample() * 0.5;
                v.click *= click_decay;
                out += (s + click) * v.level;
            }
            // Gentle tube-like drive.
            let drive = 1.0 + 4.0 * p.get(id::DRIVE);
            let x = (out * HEADROOM * drive).tanh() / drive.sqrt() * p.get(id::VOLUME) * gain;
            let (l, r) = if rotary == 0 {
                (x, x)
            } else {
                self.speed += (target_speed - self.speed) * spin;
                self.horn = (self.horn + self.speed / sr).fract();
                self.drum = (self.drum + self.speed * 0.85 / sr).fract();
                let h = sin_turns(self.horn);
                let d = sin_turns(self.drum);
                // Horn and drum swing in opposite directions: level and a little pitch
                // (doppler) per side.
                let mut side = [0.0; 2];
                for (ch, (line, sign)) in self.doppler.iter_mut().zip([1.0, -1.0]).enumerate() {
                    line.push(x);
                    let wobble = line.read(sr * (0.0012 + 0.0006 * sign * h));
                    let amp = 1.0 + 0.35 * sign * h - 0.15 * sign * d;
                    if let Some(s) = side.get_mut(ch) {
                        *s = wobble * amp;
                    }
                }
                (side[0], side[1])
            };
            for (ch, y) in [l, r].into_iter().enumerate() {
                if let Some(o) = io.output(ch).get_mut(i) {
                    *o = y;
                }
            }
        }
    }
}

impl AudioNode for Organ {
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
        let len = (sample_rate * 0.01) as usize;
        self.doppler = [DelayLine::new(len), DelayLine::new(len)];
    }

    fn process(&mut self, _ctx: &ProcessContext, io: &mut AudioIo, events: &[Event]) {
        let n = io.frames();
        split_block(
            self,
            n,
            events,
            |s, a, b| s.render(io, a, b),
            |s, e| {
                if let Some((key, _)) = handle_event(&mut s.voices, &mut s.p, e) {
                    s.note_on(key);
                }
            },
        );
    }

    fn reset(&mut self) {
        for v in self.voices.all() {
            *v = OrganVoice::default();
        }
        for d in &mut self.doppler {
            d.clear();
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
        "Full organ",
        &[
            (id::DRAWBAR, 8.0),
            (id::DRAWBAR + 1, 8.0),
            (id::DRAWBAR + 3, 8.0),
            (id::DRAWBAR + 4, 8.0),
            (id::DRAWBAR + 5, 8.0),
            (id::DRAWBAR + 6, 6.0),
            (id::DRAWBAR + 7, 6.0),
            (id::DRAWBAR + 8, 8.0),
            (id::ROTARY, 2.0),
            (id::DRIVE, 0.5),
        ],
    ),
    (
        "Jazz",
        &[
            (id::DRAWBAR, 8.0),
            (id::DRAWBAR + 1, 8.0),
            (id::DRAWBAR + 2, 8.0),
            (id::PERCUSSION, 1.0),
            (id::CLICK, 0.5),
        ],
    ),
    (
        "Gospel",
        &[
            (id::DRAWBAR, 8.0),
            (id::DRAWBAR + 1, 8.0),
            (id::DRAWBAR + 3, 8.0),
            (id::DRAWBAR + 8, 8.0),
            (id::ROTARY, 2.0),
            (id::DRIVE, 0.4),
        ],
    ),
    (
        "Soft flute",
        &[
            (id::DRAWBAR, 0.0),
            (id::DRAWBAR + 2, 6.0),
            (id::CLICK, 0.0),
            (id::DRIVE, 0.0),
        ],
    ),
];
